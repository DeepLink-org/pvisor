//! A job-local, fail-closed permission prompt channel to the owning TUI.

use pvisor_core::audit::{AuditChannel, AuditDecision, AuditKind, AuditRequest};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static ARMED: AtomicBool = AtomicBool::new(false);
static FILE_BURSTS: OnceLock<Mutex<HashMap<String, (Instant, AuditDecision)>>> = OnceLock::new();
const FILE_BURST: Duration = Duration::from_millis(250);

/// Called before the agent starts. The path is not projected into the agent.
pub fn init(socket: PathBuf) {
    let _ = SOCKET.set(socket);
    pvisor_core::audit::install(Arc::new(TuiAudit));
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

/// Blocks the intercepted operation until the TUI answers. Any IPC failure denies.
pub fn socket() -> Option<PathBuf> {
    SOCKET.get().cloned()
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

struct TuiAudit;
impl AuditChannel for TuiAudit {
    fn enabled(&self) -> bool {
        enabled()
    }
    fn socket(&self) -> Option<PathBuf> {
        socket()
    }
    fn request_at(
        &self,
        socket: &std::path::Path,
        session: &str,
        prompt: &AuditRequest,
    ) -> AuditDecision {
        request_at(socket, session, prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[cfg(unix)]
    #[test]
    fn approvals_are_bound_to_attempt_and_persisted_policies_enforce() {
        use pvisor_core::FileAccessPolicy;
        use std::io::{self, BufRead, BufReader, Write};
        let socket = std::env::temp_dir().join(format!(
            "pvisor-policy-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            for decision in ["allow", "deny"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                writeln!(stream, "\"{decision}\"").unwrap();
            }
        });
        let mut policy =
            FileAccessPolicy::new_with_ask(vec![], vec!["secret".into()], vec![]).unwrap();
        init(socket.clone());
        policy.bind_session("run", "attempt-one", "workspace");
        let restored: FileAccessPolicy =
            serde_json::from_value(serde_json::to_value(&policy).unwrap()).unwrap();
        assert_eq!(restored.context().unwrap().attempt_id, "attempt-one");
        assert!(restored.check(Path::new("secret")).is_ok());
        policy.arm();
        assert!(policy.check(Path::new("secret")).is_ok());
        assert!(policy.check(Path::new("secret")).is_ok());
        policy.bind_session("run", "attempt-two", "workspace");
        policy.arm();
        assert_eq!(
            policy.check(Path::new("secret")).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
