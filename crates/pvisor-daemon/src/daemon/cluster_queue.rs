//! Durable Home ledger and executor pull loop. The daemon store owns the directory
//! lock; this module retains the daemon across detached commits. No fence is GC'd.
use super::super::models::{ClaimIdentity, ClaimPhase, ClaimView};
use super::super::{Sandbox, claims::QueuedCreate};
use super::*;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    sync::Mutex,
};

const VERSION: u32 = 1;
const MAX_TASKS: usize = 4096;
// Both encoded ledger bytes and logical charge (including future results) must
// fit 32 MiB. Bodies remain JSON numeric byte arrays, not base64: n bytes can
// occupy 4*n+1 encoded bytes. Charge that actual expansion, never just body.len().
const MAX_BYTES: usize = 32 * 1024 * 1024;
const RESULT_BYTES: usize = BODY + 64 * 1024;
const LEDGER_OVERHEAD: usize = 1024;
const LIVE_TASK_OVERHEAD: usize = 8192;
// Live charge = encoded body + 8 KiB + 1,114,112 result bytes; terminal charge =
// encoded task + 1. With no retained terminals, at most 29 small live tasks fit
// (the small-request fixture reaches 29); near-worst-case 1 MiB bodies fit only 6.
// The 4,096 count cap includes retained terminal fences, not 4,096 live slots.
const QUEUE_SECONDS: i64 = 120;
const OFFER_SECONDS: i64 = 10;
const TASK_PREFIX: &str = "/v1/cluster/tasks/";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Peer {
    pub home: String,
    pub executor: String,
    pub key_digest: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Capacity {
    pub images: Vec<String>,
    pub count: u64,
    pub cpu: u64,
    pub memory: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Grant {
    pub executor: String,
    pub identity: ClaimIdentity,
    pub fingerprint: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Offer {
    pub grant: Grant,
    pub body: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", deny_unknown_fields)]
enum Phase {
    Queued,
    Offered {
        grant: Grant,
        expires: i64,
    },
    Assigned {
        grant: Grant,
    },
    Completed {
        grant: Option<Grant>,
        result: Box<Result<Sandbox, ApiError>>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    fingerprint: String,
    body: Option<Vec<u8>>,
    image: String,
    cpu: u64,
    memory: u64,
    deadline: i64,
    generation: u64,
    state: Phase,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u32,
    owner: String,
    tasks: Vec<Task>,
}
impl Ledger {
    fn charge(&self) -> serde_json::Result<usize> {
        self.tasks.iter().try_fold(LEDGER_OVERHEAD, |sum, task| {
            // Phase-independent headroom keeps assignment, expiry and completion
            // possible at admission capacity. This exact rule also applies on open.
            let charge = if matches!(task.state, Phase::Completed { .. }) {
                serde_json::to_vec(task)?.len() + 1
            } else {
                serde_json::to_vec(&task.body)?.len() + LIVE_TASK_OVERHEAD + RESULT_BYTES
            };
            Ok(sum.saturating_add(charge))
        })
    }

    fn fits(&self, encoded_len: usize) -> serde_json::Result<bool> {
        Ok(
            self.tasks.len() <= MAX_TASKS
                && encoded_len <= MAX_BYTES
                && self.charge()? <= MAX_BYTES,
        )
    }
}
struct State {
    ledger: Ledger,
    failed: bool,
}
pub(super) struct Queue {
    daemon: Arc<Daemon>,
    directory: File,
    _lock: File,
    state: Mutex<State>,
    observation: Mutex<QueueObservation>,
}

#[derive(Default)]
struct QueueObservation {
    counts: [usize; 4],
    oldest_created: Option<i64>,
    failed: bool,
}
impl QueueObservation {
    fn from_ledger(ledger: &Ledger) -> Self {
        let mut result = Self::default();
        for task in &ledger.tasks {
            let index = match task.state {
                Phase::Queued => {
                    let created = task.deadline.saturating_sub(QUEUE_SECONDS);
                    result.oldest_created = Some(
                        result
                            .oldest_created
                            .map_or(created, |old| old.min(created)),
                    );
                    0
                }
                Phase::Offered { .. } => 1,
                Phase::Assigned { .. } => 2,
                Phase::Completed { .. } => 3,
            };
            result.counts[index] += 1;
        }
        result
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct TaskView {
    id: String,
    home: String,
    fingerprint: String,
    phase: String,
    grant: Option<Grant>,
    result: Option<Result<Sandbox, ApiError>>,
}
impl Task {
    fn view(&self, home: &str) -> TaskView {
        let (phase, grant, result) = match &self.state {
            Phase::Queued => ("Queued", None, None),
            Phase::Offered { grant, .. } => ("Offered", Some(grant.clone()), None),
            Phase::Assigned { grant } => ("Assigned", Some(grant.clone()), None),
            Phase::Completed { grant, result } => {
                ("Completed", grant.clone(), Some((**result).clone()))
            }
        };
        TaskView {
            id: self.id.clone(),
            home: home.into(),
            fingerprint: self.fingerprint.clone(),
            phase: phase.into(),
            grant,
            result,
        }
    }
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn fingerprint(body: &[u8]) -> Result<String, ApiError> {
    // Canonical JSON recursively sorts object keys, independent of serde_json's
    // optional preserve_order feature; arrays and absent/default fields stay distinct.
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let value: Value =
        serde_json::from_slice(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(digest(
        &serde_json::to_vec(&canonical(value)).map_err(|e| ApiError::internal(e.to_string()))?,
    ))
}
fn private(file: &File, directory: bool) -> anyhow::Result<()> {
    let m = file.metadata()?;
    anyhow::ensure!(
        m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
        "queue storage must be private and owned by daemon uid"
    );
    anyhow::ensure!(
        if directory {
            m.is_dir()
        } else {
            m.is_file() && m.nlink() == 1
        },
        "invalid queue storage type/link count"
    );
    Ok(())
}
fn open_at(directory: &File, name: &str, flags: i32) -> std::io::Result<File> {
    let name = std::ffi::CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
impl Queue {
    /// Bounded in-memory counts only: no expiry sweep, disk I/O, or payloads.
    pub(super) fn observation(&self) -> serde_json::Value {
        let summary = self.observation.lock().unwrap();
        let counts = summary.counts;
        json!({"Queued": counts[0], "Offered": counts[1], "Assigned": counts[2],
            "Completed": counts[3],
            "oldest_queued_seconds": summary.oldest_created.map(|created| now().saturating_sub(created).max(0)),
            "storage_failed": summary.failed, "retained_len": counts.iter().sum::<usize>()})
    }

    pub(super) fn open(daemon: Arc<Daemon>, owner: String) -> anyhow::Result<Arc<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&daemon.config.state_dir)?;
        private(&directory, true)?;
        let mut lock = open_at(
            &directory,
            "cluster-queue.lock",
            libc::O_RDWR | libc::O_CREAT,
        )?;
        private(&lock, false)?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        let ledger = match open_at(&directory, "cluster-queue.json", libc::O_RDONLY) {
            Ok(file) => {
                private(&file, false)?;
                anyhow::ensure!(
                    file.metadata()?.len() <= MAX_BYTES as u64,
                    "queue ledger too large"
                );
                let mut bytes = Vec::new();
                file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= MAX_BYTES, "queue ledger too large");
                let ledger: Ledger = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    ledger.version == VERSION
                        && ledger.owner == owner
                        && ledger.tasks.len() <= MAX_TASKS,
                    "queue owner/version/count mismatch"
                );
                let mut ids = BTreeSet::new();
                for task in &ledger.tasks {
                    anyhow::ensure!(
                        uuid(&task.id) && ids.insert(&task.id) && valid_owner(&task.fingerprint),
                        "invalid queue task identity"
                    );
                    let terminal = matches!(task.state, Phase::Completed { .. });
                    anyhow::ensure!(
                        terminal == task.body.is_none(),
                        "queue payload/phase mismatch"
                    );
                    if let Some(body) = &task.body {
                        anyhow::ensure!(
                            body.len() <= BODY && fingerprint(body)? == task.fingerprint,
                            "queue request digest mismatch"
                        );
                        let request: QueuedCreate = serde_json::from_slice(body)?;
                        let validated = request.validate(daemon.config.max_timeout_seconds)?;
                        anyhow::ensure!(
                            validated.image.uri == task.image
                                && validated.cpu_millis == task.cpu
                                && validated.memory_bytes == task.memory,
                            "queue resource mismatch"
                        );
                    }
                    if let Some(grant) = task.view(&owner).grant {
                        anyhow::ensure!(
                            valid_owner(&grant.executor)
                                && uuid(&grant.identity.claim_id)
                                && grant.identity.home == owner
                                && grant.identity.task_id == task.id
                                && grant.identity.generation == task.generation
                                && grant.fingerprint == task.fingerprint,
                            "invalid queue assignment"
                        );
                    }
                }
                ledger
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && lock.metadata()?.len() == 0 => {
                Ledger {
                    version: VERSION,
                    owner,
                    tasks: Vec::new(),
                }
            }
            Err(e) => return Err(e.into()),
        };
        let bytes = serde_json::to_vec(&ledger)?;
        anyhow::ensure!(
            ledger.fits(bytes.len())?,
            "queue ledger exceeds retention budget"
        );
        Self::persist(&directory, &bytes)?;
        // The permanent marker makes loss of an activated ledger fail closed.
        lock.write_all(b"initialized\n")?;
        lock.sync_all()?;
        directory.sync_all()?;
        Ok(Arc::new(Self {
            daemon,
            directory,
            _lock: lock,
            observation: Mutex::new(QueueObservation::from_ledger(&ledger)),
            state: Mutex::new(State {
                ledger,
                failed: false,
            }),
        }))
    }
    fn persist(directory: &File, bytes: &[u8]) -> anyhow::Result<()> {
        let temporary = format!(".cluster-queue-{}", uuid::Uuid::new_v4());
        let mut file = open_at(
            directory,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        private(&file, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let source = std::ffi::CString::new(temporary)?;
        let target = c"cluster-queue.json";
        let result = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        directory.sync_all()?;
        Ok(())
    }
    async fn transaction<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Ledger) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let this = self.clone();
        // Blocking task owns the entire read/modify/fsync/publish critical section.
        // Cancelling its await never cancels or splits a durable mutation.
        tokio::task::spawn_blocking(move || {
            this.daemon.ensure_storage()?;
            let mut state = this.state.lock().map_err(unavailable)?;
            if state.failed {
                return Err(unavailable("queue storage fail-stop"));
            }
            let mut next = state.ledger.clone();
            let result = operation(&mut next)?;
            let bytes = serde_json::to_vec(&next).map_err(|e| ApiError::internal(e.to_string()))?;
            let previous =
                serde_json::to_vec(&state.ledger).map_err(|e| ApiError::internal(e.to_string()))?;
            if bytes == previous {
                return Ok(result);
            }
            // Reserve room for terminal results before admitting work. Fences are
            // retained forever; exhausted retention is an explicit admission error.
            if !next
                .fits(bytes.len())
                .map_err(|e| ApiError::internal(e.to_string()))?
            {
                return Err(ApiError::new(
                    429,
                    "CLUSTER_QUEUE_FULL",
                    "durable queue retention budget exhausted; no fences were evicted",
                ));
            }
            if let Err(error) = Self::persist(&this.directory, &bytes) {
                state.failed = true;
                this.observation.lock().unwrap().failed = true;
                return Err(unavailable(error));
            }
            let summary = QueueObservation::from_ledger(&next);
            state.ledger = next;
            *this.observation.lock().unwrap() = summary;
            Ok(result)
        })
        .await
        .map_err(unavailable)?
    }
    pub(super) async fn enqueue(self: &Arc<Self>, body: Vec<u8>) -> Result<TaskView, ApiError> {
        if body.len() > BODY {
            return Err(ApiError::bad_request("create body too large"));
        }
        let request: QueuedCreate =
            serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
        let request = request.validate(self.daemon.config.max_timeout_seconds)?;
        let fingerprint = fingerprint(&body)?;
        self.transaction(move |ledger| {
            let task = Task {
                id: uuid::Uuid::new_v4().to_string(),
                fingerprint,
                body: Some(body),
                image: request.image.uri,
                cpu: request.cpu_millis,
                memory: request.memory_bytes,
                deadline: now() + QUEUE_SECONDS,
                generation: 0,
                state: Phase::Queued,
            };
            let view = task.view(&ledger.owner);
            ledger.tasks.push(task);
            Ok(view)
        })
        .await
    }
    pub(super) async fn task(self: &Arc<Self>, id: String) -> Result<TaskView, ApiError> {
        self.transaction(move |ledger| {
            ledger
                .tasks
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.view(&ledger.owner))
                .ok_or_else(|| ApiError::new(404, "TASK_NOT_FOUND", "Home has no such task"))
        })
        .await
    }
    fn expire(ledger: &mut Ledger) {
        let time = now();
        for task in &mut ledger.tasks {
            if matches!(task.state, Phase::Queued | Phase::Offered { .. }) && task.deadline <= time
            {
                task.state = Phase::Completed {
                    grant: None,
                    result: Box::new(Err(ApiError::new(
                        503,
                        "QUEUE_DEADLINE_EXCEEDED",
                        "task expired before assignment; no launch authorized",
                    ))),
                };
                task.body = None;
            } else if matches!(task.state, Phase::Offered { expires, .. } if expires <= time) {
                task.state = Phase::Queued;
            }
        }
    }
    async fn assigned(self: &Arc<Self>, cursor: usize) -> Result<Vec<Grant>, ApiError> {
        self.transaction(move |ledger| {
            let count = ledger.tasks.len();
            Ok((0..count)
                .filter_map(
                    |offset| match &ledger.tasks[(cursor + offset) % count].state {
                        Phase::Assigned { grant } => Some(grant.clone()),
                        _ => None,
                    },
                )
                .take(4)
                .collect())
        })
        .await
    }
    pub(super) async fn sweep(self: &Arc<Self>) -> Result<(), ApiError> {
        self.transaction(|ledger| {
            Self::expire(ledger);
            Ok(())
        })
        .await
    }
    pub(super) async fn pull(
        self: &Arc<Self>,
        executor: String,
        capacity: Capacity,
    ) -> Result<Option<Offer>, ApiError> {
        self.transaction(move |ledger| {
            Self::expire(ledger);
            let home = ledger.owner.clone();
            let Some(task) = ledger.tasks.iter_mut().find(|task| {
                matches!(task.state, Phase::Queued)
                    && capacity.count > 0
                    && capacity.cpu >= task.cpu
                    && capacity.memory >= task.memory
                    && capacity.images.contains(&task.image)
            }) else {
                return Ok(None);
            };
            task.generation = task
                .generation
                .checked_add(1)
                .ok_or_else(|| unavailable("generation exhausted"))?;
            let grant = Grant {
                executor,
                identity: ClaimIdentity {
                    home,
                    task_id: task.id.clone(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    generation: task.generation,
                },
                fingerprint: task.fingerprint.clone(),
            };
            task.state = Phase::Offered {
                grant: grant.clone(),
                expires: now() + OFFER_SECONDS,
            };
            Ok(Some(Offer {
                grant,
                body: task.body.clone().unwrap(),
            }))
        })
        .await
    }
    pub(super) async fn accept(self: &Arc<Self>, grant: Grant) -> Result<bool, ApiError> {
        self.transaction(move |ledger| {
            Self::expire(ledger);
            let Some(task) = ledger
                .tasks
                .iter_mut()
                .find(|t| t.id == grant.identity.task_id)
            else {
                return Ok(false);
            };
            let matches = |old: &Grant| {
                old.executor == grant.executor
                    && old.identity == grant.identity
                    && old.fingerprint == grant.fingerprint
            };
            match &task.state {
                Phase::Offered { grant: old, .. } if matches(old) => {
                    task.state = Phase::Assigned { grant };
                    Ok(true)
                }
                Phase::Assigned { grant: old } if matches(old) => Ok(true),
                _ => Ok(false),
            }
        })
        .await
    }
    pub(super) async fn report(
        self: &Arc<Self>,
        grant: Grant,
        result: Result<Sandbox, ApiError>,
    ) -> Result<(), ApiError> {
        if serde_json::to_vec(&result)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .len()
            > RESULT_BYTES - 4096
        {
            return Err(ApiError::bad_request("claim result exceeds budget"));
        }
        if let Ok(sandbox) = &result
            && sandbox.id != format!("sb-{}", grant.identity.claim_id)
        {
            return Err(ApiError::bad_request("claim result ID mismatch"));
        }
        self.transaction(move |ledger| {
            let task = ledger
                .tasks
                .iter_mut()
                .find(|t| t.id == grant.identity.task_id)
                .ok_or_else(|| ApiError::new(409, "STALE_CLAIM", "unknown task"))?;
            let old = match &task.state {
                Phase::Assigned { grant }
                | Phase::Completed {
                    grant: Some(grant), ..
                } => grant,
                _ => return Err(ApiError::new(409, "STALE_CLAIM", "task not assigned")),
            };
            if old.identity != grant.identity
                || old.executor != grant.executor
                || old.fingerprint != grant.fingerprint
            {
                return Err(ApiError::new(409, "STALE_CLAIM", "assignment mismatch"));
            }
            if matches!(task.state, Phase::Assigned { .. }) {
                task.state = Phase::Completed {
                    grant: Some(grant),
                    result: Box::new(result),
                };
                task.body = None;
            }
            Ok(())
        })
        .await
    }
}

impl Peer {
    pub(super) fn check(&self, owner: &str, daemon: &Daemon) -> Result<(), ApiError> {
        if self.home != owner
            || !valid_owner(&self.executor)
            || self.key_digest != digest(daemon.config.api_key.as_bytes())
        {
            return Err(unavailable("queue peer identity/key mismatch"));
        }
        Ok(())
    }
    pub(super) fn check_grant(&self, grant: &Grant) -> Result<(), ApiError> {
        if grant.executor != self.executor
            || grant.identity.home != self.home
            || !uuid(&grant.identity.claim_id)
            || !uuid(&grant.identity.task_id)
            || !valid_owner(&grant.fingerprint)
        {
            return Err(ApiError::bad_request("invalid queue grant"));
        }
        Ok(())
    }
}

impl Cluster {
    fn peer_auth(&self, home: &str) -> Peer {
        Peer {
            home: home.into(),
            executor: self.owner.clone(),
            key_digest: digest(self.daemon.config.api_key.as_bytes()),
        }
    }
    async fn pull_offer(
        &self,
        peers: &mut [(ActorRef, Identity)],
        capacity: Capacity,
        remote_first: &mut bool,
    ) -> Option<(Offer, Option<ActorRef>)> {
        if self.discovery.lock().unwrap().check().is_err()
            || capacity.count == 0
            || capacity.cpu == 0
            || capacity.memory == 0
        {
            return None;
        }
        // Alternate eligible rounds, not successful steals: sustained local work
        // must not suppress remote polling. Empty/unreachable preferences fall back
        // in the same round. Admission still happens in reserve_claim, atomically.
        let prefer_remote = *remote_first;
        *remote_first = !prefer_remote;
        for remote in [prefer_remote, !prefer_remote] {
            if !remote {
                if let Ok(Some(offer)) = self.queue.pull(self.owner.clone(), capacity.clone()).await
                {
                    return Some((offer, None));
                }
                continue;
            }
            let sampled;
            let peers = if peers.is_empty() {
                sampled = self.sample_peers().await.unwrap_or_default();
                sampled.as_slice()
            } else {
                peers.sort_by_cached_key(|_| uuid::Uuid::new_v4());
                &*peers
            };
            for (_, status) in peers.iter().filter(|(_, s)| s.owner != self.owner).take(2) {
                let Ok((actor, status)) = self.owner_peer(&status.owner).await else {
                    continue;
                };
                let result: anyhow::Result<Result<Option<Offer>, ApiError>> = self
                    .rpc(
                        &actor,
                        Call::Pull {
                            peer: self.peer_auth(&status.owner),
                            capacity: capacity.clone(),
                        },
                        STATUS_TIME,
                    )
                    .await;
                if let Ok(Ok(Some(offer))) = result
                    && offer.grant.identity.home == status.owner
                    && offer.grant.executor == self.owner
                {
                    return Some((offer, Some(actor.clone())));
                }
            }
        }
        None
    }
    pub(super) fn start_worker(this: &Arc<Self>) {
        let weak = Arc::downgrade(this);
        let task = tokio::spawn(async move {
            let mut busy = BTreeMap::<String, tokio::task::JoinHandle<()>>::new();
            // Read-only remote inspection can be cancelled on scheduler shutdown;
            // accepted claim launches below retain their detached ownership.
            let mut home_recovery = tokio::task::JoinSet::new();
            let mut cursor = 0usize;
            let mut home_cursor = 0usize;
            let mut backoff = 150u64;
            let mut remote_first = false;
            loop {
                let mut pulled = false;
                let Some(cluster) = weak.upgrade() else {
                    break;
                };
                let mut peers = Vec::new();
                let _ = cluster.queue.sweep().await;
                busy.retain(|_, task| !task.is_finished());
                if busy.len() < 8
                    && let Ok((count, cpu, memory)) = cluster.daemon.capacity_usage().await
                {
                    let capacity = Capacity {
                        images: cluster.images.clone(),
                        count: (cluster.daemon.config.max_sandboxes as u64).saturating_sub(count),
                        cpu: cluster.daemon.config.cpu_millis.saturating_sub(cpu),
                        memory: cluster.daemon.config.memory_bytes.saturating_sub(memory),
                    };
                    if capacity.count > 0 && capacity.cpu > 0 && capacity.memory > 0 {
                        let offer = cluster
                            .pull_offer(&mut peers, capacity, &mut remote_first)
                            .await;
                        if let Some((offer, actor)) = offer {
                            pulled = true;
                            let copy = cluster.clone();
                            busy.insert(
                                offer.grant.identity.claim_id.clone(),
                                tokio::spawn(async move {
                                    if fingerprint(&offer.body).ok().as_ref()
                                        != Some(&offer.grant.fingerprint)
                                    {
                                        return;
                                    }
                                    let Ok(request) =
                                        serde_json::from_slice::<QueuedCreate>(&offer.body)
                                    else {
                                        return;
                                    };
                                    if copy
                                        .daemon
                                        .reserve_queued_claim(offer.grant.identity.clone(), request)
                                        .await
                                        .is_ok()
                                    {
                                        copy.authorize_launch(offer.grant, actor).await;
                                    }
                                }),
                            );
                        }
                    }
                }
                // Released launch failures are intentionally absent from executor
                // pending_claims. Home polls its durable assignments as well, so a
                // lost terminal report still recovers after either process restarts.
                while home_recovery.try_join_next().is_some() {}
                if home_recovery.is_empty()
                    && let Ok(grants) = cluster.queue.assigned(home_cursor).await
                {
                    home_cursor = home_cursor.wrapping_add(4);
                    let cluster = cluster.clone();
                    home_recovery.spawn(async move {
                        for grant in grants {
                            let observed = if grant.executor == cluster.owner {
                                cluster.daemon.inspect_claim(&grant.identity).await
                            } else if let Ok((actor, _)) = cluster.owner_peer(&grant.executor).await
                            {
                                cluster
                                    .rpc::<Result<ClaimView, ApiError>>(
                                        &actor,
                                        Call::Inspect {
                                            owner: grant.executor.clone(),
                                            key_digest: digest(
                                                cluster.daemon.config.api_key.as_bytes(),
                                            ),
                                            identity: grant.identity.clone(),
                                        },
                                        STATUS_TIME,
                                    )
                                    .await
                                    .map_err(unavailable)
                                    .and_then(|r| r)
                            } else {
                                continue;
                            };
                            if let Ok(view) = observed
                                && view.identity == grant.identity
                                && let Some(result) = view.creation_result
                            {
                                let _ = cluster.queue.report(grant, result).await;
                            }
                        }
                    });
                }
                let claims = cluster.daemon.pending_claims().await;
                // Rotate the recovery inventory so uncertain early IDs cannot starve
                // later claims. In-flight launches keep their slot until completion.
                let count = claims.len();
                for offset in 0..count {
                    if busy.len() >= 4 {
                        break;
                    }
                    let view = claims[(cursor + offset) % count].clone();
                    if busy.contains_key(&view.identity.claim_id) {
                        continue;
                    }
                    let copy = cluster.clone();
                    busy.insert(
                        view.identity.claim_id.clone(),
                        tokio::spawn(async move {
                            // Charge the recovery slot before any remote lookup.
                            // Failed owners cannot turn this into an unbounded,
                            // serial scan on the local scheduling path.
                            let home = &view.identity.home;
                            let actor = if home == &copy.owner {
                                None
                            } else {
                                let Ok((actor, _)) = copy.owner_peer(home).await else {
                                    return;
                                };
                                Some(actor)
                            };
                            copy.recover_claim(view, actor).await;
                        }),
                    );
                }
                cursor = if count == 0 { 0 } else { (cursor + 4) % count };
                drop(cluster);
                // No strong cluster reference while idle; aborting this scheduler
                // drops handles, not detached already accepted claim launches.
                let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]);
                backoff = if pulled { 150 } else { (backoff * 2).min(2000) };
                tokio::time::sleep(Duration::from_millis(backoff + jitter)).await;
            }
        });
        *this.worker.lock().unwrap() = Some(task);
    }
    async fn home_task(
        &self,
        home: &str,
        id: String,
        actor: Option<&ActorRef>,
    ) -> Result<TaskView, ApiError> {
        if home == self.owner {
            return self.queue.task(id).await;
        }
        let actor = actor.ok_or_else(|| unavailable("Home unavailable"))?;
        self.rpc(
            actor,
            Call::Task {
                peer: self.peer_auth(home),
                id,
            },
            STATUS_TIME,
        )
        .await
        .map_err(unavailable)?
    }
    async fn recover_claim(self: Arc<Self>, view: ClaimView, actor: Option<ActorRef>) {
        let Ok(task) = self
            .home_task(
                &view.identity.home,
                view.identity.task_id.clone(),
                actor.as_ref(),
            )
            .await
        else {
            return;
        };
        let grant = Grant {
            executor: self.owner.clone(),
            identity: view.identity.clone(),
            fingerprint: task.fingerprint,
        };
        match view.phase {
            ClaimPhase::Reserved => self.authorize_launch(grant, actor).await,
            ClaimPhase::Launching => {
                // GET reconciles live runtime state; it never invokes create.
                let _ = self.daemon.get(&view.sandbox.id).await;
                if let Ok(observed) = self.daemon.inspect_claim(&view.identity).await
                    && let Some(result) = observed.creation_result
                {
                    self.report_claim(grant, result, actor.as_ref()).await;
                }
            }
            ClaimPhase::Finished | ClaimPhase::Released => {
                if task.phase != "Completed"
                    && let Some(result) = view.creation_result
                {
                    self.report_claim(grant, result, actor.as_ref()).await;
                }
            }
        }
    }
    async fn authorize_launch(&self, grant: Grant, actor: Option<ActorRef>) {
        let accepted = if grant.identity.home == self.owner {
            self.queue.accept(grant.clone()).await
        } else if let Some(actor) = actor.as_ref() {
            self.rpc::<Result<bool, ApiError>>(
                actor,
                Call::Accept {
                    peer: self.peer_auth(&grant.identity.home),
                    grant: grant.clone(),
                },
                STATUS_TIME,
            )
            .await
            .map_err(unavailable)
            .and_then(|r| r)
        } else {
            return;
        };
        match accepted {
            Ok(true) => {
                let _ = self.daemon.launch_claim(&grant.identity).await;
                // A Launching uncertainty is not a terminal result. Preserve it for
                // runtime reconciliation, not a second launch or reassignment.
                if let Ok(view) = self.daemon.inspect_claim(&grant.identity).await
                    && let Some(result) = view.creation_result
                {
                    self.report_claim(grant, result, actor.as_ref()).await;
                }
            }
            Ok(false) => {
                let _ = self.daemon.release_claim(&grant.identity).await;
            }
            Err(_) => {} // Unknown Home/grant retains capacity indefinitely.
        }
    }
    async fn report_claim(
        &self,
        grant: Grant,
        result: Result<Sandbox, ApiError>,
        actor: Option<&ActorRef>,
    ) {
        if grant.identity.home == self.owner {
            let _ = self.queue.report(grant, result).await;
        } else if let Some(actor) = actor {
            let _: anyhow::Result<Result<(), ApiError>> = self
                .rpc(
                    actor,
                    Call::Report {
                        peer: self.peer_auth(&grant.identity.home),
                        grant,
                        result: Box::new(result),
                    },
                    STATUS_TIME,
                )
                .await;
        }
    }
    pub(super) async fn enqueue_wait(&self, body: Vec<u8>) -> Result<Response, ApiError> {
        self.discovery
            .lock()
            .unwrap()
            .check()
            .map_err(unavailable)?;
        let task = self.queue.enqueue(body).await?;
        let task_id = format!("{}.{}", self.owner, task.id);
        let waited = tokio::time::timeout(RPC_TIME, async {
            loop {
                let task = self.queue.task(task.id.clone()).await?;
                if let Some(result) = task.result {
                    return Ok::<_, ApiError>((task.grant, result));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        let mut response = match waited {
            Ok(Ok((grant, Ok(sandbox)))) => {
                let owner = grant.as_ref().map(|g| g.executor.as_str()).unwrap_or(&self.owner);
                let location = format!("/v1/sandboxes/{owner}.{}", sandbox.id);
                let mut value = serde_json::to_value(sandbox).map_err(unavailable)?;
                // Keep the OpenSandbox create response profile identical to the
                // local adapter (not the larger GET/registry Sandbox contract).
                if let Some(object) = value.as_object_mut() {
                    object.remove("image");
                    object.remove("snapshotId");
                    object.remove("updatedAt");
                }
                globalize(&mut value, owner);
                let mut response = (StatusCode::ACCEPTED, axum::Json(value)).into_response();
                response.headers_mut().insert("location", location.parse().map_err(unavailable)?);
                response
            }
            Ok(Ok((_, Err(error)))) => error.into_response(),
            _ => (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({"code":"CLUSTER_TASK_PENDING", "message":"Task durably accepted; do not resubmit. Query task status; assignment may be uncertain.", "task_id": task.id, "home": self.owner}))).into_response(),
        };
        response
            .headers_mut()
            .insert("x-pvisor-task-id", task_id.parse().unwrap());
        Ok(response)
    }
    pub(super) async fn task_http(&self, request: Request) -> Result<Response, ApiError> {
        let _ingress = self
            .ingress
            .try_acquire()
            .map_err(|_| unavailable("cluster inflight limit"))?;
        if !self.daemon.authorize(request.headers()) {
            return Err(ApiError::new(
                401,
                "UNAUTHORIZED",
                "Missing or invalid authentication credentials",
            ));
        }
        if request.method() != Method::GET {
            return Err(ApiError::new(
                405,
                "METHOD_NOT_ALLOWED",
                "task status supports GET only",
            ));
        }
        let (home, id) = request
            .uri()
            .path()
            .strip_prefix(TASK_PREFIX)
            .and_then(|v| v.split_once('.'))
            .filter(|(home, id)| valid_owner(home) && uuid(id))
            .ok_or_else(|| ApiError::bad_request("invalid global task ID"))?;
        let actor = if home == self.owner {
            None
        } else {
            Some(self.owner_peer(home).await.map_err(unavailable)?.0)
        };
        let task = self.home_task(home, id.into(), actor.as_ref()).await?;
        // An assignment is a cleanup target, not proof of a terminal create.
        // Never expose an Offered claim: it has no launch authorization yet.
        let assignment = task
            .grant
            .as_ref()
            .filter(|_| matches!(task.phase.as_str(), "Assigned" | "Completed"));
        let executor = assignment.map(|grant| grant.executor.as_str());
        let sandbox_id =
            assignment.map(|grant| format!("{}.sb-{}", grant.executor, grant.identity.claim_id));
        // Deliberately omit payload, image auth, metadata and runtime error text.
        let result = task.result.map(|r| match r {
            Ok(sandbox) => json!({"sandbox_id": format!("{}.{}", task.grant.as_ref().map(|g| g.executor.as_str()).unwrap_or(home), sandbox.id)}),
            Err(error) => json!({"error": {"code": error.code, "status": error.status}}),
        });
        Ok(
            axum::Json(json!({"task_id":id, "home":home, "phase":task.phase, "executor":executor, "sandboxId":sandbox_id, "result": result}))
                .into_response(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    async fn daemon(path: &std::path::Path) -> Arc<Daemon> {
        Daemon::open(
            super::super::super::Config {
                state_dir: path.into(),
                api_key: "queue-test-lifecycle-key-at-least-32-bytes".into(),
                public_endpoint: "127.0.0.1:8080".into(),
                max_sandboxes: 2,
                cpu_millis: 2000,
                memory_bytes: 256 * 1024 * 1024,
                max_timeout_seconds: 3600,
            },
            |_| Ok(Arc::new(super::super::tests::Fake::default()) as Arc<dyn Runtime>),
        )
        .await
        .unwrap()
    }
    fn body() -> Vec<u8> {
        serde_json::to_vec(&json!({"image":{"uri":"fixture"},"entrypoint":["/bin/sh"],"resourceLimits":{"cpu":"1","memory":"64Mi"},"env":{"SECRET":"do-not-expose"},"timeout":600})).unwrap()
    }
    fn capacity() -> Capacity {
        Capacity {
            images: vec!["fixture".into()],
            count: 1,
            cpu: 1000,
            memory: 64 * 1024 * 1024,
        }
    }
    async fn fixture() -> (tempfile::TempDir, Arc<Daemon>, Arc<Queue>) {
        let dir = tempfile::tempdir().unwrap();
        let daemon = daemon(dir.path()).await;
        let owner = digest(daemon.registry.lock().await.owner.as_bytes());
        let queue = Queue::open(daemon.clone(), owner).unwrap();
        (dir, daemon, queue)
    }

    #[tokio::test]
    async fn e2b_and_opensandbox_share_durable_home_fifo_and_profile_validation() {
        let (_dir, daemon, queue) = fixture().await;
        let owner = digest(daemon.registry.lock().await.owner.as_bytes());
        let e2b = serde_json::to_vec(&QueuedCreate::E2b {
            e2b_request: serde_json::from_value(json!({"templateID":"fixture", "timeout":5}))
                .unwrap(),
            profile: crate::runtime::E2bTemplate {
                cpu_millis: 1000,
                memory_bytes: 64 * 1024 * 1024,
                envd_version: "0.10.2".into(),
            },
        })
        .unwrap();
        let first = queue.enqueue(e2b.clone()).await.unwrap();
        let second = queue.enqueue(body()).await.unwrap();
        drop(queue);
        let queue = Queue::open(daemon.clone(), owner.clone()).unwrap();
        let offer = queue
            .pull(digest(b"executor"), capacity())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offer.grant.identity.task_id, first.id);
        assert_eq!(offer.body, e2b);
        queue.accept(offer.grant.clone()).await.unwrap();
        drop(queue);
        let queue = Queue::open(daemon, owner).unwrap();
        assert_eq!(queue.task(first.id).await.unwrap().phase, "Assigned");
        assert_eq!(
            queue
                .pull(digest(b"executor"), capacity())
                .await
                .unwrap()
                .unwrap()
                .grant
                .identity
                .task_id,
            second.id
        );
    }

    async fn stop_worker(cluster: &Cluster) {
        let worker = cluster.worker.lock().unwrap().take().unwrap();
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test]
    async fn stalled_remote_recovery_does_not_block_local_scheduling() {
        use super::super::tests::node;
        use std::sync::atomic::Ordering;

        // Exercise both Home assignment inspection and executor claim recovery.
        for home_assignment in [false, true] {
            let (cluster, runtime, _dir) = node(vec![], vec!["fixture".into()]).await;
            stop_worker(&cluster).await;
            let tasks = std::mem::take(&mut *cluster.background.lock().unwrap());
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            let remote = digest(b"unavailable remote owner");
            let mut reserved = Vec::new();
            if home_assignment {
                cluster.queue.enqueue(body()).await.unwrap();
                let offer = cluster
                    .queue
                    .pull(remote, capacity())
                    .await
                    .unwrap()
                    .unwrap();
                cluster.queue.accept(offer.grant).await.unwrap();
            } else {
                for _ in 0..8 {
                    let identity = ClaimIdentity {
                        home: remote.clone(),
                        task_id: uuid::Uuid::new_v4().to_string(),
                        claim_id: uuid::Uuid::new_v4().to_string(),
                        generation: 1,
                    };
                    cluster
                        .daemon
                        .reserve_claim(identity.clone(), serde_json::from_slice(&body()).unwrap())
                        .await
                        .unwrap();
                    reserved.push(identity);
                }
            }
            // Cold owner lookups must wait behind this refresh lock. Stealing
            // sees no remote candidates, so only recovery reaches this gate.
            let refresh = cluster.discovery_refresh.lock().await;
            Cluster::start_worker(&cluster);
            tokio::time::sleep(Duration::from_millis(100)).await;
            let first = cluster.queue.enqueue(body()).await.unwrap();
            let second = cluster.queue.enqueue(body()).await.unwrap();
            tokio::time::timeout(Duration::from_millis(2500), async {
                loop {
                    if cluster
                        .queue
                        .task(first.id.clone())
                        .await
                        .unwrap()
                        .result
                        .is_some()
                        && cluster
                            .queue
                            .task(second.id.clone())
                            .await
                            .unwrap()
                            .result
                            .is_some()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("local work must complete before the remote lookup timeout");
            assert_eq!(runtime.creates.load(Ordering::SeqCst), 2);
            for identity in reserved {
                assert_eq!(
                    cluster.daemon.inspect_claim(&identity).await.unwrap().phase,
                    ClaimPhase::Reserved
                );
            }
            if home_assignment {
                assert_eq!(cluster.queue.observation()["Assigned"], 1);
            }
            drop(refresh);
            cluster.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn observation_is_nonmutating_redacted_and_independent_of_commit_lock() {
        let (_dir, _daemon, queue) = fixture().await;
        assert_eq!(queue.observation()["oldest_queued_seconds"], Value::Null);
        queue.enqueue(body()).await.unwrap();
        {
            let _commit = queue.state.lock().unwrap();
            let observed = queue.observation();
            assert_eq!(observed["Queued"], 1);
            assert_eq!(observed["retained_len"], 1);
            assert_eq!(observed["storage_failed"], false);
            assert!(!observed.to_string().contains("SECRET"));
        }
        let executor = digest(b"executor");
        let offer = queue.pull(executor, capacity()).await.unwrap().unwrap();
        assert_eq!(queue.observation()["Offered"], 1);
        queue.accept(offer.grant.clone()).await.unwrap();
        assert_eq!(queue.observation()["Assigned"], 1);
        queue
            .report(offer.grant, Err(ApiError::bad_request("test")))
            .await
            .unwrap();
        let before = std::fs::read(_dir.path().join("cluster-queue.json")).unwrap();
        assert_eq!(queue.observation()["Completed"], 1);
        assert_eq!(queue.observation()["retained_len"], 1);
        assert_eq!(
            std::fs::read(_dir.path().join("cluster-queue.json")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn donor_identity_failure_blocks_pull_even_with_cached_peer() {
        use super::super::tests::{converge, node};
        use std::sync::atomic::Ordering;
        let (local, _, _ld) = node(vec![], vec!["fixture".into()]).await;
        let (remote, _, _rd) = node(vec![local.system.addr()], vec!["fixture".into()]).await;
        stop_worker(&local).await;
        stop_worker(&remote).await;
        converge(&local, &remote).await;
        remote.queue.enqueue(body()).await.unwrap();
        let mut peers = local.peers().await.unwrap();
        remote.faults.store(2, Ordering::SeqCst);
        {
            let mut cache = local.discovery.lock().unwrap();
            for (_, identity, at) in cache.identities.values_mut() {
                if identity.owner == remote.owner {
                    *at = tokio::time::Instant::now() - IDENTITY_FRESHNESS;
                }
            }
        }
        assert!(
            local
                .pull_offer(&mut peers, capacity(), &mut true)
                .await
                .is_none()
        );
        assert_eq!(remote.queue.observation()["Queued"], 1);
        remote.faults.store(1, Ordering::SeqCst);
        assert!(
            local
                .pull_offer(&mut peers, capacity(), &mut true)
                .await
                .is_some()
        );
        local.shutdown().await.unwrap();
        remote.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn eligible_rounds_alternate_backlogged_homes_after_capacity_release() {
        use super::super::tests::{converge, node};
        use std::sync::atomic::Ordering;
        let (local, runtime, _ld) = node(vec![], vec!["fixture".into()]).await;
        let (remote, other_runtime, _rd) =
            node(vec![local.system.addr()], vec!["fixture".into()]).await;
        converge(&local, &remote).await;
        stop_worker(&local).await;
        stop_worker(&remote).await;
        for _ in 0..3 {
            local.queue.enqueue(body()).await.unwrap();
            remote.queue.enqueue(body()).await.unwrap();
        }
        let mut peers = local.peers().await.unwrap();
        let mut remote_first = false;
        let mut none = capacity();
        none.count = 0;
        assert!(
            local
                .pull_offer(&mut peers, none, &mut remote_first)
                .await
                .is_none()
        );
        assert!(
            !remote_first,
            "ineligible rounds must not consume a fairness turn"
        );
        for expected_home in [&local.owner, &remote.owner, &local.owner, &remote.owner] {
            assert_eq!(local.daemon.capacity_usage().await.unwrap(), (0, 0, 0));
            let (offer, actor) = local
                .pull_offer(&mut peers, capacity(), &mut remote_first)
                .await
                .unwrap();
            assert_eq!(&offer.grant.identity.home, expected_home);
            let sandbox = local
                .daemon
                .reserve_claim(
                    offer.grant.identity.clone(),
                    serde_json::from_slice(&offer.body).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(local.daemon.capacity_usage().await.unwrap().0, 1);
            local.authorize_launch(offer.grant.clone(), actor).await;
            let home = if expected_home == &local.owner {
                &local
            } else {
                &remote
            };
            assert_eq!(
                home.queue
                    .task(offer.grant.identity.task_id)
                    .await
                    .unwrap()
                    .phase,
                "Completed"
            );
            local.daemon.delete(&sandbox.id).await.unwrap();
        }
        assert_eq!(runtime.creates.load(Ordering::SeqCst), 4);
        assert_eq!(other_runtime.creates.load(Ordering::SeqCst), 0);
        // Both queues stayed nonempty throughout all four eligible rounds.
        for home in [&local, &remote] {
            assert_eq!(
                home.queue
                    .state
                    .lock()
                    .unwrap()
                    .ledger
                    .tasks
                    .iter()
                    .filter(|t| matches!(t.state, Phase::Queued))
                    .count(),
                1
            );
        }
        // An empty preferred queue falls back in the same round.
        remote
            .queue
            .transaction(|ledger| {
                for task in &mut ledger.tasks {
                    task.deadline = now() - 1;
                }
                Queue::expire(ledger);
                Ok(())
            })
            .await
            .unwrap();
        remote_first = true;
        let (offer, _) = local
            .pull_offer(&mut peers, capacity(), &mut remote_first)
            .await
            .unwrap();
        assert_eq!(offer.grant.identity.home, local.owner);
        local.shutdown().await.unwrap();
        remote.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn delayed_report_uses_immutable_creation_result_after_lifecycle_and_delete() {
        use super::super::tests::node;
        use std::sync::atomic::Ordering;
        for failure in [false, true] {
            let (cluster, runtime, _dir) = node(vec![], vec!["fixture".into()]).await;
            stop_worker(&cluster).await;
            runtime.fail_clean.store(failure, Ordering::SeqCst);
            let task = cluster.queue.enqueue(body()).await.unwrap();
            let offer = cluster
                .queue
                .pull(cluster.owner.clone(), capacity())
                .await
                .unwrap()
                .unwrap();
            let sandbox = cluster
                .daemon
                .reserve_claim(
                    offer.grant.identity.clone(),
                    serde_json::from_slice(&offer.body).unwrap(),
                )
                .await
                .unwrap();
            assert!(cluster.queue.accept(offer.grant.clone()).await.unwrap());
            let original = cluster.daemon.launch_claim(&offer.grant.identity).await;
            assert_eq!(original.is_err(), failure);
            if !failure {
                cluster.daemon.pause(&sandbox.id).await.unwrap();
                cluster.daemon.delete(&sandbox.id).await.unwrap();
            }
            // Lose the immediate report and recover only after lifecycle state has
            // changed (or the failed create has already become a Released tombstone).
            let view = cluster
                .daemon
                .inspect_claim(&offer.grant.identity)
                .await
                .unwrap();
            assert_eq!(view.phase, ClaimPhase::Released);
            assert_eq!(
                serde_json::to_value(&view.creation_result).unwrap(),
                serde_json::to_value(Some(&original)).unwrap()
            );
            cluster.clone().recover_claim(view, None).await;
            let completed = cluster.queue.task(task.id).await.unwrap();
            assert_eq!(completed.phase, "Completed");
            assert_eq!(
                serde_json::to_value(completed.result).unwrap(),
                serde_json::to_value(Some(original)).unwrap()
            );
            assert_eq!(runtime.creates.load(Ordering::SeqCst), 1);
            cluster.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn uncertain_assigned_task_exposes_authenticated_cleanup_target_not_result() {
        use super::super::tests::{converge, http, node};
        use std::sync::atomic::Ordering;
        let (home, _, _hd) = node(vec![], vec!["other".into()]).await;
        let (executor, runtime, _ed) = node(vec![home.system.addr()], vec!["fixture".into()]).await;
        converge(&home, &executor).await;
        stop_worker(&home).await;
        stop_worker(&executor).await;
        runtime.uncertain.store(true, Ordering::SeqCst);
        let task = home.queue.enqueue(body()).await.unwrap();
        let mut peers = executor.peers().await.unwrap();
        let (offer, actor) = executor
            .pull_offer(&mut peers, capacity(), &mut true)
            .await
            .unwrap();
        executor
            .daemon
            .reserve_claim(
                offer.grant.identity.clone(),
                serde_json::from_slice(&offer.body).unwrap(),
            )
            .await
            .unwrap();
        executor
            .authorize_launch(offer.grant.clone(), actor.clone())
            .await;
        let view = executor
            .daemon
            .inspect_claim(&offer.grant.identity)
            .await
            .unwrap();
        assert_eq!(view.phase, ClaimPhase::Launching);
        assert!(view.creation_result.is_none());
        executor.clone().recover_claim(view, actor).await;
        let path = format!("/v1/cluster/tasks/{}.{}", home.owner, task.id);
        assert_eq!(
            http(&home, "GET", &path, vec![], false).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, task) = http(&home, "GET", &path, vec![], true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(task["phase"], "Assigned");
        assert!(task["result"].is_null());
        assert_eq!(task["executor"], executor.owner);
        let id = task["sandboxId"].as_str().unwrap();
        assert_eq!(
            id,
            format!("{}.sb-{}", executor.owner, offer.grant.identity.claim_id)
        );
        assert!(!task.to_string().contains("SECRET"));
        assert!(!task.to_string().contains("private-runtime-detail"));
        let sandbox_path = format!("/v1/sandboxes/{id}");
        // The identifier routes to the actual uncertain executor, not a fabricated
        // terminal sandbox or a second create. Once control recovers it is deletable.
        assert_eq!(
            http(&home, "GET", &sandbox_path, vec![], true).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        runtime.uncertain.store(false, Ordering::SeqCst);
        assert_eq!(
            http(&home, "DELETE", &sandbox_path, vec![], true).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(executor.daemon.capacity_usage().await.unwrap(), (0, 0, 0));
        assert_eq!(runtime.creates.load(Ordering::SeqCst), 1);
        let view = executor
            .daemon
            .inspect_claim(&offer.grant.identity)
            .await
            .unwrap();
        let actor = peers
            .iter()
            .find(|(_, s)| s.owner == home.owner)
            .unwrap()
            .0
            .clone();
        executor.clone().recover_claim(view, Some(actor)).await;
        let (_, completed) = http(&home, "GET", &path, vec![], true).await;
        assert_eq!(completed["phase"], "Completed");
        assert_eq!(completed["sandboxId"], id);
        assert_eq!(completed["executor"], executor.owner);
        home.shutdown().await.unwrap();
        executor.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stale_offer_duplicate_accept_lost_grant_and_home_restart() {
        let (dir, daemon, queue) = fixture().await;
        let task = queue.enqueue(body()).await.unwrap();
        let executor = digest(b"executor");
        let first = queue
            .pull(executor.clone(), capacity())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.grant.identity.generation, 1);
        queue
            .transaction(|ledger| {
                if let Phase::Offered { expires, .. } = &mut ledger.tasks[0].state {
                    *expires = now() - 1;
                }
                Ok(())
            })
            .await
            .unwrap();
        let second = queue.pull(executor, capacity()).await.unwrap().unwrap();
        assert_eq!(second.grant.identity.generation, 2);
        assert_ne!(
            first.grant.identity.claim_id,
            second.grant.identity.claim_id
        );
        assert!(!queue.accept(first.grant).await.unwrap());
        assert!(queue.accept(second.grant.clone()).await.unwrap());
        assert!(queue.accept(second.grant.clone()).await.unwrap());
        let owner = task.home.clone();
        drop(queue);
        drop(daemon);
        let daemon = self::daemon(dir.path()).await;
        let queue = Queue::open(daemon, owner).unwrap();
        // The successful grant response was lost. Exact accept recovers permission.
        assert!(queue.accept(second.grant.clone()).await.unwrap());
        queue
            .transaction(|ledger| {
                ledger.tasks[0].deadline = now() - 1;
                Ok(())
            })
            .await
            .unwrap();
        queue.sweep().await.unwrap();
        assert_eq!(queue.task(task.id).await.unwrap().phase, "Assigned");
        assert!(
            queue
                .pull(digest(b"another executor"), capacity())
                .await
                .unwrap()
                .is_none()
        );
        let mut wrong = second.grant.clone();
        wrong.fingerprint = digest(b"changed");
        assert!(!queue.accept(wrong).await.unwrap());
        queue
            .report(
                second.grant.clone(),
                Err(ApiError::new(500, "TEST_FAILURE", "terminal")),
            )
            .await
            .unwrap();
        queue
            .report(
                second.grant,
                Err(ApiError::new(500, "OTHER", "must not replace first result")),
            )
            .await
            .unwrap();
        let state = queue.state.lock().unwrap();
        assert!(state.ledger.tasks[0].body.is_none());
        assert!(
            matches!(&state.ledger.tasks[0].state, Phase::Completed { result, .. } if matches!(result.as_ref(), Err(e) if e.code == "TEST_FAILURE"))
        );
    }

    #[tokio::test]
    async fn reserved_claim_survives_restart_then_explicit_stale_rejection_releases_capacity() {
        let (dir, daemon, queue) = fixture().await;
        queue.enqueue(body()).await.unwrap();
        let offer = queue
            .pull(digest(b"executor"), capacity())
            .await
            .unwrap()
            .unwrap();
        daemon
            .reserve_claim(
                offer.grant.identity.clone(),
                serde_json::from_slice(&offer.body).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
        let owner = offer.grant.identity.home.clone();
        drop(queue);
        drop(daemon);
        let daemon = self::daemon(dir.path()).await;
        let queue = Queue::open(daemon.clone(), owner).unwrap();
        assert_eq!(
            daemon
                .inspect_claim(&offer.grant.identity)
                .await
                .unwrap()
                .phase,
            ClaimPhase::Reserved
        );
        assert_eq!(daemon.capacity_usage().await.unwrap().0, 1);
        queue
            .transaction(|ledger| {
                ledger.tasks[0].deadline = now() - 1;
                Ok(())
            })
            .await
            .unwrap();
        assert!(!queue.accept(offer.grant.clone()).await.unwrap());
        daemon.release_claim(&offer.grant.identity).await.unwrap();
        assert_eq!(daemon.capacity_usage().await.unwrap(), (0, 0, 0));
        assert!(daemon.launch_claim(&offer.grant.identity).await.is_err());
        assert!(
            daemon
                .reserve_claim(
                    offer.grant.identity,
                    serde_json::from_slice(&body()).unwrap()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fifo_filters_capacity_and_image_and_deadline_drops_secrets() {
        let (_dir, _daemon, queue) = fixture().await;
        let first = queue.enqueue(body()).await.unwrap();
        let second = queue.enqueue(body()).await.unwrap();
        let mut unavailable = capacity();
        unavailable.images.clear();
        assert!(
            queue
                .pull(digest(b"executor"), unavailable)
                .await
                .unwrap()
                .is_none()
        );
        let mut small = capacity();
        small.cpu = 999;
        assert!(
            queue
                .pull(digest(b"executor"), small)
                .await
                .unwrap()
                .is_none()
        );
        let offer = queue
            .pull(digest(b"executor"), capacity())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(offer.grant.identity.task_id, first.id);
        queue
            .transaction(|ledger| {
                for task in &mut ledger.tasks {
                    task.deadline = now() - 1;
                }
                Ok(())
            })
            .await
            .unwrap();
        queue.sweep().await.unwrap();
        assert!(!queue.accept(offer.grant).await.unwrap());
        assert_eq!(queue.task(second.id).await.unwrap().phase, "Completed");
        assert!(
            !String::from_utf8(serde_json::to_vec(&queue.state.lock().unwrap().ledger).unwrap())
                .unwrap()
                .contains("do-not-expose")
        );
    }

    #[tokio::test]
    async fn corrupt_missing_symlink_and_uncertain_storage_fail_closed() {
        use std::os::unix::fs::symlink;
        let (dir, daemon, queue) = fixture().await;
        let owner = queue.state.lock().unwrap().ledger.owner.clone();
        queue.enqueue(body()).await.unwrap();
        drop(queue);
        let path = dir.path().join("cluster-queue.json");
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        assert!(Queue::open(daemon.clone(), owner.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        std::fs::remove_file(&path).unwrap();
        assert!(Queue::open(daemon.clone(), owner.clone()).is_err());
        let target = dir.path().join("outside");
        std::fs::write(&target, &original).unwrap();
        symlink(&target, &path).unwrap();
        assert!(Queue::open(daemon.clone(), owner.clone()).is_err());
        std::fs::remove_file(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let queue = Queue::open(daemon, owner).unwrap();
        // Force rename failure after the temp file was synced. No subsequent queue
        // operation may publish or read from the now potentially stale memory.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(queue.enqueue(body()).await.is_err());
        std::fs::remove_dir(&path).unwrap();
        assert!(queue.enqueue(body()).await.is_err());
        assert!(queue.sweep().await.is_err());
    }

    #[tokio::test]
    async fn retention_budget_rejects_without_eviction_or_failstop() {
        let (dir, daemon, queue) = fixture().await;
        let mut accepted = Vec::new();
        loop {
            match queue.enqueue(body()).await {
                Ok(task) => accepted.push(task.id),
                Err(error) => {
                    assert_eq!(error.code, "CLUSTER_QUEUE_FULL");
                    break;
                }
            }
        }
        let per_task =
            serde_json::to_vec(&Some(body())).unwrap().len() + LIVE_TASK_OVERHEAD + RESULT_BYTES;
        assert_eq!(accepted.len(), (MAX_BYTES - LEDGER_OVERHEAD) / per_task);
        assert_eq!(accepted.len(), 29);
        assert!(accepted.len() < MAX_TASKS);
        for id in accepted {
            assert_eq!(queue.task(id).await.unwrap().phase, "Queued");
        }
        assert!(
            queue
                .pull(digest(b"executor"), capacity())
                .await
                .unwrap()
                .is_some()
        );
        let ledger = queue.state.lock().unwrap().ledger.clone();
        let original_charge = ledger.charge().unwrap();
        let owner = ledger.owner.clone();
        drop(queue);
        let reopened = Queue::open(daemon.clone(), owner.clone()).unwrap();
        assert_eq!(
            reopened.state.lock().unwrap().ledger.charge().unwrap(),
            original_charge
        );
        let Err(error) = reopened.enqueue(body()).await else {
            panic!("restart bypassed retention budget");
        };
        assert_eq!(error.code, "CLUSTER_QUEUE_FULL");
        drop(reopened);
        // A syntactically valid ledger can fit physically yet lack future result
        // headroom. Startup must reject it, not accept a charge admission forbids.
        let mut oversized = ledger;
        let mut extra = oversized.tasks.last().unwrap().clone();
        extra.id = uuid::Uuid::new_v4().to_string();
        oversized.tasks.push(extra);
        let bytes = serde_json::to_vec(&oversized).unwrap();
        assert!(bytes.len() < MAX_BYTES);
        assert!(oversized.charge().unwrap() > MAX_BYTES);
        let path = dir.path().join("cluster-queue.json");
        std::fs::write(&path, &bytes).unwrap();
        let error = Queue::open(daemon, owner)
            .err()
            .expect("overcharged ledger rejected");
        assert!(error.to_string().contains("retention budget"));
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "no repair, eviction or GC"
        );
    }

    #[tokio::test]
    async fn numeric_array_expansion_is_charged_before_admission() {
        let (_dir, _daemon, queue) = fixture().await;
        queue.enqueue(body()).await.unwrap();
        let mut ledger = queue.state.lock().unwrap().ledger.clone();
        let mut value: Value = serde_json::from_slice(&body()).unwrap();
        value["env"]["padding"] = json!("");
        let base_len = serde_json::to_vec(&value).unwrap().len();
        value["env"]["padding"] = json!("x".repeat(BODY - base_len));
        let body = serde_json::to_vec(&value).unwrap();
        assert_eq!(body.len(), BODY);
        let encoded_body_len = serde_json::to_vec(&body).unwrap().len();
        assert!(encoded_body_len > 3 * BODY);
        assert!(encoded_body_len <= 4 * BODY + 1);
        let per_task = encoded_body_len + LIVE_TASK_OVERHEAD + RESULT_BYTES;
        assert_eq!((MAX_BYTES - LEDGER_OVERHEAD) / per_task, 6);
        let mut task = ledger.tasks.pop().unwrap();
        task.fingerprint = fingerprint(&body).unwrap();
        task.body = Some(body);
        for _ in 0..6 {
            task.id = uuid::Uuid::new_v4().to_string();
            ledger.tasks.push(task.clone());
        }
        let bytes = serde_json::to_vec(&ledger).unwrap();
        assert!(ledger.fits(bytes.len()).unwrap());
        task.id = uuid::Uuid::new_v4().to_string();
        ledger.tasks.push(task);
        let bytes = serde_json::to_vec(&ledger).unwrap();
        assert!(
            bytes.len() < MAX_BYTES,
            "seven bodies alone fit; reserved results do not"
        );
        assert!(!ledger.fits(bytes.len()).unwrap());
    }

    #[test]
    fn canonical_fingerprint_preserves_values_not_object_key_order() {
        assert_eq!(
            fingerprint(br#"{"b":{"z":1,"a":2},"a":3}"#).unwrap(),
            fingerprint(br#"{"a":3,"b":{"a":2,"z":1}}"#).unwrap()
        );
        assert_ne!(
            fingerprint(br#"{"a":[1,2]}"#).unwrap(),
            fingerprint(br#"{"a":[2,1]}"#).unwrap()
        );
    }
}
