//! Persistent Session transitions across Executions; storage and lease acquisition stay outside.
use super::super::{
    RunRecord,
    session_execution::{Capture, ForkRequest, Request, ResumeRequest},
};
use anyhow::{Context, ensure};
use pvisor_core::operation::{ExecutionCheckpoint, ExecutionSuspension, SnapshotRamStorage};
use pvisor_core::{RunResult, RunSpec, RunState};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHED_SESSION_BYTES: usize = 256 * 1024;

// Cache only pure deserialization of exact, already-read bytes. Every lookup
// still opens and validates the current descriptor, enforces its byte bound and
// checks the decoded root. No path, metadata, lease or admission result is cached.
static PARSED_TASK_SESSION: std::sync::Mutex<Option<ParsedTaskSession>> =
    std::sync::Mutex::new(None);

struct ParsedTaskSession {
    hash: blake3::Hash,
    session: std::sync::Arc<Session>,
}

fn decode_task_session(bytes: &[u8]) -> anyhow::Result<Session> {
    if bytes.len() > MAX_CACHED_SESSION_BYTES {
        return Ok(serde_json::from_slice(bytes)?);
    }
    let hash = blake3::hash(bytes);
    let cached = PARSED_TASK_SESSION.lock().ok().and_then(|entry| {
        entry
            .as_ref()
            .filter(|entry| entry.hash == hash)
            .map(|entry| entry.session.clone())
    });
    if let Some(session) = cached {
        // Snapshots remain independent; cloning happens outside the cache lock.
        return Ok((*session).clone());
    }
    let session: Session = serde_json::from_slice(bytes)?;
    let parsed = ParsedTaskSession {
        hash,
        session: std::sync::Arc::new(session.clone()),
    };
    if let Ok(mut entry) = PARSED_TASK_SESSION.lock() {
        *entry = Some(parsed);
    }
    Ok(session)
}

/// Durable continuation phase of a Session, independent of an Execution's live pause state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// A persistent instance exists without an admitted Execution.
    Created,
    /// A cold Execution was admitted but has not acknowledged readiness.
    Starting,
    /// The Session owner is retaining resources while waiting to restart.
    Restarting,
    /// The selected Execution owns live execution.
    Running,
    /// Suspension was admitted; native termination is not yet confirmed.
    Suspending,
    /// A completion receipt grants continuation from the sealed head.
    Suspended,
    /// A successor was admitted but has not confirmed startup.
    Restoring,
    /// Execution ended or continuation rights were explicitly withdrawn.
    Terminal,
    /// An admitted outcome lacks sufficient completion evidence.
    Unknown,
}
impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Restarting => "restarting",
            Self::Running => "running",
            Self::Suspending => "suspending",
            Self::Suspended => "suspended",
            Self::Restoring => "restoring",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        })
    }
}

/// Persistent execution aggregate. State changes require typed transitions; callers
/// retain the Session lease through applying a transition and writing the resulting record.
/// Deserialization preserves the existing schema; reading storage validates its binding.
/// Related identities, continuation heads and request ledgers are read-only to callers.
/// Accessors observe this loaded snapshot without I/O or implicit refresh. Clones
/// are independent snapshots; persistent operations reread under their own leases.
///
/// ```compile_fail
/// fn replace_execution(session: &mut pvisor::Session) {
///     session.active_execution = "unadmitted-successor".into();
/// }
/// ```
///
/// Raw persistence is also internal; callers mutate through SessionService.
///
/// ```compile_fail
/// fn persist_unadmitted_state(session: &pvisor::Session) {
///     session.write().unwrap();
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session<Profile = TaskProfile> {
    /// Durable record schema version.
    version: u16,
    /// Logical Session identity shared by its Executions.
    run_id: String,
    /// Canonical Session storage root.
    root: PathBuf,
    /// Execution identity selected by confirmed startup.
    active_execution: String,
    /// Fences one continuous supervisor admission across automatic Executions.
    owner_epoch: String,
    state: SessionState,
    kind: SessionKind,
    /// Backend-specific admitted environment and capabilities, not another
    /// lifecycle or an independently persisted container identity.
    profile: Profile,
}

/// Behavior contract admitted for a Session; labels cannot change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// Native task execution and optional staged effects/checkpoint continuation.
    Task,
    /// Persistent container environment with cold Execution restarts.
    Container,
}

/// Native task environment, staged effects and checkpoint continuation rights.
/// Lifecycle identity/state belong to Session; this payload cannot start or stop
/// an owner independently. Fields remain opaque to frontends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProfile {
    /// Stage selected for the current or admitted successor Execution.
    active_stage: PathBuf,
    /// Previous stage retained during successor admission.
    previous_stage: PathBuf,
    /// Captured execution configuration, not frontend override authority.
    config: crate::RunConfig,
    /// Captured invocation and policy inputs for continuation.
    spec: RunSpec,
    /// Confirmed suspended checkpoint; usable only in Suspended phase.
    head: Option<String>,
    /// Published checkpoints and their retained file branches.
    checkpoints: BTreeMap<String, Capture>,
    /// Scoped capture admission and result receipts.
    requests: BTreeMap<String, Request>,
    /// Scoped successor admission receipts.
    resumes: BTreeMap<String, ResumeRequest>,
    /// Scoped execution-fork receipts.
    forks: BTreeMap<String, ForkRequest>,
    /// Snapshot stores pinned by this Session's records.
    stores: BTreeSet<PathBuf>,
}

impl std::ops::Deref for Session {
    type Target = TaskProfile;
    fn deref(&self) -> &TaskProfile {
        &self.profile
    }
}
impl std::ops::DerefMut for Session {
    fn deref_mut(&mut self) -> &mut TaskProfile {
        &mut self.profile
    }
}

impl<Profile> Session<Profile> {
    /// Read this Session's durable lifecycle phase, not process liveness.
    pub fn state(&self) -> SessionState {
        self.state
    }
    /// Read the persisted Session schema version.
    pub fn version(&self) -> u16 {
        self.version
    }
    /// Stable identity shared by all admitted Executions.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    /// Canonical private storage root that owns this Session.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Current admitted Execution identity; empty before the first admission.
    pub fn active_execution(&self) -> &str {
        &self.active_execution
    }
    /// Admission identity of the current supervisor. Automatic retries retain
    /// it; an independently admitted owner must get a new epoch.
    pub fn owner_epoch(&self) -> &str {
        &self.owner_epoch
    }
    /// Backend capabilities/configuration captured with this same Session record.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    /// Fixed behavior contract, independent of labels and frontend spelling.
    pub fn kind(&self) -> SessionKind {
        self.kind
    }

    pub(crate) fn from_profile(
        run_id: String,
        root: PathBuf,
        active_execution: String,
        state: SessionState,
        kind: SessionKind,
        owner_epoch: String,
        profile: Profile,
    ) -> anyhow::Result<Self> {
        ensure!(
            !run_id.is_empty() && root.is_absolute(),
            "invalid Session identity/root"
        );
        ensure!(
            state == SessionState::Created || !active_execution.is_empty(),
            "admitted Session requires an Execution identity"
        );
        Ok(Self {
            version: super::super::session_execution::SESSION_SCHEMA_VERSION,
            run_id,
            root,
            active_execution,
            state,
            kind,
            owner_epoch,
            profile,
        })
    }

    pub(crate) fn validate_root(&self, root: &Path) -> anyhow::Result<()> {
        ensure!(
            self.version == super::super::session_execution::SESSION_SCHEMA_VERSION
                && self.root == root
                && root.is_absolute()
                && !self.run_id.is_empty()
                && (matches!(self.state, SessionState::Created | SessionState::Restoring)
                    || !self.active_execution.is_empty() && !self.owner_epoch.is_empty()),
            "unsupported or foreign Session record"
        );
        Ok(())
    }
    pub(crate) fn into_profile(self) -> Profile {
        self.profile
    }
}

impl<Profile: Serialize> Session<Profile> {
    pub(crate) fn write(&self) -> anyhow::Result<()> {
        let legacy = match self.kind {
            SessionKind::Task => "execution-session.json",
            SessionKind::Container => "instance.json",
        };
        ensure!(
            !self.root.join(legacy).try_exists()?,
            "legacy Session authority must be handled by its owning binary"
        );
        // Session records are machine authority, reread at every admission
        // fence. Do not amplify their I/O and parsing cost with indentation.
        let body = crate::util::persistence_step(&self.run_id, "session", "serialize", || {
            serde_json::to_vec(self)
        })?;
        crate::util::write_run_bytes(
            &self.root.join(super::super::session_execution::STATE),
            &body,
            &self.run_id,
            "session",
        )
    }
}

impl<Profile: serde::de::DeserializeOwned> Session<Profile> {
    pub(crate) fn read_profile(root: &Path) -> anyhow::Result<Option<Self>> {
        Self::read_profile_using(root, |bytes| Ok(serde_json::from_slice(bytes)?))
    }

    fn read_profile_using(
        root: &Path,
        decode: impl FnOnce(&[u8]) -> anyhow::Result<Self>,
    ) -> anyhow::Result<Option<Self>> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(super::super::session_execution::STATE))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        use std::io::Read;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                // A verified open description may outlive an atomic record
                // replacement or concurrent removal. Zero links is safe for a
                // read-only snapshot; multiple links would alias authority.
                && metadata.nlink() <= 1
                && metadata.mode() & 0o077 == 0
                && metadata.len() <= MAX_SESSION_BYTES,
            "Session record must be owned/private, regular, without hardlink aliases and at most 64 MiB"
        );
        // serde_json's reader is unbuffered; passing a File would issue a read
        // syscall per byte on every lifecycle observation. Read the verified
        // descriptor in bulk and enforce the bound even if it grows after stat.
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_SESSION_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_SESSION_BYTES,
            "Session record exceeds 64 MiB"
        );
        let session = decode(&bytes)?;
        session.validate_root(root)?;
        Ok(Some(session))
    }
}

impl Session {
    pub(crate) fn read_task_profile(root: &Path) -> anyhow::Result<Option<Self>> {
        Self::read_profile_using(root, decode_task_session)
    }
}

pub(crate) enum SessionTransition<'a> {
    ExecutionStarted {
        record: &'a RunRecord,
    },
    CaptureAdmitted {
        id: &'a str,
        suspend: bool,
        ram_storage: SnapshotRamStorage,
    },
    CaptureAcknowledged {
        id: &'a str,
        checkpoint: &'a ExecutionCheckpoint,
    },
    CaptureRejected {
        id: &'a str,
        message: String,
    },
    RestoreAdmitted {
        id: &'a str,
        stage: &'a Path,
        eager_ram: bool,
    },
    RestoreAborted {
        admitted: &'a Session,
        previous: &'a Session,
        id: &'a str,
        no_successor: bool,
    },
    Completed(&'a RunResult),
    CompletionObserved {
        record: &'a RunRecord,
        result: Option<&'a RunResult>,
    },
    StoreBound {
        store: &'a Path,
    },
    CaptureReused {
        id: &'a str,
        suspend: bool,
        ram_storage: SnapshotRamStorage,
    },
    ForkAdmitted {
        id: &'a str,
        request: &'a ForkRequest,
    },
    CheckpointDeleted {
        id: &'a str,
    },
    Terminated,
}
#[path = "../session_execution.rs"]
pub mod session_execution;

impl Session {
    /// Read the Session's active stage; mutation belongs to typed transitions.
    pub fn active_stage(&self) -> &Path {
        &self.active_stage
    }

    /// Read the Session's previous stage; mutation belongs to typed transitions.
    pub fn previous_stage(&self) -> &Path {
        &self.previous_stage
    }

    /// Read the Session's config; mutation belongs to typed transitions.
    pub fn config(&self) -> &crate::RunConfig {
        &self.config
    }

    /// Read the Session's spec; mutation belongs to typed transitions.
    pub fn spec(&self) -> &RunSpec {
        &self.spec
    }

    /// Read the Session's head; mutation belongs to typed transitions.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// Read the Session's checkpoints; mutation belongs to typed transitions.
    pub fn checkpoints(&self) -> &BTreeMap<String, Capture> {
        &self.checkpoints
    }

    /// Read the Session's requests; mutation belongs to typed transitions.
    pub fn requests(&self) -> &BTreeMap<String, Request> {
        &self.requests
    }

    /// Read the Session's resumes; mutation belongs to typed transitions.
    pub fn resumes(&self) -> &BTreeMap<String, ResumeRequest> {
        &self.resumes
    }

    /// Read the Session's forks; mutation belongs to typed transitions.
    pub fn forks(&self) -> &BTreeMap<String, ForkRequest> {
        &self.forks
    }

    /// Read the Session's stores; mutation belongs to typed transitions.
    pub fn stores(&self) -> &BTreeSet<PathBuf> {
        &self.stores
    }

    pub(crate) fn new(
        run_id: String,
        stage: PathBuf,
        execution: Option<String>,
        config: crate::RunConfig,
        spec: RunSpec,
    ) -> anyhow::Result<Self> {
        ensure!(
            !run_id.is_empty() && stage.is_absolute(),
            "invalid Session identity or stage"
        );
        ensure!(
            execution.as_ref().is_none_or(|id| !id.is_empty()),
            "missing Execution identity"
        );
        let state = if execution.is_some() {
            SessionState::Running
        } else {
            SessionState::Restoring
        };
        // Restoring may be admitted before the first Execution is materialized.
        Ok(Self {
            version: super::super::session_execution::SESSION_SCHEMA_VERSION,
            run_id,
            root: stage.clone(),
            owner_epoch: execution.clone().unwrap_or_default(),
            active_execution: execution.unwrap_or_default(),
            state,
            kind: SessionKind::Task,
            profile: TaskProfile {
                active_stage: stage.clone(),
                previous_stage: stage,
                config,
                spec,
                head: None,
                checkpoints: BTreeMap::new(),
                requests: BTreeMap::new(),
                resumes: BTreeMap::new(),
                forks: BTreeMap::new(),
                stores: BTreeSet::new(),
            },
        })
    }

    pub(crate) fn owns_restore_transition(&self, admitted: &Self, id: &str) -> bool {
        self.state == SessionState::Restoring
            && matches!((serde_json::to_value(self), serde_json::to_value(admitted)), (Ok(a), Ok(b)) if a == b)
            && self
                .resumes
                .get(id)
                .is_some_and(|request| request.stage == admitted.active_stage)
    }

    // Each arm validates before mutation so rejected transitions leave no partial changes.
    pub(crate) fn apply(&mut self, input: SessionTransition<'_>) -> anyhow::Result<bool> {
        match input {
            SessionTransition::ExecutionStarted { record } => {
                ensure!(
                    self.run_id == record.run_id
                        && self.active_stage == record.stage_dir()
                        && self.state == SessionState::Restoring,
                    "invalid resumed Session ownership"
                );
                self.active_execution = record
                    .execution_id
                    .clone()
                    .context("missing Execution identity")?;
                self.owner_epoch = self.active_execution.clone();
                self.state = SessionState::Running;
                self.head = None;
            }
            SessionTransition::CaptureAdmitted {
                id,
                suspend,
                ram_storage,
            } => {
                ensure!(
                    self.state == SessionState::Running,
                    "SESSION_BUSY: execution state is {}",
                    self.state
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.requests.contains_key(id),
                    "duplicate or invalid capture admission"
                );
                self.requests.insert(
                    id.into(),
                    Request {
                        suspend,
                        ram_storage,
                        checkpoint: None,
                        error: None,
                    },
                );
                if suspend {
                    self.state = SessionState::Suspending;
                }
            }
            SessionTransition::CaptureAcknowledged { id, checkpoint } => {
                checkpoint.validate()?;
                ensure!(
                    checkpoint.source_run_id == self.run_id
                        && checkpoint.source_execution_id == self.active_execution,
                    "native capture owner mismatch"
                );
                let request = self.requests.get_mut(id).context("request disappeared")?;
                ensure!(
                    request.ram_storage == checkpoint.ram_storage,
                    "capture RAM storage mismatch"
                );
                request.checkpoint = Some(checkpoint.snapshot_id.clone());
                self.checkpoints
                    .entry(checkpoint.snapshot_id.clone())
                    .or_insert(Capture {
                        checkpoint: checkpoint.clone(),
                        branches: BTreeMap::new(),
                    });
            }
            SessionTransition::CaptureRejected { id, message } => {
                let request = self.requests.get_mut(id).context("request disappeared")?;
                request.error = Some(message);
                if request.suspend && self.state == SessionState::Suspending {
                    self.state = SessionState::Unknown;
                }
            }
            SessionTransition::RestoreAdmitted {
                id,
                stage,
                eager_ram,
            } => {
                ensure!(
                    self.state == SessionState::Suspended && self.head.is_some(),
                    "SESSION_BUSY: resume requires a confirmed suspended head (state={})",
                    self.state
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.resumes.contains_key(id),
                    "duplicate or invalid restore admission"
                );
                ensure!(
                    stage.starts_with(self.root.join("executions")),
                    "invalid successor stage"
                );
                self.previous_stage = self.active_stage.clone();
                self.active_stage = stage.to_path_buf();
                self.resumes.insert(
                    id.into(),
                    ResumeRequest {
                        stage: stage.to_path_buf(),
                        eager_ram,
                    },
                );
                self.state = SessionState::Restoring;
            }
            SessionTransition::RestoreAborted {
                admitted,
                previous,
                id,
                no_successor,
            } => {
                if !no_successor || !self.owns_restore_transition(admitted, id) {
                    return Ok(false);
                }
                ensure!(
                    previous.run_id == self.run_id
                        && previous.root == self.root
                        && previous.active_stage == self.previous_stage
                        && previous.active_execution == admitted.active_execution
                        && previous.head == admitted.head
                        && previous.state == SessionState::Suspended,
                    "invalid restore rollback"
                );
                *self = previous.clone();
            }
            SessionTransition::Completed(result) => self.complete(result)?,
            SessionTransition::CompletionObserved { record, result } => {
                if self.active_stage != record.stage_dir() {
                    return Ok(false);
                }
                self.validate_record_target(record)?;
                match result {
                    Some(result)
                        if matches!(
                            self.state,
                            SessionState::Running
                                | SessionState::Suspending
                                | SessionState::Unknown
                        ) =>
                    {
                        self.complete(result)?
                    }
                    Some(_) => return Ok(false),
                    None if self.state == SessionState::Unknown => return Ok(false),
                    None => self.state = SessionState::Unknown,
                }
            }
            SessionTransition::StoreBound { store } => {
                ensure!(store.is_absolute(), "invalid Session snapshot store");
                self.stores.insert(store.to_owned());
            }
            SessionTransition::CaptureReused {
                id,
                suspend,
                ram_storage,
            } => {
                ensure!(
                    self.state == SessionState::Suspended,
                    "Session has no suspended continuation"
                );
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.requests.contains_key(id),
                    "duplicate or invalid capture admission"
                );
                let checkpoint =
                    self.checkpoint(self.head.as_deref().context("missing suspended head")?)?;
                self.requests.insert(
                    id.into(),
                    Request {
                        suspend,
                        ram_storage,
                        checkpoint: Some(checkpoint.snapshot_id),
                        error: None,
                    },
                );
            }
            SessionTransition::ForkAdmitted { id, request } => {
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !self.forks.contains_key(id),
                    "duplicate or invalid fork admission"
                );
                ensure!(
                    !request.session_id.is_empty()
                        && request.session_id != self.run_id
                        && request.stage.is_absolute()
                        && !request.stage.starts_with(&self.root)
                        && !self.root.starts_with(&request.stage),
                    "invalid child Session binding"
                );
                let capture = self
                    .checkpoints
                    .get_mut(&request.checkpoint_id)
                    .context("checkpoint disappeared")?;
                ensure!(
                    !capture.branches.contains_key(&request.session_id),
                    "child Session already pinned"
                );
                capture
                    .branches
                    .insert(request.session_id.clone(), request.stage.clone());
                self.forks.insert(id.into(), request.clone());
            }
            SessionTransition::CheckpointDeleted { id } => {
                self.check_deletable_checkpoint(id)?;
                self.checkpoints.remove(id);
                for request in self.requests.values_mut() {
                    if request.checkpoint.as_deref() == Some(id) {
                        request.checkpoint = None;
                        request.error = Some("checkpoint deleted".into());
                    }
                }
            }
            SessionTransition::Terminated => {
                ensure!(
                    self.state == SessionState::Suspended,
                    "Session has no suspended continuation"
                );
                self.head = None;
                self.state = SessionState::Terminal;
            }
        }
        Ok(true)
    }
    pub(crate) fn check_deletable_checkpoint(&self, id: &str) -> anyhow::Result<()> {
        let capture = self.checkpoints.get(id).context("checkpoint disappeared")?;
        ensure!(
            self.head.as_deref() != Some(id) && capture.branches.is_empty(),
            "CHECKPOINT_REFERENCED: retained by suspended head or child Sessions"
        );
        Ok(())
    }

    fn complete(&mut self, result: &RunResult) -> anyhow::Result<()> {
        ensure!(
            result.state.is_terminal(),
            "cannot finish a Session with a nonterminal result"
        );
        ensure!(
            self.run_id == result.run_id.as_str()
                && self.active_execution == result.execution_id.as_str(),
            "terminal Execution identity mismatch"
        );
        if result.state == RunState::Hibernated {
            let receipt = ExecutionSuspension::from_result(result)?;
            let id = receipt.checkpoint.snapshot_id.clone();
            self.requests
                .get_mut(&receipt.request_id)
                .context("suspend receipt has no Session request")?
                .checkpoint = Some(id.clone());
            self.checkpoints.entry(id.clone()).or_insert(Capture {
                checkpoint: receipt.checkpoint,
                branches: BTreeMap::new(),
            });
            self.head = Some(id);
            self.state = SessionState::Suspended;
        } else {
            self.state = SessionState::Terminal;
            self.head = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_session_reuse_never_reuses_file_authority_or_rebinds_a_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let (root, mut session, _, _, _) = fixture();
        session.write().unwrap();
        let path = root
            .path()
            .join(super::super::super::session_execution::STATE);
        let original = Session::read_stage(root.path()).unwrap().unwrap();
        assert_eq!(
            Session::read_stage(root.path())
                .unwrap()
                .unwrap()
                .active_execution(),
            original.active_execution()
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Session::read_stage(root.path()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = root.path().join("alias.json");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(Session::read_stage(root.path()).is_err());
        std::fs::remove_file(alias).unwrap();

        let foreign = tempfile::tempdir().unwrap();
        std::fs::copy(
            &path,
            foreign
                .path()
                .join(super::super::super::session_execution::STATE),
        )
        .unwrap();
        assert!(
            Session::read_stage(foreign.path()).is_err(),
            "identical cached bytes cannot authorize a foreign root"
        );

        session.active_execution = "successor".into();
        session.owner_epoch = "successor".into();
        session.write().unwrap();
        let successor = Session::read_stage(root.path()).unwrap().unwrap();
        assert_eq!(successor.active_execution(), "successor");
        assert_eq!(original.active_execution(), "execution");
        std::fs::write(&path, b"{invalid").unwrap();
        assert!(Session::read_stage(root.path()).is_err());
    }

    #[test]
    fn session_record_bulk_read_preserves_complete_json_validation() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let profile = serde_json::json!({"payload": "雪\t".repeat(16 * 1024)});
        let session = Session::from_profile(
            "session".into(),
            root.path().into(),
            String::new(),
            SessionState::Created,
            SessionKind::Container,
            String::new(),
            profile.clone(),
        )
        .unwrap();
        session.write().unwrap();
        let path = root
            .path()
            .join(super::super::super::session_execution::STATE);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            serde_json::to_vec(&session).unwrap(),
            "machine Session records must not amplify repeated reads with indentation"
        );
        let read = || Session::<serde_json::Value>::read_profile(root.path());
        assert_eq!(read().unwrap().unwrap().profile(), &profile);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&vec![b' '; 16 * 1024]).unwrap();
        assert_eq!(read().unwrap().unwrap().profile(), &profile);
        file.write_all(b"{}").unwrap();
        assert!(read().is_err(), "trailing JSON must not be ignored");

        session.write().unwrap();
        let alias = root.path().join("aliased.json");
        std::fs::hard_link(&path, alias).unwrap();
        assert!(read().is_err(), "bulk reads must retain authority checks");

        session.write().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_SESSION_BYTES + 1)
            .unwrap();
        assert!(read().is_err(), "oversized records must not be read");
    }

    fn fixture() -> (
        tempfile::TempDir,
        Session,
        RunRecord,
        ExecutionCheckpoint,
        RunResult,
    ) {
        let root = tempfile::tempdir().unwrap();
        let session = Session::new(
            "session".into(),
            root.path().into(),
            Some("execution".into()),
            crate::RunConfig::default(),
            RunSpec::process("session", "test", "true"),
        )
        .unwrap();
        let record = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"session","execution_id":"execution","session_id":"session","agent":"test","pid":0,"command":["true"],"state":"running","storage":root.path(),"started_at_unix_ms":1,"network":{},"gateway_listen":null,"overlay":null
        })).unwrap();
        let checkpoint = ExecutionCheckpoint {
            snapshot_id: "a".repeat(64),
            store: root.path().join("snapshots"),
            source_run_id: "session".into(),
            source_execution_id: "execution".into(),
            created_at_unix_ms: 2,
            ram_storage: SnapshotRamStorage::Raw,
        };
        let result = serde_json::from_value(serde_json::json!({
            "run_id":"session","execution_id":"execution","state":"hibernated","started_at_unix_ms":1,"finished_at_unix_ms":3,"value":{"request_id":"suspend","checkpoint":checkpoint}
        })).unwrap();
        (root, session, record, checkpoint, result)
    }
    fn suspend(session: &mut Session, checkpoint: &ExecutionCheckpoint, result: &RunResult) {
        session
            .apply(SessionTransition::CaptureAdmitted {
                id: "suspend",
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
            })
            .unwrap();
        assert_eq!(session.state(), SessionState::Suspending);
        session
            .apply(SessionTransition::CaptureAcknowledged {
                id: "suspend",
                checkpoint,
            })
            .unwrap();
        assert_eq!(session.state(), SessionState::Suspending);
        assert!(session.head.is_none());
        session.apply(SessionTransition::Completed(result)).unwrap();
        assert_eq!(session.state(), SessionState::Suspended);
    }
    #[test]
    fn checkpoint_deletion_retains_request_fences_and_rejects_references_atomically() {
        let (root, mut session, _, checkpoint, result) = fixture();
        session
            .apply(SessionTransition::StoreBound {
                store: &checkpoint.store,
            })
            .unwrap();
        suspend(&mut session, &checkpoint, &result);
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::CheckpointDeleted {
                    id: &checkpoint.snapshot_id
                })
                .is_err()
        );
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
        session.apply(SessionTransition::Terminated).unwrap();
        let mut unreferenced = session.clone();
        unreferenced
            .apply(SessionTransition::CheckpointDeleted {
                id: &checkpoint.snapshot_id,
            })
            .unwrap();
        assert!(unreferenced.checkpoints().is_empty());
        assert!(unreferenced.requests()["suspend"].checkpoint.is_none());
        assert_eq!(
            unreferenced.requests()["suspend"].error.as_deref(),
            Some("checkpoint deleted")
        );
        let request = ForkRequest {
            options: crate::runtime::session_execution::ForkOptions {
                checkpoint: Some(checkpoint.snapshot_id.clone()),
                stage: None,
                name: None,
                ram_storage: None,
                eager_ram: false,
            },
            stage: root.path().with_extension("child"),
            session_id: "child".into(),
            checkpoint_id: checkpoint.snapshot_id.clone(),
        };
        session
            .apply(SessionTransition::ForkAdmitted {
                id: "fork",
                request: &request,
            })
            .unwrap();
        assert_eq!(
            session.checkpoints()[&checkpoint.snapshot_id].branches["child"],
            request.stage
        );
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::CheckpointDeleted {
                    id: &checkpoint.snapshot_id
                })
                .is_err()
        );
        assert!(
            session
                .apply(SessionTransition::ForkAdmitted {
                    id: "fork",
                    request: &request
                })
                .is_err()
        );
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
    }

    #[test]
    fn termination_withdraws_continuation_without_erasing_history_or_reviving_old_receipts() {
        let (_root, mut session, record, checkpoint, result) = fixture();
        suspend(&mut session, &checkpoint, &result);
        session.apply(SessionTransition::Terminated).unwrap();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert_eq!(before, serde_json::to_value(&session).unwrap());
        assert_eq!(session.state(), SessionState::Terminal);
        assert!(session.head.is_none());
        assert_eq!(session.checkpoints.len(), 1);
        assert!(session.requests["suspend"].checkpoint.is_some());
    }
    #[test]
    fn restore_rolls_back_only_the_unchanged_admission_without_a_successor() {
        let (root, mut session, record, checkpoint, result) = fixture();
        suspend(&mut session, &checkpoint, &result);
        let previous = session.clone();
        let stage = root.path().join("executions/next");
        session
            .apply(SessionTransition::RestoreAdmitted {
                id: "resume",
                stage: &stage,
                eager_ram: false,
            })
            .unwrap();
        let admitted = session.clone();
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert!(
            !session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: false
                })
                .unwrap()
        );
        assert!(
            session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: true
                })
                .unwrap()
        );
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            serde_json::to_value(&previous).unwrap()
        );
        session
            .apply(SessionTransition::RestoreAdmitted {
                id: "resume",
                stage: &stage,
                eager_ram: false,
            })
            .unwrap();
        let mut successor = record;
        successor.storage = stage;
        successor.execution_id = Some("successor".into());
        session
            .apply(SessionTransition::ExecutionStarted { record: &successor })
            .unwrap();
        assert!(
            !session
                .apply(SessionTransition::RestoreAborted {
                    admitted: &admitted,
                    previous: &previous,
                    id: "resume",
                    no_successor: true
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Running);
        assert_eq!(session.active_execution, "successor");
    }
    #[test]
    fn missing_receipt_recovery_is_idempotent_and_can_later_confirm_completion() {
        let (_root, mut session, record, _checkpoint, mut result) = fixture();
        assert!(
            session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: None
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Unknown);
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: None
                })
                .unwrap()
        );
        result.state = RunState::Completed;
        result.value = None;
        result.exit_code = Some(0);
        assert!(
            session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
        assert_eq!(session.state(), SessionState::Terminal);
        assert!(
            !session
                .apply(SessionTransition::CompletionObserved {
                    record: &record,
                    result: Some(&result)
                })
                .unwrap()
        );
    }

    #[test]
    fn invalid_receipts_and_duplicate_admissions_leave_the_aggregate_unchanged() {
        let (_root, mut session, _record, checkpoint, mut result) = fixture();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::Completed(&result))
                .is_err()
        ); // Missing scoped request.
        assert_eq!(before, serde_json::to_value(&session).unwrap());
        session
            .apply(SessionTransition::CaptureAdmitted {
                id: "suspend",
                suspend: true,
                ram_storage: SnapshotRamStorage::Raw,
            })
            .unwrap();
        let before = serde_json::to_value(&session).unwrap();
        assert!(
            session
                .apply(SessionTransition::CaptureAdmitted {
                    id: "suspend",
                    suspend: true,
                    ram_storage: SnapshotRamStorage::Raw
                })
                .is_err()
        );
        let mut invalid_owner = checkpoint.clone();
        invalid_owner.source_execution_id = "foreign".into();
        let mut invalid_storage = checkpoint.clone();
        invalid_storage.ram_storage = SnapshotRamStorage::Compressed;
        for invalid in [invalid_owner, invalid_storage] {
            assert!(
                session
                    .apply(SessionTransition::CaptureAcknowledged {
                        id: "suspend",
                        checkpoint: &invalid,
                    })
                    .is_err()
            );
            assert_eq!(before, serde_json::to_value(&session).unwrap());
        }
        result.execution_id = "foreign".into();
        assert!(
            session
                .apply(SessionTransition::Completed(&result))
                .is_err()
        );
        assert!(
            session
                .apply(SessionTransition::CaptureAcknowledged {
                    id: "missing",
                    checkpoint: &checkpoint
                })
                .is_err()
        );
        assert_eq!(before, serde_json::to_value(&session).unwrap());
    }
}
