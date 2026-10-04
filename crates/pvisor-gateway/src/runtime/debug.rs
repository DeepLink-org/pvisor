//! Debug logging for capture proxy: all HTTP dispatch + captured LLM request/response.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::ProxyConfig;

pub const ENV_CAPTURE_DEBUG: &str = "PVISOR_CAPTURE_DEBUG";
/// Mirror `[capture-debug]` lines to stderr (default: file only).
pub const ENV_CAPTURE_DEBUG_STDERR: &str = "PVISOR_CAPTURE_DEBUG_STDERR";

const MAX_BODY_CHARS: usize = 8192;
static DEBUG_STDERR_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn debug_flag_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("debug.enabled")
}

pub fn debug_log_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("debug.log")
}

/// Enable debug until flag file is removed (`capture run --debug` / CLI helper).
pub fn enable_debug(storage: &Path) -> anyhow::Result<()> {
    let path = debug_flag_path(storage);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, "1\n")?;
    Ok(())
}

pub fn is_debug_enabled(cfg: &ProxyConfig, storage: &Path) -> bool {
    cfg.debug || env_truthy(ENV_CAPTURE_DEBUG) || debug_flag_path(storage).is_file()
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Mirror capture diagnostics to stderr for the lifetime of this process.
///
/// This is intended for foreground CLI commands. The atomic switch avoids
/// mutating process environment variables after an async runtime has started.
pub fn enable_debug_stderr() {
    DEBUG_STDERR_ENABLED.store(true, Ordering::Relaxed);
}

fn mirror_debug_to_stderr() -> bool {
    DEBUG_STDERR_ENABLED.load(Ordering::Relaxed) || env_truthy(ENV_CAPTURE_DEBUG_STDERR)
}

fn emit(storage: &Path, line: &str) {
    let stamped = format!("{} {line}", chrono::Utc::now().to_rfc3339());
    if mirror_debug_to_stderr() {
        eprintln!("[capture-debug] {line}");
    }
    let log_path = debug_log_path(storage);
    if let Some(dir) = log_path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(f, "{stamped}");
    }
}

pub fn truncate_body_bytes(raw: &[u8]) -> String {
    let redacted = crate::sink::redact_sensitive_wire_text(&String::from_utf8_lossy(raw));
    let raw = redacted.as_bytes();
    if raw.len() <= MAX_BODY_CHARS {
        return String::from_utf8_lossy(raw).into_owned();
    }
    format!(
        "{}…[truncated]",
        String::from_utf8_lossy(&raw[..MAX_BODY_CHARS])
    )
}

pub fn truncate_body(raw: &str) -> String {
    let redacted = crate::sink::redact_sensitive_wire_text(raw);
    let raw = redacted.as_str();
    if raw.chars().count() <= MAX_BODY_CHARS {
        return raw.to_string();
    }
    let mut end = 0;
    for (n, (i, c)) in raw.char_indices().enumerate() {
        if n >= MAX_BODY_CHARS {
            break;
        }
        end = i + c.len_utf8();
    }
    format!("{}…[truncated]", &raw[..end])
}

pub fn log_daemon_start(storage: &Path, listen: &str, version: &str) {
    emit(
        storage,
        &format!("capture daemon.start listen={listen} version={version}"),
    );
}

pub fn log_connect(storage: &Path, target: &str, session_id: &str) {
    emit(
        storage,
        &format!("CONNECT target={target} session={session_id}"),
    );
}

pub fn log_network_denied(storage: &Path, host: &str, mode: &str, reason: &str, session_id: &str) {
    emit(
        storage,
        &format!("network.denied host={host} mode={mode} reason={reason} session={session_id}"),
    );
}

pub fn log_forward(
    storage: &Path,
    method: &str,
    url: &str,
    session_id: &str,
    status: u16,
    body: &str,
) {
    let url = crate::sink::redact_sensitive_url(url);
    emit(
        storage,
        &format!(
            "forward {method} {url} session={session_id} status={status} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_dispatch(storage: &Path, method: &str, uri: &str, session_id: &str, mode: &str) {
    let uri = crate::sink::redact_sensitive_url(uri);
    emit(
        storage,
        &format!("dispatch {method} {uri} session={session_id} mode={mode}"),
    );
}

pub fn log_proxy_result(storage: &Path, target: &str, status: u16, session_id: &str) {
    emit(
        storage,
        &format!("network.proxy_result target={target} status={status} session={session_id}"),
    );
}

pub fn log_proxy_error(storage: &Path, target: &str, error: &str, session_id: &str) {
    emit(
        storage,
        &format!("network.proxy_error target={target} error={error} session={session_id}"),
    );
}

/// Shared call identity for one LLM debug message.
pub struct LlmDebugContext<'a> {
    pub session_id: &'a str,
    pub agent_id: &'a str,
    pub model: &'a str,
}

pub fn log_llm_request(
    storage: &Path,
    context: LlmDebugContext<'_>,
    protocol: &str,
    path: &str,
    upstream: &str,
    body: &str,
) {
    let LlmDebugContext {
        session_id,
        agent_id,
        model,
    } = context;
    let upstream = crate::sink::redact_sensitive_url(upstream);
    emit(
        storage,
        &format!(
            "capture llm.request session={session_id} agent={agent_id} model={model} \
             protocol={protocol} path={path} upstream={upstream} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_llm_auth_resolved(storage: &Path, session_id: &str, source: &str) {
    emit(
        storage,
        &format!("capture llm.auth session={session_id} source={source}"),
    );
}

pub fn log_llm_upstream_sending(storage: &Path, session_id: &str, upstream: &str) {
    let upstream = crate::sink::redact_sensitive_url(upstream);
    emit(
        storage,
        &format!("capture llm.upstream.sending session={session_id} upstream={upstream}"),
    );
}

pub fn log_llm_upstream_headers(
    storage: &Path,
    context: LlmDebugContext<'_>,
    upstream: &str,
    status: u16,
    content_type: &str,
    stream_request: bool,
) {
    let LlmDebugContext {
        session_id,
        agent_id,
        model,
    } = context;
    let upstream = crate::sink::redact_sensitive_url(upstream);
    emit(
        storage,
        &format!(
            "capture llm.upstream.headers session={session_id} agent={agent_id} model={model} \
             upstream={upstream} status={status} content_type={content_type} stream_request={stream_request}"
        ),
    );
}

pub fn log_llm_stream_start(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    status: u16,
) {
    emit(
        storage,
        &format!(
            "capture llm.response.start session={session_id} agent={agent_id} model={model} \
             status={status} streaming=true"
        ),
    );
}

pub fn log_llm_response(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    status: u16,
    total_tokens: u64,
    body: &str,
) {
    emit(
        storage,
        &format!(
            "capture llm.response session={session_id} agent={agent_id} model={model} \
             status={status} total_tokens={total_tokens} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_llm_upstream_error(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    upstream: &str,
    error: &str,
) {
    let error = error.replace(upstream, &crate::sink::redact_sensitive_url(upstream));
    let upstream = crate::sink::redact_sensitive_url(upstream);
    emit(
        storage,
        &format!(
            "capture llm.error session={session_id} agent={agent_id} model={model} \
             upstream={upstream} error={error}"
        ),
    );
}

#[cfg(test)]
mod tests {

    #[test]
    fn debug_redacts_before_truncating_bodies_and_urls() {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::json!({"api_key":"debug-private-key","padding":"x".repeat(9000)})
            .to_string();
        assert!(!truncate_body_bytes(body.as_bytes()).contains("debug-private-key"));
        log_llm_upstream_error(
            dir.path(),
            "s",
            "a",
            "m",
            "https://example.com?key=debug-url-key",
            "error at https://example.com?key=debug-url-key",
        );
        let text = std::fs::read_to_string(debug_log_path(dir.path())).unwrap();
        assert!(!text.contains("debug-url-key"));
    }
    use super::*;

    #[test]
    fn truncate_long_body() {
        let s = "x".repeat(9000);
        let t = truncate_body(&s);
        assert!(t.contains("truncated"));
        assert!(t.len() < 9000);
    }
}
