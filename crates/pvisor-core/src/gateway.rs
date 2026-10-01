//! Capture configuration records, independent of protocol conversion.
use serde::{Deserialize, Serialize};

/// Controls how much request/response content is written to trajectory records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureLevel {
    /// Model, path, byte counts — no message text.
    Summary,
    /// User / assistant dialogue text (default).
    #[default]
    Dialogue,
    /// Full parsed JSON bodies in `payload.body`.
    Full,
}

impl CaptureLevel {
    pub fn includes_user_text(self) -> bool {
        !matches!(self, Self::Summary)
    }

    pub fn includes_assistant_text(self) -> bool {
        !matches!(self, Self::Summary)
    }

    pub fn includes_full_body(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    /// Match pattern (exact, `prefix*`, `*suffix`, `*`) or target model id.
    pub name: String,
    /// `openai` | `anthropic` | `gemini` | `vertex` | `bedrock` | `azure` | `copilot` | `custom`
    #[serde(default)]
    pub provider: Option<String>,
    /// OpenAI-compatible upstream base (include API prefix, e.g. `https://api.deepseek.com/v1`).
    #[serde(default)]
    pub upstream: Option<String>,
    /// Anthropic-compatible upstream (e.g. `https://api.deepseek.com/anthropic/v1`). Falls back to `upstream`.
    #[serde(default)]
    pub upstream_anthropic: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Forward to another `models[].name` (exact id): use its upstream and rewrite request `model`.
    #[serde(default)]
    pub forward: Option<String>,
}
