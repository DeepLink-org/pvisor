//! Parent-side permission dialog transport. The intercepted operation waits
//! on its own Unix stream; closing the TUI makes the request fail closed.

use anyhow::{Context, Result};
use persisting_control::NetworkTransport;
use persisting_control::audit::{AuditDecision, AuditKind, AuditRequest};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::IpAddr;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

const POLICY_FILE: &str = "audit-policy.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Scope {
    Exact,
    Directory,
    Suffix,
    Domain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionRule {
    kind: AuditKind,
    scope: Scope,
    value: String,
    port: Option<u16>,
    transport: Option<NetworkTransport>,
    decision: AuditDecision,
}

impl SessionRule {
    fn from_request(request: &AuditRequest, scope: Scope, decision: AuditDecision) -> Option<Self> {
        let (value, port, transport) = match request.kind {
            AuditKind::File => {
                let path = Path::new(&request.target);
                let value = match scope {
                    Scope::Exact => request.target.clone(),
                    Scope::Directory => path.parent()?.display().to_string(),
                    Scope::Suffix => file_suffix(path)?,
                    Scope::Domain => return None,
                };
                (value, None, None)
            }
            AuditKind::Network => {
                if !matches!(scope, Scope::Exact | Scope::Domain) {
                    return None;
                }
                let host = request
                    .host
                    .as_deref()?
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if scope == Scope::Domain && (host.parse::<IpAddr>().is_ok() || !host.contains('.'))
                {
                    return None;
                }
                (host, request.port, request.transport)
            }
        };
        Some(Self {
            kind: request.kind,
            scope,
            value,
            port,
            transport,
            decision,
        })
    }

    fn matches(&self, request: &AuditRequest) -> bool {
        if self.kind != request.kind {
            return false;
        }
        match request.kind {
            AuditKind::File => {
                let path = Path::new(&request.target);
                match self.scope {
                    Scope::Exact => request.target == self.value,
                    Scope::Directory => path
                        .parent()
                        .is_some_and(|parent| parent == Path::new(&self.value)),
                    Scope::Suffix => file_suffix(path).is_some_and(|suffix| suffix == self.value),
                    Scope::Domain => false,
                }
            }
            AuditKind::Network => {
                if self.port != request.port || self.transport != request.transport {
                    return false;
                }
                let Some(host) = request.host.as_deref() else {
                    return false;
                };
                let host = host.trim_end_matches('.').to_ascii_lowercase();
                match self.scope {
                    Scope::Exact => host == self.value,
                    Scope::Domain => {
                        host == self.value || host.ends_with(&format!(".{}", self.value))
                    }
                    Scope::Directory | Scope::Suffix => false,
                }
            }
        }
    }

    pub(super) fn label(&self) -> String {
        format!(
            "{:?} {:?} {} → {:?}",
            self.kind, self.scope, self.value, self.decision
        )
    }
}

fn file_suffix(path: &Path) -> Option<String> {
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        return Some(format!(".{}", extension.to_ascii_lowercase()));
    }
    let name = path.file_name()?.to_str()?;
    (name.starts_with('.') && name.len() > 1).then(|| name.to_ascii_lowercase())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionPolicy {
    schema_version: u32,
    rules: Vec<SessionRule>,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rules: Vec::new(),
        }
    }
}

impl SessionPolicy {
    pub fn load(storage: &Path) -> Result<Self> {
        let path = storage.join(POLICY_FILE);
        let mut source = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        let policy: Self = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            policy.schema_version == 1,
            "unsupported audit policy version"
        );
        for rule in &policy.rules {
            let valid = match rule.kind {
                AuditKind::File => {
                    matches!(rule.scope, Scope::Exact | Scope::Directory | Scope::Suffix)
                        && rule.port.is_none()
                        && rule.transport.is_none()
                        && (rule.scope == Scope::Directory || !rule.value.is_empty())
                }
                AuditKind::Network => {
                    matches!(rule.scope, Scope::Exact | Scope::Domain)
                        && !rule.value.is_empty()
                        && rule.port.is_some()
                        && rule.transport.is_some()
                        && (rule.scope != Scope::Domain
                            || (rule.value.contains('.') && rule.value.parse::<IpAddr>().is_err()))
                }
            };
            anyhow::ensure!(valid, "invalid session audit rule");
        }
        Ok(policy)
    }

    pub fn persist(&self, storage: &Path) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(storage)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(storage.join(POLICY_FILE))?;
        Ok(())
    }

    pub fn resolve(&self, request: &AuditRequest) -> Option<(AuditDecision, Scope)> {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(request))
            .map(|rule| (rule.decision, rule.scope))
    }

    pub fn with_decision(
        &self,
        request: &AuditRequest,
        scope: Scope,
        decision: AuditDecision,
    ) -> Option<Self> {
        let rule = SessionRule::from_request(request, scope, decision)?;
        let mut next = self.clone();
        next.rules.push(rule);
        Some(next)
    }

    pub fn rule_labels(&self) -> Vec<String> {
        self.rules.iter().map(SessionRule::label).collect()
    }
}

pub(super) fn choice(request: &AuditRequest, byte: u8) -> Option<(Scope, AuditDecision)> {
    let scope = match byte {
        b'1' | b'a' | b'A' => Scope::Exact,
        b'2' if request.kind == AuditKind::File => Scope::Directory,
        b'2' if request.kind == AuditKind::Network => Scope::Domain,
        b'3' if request.kind == AuditKind::File => Scope::Suffix,
        b'd' | b'D' | 0x1b | b'\r' | b'\n' => return Some((Scope::Exact, AuditDecision::Deny)),
        _ => return None,
    };
    SessionRule::from_request(request, scope, AuditDecision::Allow)
        .map(|_| (scope, AuditDecision::Allow))
}

struct Pending {
    request: AuditRequest,
    stream: UnixStream,
}

pub(super) struct AuditServer {
    listener: UnixListener,
    queue: VecDeque<Pending>,
}

impl AuditServer {
    pub fn bind(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path).context("create TUI audit socket")?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            queue: VecDeque::new(),
        })
    }

    pub fn poll(&mut self) -> Result<bool> {
        let mut changed = false;
        loop {
            let (mut stream, _) = match self.listener.accept() {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            };
            stream.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_ok()
                && let Ok(request) = serde_json::from_str::<AuditRequest>(&line)
            {
                self.queue.push_back(Pending { request, stream });
                changed = true;
            } else {
                let _ = stream.write_all(b"\"deny\"\n");
            }
        }
        Ok(changed)
    }

    pub fn active(&self) -> Option<&AuditRequest> {
        self.queue.front().map(|pending| &pending.request)
    }

    pub fn decide(&mut self, decision: AuditDecision) -> Option<AuditRequest> {
        let mut pending = self.queue.pop_front()?;
        if serde_json::to_writer(&mut pending.stream, &decision).is_ok() {
            let _ = pending.stream.write_all(b"\n");
        }
        Some(pending.request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_control::audit::AuditKind;

    #[test]
    fn prompt_waits_for_a_decision_and_returns_it_to_the_blocked_operation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.sock");
        let mut server = AuditServer::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(path).unwrap();
            let request = AuditRequest {
                kind: AuditKind::File,
                target: "workspace/.env".into(),
                reason: "sensitive file rule".into(),
                host: None,
                port: None,
                transport: None,
            };
            serde_json::to_writer(&mut stream, &request).unwrap();
            stream.write_all(b"\n").unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            serde_json::from_str::<AuditDecision>(&line).unwrap()
        });
        for _ in 0..100 {
            server.poll().unwrap();
            if server.active().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(server.active().unwrap().target, "workspace/.env");
        assert_eq!(
            server.decide(AuditDecision::Deny).unwrap().kind,
            AuditKind::File
        );
        assert_eq!(worker.join().unwrap(), AuditDecision::Deny);
    }

    #[test]
    fn malformed_prompts_are_denied_and_pending_prompts_close_with_the_ui() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.sock");
        let mut server = AuditServer::bind(&path).unwrap();

        let mut malformed = UnixStream::connect(&path).unwrap();
        malformed.write_all(b"not json\n").unwrap();
        server.poll().unwrap();
        let mut reply = String::new();
        BufReader::new(malformed).read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<AuditDecision>(&reply).unwrap(),
            AuditDecision::Deny
        );
        assert!(server.active().is_none());

        let mut pending = UnixStream::connect(&path).unwrap();
        // macOS rejects setting socket timeouts after the peer has closed.
        pending
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        serde_json::to_writer(&mut pending, &file("workspace/.env")).unwrap();
        pending.write_all(b"\n").unwrap();
        server.poll().unwrap();
        assert!(server.active().is_some());
        drop(server);
        let mut byte = [0];
        assert_eq!(pending.read(&mut byte).unwrap(), 0);
    }

    fn file(target: &str) -> AuditRequest {
        AuditRequest {
            kind: AuditKind::File,
            target: target.into(),
            reason: "ask".into(),
            host: None,
            port: None,
            transport: None,
        }
    }

    fn network(host: &str, port: u16) -> AuditRequest {
        AuditRequest {
            kind: AuditKind::Network,
            target: format!("{host}:{port}"),
            reason: "unlisted".into(),
            host: Some(host.into()),
            port: Some(port),
            transport: Some(NetworkTransport::TcpTunnel),
        }
    }

    #[test]
    fn file_choices_are_scoped_and_persisted_for_this_job() {
        let request = file("config/dev/token.pem");
        let mut policy = SessionPolicy::default();
        policy = policy
            .with_decision(&request, Scope::Directory, AuditDecision::Allow)
            .unwrap();
        assert_eq!(
            policy.resolve(&file("config/dev/other.txt")),
            Some((AuditDecision::Allow, Scope::Directory))
        );
        assert_eq!(policy.resolve(&file("config/prod/token.pem")), None);
        policy = policy
            .with_decision(&request, Scope::Suffix, AuditDecision::Allow)
            .unwrap();
        assert_eq!(
            policy.resolve(&file("config/prod/key.pem")),
            Some((AuditDecision::Allow, Scope::Suffix))
        );
        assert_eq!(policy.resolve(&file("config/prod/key.key")), None);
        policy = policy
            .with_decision(
                &file("config/dev/deny.pem"),
                Scope::Exact,
                AuditDecision::Deny,
            )
            .unwrap();
        assert_eq!(
            policy.resolve(&file("config/dev/deny.pem")),
            Some((AuditDecision::Deny, Scope::Exact))
        );
        let storage = tempfile::tempdir().unwrap();
        policy.persist(storage.path()).unwrap();
        let restored = SessionPolicy::load(storage.path()).unwrap();
        assert_eq!(
            restored.resolve(&file("config/prod/key.pem")),
            Some((AuditDecision::Allow, Scope::Suffix))
        );
        assert!(storage.path().join("audit-policy.json").exists());
        assert_eq!(
            choice(&file(".env"), b'3'),
            Some((Scope::Suffix, AuditDecision::Allow))
        );
    }

    #[test]
    fn persisted_network_decisions_keep_domain_port_transport_and_order() {
        let request = network("API.Example.COM.", 443);
        let policy = SessionPolicy::default()
            .with_decision(&request, Scope::Domain, AuditDecision::Allow)
            .unwrap();
        assert_eq!(
            policy.resolve(&network("sub.api.example.com", 443)),
            Some((AuditDecision::Allow, Scope::Domain))
        );
        assert_eq!(policy.resolve(&network("other.example.com", 443)), None);
        assert_eq!(policy.resolve(&network("api.example.com", 80)), None);
        let mut https = network("api.example.com", 443);
        https.transport = Some(NetworkTransport::Https);
        assert_eq!(policy.resolve(&https), None);

        let policy = policy
            .with_decision(
                &network("sub.api.example.com", 443),
                Scope::Exact,
                AuditDecision::Deny,
            )
            .unwrap();
        let storage = tempfile::tempdir().unwrap();
        policy.persist(storage.path()).unwrap();
        let restored = SessionPolicy::load(storage.path()).unwrap();
        assert_eq!(
            restored.resolve(&network("SUB.API.EXAMPLE.COM.", 443)),
            Some((AuditDecision::Deny, Scope::Exact))
        );
        assert_eq!(
            restored.resolve(&network("other.api.example.com", 443)),
            Some((AuditDecision::Allow, Scope::Domain))
        );
        assert_eq!(choice(&network("127.0.0.1", 443), b'2'), None);
        assert_eq!(choice(&file("README"), b'3'), None);
    }

    #[test]
    fn stored_policy_rejects_invalid_scope_and_unknown_version() {
        let storage = tempfile::tempdir().unwrap();
        let path = storage.path().join(POLICY_FILE);
        for document in [
            serde_json::json!({"schema_version": 2, "rules": []}),
            serde_json::json!({
                "schema_version": 1,
                "rules": [{
                    "kind": "file",
                    "scope": "domain",
                    "value": "example.com",
                    "port": null,
                    "transport": null,
                    "decision": "allow"
                }]
            }),
        ] {
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            assert!(SessionPolicy::load(storage.path()).is_err());
        }
    }
}
