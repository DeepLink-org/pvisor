//! Post-commit capture observers. Journal is the only fact store.
use super::record::{CaptureRecord, now_rfc3339};
use crate::Call;
use crate::config::CaptureLevel;
use anyhow::Result;
use pvisor_core::event::Event;
use pvisor_journal::Journal;
use serde_json::Value;
use std::sync::Arc;

pub trait CaptureEventObserver: Send + Sync {
    fn observe(&self, event: &Event) -> Result<()>;
    /// Share the owner's journal, e.g. Gateway and Run within one recording.
    fn journal(&self) -> Option<Journal> {
        None
    }
}

pub struct JournalObserver {
    pub journal: Journal,
}
impl CaptureEventObserver for JournalObserver {
    fn observe(&self, _event: &Event) -> Result<()> {
        Ok(())
    }
    fn journal(&self) -> Option<Journal> {
        Some(self.journal.clone())
    }
}

pub struct NoopCaptureObserver;
impl NoopCaptureObserver {
    pub fn new() -> Self {
        Self
    }
}
impl Default for NoopCaptureObserver {
    fn default() -> Self {
        Self
    }
}
impl CaptureEventObserver for NoopCaptureObserver {
    fn observe(&self, _event: &Event) -> Result<()> {
        Ok(())
    }
}

pub struct CallbackObserver {
    #[allow(clippy::type_complexity)]
    callback: Arc<dyn Fn(&Event) -> Result<()> + Send + Sync>,
}
impl CallbackObserver {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(&Event) -> Result<()> + Send + Sync + 'static,
    {
        Self {
            callback: Arc::new(callback),
        }
    }
}
impl CaptureEventObserver for CallbackObserver {
    fn observe(&self, event: &Event) -> Result<()> {
        (self.callback)(event)
    }
}

/// Sensitive header names (lowercase) — values replaced with `<redacted>` when recorded.
const REDACT_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
];

const REDACTED_VALUE: &str = "<redacted>";

fn is_sensitive_header_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    REDACT_HEADER_NAMES.contains(&name.as_str())
        || name.ends_with("-api-key")
        || name.ends_with("-token")
        || name.contains("secret-key")
}

fn is_sensitive_field_name(name: &str) -> bool {
    const SECRET_FIELDS: &[&str] = &[
        "apikey",
        "accesstoken",
        "refreshtoken",
        "authorization",
        "password",
        "secret",
        "clientsecret",
        "cookie",
        "setcookie",
        "token",
        "idtoken",
        "sessiontoken",
        "bearertoken",
        "secretaccesskey",
        "privatekey",
    ];
    let normalized = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    SECRET_FIELDS.contains(&normalized.as_str())
}

fn is_sensitive_query_field_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("key") || is_sensitive_field_name(name)
}

/// Return a persistence-safe copy of HTTP headers without mutating the wire
/// request. Header names and duplicates are retained for replay diagnostics.
pub(crate) fn redact_sensitive_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_sensitive_header_name(name) {
                REDACTED_VALUE.to_string()
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

/// Redact credentials carried in URL user-info or well-known query fields.
/// Invalid/relative URL syntax is retained, with its query sanitized when possible.
pub(crate) fn redact_sensitive_url(value: &str) -> String {
    let (mut parsed, scheme_relative) = if value.starts_with("//") {
        match url::Url::parse(&format!("http:{value}")) {
            Ok(url) => (Some(url), true),
            Err(_) => (None, false),
        }
    } else {
        (url::Url::parse(value).ok(), false)
    };

    let value = if let Some(url) = parsed.as_mut() {
        if !url.username().is_empty() {
            let _ = url.set_username(REDACTED_VALUE);
        }
        if url.password().is_some() {
            let _ = url.set_password(Some(REDACTED_VALUE));
        }
        let rendered = url.to_string();
        if scheme_relative {
            rendered
                .strip_prefix("http:")
                .unwrap_or(&rendered)
                .to_string()
        } else {
            rendered
        }
    } else {
        value.to_string()
    };

    let Some((prefix, query_and_fragment)) = value.split_once('?') else {
        return value;
    };
    let (query, fragment) = query_and_fragment
        .split_once('#')
        .map_or((query_and_fragment, None), |(query, fragment)| {
            (query, Some(fragment))
        });
    let pairs = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
    if !pairs
        .iter()
        .any(|(name, _)| is_sensitive_query_field_name(name))
    {
        return value;
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, field_value) in pairs {
        serializer.append_pair(
            &name,
            if is_sensitive_query_field_name(&name) {
                REDACTED_VALUE
            } else {
                &field_value
            },
        );
    }
    let mut redacted_url = format!("{prefix}?{}", serializer.finish());
    if let Some(fragment) = fragment {
        redacted_url.push('#');
        redacted_url.push_str(fragment);
    }
    redacted_url
}

/// Infer keep-alive / persistent connection flags from request headers + HTTP version.
pub fn infer_connection_persistent(
    headers: &[(String, String)],
    http_version: Option<&str>,
) -> (bool, Option<String>, Option<String>, Option<String>) {
    let mut connection_header = None;
    let mut keep_alive = None;
    let mut upgrade = None;
    for (name, value) in headers {
        match name.to_ascii_lowercase().as_str() {
            "connection" => connection_header = Some(value.clone()),
            "keep-alive" => keep_alive = Some(value.clone()),
            "upgrade" => upgrade = Some(value.clone()),
            _ => {}
        }
    }
    let conn_l = connection_header
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let persistent = if conn_l.split(',').any(|p| p.trim() == "close") {
        false
    } else if conn_l.split(',').any(|p| p.trim() == "keep-alive") {
        true
    } else {
        !matches!(http_version.unwrap_or("HTTP/1.1"), v if v.starts_with("HTTP/1.0"))
    };
    (persistent, connection_header, keep_alive, upgrade)
}

/// Attach `connection.*` and `client.*` onto the event payload.
pub fn attach_connection_and_client(
    payload: &mut Value,
    headers: &[(String, String)],
    http_version: Option<&str>,
    client_peer: Option<&str>,
    client_meta: Option<&crate::session::client::SessionClientMeta>,
) {
    let (persistent, connection_header, keep_alive, upgrade) =
        infer_connection_persistent(headers, http_version);
    let mut connection = serde_json::Map::new();
    if let Some(v) = http_version {
        connection.insert("http_version".into(), Value::String(v.to_string()));
    }
    connection.insert("persistent".into(), Value::Bool(persistent));
    if let Some(v) = connection_header {
        connection.insert("connection_header".into(), Value::String(v));
    }
    if let Some(v) = keep_alive {
        connection.insert("keep_alive".into(), Value::String(v));
    }
    if let Some(v) = upgrade {
        connection.insert("upgrade".into(), Value::String(v));
    }
    if let Some(peer) = client_peer {
        // The accepted socket's peer address is stable for the lifetime of a
        // keep-alive connection and does not expose credentials.
        connection.insert("id".into(), Value::String(format!("client:{peer}")));
    }
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("connection".into(), Value::Object(connection));

    let mut client = serde_json::Map::new();
    if let Some(peer) = client_peer {
        client.insert("peer".into(), Value::String(peer.to_string()));
        if let Some((ip, port)) = peer.rsplit_once(':') {
            client.insert("peer_ip".into(), Value::String(ip.to_string()));
            if let Ok(p) = port.parse::<u16>() {
                client.insert("peer_port".into(), Value::Number(p.into()));
            }
        }
    }
    if let Some(meta) = client_meta {
        if client.get("peer").is_none() && !meta.peer.is_empty() {
            client.insert("peer".into(), Value::String(meta.peer.clone()));
        }
        if client.get("peer_port").is_none() {
            client.insert("peer_port".into(), Value::Number(meta.peer_port.into()));
        }
        if meta.pid > 0 {
            client.insert("pid".into(), Value::Number(meta.pid.into()));
        }
        if !meta.command.is_empty() {
            client.insert("command".into(), Value::String(meta.command.clone()));
        }
        if let Some(fp) = &meta.machine_fp {
            client.insert("machine_fp".into(), Value::String(fp.clone()));
        }
    }
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("user-agent") {
            client.insert("user_agent".into(), Value::String(value.clone()));
            break;
        }
    }
    if !client.is_empty() {
        obj.insert("client".into(), Value::Object(client));
    }
}

/// Dual-write RFC-0002 `payload.http.*` request wire fields (keeps flat compat keys).
pub fn attach_http_wire_request(
    payload: &mut Value,
    method: &str,
    path: &str,
    url: Option<&str>,
    body: Option<&Value>,
    body_present: bool,
) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("method".into(), Value::String(method.to_string()));
    if let Some(u) = url {
        obj.insert("url".into(), Value::String(redact_sensitive_url(u)));
    }
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("method".into(), Value::String(method.to_string()));
        http_obj.insert("path".into(), Value::String(path.to_string()));
        if let Some(u) = url {
            http_obj.insert("url".into(), Value::String(redact_sensitive_url(u)));
        }
        if let Some(b) = body {
            http_obj.insert("request_body".into(), redact_sensitive_body(b));
            http_obj.insert("body_encoding".into(), Value::String("json".into()));
        }
    }
    if !body_present {
        obj.insert("degraded".into(), Value::Bool(true));
    }
}

/// Dual-write RFC-0002 `payload.http.*` response wire fields.
pub fn attach_http_wire_response(
    payload: &mut Value,
    status: u16,
    url: Option<&str>,
    body: Option<&Value>,
    body_present: bool,
    streaming: bool,
    headers_present: bool,
) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    if let Some(u) = url {
        obj.insert("url".into(), Value::String(redact_sensitive_url(u)));
    }
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("status".into(), Value::Number(status.into()));
        if let Some(u) = url {
            http_obj.insert("url".into(), Value::String(redact_sensitive_url(u)));
        }
        http_obj.insert("streaming".into(), Value::Bool(streaming));
        if let Some(b) = body {
            http_obj.insert("response_body".into(), redact_sensitive_body(b));
            let enc = if streaming { "sse-wire" } else { "json" };
            http_obj.insert("body_encoding".into(), Value::String(enc.into()));
        }
    }
    if !body_present || !headers_present {
        obj.insert("degraded".into(), Value::Bool(true));
    }
}

/// Redact common credential fields recursively before a JSON body reaches the
/// canonical store. Provider-specific policies can pre-redact additional
/// fields; this is the non-disableable safety floor.
pub fn redact_sensitive_body(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_field_name(key) {
                        Value::String(REDACTED_VALUE.into())
                    } else if matches!(key.as_str(), "url" | "uri" | "upstream_url" | "path") {
                        value
                            .as_str()
                            .map(|text| Value::String(redact_sensitive_url(text)))
                            .unwrap_or_else(|| redact_sensitive_body(value))
                    } else {
                        redact_sensitive_body(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive_body).collect()),
        Value::String(text) => Value::String(redact_sensitive_wire_text(text)),
        _ => value.clone(),
    }
}

/// Apply the same credential floor to JSON and SSE encoded as wire text.
/// Unchanged bodies retain their exact bytes and SSE framing.
pub(crate) fn redact_sensitive_wire_text(text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let redacted = redact_sensitive_body(&value);
        if value != redacted {
            return redacted.to_string();
        }
        return text.to_owned();
    }
    text.split_inclusive('\n')
        .map(|line| {
            let body = line.trim_end_matches(['\r', '\n']);
            let Some(data) = body.strip_prefix("data:") else {
                return line.to_owned();
            };
            let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                return line.to_owned();
            };
            let redacted = redact_sensitive_body(&value);
            if value == redacted {
                return line.to_owned();
            }
            format!("data: {redacted}{}", &line[body.len()..])
        })
        .collect()
}

/// Persist HTTP headers onto an event payload (flat `headers` + nested `http.headers`).
///
/// Sensitive values are replaced with `<redacted>` and `headers_redacted` is set.
/// Empty `headers` still writes an empty object so callers can tell "recorded empty"
/// from "not recorded" only if they omit this call entirely.
pub fn attach_recorded_headers(payload: &mut Value, headers: &[(String, String)]) {
    let mut map = serde_json::Map::new();
    let mut redacted = false;
    for ((name, _), (_, value)) in headers.iter().zip(redact_sensitive_headers(headers)) {
        let key = name.to_ascii_lowercase();
        if is_sensitive_header_name(name) {
            redacted = true;
        }
        map.insert(key, Value::String(value));
    }
    let headers_val = Value::Object(map);
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("headers".into(), headers_val.clone());
    let http = obj
        .entry("http".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(http_obj) = http {
        http_obj.insert("headers".into(), headers_val);
        if redacted {
            http_obj.insert("headers_redacted".into(), Value::Bool(true));
        }
    }
    if redacted {
        obj.insert("headers_redacted".into(), Value::Bool(true));
    }
}

fn stamp_request_payload(payload: &mut Value, body_json: Option<&Value>) {
    if let Some(body) = body_json {
        payload["user_message_count"] =
            serde_json::json!(crate::dialogue_extract::count_visible_user_messages(body));
    }
}

fn attach_call_context(rec: &mut CaptureRecord, call: &Call) {
    rec.trace_id = Some(call.trace_id.clone());
    rec.call_id = Some(call.call_id.clone());
}

#[allow(clippy::too_many_arguments)]
pub fn llm_request_summary_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body_bytes: usize,
    protocol: &str,
    provider: &str,
    user_content: Option<String>,
    forward_to: Option<&str>,
    call: &Call,
    level: CaptureLevel,
    body_json: Option<&Value>,
) -> CaptureRecord {
    let mut payload = serde_json::json!({
        "model": model,
        "path": path,
        "body_bytes": body_bytes,
        "protocol": protocol,
        "provider": provider,
    });
    if level.includes_user_text()
        && let Some(content) = user_content.filter(|s| !s.is_empty())
    {
        payload["user_content"] = serde_json::Value::String(content);
    }
    if let Some(fwd) = forward_to.filter(|s| !s.is_empty() && *s != model) {
        payload["forward_to"] = serde_json::Value::String(fwd.to_string());
    }
    if let Some(body) = body_json {
        stamp_request_payload(&mut payload, Some(body));
        if level.includes_full_body() {
            payload["body"] = redact_sensitive_body(body);
        }
    }
    let mut rec = CaptureRecord {
        event_id: None,
        observed_at_unix_ms: None,

        kind: "llm.request".to_string(),
        timestamp: Some(call.started_at.clone()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}

/// Full request body in payload — tests and fixtures only; production uses [`llm_request_summary_record`].
#[doc(hidden)]
pub fn llm_request_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body: &serde_json::Value,
) -> CaptureRecord {
    CaptureRecord {
        event_id: None,
        observed_at_unix_ms: None,

        kind: "llm.request".to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "model": model,
            "path": path,
            "body": body,
        }),
    }
}

pub fn llm_response_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    body: &serde_json::Value,
    streaming: bool,
    call: &Call,
) -> CaptureRecord {
    let mut rec = CaptureRecord {
        event_id: None,
        observed_at_unix_ms: None,

        kind: if streaming {
            "llm.response.stream".to_string()
        } else {
            "llm.response".to_string()
        },
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "status": status,
            "body": body,
        }),
    };
    attach_call_context(&mut rec, call);
    rec
}

#[allow(clippy::too_many_arguments)]
pub fn llm_response_record_with_content(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    payload: &serde_json::Value,
    streaming: bool,
    assistant_content: Option<String>,
    call: &Call,
    level: CaptureLevel,
) -> CaptureRecord {
    let mut payload = redact_sensitive_body(payload);
    payload["status"] = serde_json::json!(status);
    if level.includes_assistant_text()
        && let Some(content) = assistant_content.filter(|s| !s.is_empty())
    {
        payload["assistant_content"] = serde_json::Value::String(content);
    }
    let kind = if streaming {
        "llm.response.stream"
    } else {
        "llm.response"
    };
    let mut rec = CaptureRecord {
        event_id: None,
        observed_at_unix_ms: None,

        kind: kind.to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}

/// Retention applies to every persisted copy, including enrichment fields.
pub(crate) fn retain_capture_content(payload: &mut Value, level: crate::config::CaptureLevel) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    if !level.includes_full_body() {
        for key in ["body", "llm_request", "llm_response", "spawn_hints"] {
            object.remove(key);
        }
        if let Some(links) = object.get_mut("spawn_links").and_then(Value::as_array_mut) {
            links.retain(Value::is_object);
            for link in links {
                if let Some(link) = link.as_object_mut() {
                    link.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "subagent_type" | "subagent_id" | "subagent_trajectory"
                        )
                    });
                }
            }
        }
        if let Some(http) = object.get_mut("http").and_then(Value::as_object_mut) {
            for key in ["request_body", "response_body", "body_encoding"] {
                http.remove(key);
            }
        }
    }
    if level == crate::config::CaptureLevel::Summary {
        object.remove("user_content");
        object.remove("assistant_content");
    }
}

#[cfg(test)]
mod header_tests {

    #[test]
    fn normal_wire_records_redact_url_credentials_at_all_copies() {
        let mut payload = serde_json::json!({});
        let url = "https://private-user:private-pass@example.com/v1?api_key=private-query&safe=yes";
        super::attach_http_wire_request(&mut payload, "POST", "/v1", Some(url), None, true);
        let request = super::redact_sensitive_body(&payload).to_string();
        super::attach_http_wire_response(&mut payload, 200, Some(url), None, true, false, true);
        let response = super::redact_sensitive_body(&payload).to_string();
        for wire in [request, response] {
            for secret in ["private-user", "private-pass", "private-query"] {
                assert!(!wire.contains(secret));
            }
            assert!(wire.contains("safe=yes"));
        }
    }
    #[test]
    fn encoded_json_and_sse_credentials_are_redacted_without_changing_other_wire_text() {
        let wire = "event: message\r\ndata: {\"api_key\":\"never-record-this\",\"content\":\"ok\"}\r\n\r\ndata: [DONE]\n";
        let redacted = super::redact_sensitive_wire_text(wire);
        assert!(!redacted.contains("never-record-this"));
        assert!(redacted.contains("<redacted>"));
        assert!(redacted.starts_with("event: message\r\n"));
        assert!(redacted.ends_with("\r\n\r\ndata: [DONE]\n"));
        let harmless = "data: { \"content\": \"hello\" }\n\n";
        assert_eq!(super::redact_sensitive_wire_text(harmless), harmless);
        let json = "{\"access_token\":\"secret\"}";
        assert!(!super::redact_sensitive_wire_text(json).contains("secret"));
    }

    use super::{attach_recorded_headers, redact_sensitive_headers, redact_sensitive_url};
    use serde_json::json;

    #[test]
    fn infer_connection_persistent_http11_default() {
        let (p, _, _, _) = super::infer_connection_persistent(&[], Some("HTTP/1.1"));
        assert!(p);
        let (p, _, _, _) = super::infer_connection_persistent(
            &[("Connection".into(), "close".into())],
            Some("HTTP/1.1"),
        );
        assert!(!p);
    }

    #[test]
    fn attach_connection_and_client_writes_peer() {
        let mut payload = json!({});
        super::attach_connection_and_client(
            &mut payload,
            &[("Connection".into(), "keep-alive".into())],
            Some("HTTP/1.1"),
            Some("127.0.0.1:9"),
            None,
        );
        assert_eq!(payload["connection"]["persistent"], true);
        assert_eq!(payload["client"]["peer"], "127.0.0.1:9");
        assert_eq!(payload["client"]["peer_port"], 9);
    }

    #[test]
    fn attach_http_wire_request_sets_nested_fields() {
        let mut payload = json!({"path": "/v1/chat/completions"});
        let body = json!({"messages":[{"role":"user","content":"hi"}]});
        super::attach_http_wire_request(
            &mut payload,
            "POST",
            "/v1/chat/completions",
            Some("//localhost/v1/chat/completions"),
            Some(&body),
            true,
        );
        assert_eq!(payload["method"], "POST");
        assert_eq!(payload["http"]["method"], "POST");
        assert_eq!(payload["http"]["path"], "/v1/chat/completions");
        assert_eq!(payload["http"]["url"], "//localhost/v1/chat/completions");
        assert_eq!(
            payload["http"]["request_body"]["messages"][0]["content"],
            "hi"
        );
        assert!(payload.get("degraded").is_none());
    }

    #[test]
    fn attach_http_wire_marks_degraded_without_body() {
        let mut payload = json!({});
        super::attach_http_wire_request(&mut payload, "GET", "/v1/models", None, None, false);
        assert_eq!(payload["degraded"], true);
    }

    #[test]
    fn nested_body_credentials_are_redacted() {
        let body = serde_json::json!({
            "request": {
                "api_key": "sk-live",
                "clientSecret": "client-live",
                "session-token": "session-live",
                "safe": "x"
            }
        });
        let redacted = super::redact_sensitive_body(&body);
        assert_eq!(redacted["request"]["api_key"], "<redacted>");
        assert_eq!(redacted["request"]["clientSecret"], "<redacted>");
        assert_eq!(redacted["request"]["session-token"], "<redacted>");
        assert_eq!(redacted["request"]["safe"], "x");
    }

    #[test]
    fn persistence_header_copy_redacts_common_and_vendor_credentials() {
        let headers = vec![
            ("Authorization".into(), "Bearer live".into()),
            ("x-goog-api-key".into(), "google-live".into()),
            ("x-vendor-access-token".into(), "vendor-live".into()),
            ("x-request-id".into(), "req-1".into()),
        ];
        let redacted = redact_sensitive_headers(&headers);
        assert_eq!(redacted[0].1, "<redacted>");
        assert_eq!(redacted[1].1, "<redacted>");
        assert_eq!(redacted[2].1, "<redacted>");
        assert_eq!(redacted[3].1, "req-1");
        assert_eq!(
            headers[0].1, "Bearer live",
            "wire headers must be untouched"
        );
    }

    #[test]
    fn persisted_urls_redact_userinfo_and_sensitive_query_fields() {
        let redacted = redact_sensitive_url(
            "https://live-user:live-password@example.com/v1/models?key=live-key&alt=sse",
        );
        assert!(!redacted.contains("live-user"));
        assert!(!redacted.contains("live-password"));
        assert!(!redacted.contains("live-key"));
        assert!(redacted.contains("alt=sse"));

        assert_eq!(
            redact_sensitive_url("//example.com/v1/models?api_key=live-key&safe=kept"),
            "//example.com/v1/models?api_key=%3Credacted%3E&safe=kept"
        );
        assert_eq!(
            redact_sensitive_url("/v1/models?safe=kept"),
            "/v1/models?safe=kept"
        );
    }

    #[test]
    fn attach_recorded_headers_redacts_authorization() {
        let mut payload = json!({"path": "/v1/chat/completions"});
        attach_recorded_headers(
            &mut payload,
            &[
                ("Content-Type".into(), "application/json".into()),
                ("Authorization".into(), "Bearer secret".into()),
            ],
        );
        assert_eq!(payload["headers"]["content-type"], "application/json");
        assert_eq!(payload["headers"]["authorization"], "<redacted>");
        assert_eq!(payload["headers_redacted"], true);
        assert_eq!(payload["http"]["headers"]["authorization"], "<redacted>");
    }
}
