//! A job-local, fail-closed permission prompt channel to the owning TUI.

use crate::NetworkTransport;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static ARMED: AtomicBool = AtomicBool::new(false);
static FILE_BURSTS: OnceLock<Mutex<HashMap<String, (Instant, AuditDecision)>>> = OnceLock::new();
const FILE_BURST: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    File,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditScope {
    pub run_id: String,
    pub attempt_id: String,
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<AuditScope>,
    pub kind: AuditKind,
    pub target: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<NetworkTransport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    Allow,
    Deny,
}

/// Called before the agent starts. The path is not projected into the agent.
pub fn init(socket: PathBuf) {
    let _ = SOCKET.set(socket);
}

/// Start prompting only after the Job record exists and the Agent is ready.
pub fn arm() {
    if SOCKET.get().is_some() {
        ARMED.store(true, Ordering::Release);
    }
}

pub fn enabled() -> bool {
    SOCKET.get().is_some() && ARMED.load(Ordering::Acquire)
}

pub fn configured() -> bool {
    SOCKET.get().is_some()
}

/// Blocks the intercepted operation until the TUI answers. Any IPC failure denies.
pub fn socket() -> Option<PathBuf> {
    SOCKET.get().cloned()
}

pub fn request(prompt: &AuditRequest) -> AuditDecision {
    if !enabled() {
        return AuditDecision::Deny;
    }
    let Some(socket) = SOCKET.get() else {
        return AuditDecision::Deny;
    };
    request_at(socket, "standalone", prompt)
}

/// The caller supplies the immutable Session endpoint and cache namespace.
pub fn request_at(socket: &std::path::Path, session: &str, prompt: &AuditRequest) -> AuditDecision {
    if prompt.kind == AuditKind::File {
        let Ok(mut recent) = FILE_BURSTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
        else {
            return AuditDecision::Deny;
        };
        recent.retain(|_, (at, _)| at.elapsed() < FILE_BURST);
        let key = format!(
            "{}:{session}:{}:{}",
            socket.display(),
            prompt.target,
            prompt.reason
        );
        if let Some((_, decision)) = recent.get(&key) {
            return *decision;
        }
        let decision = request_uncached(socket, prompt);
        recent.insert(key, (Instant::now(), decision));
        return decision;
    }
    request_uncached(socket, prompt)
}

fn request_uncached(socket: &std::path::Path, prompt: &AuditRequest) -> AuditDecision {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return AuditDecision::Deny;
    };
    if serde_json::to_writer(&mut stream, prompt).is_err() || stream.write_all(b"\n").is_err() {
        return AuditDecision::Deny;
    }
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() {
        return AuditDecision::Deny;
    }
    serde_json::from_str(&line).unwrap_or(AuditDecision::Deny)
}
