//! DeepSeek Chat Completions multi-turn tool-call reasoning replay.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Per-session cache: tool_call_id → reasoning_content to echo on follow-up requests.
#[derive(Debug, Default)]
pub struct ReasoningCache {
    by_tool_call: HashMap<String, String>,
}

impl ReasoningCache {
    pub fn remember(&mut self, tool_call_ids: &[String], reasoning: &str) {
        if reasoning.is_empty() {
            return;
        }
        if reasoning.len() > 16 * 1024 {
            return;
        }
        for id in tool_call_ids {
            if id.is_empty() || id.len() > 4096 {
                continue;
            }
            if self.by_tool_call.len() >= 64 {
                self.by_tool_call.clear();
            }
            self.by_tool_call.insert(id.clone(), reasoning.to_string());
        }
    }

    pub fn get(&self, tool_call_id: &str) -> Option<&str> {
        self.by_tool_call.get(tool_call_id).map(String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct ReasoningCacheHandle {
    inner: Arc<Mutex<HashMap<ReasoningScope, ReasoningCache>>>,
    scope: ReasoningScope,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
struct ReasoningScope {
    agent: String,
    story: String,
    provider: String,
    upstream: Option<String>,
}

impl ReasoningScope {
    fn bytes(&self) -> usize {
        fn quoted_len(value: &str) -> usize {
            value.bytes().fold(2usize, |total, byte| {
                total.saturating_add(match byte {
                    b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
                    0..=31 => 6,
                    _ => 1,
                })
            })
        }
        // Preserve the former JSON tuple's byte limit without serializing its key.
        5usize
            .saturating_add(quoted_len(&self.agent))
            .saturating_add(quoted_len(&self.story))
            .saturating_add(quoted_len(&self.provider))
            .saturating_add(self.upstream.as_deref().map_or(4, quoted_len))
    }
}

impl ReasoningCacheHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn scoped(&self, ctx: &crate::engine::CallContext) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            scope: ReasoningScope {
                agent: ctx.agent_id().to_string(),
                story: ctx.story.story_id().as_str().to_string(),
                provider: ctx.provider.as_str().to_string(),
                upstream: ctx.upstream_url.clone(),
            },
        }
    }

    pub fn remember(&self, tool_call_ids: &[String], reasoning: &str) {
        if self.scope.bytes() > 8192 {
            return;
        }
        if let Ok(mut g) = self.inner.lock() {
            if !g.contains_key(&self.scope) && g.len() >= 16 {
                g.clear();
            }
            g.entry(self.scope.clone())
                .or_default()
                .remember(tool_call_ids, reasoning);
        }
    }

    pub fn apply_to_messages(&self, messages: &mut [Value]) {
        let Ok(cache) = self.inner.lock() else {
            return;
        };
        if let Some(cache) = cache.get(&self.scope) {
            apply_deepseek_message_fixup(messages, cache);
        } else {
            apply_deepseek_message_fixup(messages, &ReasoningCache::default());
        }
    }
}

/// Add cached or empty `reasoning_content` on assistant messages with `tool_calls`.
pub fn apply_deepseek_message_fixup(messages: &mut [Value], cache: &ReasoningCache) {
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(tool_calls) = obj.get("tool_calls").and_then(|t| t.as_array()) else {
            continue;
        };
        if tool_calls.is_empty() {
            continue;
        }
        if obj
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            continue;
        }
        for tc in tool_calls {
            let Some(id) = tc.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(reasoning) = cache.get(id) {
                obj.insert("reasoning_content".into(), json!(reasoning));
                break;
            }
        }
        if !obj.contains_key("reasoning_content") {
            obj.insert("reasoning_content".into(), json!(""));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::no_upstream(None)]
    #[case::escaped_upstream(Some("https://example.invalid/\"\\\n\r\t\u{0001}"))]
    fn typed_scope_counts_escaped_controls(#[case] upstream: Option<&str>) {
        let scope = ReasoningScope {
            agent: "\u{0000}\u{0008}\u{000c}\r\n\t\"\\".into(),
            story: "你好😀".into(),
            provider: "test".into(),
            upstream: upstream.map(str::to_owned),
        };
        assert_eq!(
            scope.bytes(),
            serde_json::to_string(&(&scope.agent, &scope.story, &scope.provider, &scope.upstream))
                .unwrap()
                .len()
        );
    }

    proptest::proptest! {
        #[test]
        fn typed_scope_retains_json_byte_limit(
            agent in ".{0,32}", story in ".{0,32}", provider in ".{0,32}",
            upstream in proptest::option::of(".{0,32}"),
        ) {
            let scope = ReasoningScope { agent, story, provider, upstream };
            let encoded = serde_json::to_string(&(
                &scope.agent, &scope.story, &scope.provider, &scope.upstream,
            )).unwrap();
            proptest::prop_assert_eq!(scope.bytes(), encoded.len());
        }
    }

    #[test]
    fn scoped_reasoning_does_not_cross_story_or_provider() {
        let handle = ReasoningCacheHandle::new();
        let mut ctx = crate::engine::CallContext::new(
            crate::engine::StoryContext::from_route(
                crate::session::storage::CaptureRoute {
                    root_session: Some("run".into()),
                    session_id: "s1".into(),
                    storage_session_id: "s1".into(),
                    subagent_id: None,
                },
                "agent",
            ),
            crate::Call::from_headers(&axum::http::HeaderMap::new()),
            Vec::new(),
            crate::engine::CallCaptureConfig {
                level: crate::config::CaptureLevel::Full,
                client_model: "m".into(),
                upstream_model: "m".into(),
                provider: crate::provider::ProviderKind::OpenAi,
                protocol: crate::protocol::ProtocolKind::Responses,
                debug_on: false,
            },
        );
        ctx.attach_upstream_url("https://one.invalid/v1/chat/completions");
        handle.scoped(&ctx).remember(&["same".into()], "one");
        let message = json!({"role":"assistant", "tool_calls":[{"id":"same"}]});
        let mut own = vec![message.clone()];
        handle.scoped(&ctx).apply_to_messages(&mut own);
        assert_eq!(own[0]["reasoning_content"], "one");
        ctx.attach_upstream_url("https://other.invalid/v1/chat/completions");
        let mut other = vec![message];
        handle.scoped(&ctx).apply_to_messages(&mut other);
        assert_eq!(other[0]["reasoning_content"], "");
        let mut cache = ReasoningCache::default();
        for i in 0..1000 {
            cache.remember(&[i.to_string()], "reason");
        }
        assert!(cache.by_tool_call.len() <= 64);
    }

    #[test]
    fn injects_empty_reasoning_for_tool_call_assistant() {
        let mut cache = ReasoningCache::default();
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "shell", "arguments": "{}"}}]
        })];
        apply_deepseek_message_fixup(&mut msgs, &cache);
        assert_eq!(msgs[0]["reasoning_content"], "");

        cache.remember(&["call_1".into()], "thinking...");
        apply_deepseek_message_fixup(&mut msgs, &cache);
        assert_eq!(msgs[0]["reasoning_content"], "thinking...");
    }
}
