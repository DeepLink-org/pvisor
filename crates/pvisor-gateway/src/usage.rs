//! Token usage extraction and rough USD cost estimation.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ProviderKind;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// Streaming capture metrics (TTFT + incremental usage).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamMetrics {
    pub usage: TokenUsage,
    pub ttft_ms: Option<u64>,
}

impl TokenUsage {
    /// Add every counter, leaving this value unchanged if any counter overflows.
    ///
    /// ```
    /// use pvisor_gateway::usage::TokenUsage;
    /// let mut usage = TokenUsage { output_tokens: u64::MAX, ..Default::default() };
    /// let before = usage.clone();
    /// let delta = TokenUsage { input_tokens: 1, output_tokens: 1, ..Default::default() };
    /// assert!(usage.merge(&delta).is_err());
    /// assert_eq!(usage, before);
    /// usage.merge(&TokenUsage { input_tokens: 2, ..Default::default() })?;
    /// assert_eq!(usage.input_tokens, 2);
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn merge(&mut self, other: &TokenUsage) -> Result<()> {
        let merged = Self {
            input_tokens: checked_token_add(self.input_tokens, other.input_tokens, "input_tokens")?,
            output_tokens: checked_token_add(
                self.output_tokens,
                other.output_tokens,
                "output_tokens",
            )?,
            total_tokens: checked_token_add(self.total_tokens, other.total_tokens, "total_tokens")?,
            cache_read_tokens: checked_token_add(
                self.cache_read_tokens,
                other.cache_read_tokens,
                "cache_read_tokens",
            )?,
            cache_write_tokens: checked_token_add(
                self.cache_write_tokens,
                other.cache_write_tokens,
                "cache_write_tokens",
            )?,
            reasoning_tokens: checked_token_add(
                self.reasoning_tokens,
                other.reasoning_tokens,
                "reasoning_tokens",
            )?,
        };
        *self = merged;
        Ok(())
    }
}

pub(crate) fn checked_token_add(left: u64, right: u64, field: &str) -> Result<u64> {
    left.checked_add(right)
        .with_context(|| format!("token usage overflow in {field}"))
}

/// Sum usage from SSE `data:` lines, including nested Messages usage.
///
/// Empty, terminal and malformed JSON lines are ignored. Returns an error if
/// extracting any event or adding any counter overflows; no partial sum is returned.
pub fn extract_usage_from_sse(raw: &str) -> Result<TokenUsage> {
    let mut merged = TokenUsage::default();
    for line in raw.lines() {
        let line = line.trim();
        let Some(json_str) = line.strip_prefix("data:") else {
            continue;
        };
        let json_str = json_str.trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            merged.merge(&extract_usage_from_response(&v)?)?;
            if let Some(msg) = v.get("message") {
                merged.merge(&extract_usage_from_response(msg)?)?;
            }
        }
    }
    Ok(merged)
}

/// Extract provider counters, rejecting an unrepresentable inferred total.
///
/// Missing or non-u64 counters are zero. A missing or zero total is inferred
/// from input plus output; an explicit nonzero total is preserved.
///
/// ```
/// use pvisor_gateway::usage::extract_usage_from_response;
/// use serde_json::json;
/// let usage = extract_usage_from_response(&json!({
///     "usage": {"input_tokens": 7, "output_tokens": 3}
/// }))?;
/// assert_eq!(usage.total_tokens, 10);
/// assert!(extract_usage_from_response(&json!({
///     "usage": {"input_tokens": u64::MAX, "output_tokens": 1}
/// })).is_err());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn extract_usage_from_response(body: &Value) -> Result<TokenUsage> {
    let mut u = TokenUsage::default();
    let usage = body
        .get("usage")
        .or_else(|| body.get("Usage"))
        .or_else(|| body.get("usageMetadata"));
    let Some(usage) = usage else {
        return Ok(u);
    };
    u.input_tokens = pick_u64(
        usage,
        &["prompt_tokens", "input_tokens", "promptTokenCount"],
    );
    u.output_tokens = pick_u64(
        usage,
        &["completion_tokens", "output_tokens", "candidatesTokenCount"],
    );
    u.total_tokens = pick_u64(usage, &["total_tokens", "totalTokenCount"]);
    if u.total_tokens == 0 {
        u.total_tokens = checked_token_add(u.input_tokens, u.output_tokens, "total_tokens")?;
    }
    if let Some(details) = usage.get("prompt_tokens_details") {
        u.cache_read_tokens = pick_u64(details, &["cached_tokens"]);
        u.cache_write_tokens = pick_u64(details, &["cache_write_tokens"]);
    }
    if usage.get("cache_creation_input_tokens").is_some() {
        u.cache_write_tokens = pick_u64(usage, &["cache_creation_input_tokens"]);
    }
    if let Some(details) = usage.get("completion_tokens_details") {
        u.reasoning_tokens = pick_u64(details, &["reasoning_tokens"]);
    }
    if usage.get("cache_read_input_tokens").is_some() {
        u.cache_read_tokens = pick_u64(usage, &["cache_read_input_tokens"]);
    }
    if usage.get("cachedContentTokenCount").is_some() {
        u.cache_read_tokens = pick_u64(usage, &["cachedContentTokenCount"]);
    }
    if usage.get("thoughtsTokenCount").is_some() {
        u.reasoning_tokens = pick_u64(usage, &["thoughtsTokenCount"]);
    }
    Ok(u)
}

fn pick_u64(v: &Value, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return n;
        }
    }
    0
}

/// Per-million-token USD list prices (rough public list; override via config later).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Default for ModelPricing {
    fn default() -> Self {
        Self {
            input_per_million: 1.0,
            output_per_million: 2.0,
        }
    }
}

pub fn default_pricing_table() -> Vec<(&'static str, ModelPricing)> {
    vec![
        (
            "gpt-4o",
            ModelPricing {
                input_per_million: 2.5,
                output_per_million: 10.0,
            },
        ),
        (
            "gpt-4o-mini",
            ModelPricing {
                input_per_million: 0.15,
                output_per_million: 0.6,
            },
        ),
        (
            "claude-3-5-sonnet",
            ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
            },
        ),
        (
            "deepseek-chat",
            ModelPricing {
                input_per_million: 0.27,
                output_per_million: 1.1,
            },
        ),
        (
            "gemini-2.0-flash",
            ModelPricing {
                input_per_million: 0.1,
                output_per_million: 0.4,
            },
        ),
    ]
}

pub fn estimate_cost_usd(model: &str, provider: ProviderKind, usage: &TokenUsage) -> f64 {
    let table = default_pricing_table();
    let model_key = model.to_ascii_lowercase();
    let fallback = ModelPricing {
        input_per_million: match provider {
            ProviderKind::Anthropic => 3.0,
            ProviderKind::Gemini | ProviderKind::Vertex => 0.15,
            _ => 1.0,
        },
        output_per_million: match provider {
            ProviderKind::Anthropic => 15.0,
            ProviderKind::Gemini | ProviderKind::Vertex => 0.6,
            _ => 2.0,
        },
    };
    let price = table
        .iter()
        .filter(|(m, _)| model_key.contains(m))
        .max_by_key(|(m, _)| m.len())
        .map(|(_, p)| p)
        .unwrap_or(&fallback);
    (usage.input_tokens as f64 / 1_000_000.0) * price.input_per_million
        + (usage.output_tokens as f64 / 1_000_000.0) * price.output_per_million
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn longer_model_names_select_their_own_price() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        assert_eq!(
            estimate_cost_usd("gpt-4o-mini", ProviderKind::OpenAi, &usage),
            0.15
        );
        assert_eq!(
            estimate_cost_usd("gpt-4o", ProviderKind::OpenAi, &usage),
            2.5
        );
    }

    #[test]
    fn openai_usage() {
        let body = json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });
        let u = extract_usage_from_response(&body).unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn anthropic_sse_usage() {
        let sse = "event: message_delta\n\
                   data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":12}}\n\n";
        let u = extract_usage_from_sse(sse).unwrap();
        assert_eq!(u.output_tokens, 12);
    }

    #[test]
    fn cost_positive() {
        let u = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        let c = estimate_cost_usd("gpt-4o", ProviderKind::OpenAi, &u);
        assert!(c > 2.0);
    }
}
