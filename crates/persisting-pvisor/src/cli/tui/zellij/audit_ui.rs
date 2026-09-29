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
use std::path::{Path, PathBuf};

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
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.schema_version == 1, "unsupported audit policy version");
        for rule in &self.rules {
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
        Ok(())
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
        next.rules.retain(|old| {
            !(old.kind == rule.kind
                && old.scope == rule.scope
                && old.value == rule.value
                && old.port == rule.port
                && old.transport == rule.transport)
        });
        next.rules.push(rule);
        Some(next)
    }

    pub fn rule_labels(&self) -> Vec<String> {
        self.rules.iter().map(SessionRule::label).collect()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Lifetime {
    #[default]
    Session,
    Workspace,
    User,
}

impl Lifetime {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::User => "user",
        }
    }

    pub fn key(byte: u8) -> Option<Self> {
        match byte {
            b's' => Some(Self::Session),
            b'w' => Some(Self::Workspace),
            b'u' => Some(Self::User),
            _ => None,
        }
    }
}

pub(super) fn permissions_config_path() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("personal configuration directory unavailable")?;
    Ok(root.join("pvisor/config.toml"))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SavedPermissions {
    user: SessionPolicy,
    workspaces: std::collections::BTreeMap<String, SessionPolicy>,
}

impl SavedPermissions {
    fn read(path: &Path) -> Result<(toml_edit::DocumentMut, Self)> {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let doc = source.parse::<toml_edit::DocumentMut>()?;
        let config: toml::Table = toml::from_str(&source)?;
        let saved: Self = config
            .get("permissions")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default();
        saved.user.validate()?;
        for policy in saved.workspaces.values() {
            policy.validate()?;
        }
        Ok((doc, saved))
    }
}

pub(super) struct Permissions {
    session: SessionPolicy,
    saved: SavedPermissions,
    config: PathBuf,
    workspace: String,
    file_root: PathBuf,
}

impl Permissions {
    pub fn load(
        storage: &Path,
        workspace: &Path,
        file_root: &Path,
        config: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            session: SessionPolicy::load(storage)?,
            saved: SavedPermissions::read(&config)?.1,
            config,
            workspace: workspace
                .canonicalize()?
                .to_str()
                .context("workspace must be UTF-8")?
                .into(),
            file_root: file_root.canonicalize()?,
        })
    }

    // Ask paths are relative to the overlay target, not necessarily the cwd.
    // Persist original absolute paths so a user rule cannot approve a same-named
    // file in an unrelated workspace merely because its relative path matches.
    fn persistent_request(&self, request: &AuditRequest) -> Result<AuditRequest> {
        let mut request = request.clone();
        if request.kind == AuditKind::File {
            let path = Path::new(&request.target);
            anyhow::ensure!(
                !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
                "audit path contains parent traversal"
            );
            request.target = self
                .file_root
                .join(path)
                .to_str()
                .context("audit path must be UTF-8")?
                .into();
        }
        Ok(request)
    }

    pub fn resolve(&self, request: &AuditRequest) -> Option<(AuditDecision, Scope, Lifetime)> {
        if let Some((decision, scope)) = self.session.resolve(request) {
            return Some((decision, scope, Lifetime::Session));
        }
        let request = self.persistent_request(request).ok()?;
        if let Some((decision, scope)) = self
            .saved
            .workspaces
            .get(&self.workspace)
            .and_then(|p| p.resolve(&request))
        {
            return Some((decision, scope, Lifetime::Workspace));
        }
        self.saved
            .user
            .resolve(&request)
            .map(|(d, s)| (d, s, Lifetime::User))
    }

    pub fn remember(
        &mut self,
        storage: &Path,
        request: &AuditRequest,
        scope: Scope,
        decision: AuditDecision,
        lifetime: Lifetime,
    ) -> Result<()> {
        if lifetime == Lifetime::Session {
            let next = self
                .session
                .with_decision(request, scope, decision)
                .context("invalid audit scope")?;
            next.persist(storage)?;
            self.session = next;
            return Ok(());
        }
        let request = self.persistent_request(request)?;
        self.update_saved(|saved, workspace| {
            let policy = match lifetime {
                Lifetime::Workspace => saved.workspaces.entry(workspace.into()).or_default(),
                Lifetime::User => &mut saved.user,
                Lifetime::Session => unreachable!(),
            };
            *policy = policy
                .with_decision(&request, scope, decision)
                .context("invalid audit scope")?;
            Ok(())
        })
    }

    fn update_saved(
        &mut self,
        update: impl FnOnce(&mut SavedPermissions, &str) -> Result<()>,
    ) -> Result<()> {
        let parent = self
            .config
            .parent()
            .context("configuration path has no parent")?;
        std::fs::create_dir_all(parent)?;
        use std::os::unix::fs::OpenOptionsExt;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent.join("permissions.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        // Re-read under the lock: simultaneous TUIs must not overwrite each other.
        let (mut doc, mut saved) = SavedPermissions::read(&self.config)?;
        update(&mut saved, &self.workspace)?;
        let encoded = toml::to_string(&saved)?.parse::<toml_edit::DocumentMut>()?;
        doc["permissions"] = toml_edit::Item::Table(encoded.as_table().clone());
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(doc.to_string().as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.config)?;
        std::fs::File::open(parent)?.sync_all()?;
        self.saved = saved;
        Ok(())
    }

    fn rule_entries(&self) -> Vec<(Lifetime, SessionRule)> {
        [
            (Lifetime::Session, Some(&self.session)),
            (
                Lifetime::Workspace,
                self.saved.workspaces.get(&self.workspace),
            ),
            (Lifetime::User, Some(&self.saved.user)),
        ]
        .into_iter()
        .flat_map(|(lifetime, policy)| {
            policy
                .into_iter()
                .flat_map(move |p| p.rules.iter().rev().cloned().map(move |r| (lifetime, r)))
        })
        .collect()
    }

    pub fn rule_labels(&self) -> Vec<String> {
        self.rule_entries()
            .iter()
            .map(|(lifetime, rule)| format!("[{}] {}", lifetime.label(), rule.label()))
            .collect()
    }

    /// Remove the selected decision, not whatever occupies its index after another writer saves.
    pub fn forget(&mut self, storage: &Path, index: usize) -> Result<()> {
        let (lifetime, rule) = self
            .rule_entries()
            .get(index)
            .cloned()
            .context("no decision selected")?;
        if lifetime == Lifetime::Session {
            let mut next = self.session.clone();
            next.rules.retain(|r| r != &rule);
            next.persist(storage)?;
            self.session = next;
            return Ok(());
        }
        self.update_saved(|saved, workspace| {
            let policy = match lifetime {
                Lifetime::Workspace => saved.workspaces.get_mut(workspace),
                Lifetime::User => Some(&mut saved.user),
                Lifetime::Session => unreachable!(),
            };
            if let Some(policy) = policy {
                policy.rules.retain(|r| r != &rule);
            }
            Ok(())
        })
    }

    pub fn display_request(&self, request: &AuditRequest) -> AuditRequest {
        self.persistent_request(request)
            .unwrap_or_else(|_| request.clone())
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

#[derive(Default)]
pub(super) struct Prompt {
    pub lifetime: Lifetime,
    pub scope: Option<Scope>,
    pub focus: u8,
    escape: u8,
}

impl Prompt {
    pub fn input(&mut self, request: &AuditRequest, byte: u8) -> Option<(Scope, AuditDecision)> {
        // Consume complete cursor sequences, even when reads split their bytes.
        if self.escape != 0 {
            if self.escape == 1 && matches!(byte, b'[' | b'O') {
                self.escape = 2;
                return None;
            }
            let cursor = self.escape == 2;
            self.escape = 0;
            if cursor {
                match byte {
                    b'A' => self.focus = (self.focus + 2) % 3,
                    b'B' => self.focus = (self.focus + 1) % 3,
                    b'C' | b'D' => {
                        let step = if byte == b'C' { 1 } else { -1 };
                        match self.focus {
                            0 => {
                                let scopes: Vec<_> = (*b"123")
                                    .into_iter()
                                    .filter_map(|key| choice(request, key).map(|v| v.0))
                                    .collect();
                                let index = scopes
                                    .iter()
                                    .position(|s| Some(*s) == self.scope)
                                    .unwrap_or(0);
                                self.scope = Some(
                                    scopes[(index as isize + step).rem_euclid(scopes.len() as isize)
                                        as usize],
                                );
                            }
                            1 => {
                                let index = match self.lifetime {
                                    Lifetime::Session => 0,
                                    Lifetime::Workspace => 1,
                                    Lifetime::User => 2,
                                };
                                self.lifetime =
                                    [Lifetime::Session, Lifetime::Workspace, Lifetime::User]
                                        [(index + step).rem_euclid(3) as usize];
                            }
                            _ => {
                                self.scope = if self.scope.is_some() {
                                    None
                                } else {
                                    Some(Scope::Exact)
                                }
                            }
                        }
                    }
                    _ => {}
                }
                return None;
            }
        }
        match byte {
            0x1b => {
                self.escape = 1;
            }
            b'\t' => self.focus = (self.focus + 1) % 3,
            b'\r' | b'\n' if self.focus != 2 => self.focus += 1,
            b'\r' | b'\n' => {
                return Some(self.scope.map_or((Scope::Exact, AuditDecision::Deny), |s| {
                    (s, AuditDecision::Allow)
                }));
            }
            b'd' | b'D' => return Some((Scope::Exact, AuditDecision::Deny)),
            _ => {
                if let Some(lifetime) = Lifetime::key(byte) {
                    self.lifetime = lifetime;
                }
                if let Some((scope, AuditDecision::Allow)) = choice(request, byte) {
                    self.scope = Some(scope);
                    self.focus = 2;
                }
            }
        }
        None
    }
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

    #[test]
    fn prompt_navigation_never_grants_without_confirming_the_button() {
        let request = file("private/one.txt");
        let mut prompt = Prompt::default();
        for byte in b"\x1b[C\t\x1b[C" {
            assert_eq!(prompt.input(&request, *byte), None);
        }
        assert_eq!(prompt.scope, Some(Scope::Directory));
        assert_eq!(prompt.lifetime, Lifetime::Workspace);
        assert_eq!(prompt.input(&request, b'\t'), None);
        assert_eq!(
            prompt.input(&request, b'\r'),
            Some((Scope::Directory, AuditDecision::Allow))
        );
        let mut prompt = Prompt::default();
        for byte in b"\t\t" {
            assert_eq!(prompt.input(&request, *byte), None);
        }
        assert_eq!(
            prompt.input(&request, b'\r'),
            Some((Scope::Exact, AuditDecision::Deny))
        );
        assert_eq!(prompt.input(&request, b'2'), None);
        assert_eq!(
            prompt.input(&request, b'\r'),
            Some((Scope::Directory, AuditDecision::Allow))
        );
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
    fn persistent_permissions_isolate_workspaces_and_keep_user_paths_absolute() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for path in [&a, &b, &first, &second] {
            std::fs::create_dir(path).unwrap();
        }
        let config = temp.path().join("config.toml");
        std::fs::write(
            &config,
            "# keep my settings\n[run]\nname = 'example' # keep this too\n",
        )
        .unwrap();
        let mut policy = Permissions::load(&first, &a, &a, config.clone()).unwrap();
        policy
            .remember(
                &first,
                &file("private/key.txt"),
                Scope::Directory,
                AuditDecision::Allow,
                Lifetime::Workspace,
            )
            .unwrap();
        policy
            .remember(
                &first,
                &file("user.txt"),
                Scope::Exact,
                AuditDecision::Allow,
                Lifetime::User,
            )
            .unwrap();
        policy
            .remember(
                &first,
                &file("session.txt"),
                Scope::Exact,
                AuditDecision::Allow,
                Lifetime::Session,
            )
            .unwrap();
        let same = Permissions::load(&second, &a, &a, config.clone()).unwrap();
        assert_eq!(
            same.resolve(&file("private/next.txt")),
            Some((AuditDecision::Allow, Scope::Directory, Lifetime::Workspace))
        );
        assert!(same.resolve(&file("session.txt")).is_none());
        let other = Permissions::load(&second, &b, &b, config.clone()).unwrap();
        assert!(other.resolve(&file("private/next.txt")).is_none());
        assert!(other.resolve(&file("user.txt")).is_none());
        let absolute = a.canonicalize().unwrap().join("user.txt");
        assert_eq!(
            other.resolve(&file(absolute.to_str().unwrap())),
            Some((AuditDecision::Allow, Scope::Exact, Lifetime::User))
        );
        let saved = std::fs::read_to_string(&config).unwrap();
        assert!(saved.contains("# keep my settings"));
        assert!(saved.contains("name = 'example' # keep this too"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(config).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn persistent_permissions_merge_writers_and_prefer_narrower_lifetimes() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let config = temp.path().join("config.toml");
        let mut one = Permissions::load(&first, temp.path(), temp.path(), config.clone()).unwrap();
        let mut two = Permissions::load(&second, temp.path(), temp.path(), config.clone()).unwrap();
        one.remember(
            &first,
            &file("one.txt"),
            Scope::Exact,
            AuditDecision::Allow,
            Lifetime::User,
        )
        .unwrap();
        two.remember(
            &second,
            &file("two.txt"),
            Scope::Exact,
            AuditDecision::Allow,
            Lifetime::Workspace,
        )
        .unwrap();
        let mut loaded = Permissions::load(&first, temp.path(), temp.path(), config).unwrap();
        assert!(loaded.resolve(&file("one.txt")).is_some());
        assert!(loaded.resolve(&file("two.txt")).is_some());
        loaded
            .remember(
                &first,
                &file("one.txt"),
                Scope::Exact,
                AuditDecision::Deny,
                Lifetime::Workspace,
            )
            .unwrap();
        assert_eq!(
            loaded.resolve(&file("one.txt")).unwrap().0,
            AuditDecision::Deny
        );
        loaded
            .remember(
                &first,
                &file("one.txt"),
                Scope::Exact,
                AuditDecision::Allow,
                Lifetime::Session,
            )
            .unwrap();
        assert_eq!(
            loaded.resolve(&file("one.txt")).unwrap().2,
            Lifetime::Session
        );
        assert!(
            loaded
                .remember(
                    &first,
                    &file("../escape"),
                    Scope::Exact,
                    AuditDecision::Allow,
                    Lifetime::User
                )
                .is_err()
        );
    }

    #[test]
    fn malformed_persistent_permissions_are_not_overwritten_or_applied() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let mut policy =
            Permissions::load(temp.path(), temp.path(), temp.path(), config.clone()).unwrap();
        for bad in [
            "[broken",
            "[permissions.user]\nschema_version = 999\nrules = []\n",
        ] {
            std::fs::write(&config, bad).unwrap();
            assert!(
                Permissions::load(temp.path(), temp.path(), temp.path(), config.clone()).is_err()
            );
            assert!(
                policy
                    .remember(
                        temp.path(),
                        &file("secret"),
                        Scope::Exact,
                        AuditDecision::Allow,
                        Lifetime::User
                    )
                    .is_err()
            );
            assert!(policy.resolve(&file("secret")).is_none());
            assert_eq!(std::fs::read_to_string(&config).unwrap(), bad);
        }
    }

    #[test]
    fn forgetting_a_selected_decision_keeps_other_writers_and_scopes() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        let mut policy =
            Permissions::load(temp.path(), temp.path(), temp.path(), config.clone()).unwrap();
        for (target, lifetime) in [
            ("session", Lifetime::Session),
            ("workspace", Lifetime::Workspace),
            ("user", Lifetime::User),
        ] {
            policy
                .remember(
                    temp.path(),
                    &file(target),
                    Scope::Exact,
                    AuditDecision::Allow,
                    lifetime,
                )
                .unwrap();
        }
        let mut other =
            Permissions::load(temp.path(), temp.path(), temp.path(), config.clone()).unwrap();
        other
            .remember(
                temp.path(),
                &file("other"),
                Scope::Exact,
                AuditDecision::Allow,
                Lifetime::User,
            )
            .unwrap();
        policy.forget(temp.path(), 0).unwrap();
        assert!(policy.resolve(&file("session")).is_none());
        policy.forget(temp.path(), 0).unwrap();
        assert!(policy.resolve(&file("workspace")).is_none());
        let user = policy
            .rule_entries()
            .iter()
            .position(|(_, r)| r.value.ends_with("/user"))
            .unwrap();
        policy.forget(temp.path(), user).unwrap();
        let reloaded = Permissions::load(temp.path(), temp.path(), temp.path(), config).unwrap();
        assert!(reloaded.resolve(&file("user")).is_none());
        assert!(reloaded.resolve(&file("other")).is_some());
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
