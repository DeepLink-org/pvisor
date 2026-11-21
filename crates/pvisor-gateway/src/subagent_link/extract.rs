//! Extract subagent ids and spawn hints from Claude traffic.

use std::collections::HashSet;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

use super::types::SpawnHint;

static AGENT_ID_JSON: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"(?i)"agentId"\s*:\s*"([a-zA-Z0-9_-]{4,})""#));
/// Claude Code tool_result line: `agentId: a583b6c00c3d06436` (not CLI `--agent-id`).
static AGENT_ID_TOOL_RESULT: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?m)(?:^|\n)\s*agentId:\s*([a-zA-Z0-9_-]{4,})"));
static TOOL_AGENT_BLOCK: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"```tool:(?:Agent|Task)\s*\n([\s\S]*?)```"));
/// Design-doc paths in current bilingual and older unlocalized prompts.
static DOC_TARGET: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"docs/src/(?:en/|zh/)?design/[^\s\)\]]+\.md"));
static TASK_ID_XML: LazyLock<std::result::Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?i)<task-id>\s*([a-zA-Z0-9_-]{4,})\s*</task-id>"));

fn compiled(regex: &LazyLock<std::result::Result<Regex, regex::Error>>) -> Result<&Regex> {
    regex
        .as_ref()
        .map_err(|error| anyhow::Error::new(error.clone()))
        .context("compile subagent extraction regex")
}

pub(crate) fn extract_doc_target(text: &str) -> Result<Option<String>> {
    Ok(compiled(&DOC_TARGET)?
        .find(text)
        .map(|m| m.as_str().to_string()))
}

/// Extract sorted unique IDs; a matcher initialization failure returns an error.
pub fn extract_subagent_ids_from_request(body: &Value) -> Result<Vec<String>> {
    let mut ids = HashSet::new();
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return Ok(Vec::new());
    };
    for msg in messages {
        let Some(content) = msg.get("content") else {
            continue;
        };
        match content {
            Value::Array(parts) => {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        if let Some(inner) = part.get("content") {
                            collect_subagent_ids_from_value(inner, &mut ids)?;
                        }
                    } else if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        for id in extract_subagent_ids_from_text(text)? {
                            ids.insert(id);
                        }
                    } else {
                        collect_subagent_ids_from_value(part, &mut ids)?;
                    }
                }
            }
            other => collect_subagent_ids_from_value(other, &mut ids)?,
        }
    }
    let mut out: Vec<_> = ids.into_iter().collect();
    out.sort();
    Ok(out)
}

fn collect_subagent_ids_from_value(v: &Value, out: &mut HashSet<String>) -> Result<()> {
    match v {
        Value::String(s) => {
            for id in extract_subagent_ids_from_text(s)? {
                out.insert(id);
            }
        }
        Value::Array(parts) => {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    for id in extract_subagent_ids_from_text(t)? {
                        out.insert(id);
                    }
                }
            }
        }
        Value::Object(map) => {
            if let Some(id) = map
                .get("agentId")
                .or_else(|| map.get("agent_id"))
                .and_then(|x| x.as_str())
                .filter(|s| plausible_agent_id(s))
            {
                out.insert(id.to_string());
            }
            for v in map.values() {
                collect_subagent_ids_from_value(v, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Extract sorted unique IDs; matcher failures do not become empty results.
///
/// ```
/// use pvisor_gateway::subagent_link::extract_subagent_ids_from_text;
/// assert_eq!(extract_subagent_ids_from_text("agentId: abcd\nagentId: abcd")?, vec!["abcd"]);
/// assert!(extract_subagent_ids_from_text("--agent-id abcd")?.is_empty());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn extract_subagent_ids_from_text(text: &str) -> Result<Vec<String>> {
    let mut ids = HashSet::new();
    for cap in compiled(&AGENT_ID_JSON)?.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str())
            && plausible_agent_id(id)
        {
            ids.insert(id.to_string());
        }
    }
    for cap in compiled(&AGENT_ID_TOOL_RESULT)?.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str())
            && plausible_agent_id(id)
        {
            ids.insert(id.to_string());
        }
    }
    for cap in compiled(&TASK_ID_XML)?.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str())
            && plausible_agent_id(id)
        {
            ids.insert(id.to_string());
        }
    }
    let mut out: Vec<_> = ids.into_iter().collect();
    out.sort();
    Ok(out)
}

/// Extract spawn hints, ignoring malformed tool JSON but returning matcher failures.
pub fn extract_spawn_hints_from_assistant(text: &str) -> Result<Vec<SpawnHint>> {
    let mut hints = Vec::new();
    for cap in compiled(&TOOL_AGENT_BLOCK)?.captures_iter(text) {
        let Some(json_str) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json_str.trim()) else {
            continue;
        };
        let subagent_type = v
            .get("subagent_type")
            .or_else(|| v.get("agent_type"))
            .or_else(|| v.get("description"))
            .and_then(|x| x.as_str())
            .unwrap_or("Agent");
        let description = v
            .get("description")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let prompt = v.get("prompt").and_then(|x| x.as_str()).map(str::to_string);
        let doc_target = prompt
            .as_deref()
            .map(extract_doc_target)
            .transpose()?
            .flatten();
        hints.push(SpawnHint {
            subagent_type: subagent_type.to_string(),
            description,
            prompt,
            doc_target,
        });
    }
    Ok(hints)
}

fn plausible_agent_id(id: &str) -> bool {
    id.len() >= 4
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    fn invalid_matcher_returns_error_without_poisoning_initializer() {
        let invalid: LazyLock<std::result::Result<Regex, regex::Error>> =
            LazyLock::new(|| regex::RegexBuilder::new(".+").size_limit(0).build());
        for _ in 0..2 {
            let error = compiled(&invalid).unwrap_err();
            assert!(error.downcast_ref::<regex::Error>().is_some());
        }
    }
}
