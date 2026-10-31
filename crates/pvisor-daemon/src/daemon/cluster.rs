//! Opt-in trusted-peer OpenSandbox and E2B routing. TLS proves possession of the shared
//! passphrase, not node identity: every member must be trusted with lifecycle access.
//! All members must configure the same external lifecycle key. Endpoint responses
//! deliberately retain the owner's direct, local-ID authenticated URL. Peer HTTP
//! is plaintext and restricted to literal loopback IPs unless explicitly opted in.
//! Non-loopback deployments require a trusted/protected network and the opt-in.
//! Each daemon's advertised HTTP authority must route uniquely to that owner,
//! not to a shared load balancer: direct endpoint URLs contain owner-local IDs.
//! Authority strings cannot establish uniqueness across DNS/proxy aliases.

use super::{ApiError, CreateRequest, Daemon, api};
#[path = "cluster_e2b.rs"]
mod e2b;
#[path = "cluster_queue.rs"]
mod queue;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use pulsing_actor::{
    actor::ActorPath,
    prelude::*,
    transport::http2::{Http2Client, Http2Config, Http2RemoteTransport, TransportTarget},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tower::ServiceExt;

const SERVICE: &str = "pvisor/node";
const WIRE: &str = "pvisor.cluster.v1";
const BODY: usize = 1024 * 1024;
const REPLY: usize = 32 * 1024 * 1024;
// Base64 expands exactly to 4 * ceil(n / 3). Reserve separate envelope
// overhead; both producer and consumer enforce the identical wire bound.
const ENCODED_BODY: usize = REPLY.div_ceil(3) * 4;
const WIRE_LIMIT: usize = ENCODED_BODY + 64 * 1024;
const INVENTORY_PAGE: usize = 16;
const INVENTORY_ITEMS: usize = 65_536;
const RPC_TIME: Duration = Duration::from_secs(120);
const STATUS_TIME: Duration = Duration::from_secs(3);
const IDENTITY_FRESHNESS: Duration = Duration::from_secs(30);
const DESCRIPTOR: &str = "pvisor.discovery.v1";
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);
const DISCOVERY_FRESHNESS: Duration = Duration::from_secs(10);
const OBSERVATION_INTERVAL: Duration = Duration::from_secs(2);
const OBSERVATION_FRESHNESS: Duration = Duration::from_secs(10);
const BACKGROUND_BATCH: usize = 8;

#[derive(Clone)]
struct Candidate {
    actor: ActorRef,
    generation: String,
    hint: Option<Identity>,
}

#[derive(Default)]
struct Sample {
    success: Option<(tokio::time::Instant, Status)>,
    attempt: Option<tokio::time::Instant>,
    outcome: Option<&'static str>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    owner: String,
    authority: String,
    key_digest: String,
    protocol: String,
}

#[derive(Default)]
struct Discovery {
    identities: std::collections::BTreeMap<String, (ActorRef, Identity, tokio::time::Instant)>,
    rejected: BTreeSet<String>,
    members: BTreeSet<String>,
    refreshed: Option<tokio::time::Instant>,
    candidates: std::collections::BTreeMap<String, Candidate>,
    samples: std::collections::BTreeMap<String, Sample>,
}
impl Discovery {
    fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.rejected.is_empty(), "observed invalid peer identity");
        let mut owners = BTreeSet::new();
        for (_, identity, _) in self.identities.values() {
            anyhow::ensure!(owners.insert(&identity.owner), "duplicate stable owner");
        }
        for (key, candidate) in &self.candidates {
            if !self.identities.contains_key(key)
                && let Some(identity) = &candidate.hint
            {
                anyhow::ensure!(
                    owners.insert(&identity.owner),
                    "duplicate stable owner hint"
                );
            }
        }
        Ok(())
    }
}

/// Cluster bootstrap configuration. Image keys are an explicit local allowlist,
/// not a promise inferred from another node's inventory.
pub struct ClusterConfig {
    /// Pulsing TLS listener; use a reachable, non-wildcard unicast IP for gossip.
    /// Port zero requests an ephemeral port; wildcard/multicast IPs are rejected.
    pub bind: SocketAddr,
    /// Trusted Pulsing seed listeners (not HTTP endpoints).
    pub seeds: Vec<SocketAddr>,
    /// Shared TLS passphrase, at least 32 bytes; never expose to clients.
    pub secret: String,
    /// Supported local image keys: OpenSandbox image.uri or E2B templateID.
    /// Runtime syntax: 1–128 ASCII alphanumeric, '-', '_' or '.' bytes, with
    /// no leading '.'. Library callers supply this startup snapshot; the CLI
    /// discovers it from direct regular manifests in its canonical images directory.
    /// Empty means no compatible execution tasks, never wildcard support.
    /// Manifest contents and continued availability remain the runtime/operator's concern.
    pub images: Vec<String>,
    /// Explicitly permit plaintext peer HTTP to non-loopback destinations.
    /// False permits only literal loopback IPs (not DNS names, avoiding rebinding).
    /// This does not affect TLS-protected Pulsing lifecycle RPC.
    pub allow_insecure_peer_http: bool,
}

/// Cluster facade. Dropping an HTTP request never retries an accepted mutation.
/// Keep this alive until HTTP serving stops, then call shutdown.
pub struct Cluster {
    daemon: Arc<Daemon>,
    system: Arc<ActorSystem>,
    peer_client: Arc<Http2Client>,
    local: Router,
    http: reqwest::Client,
    owner: String,
    outgoing: Arc<Semaphore>,
    bodies: Arc<Semaphore>,
    proxies: Arc<Semaphore>,
    ingress: Arc<Semaphore>,
    allow_insecure_peer_http: bool,
    queue: Arc<queue::Queue>,
    images: Vec<String>,
    worker: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    discovery: std::sync::Mutex<Discovery>,
    background: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    observation_outgoing: Arc<Semaphore>,
    local_observation: std::sync::Mutex<Sample>,
    discovery_refresh: tokio::sync::Mutex<()>,
    #[cfg(test)]
    faults: Arc<std::sync::atomic::AtomicU8>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Status {
    owner: String,
    authority: String,
    images: Vec<String>,
    healthy: bool,
    key_digest: String,
    count: u64,
    cpu: u64,
    memory: u64,
    max_count: u64,
    max_cpu: u64,
    max_memory: u64,
    #[serde(default)]
    queue: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Call {
    Identity,
    E2bTemplate {
        image: String,
    },
    Status,
    Inspect {
        owner: String,
        key_digest: String,
        identity: super::models::ClaimIdentity,
    },
    Pull {
        peer: queue::Peer,
        capacity: queue::Capacity,
    },
    Accept {
        peer: queue::Peer,
        grant: queue::Grant,
    },
    Report {
        peer: queue::Peer,
        grant: queue::Grant,
        result: Box<Result<super::Sandbox, ApiError>>,
    },
    Task {
        peer: queue::Peer,
        id: String,
    },
    Http {
        method: String,
        path: String,
        #[serde(with = "wire_body")]
        body: Vec<u8>,
    },
}
#[derive(Serialize, Deserialize)]
struct HttpReply {
    status: u16,
    #[serde(with = "wire_body")]
    body: Vec<u8>,
    location: Option<String>,
}

struct Node {
    daemon: Arc<Daemon>,
    local: Router,
    owner: String,
    images: Vec<String>,
    work: Arc<Semaphore>,
    status_work: Arc<Semaphore>,
    identity_work: Arc<Semaphore>,
    queue: Arc<queue::Queue>,
    #[cfg(test)]
    faults: Arc<std::sync::atomic::AtomicU8>,
}
fn digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
mod wire_body {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};
    pub(super) fn serialize<S: Serializer>(body: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if body.len() > super::REPLY {
            return Err(serde::ser::Error::custom("RPC body exceeds budget"));
        }
        serializer.serialize_str(&STANDARD.encode(body))
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > super::ENCODED_BODY {
            return Err(serde::de::Error::custom("RPC encoded body exceeds budget"));
        }
        let body = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if body.len() > super::REPLY {
            return Err(serde::de::Error::custom("RPC body exceeds budget"));
        }
        Ok(body)
    }
}
struct WireBuffer(Vec<u8>);
impl std::io::Write for WireBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > WIRE_LIMIT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "cluster envelope exceeds wire budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn wire<T: Serialize>(value: &T) -> pulsing_actor::error::Result<Message> {
    let mut buffer = WireBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, value).map_err(actor_error)?;
    Ok(Message::single(WIRE, buffer.0))
}
fn actor_error(error: impl std::fmt::Display) -> pulsing_actor::error::PulsingError {
    pulsing_actor::error::RuntimeError::Other(error.to_string()).into()
}
fn decode<T: DeserializeOwned>(message: Message) -> anyhow::Result<T> {
    match message {
        Message::Single { msg_type, data } if msg_type == WIRE && data.len() <= WIRE_LIMIT => {
            Ok(serde_json::from_slice(&data)?)
        }
        _ => anyhow::bail!("invalid cluster envelope"),
    }
}
#[async_trait::async_trait]
impl Actor for Node {
    async fn receive(
        &mut self,
        message: Message,
        _: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let call: Call = decode(message).map_err(actor_error)?;
        #[cfg(test)]
        {
            let faults = self.faults.load(std::sync::atomic::Ordering::SeqCst);
            if (matches!(call, Call::Status) && faults & 1 != 0)
                || (matches!(call, Call::Identity) && faults & 2 != 0)
            {
                return Err(actor_error("injected identity/usage failure"));
            }
        }
        let slots = match call {
            Call::Status => &self.status_work,
            Call::Identity => &self.identity_work,
            _ => &self.work,
        };
        let permit = slots.clone().try_acquire_owned().map_err(actor_error)?;
        let daemon = self.daemon.clone();
        let local = self.local.clone();
        let owner = self.owner.clone();
        let images = self.images.clone();
        let queue = self.queue.clone();
        #[cfg(test)]
        let faults = self.faults.clone();
        // Spawn before returning the stream: dropping the response cannot cancel an
        // accepted mutation or release its concurrency slot prematurely.
        let task = tokio::spawn(async move {
            let _permit = permit;
            #[cfg(test)]
            if matches!(call, Call::Identity)
                && faults.load(std::sync::atomic::Ordering::SeqCst) & 4 != 0
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            match call {
                Call::Identity => wire(&Identity {
                    owner,
                    authority: daemon.config.public_endpoint.clone(),
                    key_digest: digest(daemon.config.api_key.as_bytes()),
                    protocol: WIRE.into(),
                }),
                Call::Status => {
                    // A timed-out client cannot leave a server telemetry slot
                    // permanently held behind a stalled local registry.
                    let usage = tokio::time::timeout(STATUS_TIME, daemon.capacity_usage())
                        .await
                        .ok()
                        .and_then(Result::ok);
                    let (count, cpu, memory) = usage.unwrap_or((u64::MAX, u64::MAX, u64::MAX));
                    wire(&Status {
                        owner,
                        authority: daemon.config.public_endpoint.clone(),
                        images,
                        healthy: usage.is_some(),
                        key_digest: digest(daemon.config.api_key.as_bytes()),
                        count,
                        cpu,
                        memory,
                        max_count: daemon.config.max_sandboxes as u64,
                        max_cpu: daemon.config.cpu_millis,
                        max_memory: daemon.config.memory_bytes,
                        queue: queue.observation(),
                    })
                }
                Call::Inspect {
                    owner: expected,
                    key_digest,
                    identity,
                } => {
                    if expected != owner || key_digest != digest(daemon.config.api_key.as_bytes()) {
                        return Err(actor_error("claim inspection owner/key mismatch"));
                    }
                    wire(&daemon.inspect_claim(&identity).await)
                }
                Call::Pull { peer, capacity } => {
                    peer.check(&owner, &daemon).map_err(actor_error)?;
                    wire(&queue.pull(peer.executor, capacity).await)
                }
                Call::Accept { peer, grant } => {
                    peer.check(&owner, &daemon).map_err(actor_error)?;
                    peer.check_grant(&grant).map_err(actor_error)?;
                    wire(&queue.accept(grant).await)
                }
                Call::Report {
                    peer,
                    grant,
                    result,
                } => {
                    peer.check(&owner, &daemon).map_err(actor_error)?;
                    peer.check_grant(&grant).map_err(actor_error)?;
                    wire(&queue.report(grant, *result).await)
                }
                Call::Task { peer, id } => {
                    peer.check(&owner, &daemon).map_err(actor_error)?;
                    wire(&queue.task(id).await)
                }
                Call::E2bTemplate { image } => {
                    wire(&daemon.runtime.e2b_template(&image).await.map_err(|_| {
                        ApiError::new(
                            404,
                            "TEMPLATE_NOT_FOUND",
                            "Prepared E2B template is unavailable",
                        )
                    }))
                }
                Call::Http { method, path, body } => {
                    if (method == "POST"
                        && matches!(
                            path.split('?').next(),
                            Some("/v1/sandboxes" | "/sandboxes" | "/v2/sandboxes")
                        ))
                        || body.len() > BODY
                        || !valid_control_path(&path)
                    {
                        return Err(actor_error("invalid lifecycle path/body"));
                    }
                    let request = Request::builder()
                        .method(method.as_str())
                        .uri(&path)
                        .header(
                            if e2b::control_path(&path) {
                                "x-api-key"
                            } else {
                                "open-sandbox-api-key"
                            },
                            &daemon.config.api_key,
                        )
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .map_err(actor_error)?;
                    let response = local.oneshot(request).await.map_err(actor_error)?;
                    let status = response.status().as_u16();
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    let body = to_bytes(response.into_body(), REPLY)
                        .await
                        .map_err(actor_error)?
                        .to_vec();
                    wire(&HttpReply {
                        status,
                        body,
                        location,
                    })
                }
            }
        });
        Ok(Message::stream(
            WIRE,
            stream::once(async move { task.await.map_err(actor_error)? }),
        ))
    }
}

// Remote JSON is untrusted even on the shared-secret transport. Never echo
// arbitrary queue fields (including metadata, credentials or environment).
fn redacted_queue(value: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in [
        "Queued",
        "Offered",
        "Assigned",
        "Completed",
        "oldest_queued_seconds",
        "retained_len",
    ] {
        result.insert(
            key.into(),
            value
                .get(key)
                .and_then(Value::as_u64)
                .map_or(Value::Null, Value::from),
        );
    }
    result.insert(
        "storage_failed".into(),
        value
            .get("storage_failed")
            .and_then(Value::as_bool)
            .map_or(Value::Null, Value::from),
    );
    Value::Object(result)
}

impl Cluster {
    /// Starts TLS gossip and the shared service actor. Does not alter the local
    /// API router or persist Pulsing's ephemeral NodeId. Failure leaves no facade.
    pub async fn start(daemon: Arc<Daemon>, config: ClusterConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.secret.len() >= 32,
            "cluster secret must be at least 32 bytes"
        );
        anyhow::ensure!(
            concrete_ip(config.bind.ip()),
            "cluster bind requires a concrete non-wildcard unicast IP"
        );
        anyhow::ensure!(
            config
                .seeds
                .iter()
                .all(|seed| concrete_ip(seed.ip()) && seed.port() != 0),
            "cluster seeds require concrete non-wildcard unicast IPs and nonzero ports"
        );

        // Match runtime::validate_spec's prepared-image key syntax. There is no
        // image-only runtime validator; do not manufacture a workload RuntimeSpec.
        anyhow::ensure!(
            config.images.iter().all(|image| {
                !image.is_empty()
                    && image.len() <= 128
                    && !image.starts_with('.')
                    && image.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            }),
            "cluster image must be a local prepared-image key"
        );
        validate_authority(&daemon.config.public_endpoint)?;
        let owner = digest(daemon.registry.lock().await.owner.as_bytes());
        let queue_daemon = daemon.clone();
        let queue_owner = owner.clone();
        let queue =
            tokio::task::spawn_blocking(move || queue::Queue::open(queue_daemon, queue_owner))
                .await??;
        let http = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(STATUS_TIME)
            .pool_max_idle_per_host(0)
            .build()?;
        let transport = Http2Config::default()
            .max_retries(0)
            .connect_timeout(STATUS_TIME)
            .request_timeout(RPC_TIME)
            .stream_timeout(RPC_TIME)
            .max_concurrent_streams(64)
            .with_tls(&config.secret)?;
        let system = ActorSystem::builder()
            .addr(config.bind)
            .seeds(config.seeds)
            .http2_config(transport.clone())
            .build()
            .await?;
        let peer_client = Arc::new(Http2Client::new(transport));
        let local = api::router(daemon.clone());
        #[cfg(test)]
        let faults = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let result = system
            .spawning()
            .name(SERVICE)
            // At the pinned revision, empty metadata uses legacy name-only
            // publication, which resolve_all_instances cannot enumerate.
            .metadata(std::collections::HashMap::from([
                ("protocol".into(), WIRE.into()),
                (
                    DESCRIPTOR.into(),
                    serde_json::to_string(&Identity {
                        owner: owner.clone(),
                        authority: daemon.config.public_endpoint.clone(),
                        key_digest: digest(daemon.config.api_key.as_bytes()),
                        protocol: WIRE.into(),
                    })?,
                ),
            ]))
            .mailbox_capacity(64)
            .spawn(Node {
                daemon: daemon.clone(),
                local: local.clone(),
                owner: owner.clone(),
                images: config.images.clone(),
                queue: queue.clone(),
                work: Arc::new(Semaphore::new(32)),
                status_work: Arc::new(Semaphore::new(8)),
                identity_work: Arc::new(Semaphore::new(8)),
                #[cfg(test)]
                faults: faults.clone(),
            })
            .await;
        if let Err(error) = result {
            let _ = system.shutdown().await;
            return Err(error.into());
        }
        let cluster = Arc::new(Self {
            daemon,
            system,
            peer_client,
            local,
            http,
            owner,
            outgoing: Arc::new(Semaphore::new(64)),
            bodies: Arc::new(Semaphore::new(32)),
            proxies: Arc::new(Semaphore::new(128)),
            ingress: Arc::new(Semaphore::new(64)),
            allow_insecure_peer_http: config.allow_insecure_peer_http,
            queue,
            images: config.images,
            worker: std::sync::Mutex::new(None),
            discovery: std::sync::Mutex::new(Discovery::default()),
            background: std::sync::Mutex::new(Vec::new()),
            observation_outgoing: Arc::new(Semaphore::new(BACKGROUND_BATCH)),
            local_observation: std::sync::Mutex::new(Sample::default()),
            discovery_refresh: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            faults,
        });
        cluster.sample_local().await;
        Self::start_background(&cluster);
        Self::start_worker(&cluster);
        Ok(cluster)
    }
    /// Standalone cluster HTTP router; unknown routes and local IDs use the
    /// unchanged local router. Do not merge this fallback with the local router.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new().fallback(entry).with_state(self.clone())
    }
    /// Stops cluster discovery/transport, not the daemon's owned sandboxes.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let worker = self.worker.lock().unwrap().take();
        if let Some(worker) = worker {
            worker.abort();
            let _ = worker.await;
        }
        let tasks = std::mem::take(&mut *self.background.lock().unwrap());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        self.peer_client.shutdown();
        Ok(self.system.shutdown().await?)
    }

    async fn rpc<T: DeserializeOwned>(
        &self,
        actor: &ActorRef,
        call: Call,
        deadline: Duration,
    ) -> anyhow::Result<T> {
        let slots = if matches!(call, Call::Status) {
            &self.observation_outgoing
        } else {
            &self.outgoing
        };
        let _permit = slots.clone().try_acquire_owned()?;
        tokio::time::timeout(deadline, async {
            // Raw send intentionally bypasses ask/bincode and all application retries.
            let response = actor.send(wire(&call)?).await?;
            let response = match response {
                Message::Stream { mut stream, .. } => {
                    let first = stream
                        .next()
                        .await
                        .ok_or_else(|| anyhow::anyhow!("empty RPC stream"))??;
                    anyhow::ensure!(stream.next().await.is_none(), "multiple RPC responses");
                    first
                }
                message => message,
            };
            decode(response)
        })
        .await?
    }
    #[cfg(test)]
    async fn peers(&self) -> anyhow::Result<Vec<(ActorRef, Identity)>> {
        self.peers_inner(false).await
    }

    // Build a complete local-gossip snapshot before publishing it. Failed refreshes
    // retain the previous snapshot and its original age, never a fresh empty view.
    async fn members(&self) -> anyhow::Result<Vec<(String, Candidate)>> {
        let _refresh = self.discovery_refresh.lock().await;
        let path = ActorPath::new(SERVICE)?;
        let members =
            tokio::time::timeout(STATUS_TIME, self.system.get_named_instances_detailed(&path))
                .await?;
        let members: Vec<_> = members
            .into_iter()
            .filter(|(m, _)| m.status == pulsing_actor::cluster::MemberStatus::Alive)
            .collect();
        anyhow::ensure!(
            !members.is_empty() && members.len() <= 256,
            "empty or oversized cluster inventory"
        );
        let mut candidates = std::collections::BTreeMap::new();
        for (member, instance) in members {
            let descriptor = instance.as_ref().and_then(|i| i.metadata.get(DESCRIPTOR));
            let hint = descriptor.and_then(|s| serde_json::from_str::<Identity>(s).ok());
            let generation = format!(
                "{}:{}:{:?}:{}",
                member.addr,
                member.incarnation,
                instance.as_ref().map(|i| i.actor_id),
                descriptor.map(String::as_str).unwrap_or("")
            );
            let actor = if let Some(instance) = instance {
                if member.node_id == *self.system.node_id() {
                    self.system.actor_ref(&instance.actor_id).await?
                } else {
                    // Upstream's named references target a name, not an actor ID;
                    // actor_ref cannot locate named IDs through its global index.
                    // Pin the advertised ID directly so cached authorization can
                    // never silently follow a replacement at the same name.
                    let transport = Http2RemoteTransport::builder(
                        self.peer_client.clone(),
                        member.addr,
                        TransportTarget::ById(instance.actor_id),
                    )
                    .build();
                    ActorRef::remote(instance.actor_id, member.addr, Arc::new(transport))
                }
            } else {
                self.system
                    .resolve_named(SERVICE, Some(&member.node_id))
                    .await?
            };
            candidates.insert(
                member.node_id.to_string(),
                Candidate {
                    actor,
                    generation,
                    hint,
                },
            );
        }
        let mut cache = self.discovery.lock().unwrap();
        let changed: Vec<_> = cache
            .candidates
            .iter()
            .filter(|(key, old)| {
                candidates
                    .get(*key)
                    .is_none_or(|new| new.generation != old.generation)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in changed {
            cache.identities.remove(&key);
            cache.rejected.remove(&key);
            cache.samples.remove(&key);
        }
        // Retain the concrete reference while its gossip generation is unchanged.
        for (key, candidate) in &mut candidates {
            if let Some(old) = cache.candidates.get(key)
                && old.generation == candidate.generation
            {
                candidate.actor = old.actor.clone();
            }
        }
        cache.members = candidates.keys().cloned().collect();
        cache.candidates = candidates;
        cache.refreshed = Some(tokio::time::Instant::now());
        Ok(cache
            .candidates
            .iter()
            .map(|(key, c)| (key.clone(), c.clone()))
            .collect())
    }

    async fn identity(&self, key: &str, captured: &Candidate) -> anyhow::Result<Identity> {
        let actor = &captured.actor;
        let generation = {
            let cache = self.discovery.lock().unwrap();
            cache.check()?;
            let candidate = cache
                .candidates
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("obsolete candidate"))?;
            anyhow::ensure!(
                candidate.generation == captured.generation && candidate.actor.id() == actor.id(),
                "obsolete actor reference"
            );
            if let Some((_, identity, at)) = cache.identities.get(key)
                && at.elapsed() < IDENTITY_FRESHNESS
            {
                return Ok(identity.clone());
            }
            candidate.generation.clone()
        };
        let identity: Identity = self.rpc(actor, Call::Identity, STATUS_TIME).await?;
        let valid = valid_owner(&identity.owner)
            && identity.protocol == WIRE
            && identity.key_digest == digest(self.daemon.config.api_key.as_bytes())
            && validate_authority(&identity.authority).is_ok();
        let mut cache = self.discovery.lock().unwrap();
        let candidate = cache
            .candidates
            .get(key)
            .filter(|c| c.generation == generation)
            .ok_or_else(|| anyhow::anyhow!("obsolete identity response"))?;
        let valid = valid && candidate.hint.as_ref().is_none_or(|hint| hint == &identity);
        if !valid {
            cache.rejected.insert(key.into());
            anyhow::bail!("invalid peer owner/authority/key/protocol");
        }
        cache.identities.insert(
            key.into(),
            (actor.clone(), identity.clone(), tokio::time::Instant::now()),
        );
        cache.check()?;
        Ok(identity)
    }

    async fn peers_inner(&self, strict: bool) -> anyhow::Result<Vec<(ActorRef, Identity)>> {
        let members = self.members().await?;
        let expected = members.len();
        let peers = stream::iter(members.into_iter().map(|(key, candidate)| async move {
            self.identity(&key, &candidate)
                .await
                .map(|identity| (candidate.actor, identity))
        }))
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
        self.discovery.lock().unwrap().check()?;
        let peers: Vec<_> = peers.into_iter().filter_map(Result::ok).collect();
        anyhow::ensure!(
            !strict || peers.len() == expected,
            "discovered peer inventory unavailable"
        );
        Ok(peers)
    }

    async fn sample_peers(&self) -> anyhow::Result<Vec<(ActorRef, Identity)>> {
        let mut members: Vec<_> = {
            let cache = self.discovery.lock().unwrap();
            cache.check()?;
            cache
                .candidates
                .iter()
                .filter(|(key, _)| **key != self.system.node_id().to_string())
                .map(|(key, c)| (key.clone(), c.clone()))
                .collect()
        };
        members.sort_by_cached_key(|_| uuid::Uuid::new_v4());
        let peers = stream::iter(
            members
                .into_iter()
                .take(2)
                .map(|(key, candidate)| async move {
                    self.identity(&key, &candidate)
                        .await
                        .map(|identity| (candidate.actor, identity))
                }),
        )
        .buffer_unordered(2)
        .collect::<Vec<_>>()
        .await;
        self.discovery.lock().unwrap().check()?;
        Ok(peers.into_iter().filter_map(Result::ok).collect())
    }

    async fn owner_peer(&self, owner: &str) -> anyhow::Result<(ActorRef, Identity)> {
        for refresh in [false, true] {
            if refresh {
                // Cold owner index: consult local gossip only, never unrelated identities.
                tokio::time::timeout(STATUS_TIME, self.members()).await??;
            }
            let cached = {
                let cache = self.discovery.lock().unwrap();
                cache.check()?;
                cache
                    .candidates
                    .iter()
                    .find(|(key, candidate)| {
                        candidate.hint.as_ref().is_some_and(|i| i.owner == owner)
                            || cache
                                .identities
                                .get(*key)
                                .is_some_and(|(_, i, _)| i.owner == owner)
                    })
                    .map(|(key, c)| (key.clone(), c.clone()))
            };
            if let Some((key, candidate)) = cached {
                let identity = self.identity(&key, &candidate).await?;
                anyhow::ensure!(identity.owner == owner, "owner identity changed");
                return Ok((candidate.actor, identity));
            }
        }
        anyhow::bail!("owner not yet discovered or unavailable")
    }

    fn start_background(this: &Arc<Self>) {
        for observation in [false, true] {
            let weak = Arc::downgrade(this);
            let task = tokio::spawn(async move {
                let mut cursor = 0usize;
                loop {
                    let Some(cluster) = weak.upgrade() else { break };
                    if observation {
                        tokio::join!(
                            cluster.sample_local(),
                            cluster.sample_observations(&mut cursor)
                        );
                    } else {
                        let _ = tokio::time::timeout(STATUS_TIME, cluster.members()).await;
                        let batch = cluster.candidate_batch(&mut cursor);
                        // Old metadata-less peers are validated only in bounded
                        // background batches, never randomly probed for an owner.
                        stream::iter(batch.into_iter().map(|(key, c)| {
                            let cluster = &cluster;
                            async move {
                                let _ = cluster.identity(&key, &c).await;
                            }
                        }))
                        .buffer_unordered(BACKGROUND_BATCH)
                        .collect::<Vec<_>>()
                        .await;
                    }
                    drop(cluster);
                    tokio::time::sleep(if observation {
                        OBSERVATION_INTERVAL
                    } else {
                        DISCOVERY_INTERVAL
                    })
                    .await;
                }
            });
            this.background.lock().unwrap().push(task);
        }
    }

    fn candidate_batch(&self, cursor: &mut usize) -> Vec<(String, Candidate)> {
        let cache = self.discovery.lock().unwrap();
        let candidates: Vec<_> = cache
            .candidates
            .iter()
            .filter(|(key, _)| **key != self.system.node_id().to_string())
            .map(|(key, c)| (key.clone(), c.clone()))
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        let batch = (0..candidates.len().min(BACKGROUND_BATCH))
            .map(|i| candidates[(*cursor + i) % candidates.len()].clone())
            .collect();
        *cursor = (*cursor + BACKGROUND_BATCH) % candidates.len();
        batch
    }

    async fn sample_local(&self) {
        let attempt = tokio::time::Instant::now();
        let usage = tokio::time::timeout(STATUS_TIME, self.daemon.capacity_usage()).await;
        let mut sample = self.local_observation.lock().unwrap();
        sample.attempt = Some(attempt);
        sample.outcome = Some(match usage {
            Ok(Ok((count, cpu, memory))) => {
                sample.success = Some((
                    tokio::time::Instant::now(),
                    Status {
                        owner: self.owner.clone(),
                        authority: String::new(),
                        images: Vec::new(),
                        key_digest: String::new(),
                        healthy: true,
                        count,
                        cpu,
                        memory,
                        max_count: self.daemon.config.max_sandboxes as u64,
                        max_cpu: self.daemon.config.cpu_millis,
                        max_memory: self.daemon.config.memory_bytes,
                        queue: self.queue.observation(),
                    },
                ));
                "success"
            }
            Ok(Err(_)) => "status_error",
            Err(_) => "timeout",
        });
    }

    async fn sample_observations(&self, cursor: &mut usize) {
        let batch = self.candidate_batch(cursor);
        stream::iter(batch.into_iter().map(|(key, candidate)| async move {
            let attempt = tokio::time::Instant::now();
            // Metrics are never identity authority and never refresh identity age.
            let result = self
                .rpc::<Value>(&candidate.actor, Call::Status, STATUS_TIME)
                .await;
            let (status, outcome) = match result {
                Err(error)
                    if error
                        .downcast_ref::<tokio::time::error::Elapsed>()
                        .is_some() =>
                {
                    (None, "timeout")
                }
                Err(error) if error.downcast_ref::<serde_json::Error>().is_some() => {
                    (None, "parse_error")
                }
                Err(_) => (None, "transport_error"),
                Ok(value) => match serde_json::from_value::<Status>(value) {
                    Err(_) => (None, "parse_error"),
                    Ok(status)
                        if !status.healthy
                            || !valid_owner(&status.owner)
                            || status.key_digest
                                != digest(self.daemon.config.api_key.as_bytes())
                            || candidate
                                .hint
                                .as_ref()
                                .is_some_and(|hint| hint.owner != status.owner) =>
                    {
                        (None, "status_error")
                    }
                    Ok(status) => (Some(status), "success"),
                },
            };
            let mut cache = self.discovery.lock().unwrap();
            if !cache
                .candidates
                .get(&key)
                .is_some_and(|c| c.generation == candidate.generation)
            {
                return;
            }
            let sample = cache.samples.entry(key).or_default();
            sample.attempt = Some(attempt);
            sample.outcome = Some(outcome);
            if let Some(status) = status {
                sample.success = Some((tokio::time::Instant::now(), status));
            }
        }))
        .buffer_unordered(BACKGROUND_BATCH)
        .collect::<Vec<_>>()
        .await;
    }

    /// Cache-only local and remote observations; stale samples are never totals.
    /// No membership, identity, status, disk or peer calls are made by this endpoint.
    pub(super) async fn observation(&self) -> Value {
        let local = self.local_observation.lock().unwrap();
        let success = local.success.as_ref();
        let local_fresh = success.is_some_and(|(at, _)| at.elapsed() < OBSERVATION_FRESHNESS);
        let usage = success
            .filter(|_| local_fresh)
            .map(|(_, s)| (s.count, s.cpu, s.memory));
        let node = json!({
            "freshness": if local_fresh { "fresh" } else if success.is_some() { "stale" } else { "missing" },
            "sample_age_seconds": success.map(|(at, _)| at.elapsed().as_secs()),
            "lastAttempt": {"age_seconds": local.attempt.map(|at| at.elapsed().as_secs()), "outcome": local.outcome},
            "count": success.map(|(_, s)| s.count), "cpu": success.map(|(_, s)| s.cpu),
            "memory": success.map(|(_, s)| s.memory),
            "max_count": self.daemon.config.max_sandboxes,
            "max_cpu": self.daemon.config.cpu_millis, "max_memory": self.daemon.config.memory_bytes
        });
        drop(local);
        let cache = self.discovery.lock().unwrap();
        let mut totals = usage;
        let mut fresh = 0usize;
        let mut stale = 0usize;
        let mut missing = 0usize;
        let peers: Vec<_> = cache.candidates.iter()
            .filter(|(key, _)| **key != self.system.node_id().to_string())
            .map(|(key, candidate)| {
                let sample = cache.samples.get(key);
                let success = sample.and_then(|s| s.success.as_ref());
                let state = match success {
                    Some((at, status)) if at.elapsed() < OBSERVATION_FRESHNESS => {
                        fresh += 1;
                        let sum = totals.get_or_insert((0, 0, 0));
                        sum.0 = sum.0.saturating_add(status.count);
                        sum.1 = sum.1.saturating_add(status.cpu);
                        sum.2 = sum.2.saturating_add(status.memory);
                        "fresh"
                    }
                    Some(_) => { stale += 1; "stale" }
                    None => { missing += 1; "missing" }
                };
                json!({
                    "node_id": key, "owner": candidate.hint.as_ref().filter(|i| valid_owner(&i.owner)).map(|i| &i.owner),
                    "freshness": state,
                    "sample_age_seconds": success.map(|(at, _)| at.elapsed().as_secs()),
                    "lastAttempt": { "age_seconds": sample.and_then(|s| s.attempt).map(|at| at.elapsed().as_secs()),
                        "outcome": sample.and_then(|s| s.outcome) },
                    "node": success.map(|(_, s)| json!({"count": s.count, "cpu": s.cpu,
                        "memory": s.memory, "max_count": s.max_count, "max_cpu": s.max_cpu,
                        "max_memory": s.max_memory})),
                    "queue": success.map(|(_, s)| redacted_queue(&s.queue))
                })
            }).collect();
        let discovery_fresh = cache
            .refreshed
            .is_some_and(|at| at.elapsed() < DISCOVERY_FRESHNESS);
        json!({
            "version": 1, "timestamp": chrono::Utc::now().to_rfc3339(),
            "source": "local", "owner": self.owner, "node": node,
            "queue": self.queue.observation(), "peers": peers,
            "totals": totals.map(|u| json!({"count": u.0, "cpu": u.1, "memory": u.2})),
            "contributors": {"local": usize::from(usage.is_some()), "fresh": fresh, "stale": stale, "missing": missing},
            "membership_complete": false,
            "complete_for_discovered_members": discovery_fresh && usage.is_some() && stale == 0 && missing == 0 && cache.check().is_ok(),
            "peer_coverage": {
                "discovered": cache.members.len(),
                "validated": cache.identities.values().filter(|(_, _, at)| at.elapsed() < IDENTITY_FRESHNESS).count(),
                "discovery_age_seconds": cache.refreshed.map(|at| at.elapsed().as_secs()),
                "discovery_fresh": discovery_fresh,
                "identity_max_age_seconds": IDENTITY_FRESHNESS.as_secs(),
                "sample_max_age_seconds": OBSERVATION_FRESHNESS.as_secs(),
                "identity_conflict": cache.check().is_err(),
                "complete": false, "unknown_membership": true
            }
        })
    }
    async fn handle(&self, mut request: Request) -> Result<Response, ApiError> {
        let path = request.uri().path().to_owned();
        if e2b::route(&path) {
            return self.handle_e2b(request).await;
        }
        if path == "/v1/cluster/observations" {
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
                    "observations supports GET only",
                ));
            }
            return Ok(axum::Json(self.observation().await).into_response());
        }
        if path.starts_with("/v1/cluster/tasks/") {
            return self.task_http(request).await;
        }
        let collection = path == "/v1/sandboxes";
        let global = path
            .strip_prefix("/v1/sandboxes/")
            .and_then(|v| v.split('/').next())
            .filter(|v| v.contains('.'));
        if !collection && global.is_none() {
            return Ok(self.local.clone().oneshot(request).await.unwrap());
        }
        let _ingress = self
            .ingress
            .try_acquire()
            .map_err(|_| unavailable("cluster inflight limit"))?;
        let parsed = global.map(split_id).transpose()?;
        let proxy = parsed.is_some() && path.split('/').nth(4) == Some("proxy");
        if !proxy && !self.daemon.authorize(request.headers()) {
            return Err(ApiError::new(
                401,
                "UNAUTHORIZED",
                "Missing or invalid authentication credentials",
            ));
        }
        if let Some((owner, id)) = parsed {
            let suffix = path
                .strip_prefix(&format!("/v1/sandboxes/{owner}.{id}"))
                .unwrap();
            let target = format!(
                "/v1/sandboxes/{id}{suffix}{}",
                request
                    .uri()
                    .query()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default()
            );
            if proxy {
                validate_proxy_path(&target)?;
            } else if !valid_control_path(&target) {
                return Err(ApiError::bad_request("invalid lifecycle path"));
            }
            *request.uri_mut() = target
                .parse()
                .map_err(|_| ApiError::bad_request("invalid URI"))?;
            if owner == self.owner {
                if proxy {
                    return Ok(self.local.clone().oneshot(request).await.unwrap());
                }
                let reply = self.local.clone().oneshot(request).await.unwrap();
                return globalize_response(reply, owner).await;
            }
            let (actor, status) = self.owner_peer(owner).await.map_err(unavailable)?;
            if proxy {
                return self.proxy(request, &status.authority).await;
            }
            let call = self.control_call(request).await?;
            let deadline = if matches!(&call, Call::Http { method, .. } if method == "GET") {
                STATUS_TIME
            } else {
                RPC_TIME
            };
            let reply: HttpReply = self
                .rpc(&actor, call, deadline)
                .await
                .map_err(unavailable)?;
            return globalize_response(reply.response()?, owner).await;
        }
        if request.method() == Method::GET {
            return self.list(request).await;
        }
        if request.method() != Method::POST {
            return Ok(self.local.clone().oneshot(request).await.unwrap());
        }
        let call = self.control_call(request).await?;
        let Call::Http { ref body, .. } = call else {
            unreachable!()
        };
        let request: CreateRequest = serde_json::from_slice(body)
            .map_err(|_| ApiError::bad_request("Invalid create JSON"))?;
        request.validate(self.daemon.config.max_timeout_seconds)?;
        self.enqueue_wait(body.clone()).await
    }
    async fn control_call(&self, request: Request) -> Result<Call, ApiError> {
        let _permit = self.bodies.try_acquire().map_err(|_| {
            ApiError::new(
                429,
                "CONTROL_BODY_CAPACITY_EXCEEDED",
                "cluster body capacity exhausted",
            )
        })?;
        if request.method() == Method::POST
            && (request.uri().path() == "/v1/sandboxes"
                || (request.uri().path().starts_with("/v1/sandboxes/")
                    && request.uri().path().ends_with("/renew-expiration")))
        {
            let media = request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if media != "application/json"
                && !(media.starts_with("application/") && media.ends_with("+json"))
            {
                return Err(ApiError::bad_request(
                    "Expected request with Content-Type: application/json",
                ));
            }
        }
        let (parts, body) = request.into_parts();
        let body = tokio::time::timeout(Duration::from_secs(30), to_bytes(body, BODY))
            .await
            .map_err(|_| ApiError::new(408, "CONTROL_BODY_TIMEOUT", "Control body read timed out"))?
            .map_err(|_| ApiError::bad_request("Control body is unreadable or exceeds 1 MiB"))?;
        Ok(Call::Http {
            method: parts.method.to_string(),
            path: parts.uri.path_and_query().unwrap().to_string(),
            body: body.to_vec(),
        })
    }
    async fn list(&self, request: Request) -> Result<Response, ApiError> {
        // Ask each local router to apply its exact filter semantics, but paginate
        // only after aggregation. No discovered-peer failure is an empty inventory.
        // This is the sole grammar/validation authority, shared with local list.
        // Validate before any peer discovery, so local 400s never become 503s.
        api::parse_list_query(request.uri().query())?;
        let raw = request.uri().query().unwrap_or_default();
        let mut page = 1usize;
        let mut size = 20usize;
        let path = {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
                if key == "page" || key == "pageSize" {
                    // Extract validated paging values while leaving ListQuery's
                    // fields private. No independent acceptance grammar here.
                    let n = value
                        .parse::<usize>()
                        .expect("local parser validated pagination");
                    if key == "page" {
                        page = n;
                    } else {
                        size = n;
                    }
                } else {
                    query.append_pair(&key, &value);
                }
            }
            query.append_pair("pageSize", &INVENTORY_PAGE.to_string());
            format!("/v1/sandboxes?{}", query.finish())
        };
        let offset = (page - 1)
            .checked_mul(size)
            .expect("local parser validated pagination offset");
        let _ = self.control_call(request).await?;
        let peers = self.peers_inner(true).await.map_err(unavailable)?;
        let mut items = Vec::new();
        let mut bytes = 0usize;
        let mut ids = BTreeSet::new();
        for (actor, status) in peers {
            let mut peer_page = 1usize;
            let mut peer_total = None;
            loop {
                let reply: HttpReply = self
                    .rpc(
                        &actor,
                        Call::Http {
                            method: "GET".into(),
                            path: format!("{path}&page={peer_page}"),
                            body: vec![],
                        },
                        STATUS_TIME,
                    )
                    .await
                    .map_err(unavailable)?;
                if reply.status != 200 {
                    return Err(unavailable("peer inventory unavailable"));
                }
                charge_inventory(&mut bytes, reply.body.len())?;
                let value: Value = serde_json::from_slice(&reply.body).map_err(unavailable)?;
                let total = value["pagination"]["totalItems"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v <= INVENTORY_ITEMS)
                    .ok_or_else(|| unavailable("invalid inventory total"))?;
                if peer_total.is_some_and(|previous| previous != total)
                    || value["pagination"]["page"].as_u64() != Some(peer_page as u64)
                    || value["pagination"]["pageSize"].as_u64() != Some(INVENTORY_PAGE as u64)
                {
                    return Err(unavailable("peer inventory changed during pagination"));
                }
                peer_total = Some(total);
                let page_items = value["items"]
                    .as_array()
                    .ok_or_else(|| unavailable("invalid inventory"))?;
                let consumed = (peer_page - 1) * INVENTORY_PAGE;
                if page_items.len() != total.saturating_sub(consumed).min(INVENTORY_PAGE) {
                    return Err(unavailable("incomplete inventory page"));
                }
                for item in page_items {
                    let local = item["id"]
                        .as_str()
                        .filter(|id| valid_local(id))
                        .ok_or_else(|| unavailable("invalid inventory ID"))?;
                    if !ids.insert(format!("{}.{local}", status.owner)) {
                        return Err(unavailable("duplicate inventory ID"));
                    }
                    charge_inventory(&mut bytes, status.owner.len() + 1)?;
                    if items.len() >= INVENTORY_ITEMS {
                        return Err(unavailable("cluster inventory exceeds response budget"));
                    }
                    let mut item = item.clone();
                    globalize(&mut item, &status.owner);
                    items.push(item);
                }
                if consumed + page_items.len() >= total {
                    break;
                }
                peer_page += 1;
            }
        }
        items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let total = items.len();
        let pages = total / size + usize::from(total % size != 0);
        let result = json!({"items": items.into_iter().skip(offset).take(size).collect::<Vec<_>>(), "pagination": {"page":page,"pageSize":size,"totalItems":total,"totalPages":pages,"hasNextPage":page < pages}});
        let body = serde_json::to_vec(&result).map_err(unavailable)?;
        if body.len() > REPLY {
            return Err(unavailable("cluster response exceeds response budget"));
        }
        Ok(([("content-type", "application/json")], body).into_response())
    }
    async fn proxy(&self, request: Request, authority: &str) -> Result<Response, ApiError> {
        if request.method() == Method::CONNECT || request.headers().contains_key("upgrade") {
            return Err(ApiError::bad_request("upgrades are unsupported"));
        }
        let permit = self.proxies.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                429,
                "PROXY_CAPACITY_EXCEEDED",
                "cluster proxy capacity exhausted",
            )
        })?;
        validate_authority(authority).map_err(unavailable)?;
        let mut url = url::Url::parse(&format!("http://{authority}/")).map_err(unavailable)?;
        url.set_path(request.uri().path());
        url.set_query(request.uri().query());
        if url.path() != request.uri().path() || url.query() != request.uri().query() {
            return Err(ApiError::bad_request("proxy URL normalization refused"));
        }
        check_peer_http(&url, self.allow_insecure_peer_http)?;
        let (parts, body) = request.into_parts();
        let mut headers = parts.headers;
        strip_hop(&mut headers);
        headers.remove("host");
        // Preserve the client's credentials exactly; never synthesize a lifecycle key.
        let upstream = tokio::time::timeout(
            RPC_TIME,
            self.http
                .request(parts.method, url)
                .headers(headers)
                .body(reqwest::Body::wrap_stream(body.into_data_stream()))
                .send(),
        )
        .await
        .map_err(unavailable)?
        .map_err(unavailable)?;
        let status = upstream.status();
        let mut headers = upstream.headers().clone();
        strip_hop(&mut headers);
        let stream = upstream.bytes_stream();
        let body = Body::from_stream(stream::unfold(
            (stream, permit),
            |(mut stream, permit)| async move {
                stream.next().await.map(|item| (item, (stream, permit)))
            },
        ));
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        Ok(response)
    }
}
async fn entry(State(cluster): State<Arc<Cluster>>, request: Request) -> Response {
    let http1 = request.version() <= axum::http::Version::HTTP_11;
    let is_e2b = e2b::route(request.uri().path());
    let mut response = match cluster.handle(request).await {
        Ok(response) => response,
        Err(error) if is_e2b => super::e2b::Error(error).into_response(),
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-request-id",
        uuid::Uuid::new_v4().to_string().parse().unwrap(),
    );
    if http1 && (response.status().is_client_error() || response.status().is_server_error()) {
        response
            .headers_mut()
            .insert("connection", "close".parse().unwrap());
    }
    response
}
fn charge_inventory(bytes: &mut usize, added: usize) -> Result<(), ApiError> {
    let total = bytes
        .checked_add(added)
        .filter(|total| *total <= REPLY)
        .ok_or_else(|| unavailable("cluster inventory exceeds response budget"))?;
    *bytes = total;
    Ok(())
}
fn unavailable(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        503,
        "CLUSTER_UNAVAILABLE",
        "Peer unavailable or response uncertain; a dispatched mutation may have completed and is never retried automatically",
    )
}
impl HttpReply {
    fn response(self) -> Result<Response, ApiError> {
        let mut response = Response::new(Body::from(self.body));
        *response.status_mut() = StatusCode::from_u16(self.status).map_err(unavailable)?;
        response
            .headers_mut()
            .insert("content-type", "application/json".parse().unwrap());
        if let Some(location) = self.location {
            response
                .headers_mut()
                .insert("location", location.parse().map_err(unavailable)?);
        }
        Ok(response)
    }
}
fn globalize(value: &mut Value, owner: &str) {
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        value["id"] = Value::String(format!("{owner}.{id}"));
    }
}
async fn globalize_response(response: Response, owner: &str) -> Result<Response, ApiError> {
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, REPLY).await.map_err(unavailable)?;
    let body = if parts.status.is_success() && !bytes.is_empty() {
        let mut value: Value = serde_json::from_slice(&bytes).map_err(unavailable)?;
        globalize(&mut value, owner);
        serde_json::to_vec(&value).map_err(unavailable)?
    } else {
        bytes.to_vec()
    };
    if let Some(location) = parts
        .headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("/v1/sandboxes/"))
    {
        let location = format!("/v1/sandboxes/{owner}.{location}")
            .parse()
            .map_err(unavailable)?;
        parts.headers.insert("location", location);
    }
    parts.headers.remove("content-length");
    Ok(Response::from_parts(parts, Body::from(body)))
}
fn valid_owner(owner: &str) -> bool {
    owner.len() == 64
        && owner
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_local(id: &str) -> bool {
    id.strip_prefix("sb-")
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .is_some_and(|v| format!("sb-{v}") == id)
}
fn split_id(id: &str) -> Result<(&str, &str), ApiError> {
    let (owner, local) = id
        .split_once('.')
        .ok_or_else(|| ApiError::bad_request("invalid global ID"))?;
    if !valid_owner(owner) || !valid_local(local) {
        return Err(ApiError::bad_request("invalid global ID"));
    }
    Ok((owner, local))
}
fn valid_control_path(path: &str) -> bool {
    if e2b::control_path(path) {
        return true;
    }
    let path = path.split('?').next().unwrap_or_default();
    if path == "/v1/sandboxes" {
        return true;
    }
    let Some(rest) = path.strip_prefix("/v1/sandboxes/") else {
        return false;
    };
    let (id, suffix) = rest.split_once('/').unwrap_or((rest, ""));
    valid_local(id)
        && (matches!(
            suffix,
            "" | "pause"
                | "resume"
                | "renew-expiration"
                | "snapshots"
                | "metadata"
                | "networkpolicy"
        ) || suffix
            .strip_prefix("endpoints/")
            .is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p > 0)))
}
fn check_peer_http(url: &url::Url, allow_insecure: bool) -> Result<(), ApiError> {
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
        _ => false,
    };
    if url.scheme() == "http" && (loopback || allow_insecure) {
        Ok(())
    } else {
        Err(ApiError::new(
            503,
            "INSECURE_PEER_HTTP_DISABLED",
            "Plaintext peer HTTP requires a literal loopback address or explicit allow_insecure_peer_http opt-in",
        ))
    }
}
fn concrete_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast()
        }
        std::net::IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || !ip.is_unspecified() && !ip.is_multicast(),
            |ip| concrete_ip(ip.into()),
        ),
    }
}
fn validate_authority(authority: &str) -> anyhow::Result<()> {
    let parsed: axum::http::uri::Authority = authority.parse()?;
    anyhow::ensure!(
        !authority.contains('@') && !authority.contains('\\') && !parsed.host().is_empty(),
        "invalid peer HTTP authority"
    );
    Ok(())
}
fn validate_proxy_path(path: &str) -> Result<(), ApiError> {
    let (raw, query) = path
        .split_once('?')
        .map_or((path, None), |(p, q)| (p, Some(q)));
    let segments: Vec<_> = raw.splitn(7, '/').collect();
    if segments.len() < 6
        || !segments[0].is_empty()
        || segments[1] != "v1"
        || segments[2] != "sandboxes"
        || !valid_local(segments[3])
        || segments[4] != "proxy"
        || !segments[5].bytes().all(|b| b.is_ascii_digit())
        || !segments[5]
            .parse::<u16>()
            .is_ok_and(|p| p > 0 && p.to_string() == segments[5])
    {
        return Err(ApiError::bad_request("invalid proxy routing path"));
    }
    // Only inspect decoded bytes for unsafe path semantics. Forward the original
    // escaped suffix, preserving spaces, UTF-8, escaped slashes and literal '%'.
    let mut decoded = Vec::with_capacity(raw.len());
    let mut input = raw.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        let byte = if byte == b'%' {
            let high = input.next().and_then(|v| char::from(v).to_digit(16));
            let low = input.next().and_then(|v| char::from(v).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => (high * 16 + low) as u8,
                _ => return Err(ApiError::bad_request("invalid proxy percent escape")),
            }
        } else {
            byte
        };
        if byte == b'\\' || byte.is_ascii_control() {
            return Err(ApiError::bad_request("unsafe proxy path byte"));
        }
        decoded.push(byte);
    }
    if decoded
        .split(|b| *b == b'/')
        .any(|segment| segment == b"." || segment == b"..")
    {
        return Err(ApiError::bad_request("proxy path traversal"));
    }
    // Match the local adapter: set a path on a fixed origin, never URL-join a
    // client suffix, and reject any normalization of the path or query.
    let mut url = url::Url::parse("http://127.0.0.1/").unwrap();
    url.set_path(raw);
    url.set_query(query);
    if url.path() != raw || url.query() != query {
        return Err(ApiError::bad_request(
            "proxy path/query normalization refused",
        ));
    }
    Ok(())
}
fn strip_hop(headers: &mut HeaderMap) {
    let names: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|s| s.trim().to_owned()))
        .collect();
    for name in names {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Runtime, RuntimeSpec, RuntimeState};
    use std::{
        collections::BTreeMap,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    const KEY: &str = "cluster-test-lifecycle-key-32-bytes-long";
    const SECRET: &str = "cluster-test-tls-passphrase-32-bytes-long";
    #[derive(Default)]
    pub(super) struct Fake {
        states: Mutex<BTreeMap<String, RuntimeState>>,
        pub(super) creates: AtomicUsize,
        pub(super) uncertain: std::sync::atomic::AtomicBool,
        pub(super) fail_clean: std::sync::atomic::AtomicBool,
        delay: std::sync::atomic::AtomicBool,
        endpoint_url: Mutex<Option<String>>,
        e2b_template: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl Runtime for Fake {
        async fn e2b_template(&self, image: &str) -> anyhow::Result<crate::runtime::E2bTemplate> {
            anyhow::ensure!(
                image == "fixture" && self.e2b_template.load(Ordering::SeqCst),
                "missing template"
            );
            Ok(crate::runtime::E2bTemplate {
                cpu_millis: 1000,
                memory_bytes: 64 * 1024 * 1024,
                envd_version: "0.10.2".into(),
            })
        }
        async fn create(&self, spec: &RuntimeSpec) -> anyhow::Result<()> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            if self.uncertain.load(Ordering::SeqCst) {
                anyhow::bail!("uncertain create; private-runtime-detail");
            }
            if self.fail_clean.load(Ordering::SeqCst) {
                return Err(crate::runtime::CreateError::new("confirmed cleanup", false).into());
            }
            if self.delay.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            self.states
                .lock()
                .unwrap()
                .insert(spec.id.clone(), RuntimeState::Running);
            Ok(())
        }
        async fn inspect(&self, id: &str) -> anyhow::Result<RuntimeState> {
            if self.uncertain.load(Ordering::SeqCst) {
                anyhow::bail!("runtime observation unavailable");
            }
            Ok(*self
                .states
                .lock()
                .unwrap()
                .get(id)
                .unwrap_or(&RuntimeState::Missing))
        }
        async fn pause(&self, id: &str) -> anyhow::Result<RuntimeState> {
            self.states
                .lock()
                .unwrap()
                .insert(id.into(), RuntimeState::Paused);
            Ok(RuntimeState::Paused)
        }
        async fn resume(&self, id: &str) -> anyhow::Result<RuntimeState> {
            self.states
                .lock()
                .unwrap()
                .insert(id.into(), RuntimeState::Running);
            Ok(RuntimeState::Running)
        }
        async fn delete(&self, id: &str) -> anyhow::Result<()> {
            self.states.lock().unwrap().remove(id);
            Ok(())
        }
        async fn endpoint(&self, _: &str, port: u16) -> anyhow::Result<String> {
            Ok(self
                .endpoint_url
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| format!("http://127.0.0.1:{port}")))
        }
    }
    pub(super) async fn node(
        seeds: Vec<SocketAddr>,
        images: Vec<String>,
    ) -> (Arc<Cluster>, Arc<Fake>, tempfile::TempDir) {
        node_at(seeds, images, "127.0.0.1:8080".into()).await
    }
    async fn node_at(
        seeds: Vec<SocketAddr>,
        images: Vec<String>,
        authority: String,
    ) -> (Arc<Cluster>, Arc<Fake>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Arc::new(Fake::default());
        let copy = runtime.clone();
        let daemon = Daemon::open(
            super::super::Config {
                state_dir: dir.path().into(),
                api_key: KEY.into(),
                public_endpoint: authority,
                max_sandboxes: 64,
                cpu_millis: 64_000,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                max_timeout_seconds: 3600,
            },
            move |_| Ok(copy as Arc<dyn Runtime>),
        )
        .await
        .unwrap();
        let cluster = Cluster::start(
            daemon,
            ClusterConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                seeds,
                secret: SECRET.into(),
                images,
                allow_insecure_peer_http: false,
            },
        )
        .await
        .unwrap();
        (cluster, runtime, dir)
    }
    fn payload() -> Vec<u8> {
        serde_json::to_vec(&json!({"image":{"uri":"fixture"},"entrypoint":["/bin/sh"],"resourceLimits":{"cpu":"1","memory":"64Mi"},"timeout":600})).unwrap()
    }
    pub(super) async fn http(
        cluster: &Arc<Cluster>,
        method: &str,
        path: &str,
        body: Vec<u8>,
        auth: bool,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if auth {
            request = request.header("open-sandbox-api-key", KEY);
        }
        let response = cluster
            .router()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), REPLY).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    pub(super) async fn converge(a: &Cluster, b: &Cluster) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let ap = a.peers().await;
                let bp = b.peers().await;

                if ap.is_ok_and(|v| v.len() == 2) && bp.is_ok_and(|v| v.len() == 2) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "TLS peer discovery timed out: a={}, b={}",
                a.system.addr(),
                b.system.addr()
            )
        });
    }
    #[tokio::test]
    async fn two_tls_nodes_remote_lifecycle_list_auth_and_unavailable_owner() {
        let (a, ar, _ad) = node(vec![], vec!["other".into()]).await;
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        assert_eq!(
            http(&a, "POST", "/v1/sandboxes", payload(), false).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, created) = http(&a, "POST", "/v1/sandboxes", payload(), true).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{created}");
        let id = created["id"].as_str().unwrap();
        assert!(id.starts_with(&b.owner));
        assert_eq!(ar.creates.load(Ordering::SeqCst), 0);
        assert_eq!(br.creates.load(Ordering::SeqCst), 1);
        let path = format!("/v1/sandboxes/{id}");
        assert_eq!(
            http(&a, "POST", &format!("{path}/pause"), vec![], true)
                .await
                .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            http(&a, "GET", &path, vec![], true).await.1["status"]["state"],
            "Paused"
        );
        let (_, listed) = http(
            &a,
            "GET",
            "/v1/sandboxes?state=Paused&pageSize=1",
            vec![],
            true,
        )
        .await;
        assert_eq!(listed["items"][0]["id"], id);
        assert_eq!(listed["pagination"]["totalItems"], 1);
        b.shutdown().await.unwrap();
        assert_eq!(
            http(&a, "GET", &path, vec![], true).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        a.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn uncertain_rpc_is_not_retried_and_status_is_not_blocked() {
        let (a, _, _ad) = node(vec![], vec!["other".into()]).await;
        let (b, runtime, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        runtime.delay.store(true, Ordering::SeqCst);
        let (actor, _) = a
            .peers()
            .await
            .unwrap()
            .into_iter()
            .find(|(_, s)| s.owner == b.owner)
            .unwrap();
        let result: anyhow::Result<HttpReply> = a
            .rpc(
                &actor,
                Call::Http {
                    method: "POST".into(),
                    path: "/v1/sandboxes".into(),
                    body: payload(),
                },
                Duration::from_millis(100),
            )
            .await;
        assert!(result.is_err());
        let _: Status = a
            .rpc(&actor, Call::Status, Duration::from_millis(100))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        // Generic mutation RPCs must not bypass the durable queue.
        assert_eq!(runtime.creates.load(Ordering::SeqCst), 0);
        assert_eq!(b.daemon.list().await.unwrap().len(), 0);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
    async fn stop_background(cluster: &Cluster) {
        let tasks = std::mem::take(&mut *cluster.background.lock().unwrap());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    #[tokio::test]
    async fn cached_observations_keep_success_age_and_exclude_missing_and_stale_totals() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        let (c, _, _cd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        discovered(&a, 3).await;
        stop_background(&a).await;
        a.members().await.unwrap();
        a.discovery.lock().unwrap().samples.clear();
        b.daemon
            .create(serde_json::from_slice(&payload()).unwrap())
            .await
            .unwrap();
        c.faults.store(1, Ordering::SeqCst);
        a.sample_observations(&mut 0).await;
        let first = a.observation().await;
        assert_eq!(first["contributors"]["fresh"], 1);
        assert_eq!(first["contributors"]["missing"], 1);
        assert_eq!(first["totals"]["count"], 1);
        assert_eq!(first["membership_complete"], false);
        assert_eq!(first["complete_for_discovered_members"], false);
        let key = b.system.node_id().to_string();
        let at = a.discovery.lock().unwrap().samples[&key]
            .success
            .as_ref()
            .unwrap()
            .0;
        b.faults.store(1, Ordering::SeqCst);
        a.sample_observations(&mut 0).await;
        assert_eq!(
            a.discovery.lock().unwrap().samples[&key]
                .success
                .as_ref()
                .unwrap()
                .0,
            at
        );
        {
            let mut cache = a.discovery.lock().unwrap();
            cache
                .samples
                .get_mut(&key)
                .unwrap()
                .success
                .as_mut()
                .unwrap()
                .0 -= OBSERVATION_FRESHNESS;
        }
        a.local_observation
            .lock()
            .unwrap()
            .success
            .as_mut()
            .unwrap()
            .0 -= OBSERVATION_FRESHNESS;
        // Even a stalled local registry cannot block this cache-only endpoint.
        let locked = a.daemon.registry.lock().await;
        let result = tokio::time::timeout(Duration::from_millis(100), a.observation())
            .await
            .unwrap();
        assert_eq!(result["contributors"]["stale"], 1);
        assert_eq!(result["totals"], Value::Null);
        assert_eq!(result["node"]["freshness"], "stale");
        drop(locked);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn status_server_and_client_saturation_do_not_consume_identity_slots() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        stop_background(&a).await;
        stop_background(&b).await;
        let (actor, _) = a.owner_peer(&b.owner).await.unwrap();
        let lock = b.daemon.registry.lock().await;
        let mut streams = Vec::new();
        for _ in 0..8 {
            streams.push(actor.send(wire(&Call::Status).unwrap()).await.unwrap());
        }
        let slots = a
            .observation_outgoing
            .clone()
            .acquire_many_owned(BACKGROUND_BATCH as u32)
            .await
            .unwrap();
        a.discovery.lock().unwrap().identities.clear();
        let identity = tokio::time::timeout(Duration::from_millis(500), a.owner_peer(&b.owner))
            .await
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(identity.owner, b.owner);
        // Work stealing uses Identity and Pull, never Status.
        let (status, _) = tokio::time::timeout(
            Duration::from_secs(2),
            http(&a, "POST", "/v1/sandboxes", payload(), true),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, created) = tokio::time::timeout(
            Duration::from_secs(2),
            http(&b, "POST", "/v1/sandboxes", payload(), true),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(created["id"].as_str().unwrap().starts_with(&a.owner));
        drop(slots);
        drop(lock);
        drop(streams);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cold_candidates_exclude_self_and_replacement_discards_late_identity_and_samples() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        stop_background(&a).await;
        let worker = a.worker.lock().unwrap().take().unwrap();
        worker.abort();
        let _ = worker.await;
        a.discovery.lock().unwrap().identities.clear();
        let batch = a.candidate_batch(&mut 0);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, b.system.node_id().to_string());
        a.sample_observations(&mut 0).await;
        let (key, candidate) = batch[0].clone();
        b.faults.store(4, Ordering::SeqCst);
        let request = a.identity(&key, &candidate);
        let replace = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            a.discovery
                .lock()
                .unwrap()
                .candidates
                .get_mut(&key)
                .unwrap()
                .generation
                .push_str("replacement");
        };
        let (result, ()) = tokio::join!(request, replace);
        assert!(result.is_err());
        a.members().await.unwrap();
        {
            let cache = a.discovery.lock().unwrap();
            assert!(!cache.identities.contains_key(&key));
            assert!(!cache.samples.contains_key(&key));
        }
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn identity_rejects_replacement_before_entry_even_with_fresh_cache() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        stop_background(&a).await;
        let worker = a.worker.lock().unwrap().take().unwrap();
        worker.abort();
        let _ = worker.await;
        let (key, captured) = a.candidate_batch(&mut 0).pop().unwrap();
        {
            let mut cache = a.discovery.lock().unwrap();
            cache
                .candidates
                .get_mut(&key)
                .unwrap()
                .generation
                .push_str("replacement");
            cache.identities.remove(&key);
        }
        assert!(a.identity(&key, &captured).await.is_err());
        assert!(!a.discovery.lock().unwrap().identities.contains_key(&key));
        let replacement = a.discovery.lock().unwrap().candidates[&key].clone();
        a.identity(&key, &replacement).await.unwrap();
        assert!(a.identity(&key, &captured).await.is_err());
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    #[test]
    fn observation_queue_allowlist_never_echoes_remote_strings() {
        let value = redacted_queue(
            &json!({"Queued": "SECRET", "env": "SECRET", "Completed": 2, "storage_failed": false}),
        );
        assert_eq!(value["Queued"], Value::Null);
        assert_eq!(value["Completed"], 2);
        assert!(!value.to_string().contains("SECRET"));
    }

    #[tokio::test]
    async fn observations_are_authenticated_local_and_redacted_without_peer_rpc() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        a.faults.store(3, Ordering::SeqCst);
        let path = "/v1/cluster/observations";
        assert_eq!(
            http(&a, "GET", path, vec![], false).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http(&a, "POST", path, vec![], true).await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        let (status, value) = http(&a, "GET", path, vec![], true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["source"], "local");
        assert_eq!(value["owner"], a.owner);
        assert_eq!(value["peer_coverage"]["complete"], false);
        assert_eq!(value["queue"]["retained_len"], 0);
        let encoded = value.to_string();
        assert!(!encoded.contains(KEY));
        assert!(!encoded.contains(SECRET));
        assert!(!encoded.contains("key_digest"));
        a.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn usage_failure_does_not_block_stealing_or_routing_but_identity_failure_does() {
        let (a, _, _ad) = node(vec![], vec!["other".into()]).await;
        let (b, runtime, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        a.faults.store(1, Ordering::SeqCst);
        b.faults.store(1, Ordering::SeqCst);
        converge(&a, &b).await;
        let (c, _, _cd) = node(vec![a.system.addr()], vec!["other".into()]).await;
        c.faults.store(3, Ordering::SeqCst);
        discovered(&a, 3).await;
        let (actor, _) = a.owner_peer(&b.owner).await.unwrap();
        assert!(
            a.rpc::<Status>(&actor, Call::Status, STATUS_TIME)
                .await
                .is_err()
        );
        let (status, created) = http(&a, "POST", "/v1/sandboxes", payload(), true).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{created}");
        assert_eq!(runtime.creates.load(Ordering::SeqCst), 1);
        let path = format!("/v1/sandboxes/{}", created["id"].as_str().unwrap());
        assert_eq!(http(&a, "GET", &path, vec![], true).await.0, StatusCode::OK);
        b.faults.store(3, Ordering::SeqCst);
        {
            let mut cache = a.discovery.lock().unwrap();
            for (_, identity, at) in cache.identities.values_mut() {
                if identity.owner == b.owner {
                    *at = tokio::time::Instant::now() - IDENTITY_FRESHNESS;
                }
            }
        }
        assert_eq!(
            http(&a, "GET", &path, vec![], true).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn two_eligible_peers_pull_without_duplicate_launch() {
        let (a, ar, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        a.daemon
            .create(serde_json::from_slice(&payload()).unwrap())
            .await
            .unwrap();
        let (status, created) = http(&a, "POST", "/v1/sandboxes", payload(), true).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{created}");
        let id = created["id"].as_str().unwrap();
        assert!(id.starts_with(&a.owner) || id.starts_with(&b.owner));
        assert_eq!(
            ar.creates.load(Ordering::SeqCst) + br.creates.load(Ordering::SeqCst),
            2
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
    async fn discovered(cluster: &Cluster, count: usize) {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let instances = cluster
                    .system
                    .get_named_instances_detailed(&ActorPath::new(SERVICE).unwrap())
                    .await;
                let live = instances
                    .iter()
                    .filter(|(m, _)| m.status == pulsing_actor::cluster::MemberStatus::Alive)
                    .count();
                if live == count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("named service discovery");
    }
    #[tokio::test]
    async fn unavailable_third_peer_does_not_block_healthy_owner_or_placement() {
        let (a, _, _ad) = node(vec![], vec!["other".into()]).await;
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        let (c, _, _cd) = node(vec![a.system.addr()], vec!["other".into()]).await;
        discovered(&a, 3).await;
        // A live discovered member whose status RPC cannot complete. This is
        // deterministic, unlike graceful shutdown which removes discovery state.
        let stalled = c.daemon.registry.lock().await;
        let (status, created) = http(&a, "POST", "/v1/sandboxes", payload(), true).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{created}");
        let id = created["id"].as_str().unwrap();
        assert!(id.starts_with(&b.owner));
        assert_eq!(br.creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            http(&a, "GET", &format!("/v1/sandboxes/{id}"), vec![], true)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            http(
                &a,
                "POST",
                &format!("/v1/sandboxes/{id}/pause"),
                vec![],
                true
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            http(&a, "GET", "/v1/sandboxes", vec![], true).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let missing_owner_id = format!("{}.sb-{}", c.owner, uuid::Uuid::new_v4());
        assert_eq!(
            http(
                &a,
                "GET",
                &format!("/v1/sandboxes/{missing_owner_id}"),
                vec![],
                true
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(stalled);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
    }
    struct StatusActor(Identity);
    #[async_trait::async_trait]
    impl Actor for StatusActor {
        async fn receive(
            &mut self,
            _: Message,
            _: &mut ActorContext,
        ) -> pulsing_actor::error::Result<Message> {
            wire(&self.0)
        }
    }
    #[tokio::test]
    async fn duplicate_owners_and_mismatched_keys_still_fail_closed() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        let status = a
            .peers()
            .await
            .unwrap()
            .into_iter()
            .find(|(_, s)| s.owner == a.owner)
            .unwrap()
            .1;
        let sandbox = b
            .daemon
            .create(serde_json::from_slice(&payload()).unwrap())
            .await
            .unwrap();
        for wrong_key in [false, true] {
            let mut conflict = status.clone();
            if wrong_key {
                conflict.owner = digest(b"different-owner");
                conflict.key_digest = digest(b"wrong-key");
            }
            let system = ActorSystem::builder()
                .addr("127.0.0.1:0")
                .seeds([a.system.addr()])
                .http2_config(Http2Config::default().max_retries(0))
                .tls(SECRET)
                .unwrap()
                .build()
                .await
                .unwrap();
            system
                .spawning()
                .name(SERVICE)
                .metadata(std::collections::HashMap::from([(
                    "protocol".into(),
                    WIRE.into(),
                )]))
                .spawn(StatusActor(conflict))
                .await
                .unwrap();
            discovered(&a, 3).await;
            assert!(a.peers().await.is_err());
            // Owner routing still fails closed. Creates now commit locally and
            // do not sample peer status on the HTTP request path.
            assert_eq!(
                http(
                    &a,
                    "GET",
                    &format!("/v1/sandboxes/{}.{}", b.owner, sandbox.id),
                    vec![],
                    true
                )
                .await
                .0,
                StatusCode::SERVICE_UNAVAILABLE
            );
            system.shutdown().await.unwrap();
            discovered(&a, 2).await;
        }
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
    async fn local_list(cluster: &Cluster, query: &str) -> (StatusCode, Value) {
        let response = cluster
            .local
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/sandboxes?{query}"))
                    .header("open-sandbox-api-key", KEY)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), REPLY).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    #[tokio::test]
    async fn invalid_list_queries_match_local_before_discovery() {
        let (cluster, _, _dir) = node(vec![], vec!["fixture".into()]).await;
        // Every discovery RPC would fail admission. Query validation must win.
        let permits = cluster.outgoing.acquire_many(64).await.unwrap();
        let overflow = format!("page={}&pageSize=2", usize::MAX);
        for query in [
            "page=0",
            "page=",
            "page=-1",
            "page=%2B1",
            "page=1.5",
            "page=1&page=2",
            "pageSize=0",
            "pageSize=",
            "pageSize=20&pageSize=20",
            "metadata=a%3Db&metadata=c%3Dd",
            "page=184467440737095516160",
            overflow.as_str(),
        ] {
            let expected = local_list(&cluster, query).await;
            assert_eq!(expected.0, StatusCode::BAD_REQUEST, "{query}");
            let actual = http(
                &cluster,
                "GET",
                &format!("/v1/sandboxes?{query}"),
                vec![],
                true,
            )
            .await;
            assert_eq!(actual, expected, "{query}");
        }
        assert_eq!(
            http(&cluster, "GET", "/v1/sandboxes", vec![], true).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(permits);
        // Do not tighten the local parser's intentionally lenient state,
        // metadata, percent decoding or unknown-parameter semantics.
        for query in [
            "state=Unknown",
            "state=Running&state=Paused",
            "metadata=broken",
            "metadata=k%3Dv%252Fz",
            "metadata=%FF",
            "unknown=value",
            "page=01&pageSize=002",
        ] {
            let expected = local_list(&cluster, query).await;
            assert_eq!(expected.0, StatusCode::OK, "{query}");
            let actual = http(
                &cluster,
                "GET",
                &format!("/v1/sandboxes?{query}"),
                vec![],
                true,
            )
            .await;
            assert_eq!(actual, expected, "{query}");
        }
        cluster.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn three_nodes_idle_executor_steals_and_task_status_is_authenticated_redacted() {
        let (a, ar, _ad) = node(vec![], vec!["other".into()]).await;
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        let (c, cr, _cd) = node(vec![a.system.addr()], vec!["other".into()]).await;
        discovered(&b, 3).await;
        let mut body: Value = serde_json::from_slice(&payload()).unwrap();
        body["env"] = json!({"PRIVATE_SECRET":"never-return-in-task-status"});
        let response = a
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/sandboxes")
                    .header("content-type", "application/json")
                    .header("open-sandbox-api-key", KEY)
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let task_id = response.headers()["x-pvisor-task-id"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(task_id.starts_with(&a.owner));
        let location = response.headers()["location"].to_str().unwrap().to_string();
        let created: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), REPLY).await.unwrap()).unwrap();
        assert_eq!(
            location,
            format!("/v1/sandboxes/{}", created["id"].as_str().unwrap())
        );
        assert!(created.get("image").is_none());
        assert!(created.get("snapshotId").is_none());
        assert!(created.get("updatedAt").is_none());
        let path = format!("/v1/cluster/tasks/{task_id}");
        assert_eq!(
            http(&c, "GET", &path, vec![], false).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (status, task) = http(&c, "GET", &path, vec![], true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(task["phase"], "Completed");
        assert_eq!(task["result"]["sandbox_id"], created["id"]);
        assert!(!task.to_string().contains("PRIVATE_SECRET"));
        assert!(!task.to_string().contains("never-return"));
        assert_eq!(ar.creates.load(Ordering::SeqCst), 0);
        assert_eq!(br.creates.load(Ordering::SeqCst), 1);
        assert_eq!(cr.creates.load(Ordering::SeqCst), 0);
        for cluster in [&a, &b, &c] {
            cluster.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn tls_lost_grant_home_restart_and_reserved_executor_recovery() {
        let (a, ar, ad) = node(vec![], vec!["other".into()]).await;
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        // Hold executor recovery until after the durable grant response is lost.
        for cluster in [&a, &b] {
            let worker = cluster.worker.lock().unwrap().take().unwrap();
            worker.abort();
            let _ = worker.await;
        }
        a.queue.enqueue(payload()).await.unwrap();
        let actor = b
            .peers()
            .await
            .unwrap()
            .into_iter()
            .find(|(_, status)| status.owner == a.owner)
            .unwrap()
            .0;
        let peer = queue::Peer {
            home: a.owner.clone(),
            executor: b.owner.clone(),
            key_digest: digest(KEY.as_bytes()),
        };
        let offer: Result<Option<queue::Offer>, ApiError> = b
            .rpc(
                &actor,
                Call::Pull {
                    peer: peer.clone(),
                    capacity: queue::Capacity {
                        images: vec!["fixture".into()],
                        count: 1,
                        cpu: 1000,
                        memory: 64 * 1024 * 1024,
                    },
                },
                STATUS_TIME,
            )
            .await
            .unwrap();
        let offer = offer.unwrap().unwrap();
        b.daemon
            .reserve_claim(
                offer.grant.identity.clone(),
                serde_json::from_slice(&offer.body).unwrap(),
            )
            .await
            .unwrap();
        let granted: Result<bool, ApiError> = b
            .rpc(
                &actor,
                Call::Accept {
                    peer: peer.clone(),
                    grant: offer.grant.clone(),
                },
                STATUS_TIME,
            )
            .await
            .unwrap();
        assert!(granted.unwrap());
        // No runtime call consumes this grant. A repeat gets the same permission.
        let repeated: Result<bool, ApiError> = b
            .rpc(
                &actor,
                Call::Accept {
                    peer: peer.clone(),
                    grant: offer.grant.clone(),
                },
                STATUS_TIME,
            )
            .await
            .unwrap();
        assert!(repeated.unwrap());
        let mut stale = offer.grant.clone();
        stale.identity.generation += 1;
        let rejected: Result<bool, ApiError> = b
            .rpc(&actor, Call::Accept { peer, grant: stale }, STATUS_TIME)
            .await
            .unwrap();
        assert!(!rejected.unwrap());
        assert_eq!(br.creates.load(Ordering::SeqCst), 0);
        let owner = a.owner.clone();
        a.shutdown().await.unwrap();
        drop(a);
        let runtime = ar.clone();
        let daemon = Daemon::open(
            super::super::Config {
                state_dir: ad.path().into(),
                api_key: KEY.into(),
                public_endpoint: "127.0.0.1:8080".into(),
                max_sandboxes: 64,
                cpu_millis: 64_000,
                memory_bytes: 4 * 1024 * 1024 * 1024,
                max_timeout_seconds: 3600,
            },
            move |_| Ok(runtime as Arc<dyn Runtime>),
        )
        .await
        .unwrap();
        let mut config = startup_config();
        config.images = vec!["other".into()];
        config.seeds = vec![b.system.addr()];
        let restarted = Cluster::start(daemon, config).await.unwrap();
        assert_eq!(restarted.owner, owner);
        converge(&restarted, &b).await;
        assert!(
            restarted
                .queue
                .pull(
                    digest(b"different executor"),
                    queue::Capacity {
                        images: vec!["fixture".into()],
                        count: 1,
                        cpu: 1000,
                        memory: 64 * 1024 * 1024
                    }
                )
                .await
                .unwrap()
                .is_none()
        );
        Cluster::start_worker(&b);
        let path = format!("/v1/cluster/tasks/{owner}.{}", offer.grant.identity.task_id);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, task) = http(&restarted, "GET", &path, vec![], true).await;
                if task["phase"] == "Completed" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(br.creates.load(Ordering::SeqCst), 1);
        assert_eq!(ar.creates.load(Ordering::SeqCst), 0);
        restarted.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn disconnected_http_wait_does_not_cancel_durable_task() {
        let (a, ar, ad) = node(vec![], vec!["other".into()]).await;
        let copy = a.clone();
        let request =
            tokio::spawn(
                async move { http(&copy, "POST", "/v1/sandboxes", payload(), true).await },
            );
        let id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ledger: Value = serde_json::from_slice(
                    &std::fs::read(ad.path().join("cluster-queue.json")).unwrap(),
                )
                .unwrap();
                if let Some(id) = ledger["tasks"][0]["id"].as_str() {
                    break id.to_string();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let _ = request.await;
        // The executor joins only after the client disconnected.
        let (b, br, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        let path = format!("/v1/cluster/tasks/{}.{id}", a.owner);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (_, task) = http(&a, "GET", &path, vec![], true).await;
                if task["phase"] == "Completed" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(ar.creates.load(Ordering::SeqCst), 0);
        assert_eq!(br.creates.load(Ordering::SeqCst), 1);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    fn startup_config() -> ClusterConfig {
        ClusterConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            seeds: vec![],
            secret: SECRET.into(),
            images: vec!["fixture".into()],
            allow_insecure_peer_http: false,
        }
    }
    #[tokio::test]
    async fn public_start_validates_bind_seeds_and_runtime_image_key_syntax() {
        let (fixture, _, _dir) = node(vec![], vec!["fixture".into()]).await;
        for bind in [
            "0.0.0.0:0",
            "[::]:0",
            "[::ffff:0.0.0.0]:0",
            "224.0.0.1:10000",
            "[ff02::1]:10000",
            "255.255.255.255:10000",
        ] {
            let mut config = startup_config();
            config.bind = bind.parse().unwrap();
            let error = Cluster::start(fixture.daemon.clone(), config)
                .await
                .err()
                .expect("invalid bind rejected");
            assert!(
                error.to_string().contains("cluster bind"),
                "{bind}: {error}"
            );
        }
        for seed in ["0.0.0.0:10000", "[::]:10000", "127.0.0.1:0"] {
            let mut config = startup_config();
            config.seeds = vec![seed.parse().unwrap()];
            let error = Cluster::start(fixture.daemon.clone(), config)
                .await
                .err()
                .expect("invalid seed rejected");
            assert!(
                error.to_string().contains("cluster seeds"),
                "{seed}: {error}"
            );
        }
        for image in [
            "",
            ".hidden",
            "..",
            "../escape",
            "/absolute",
            "name/path",
            "name\\path",
            "ubuntu:latest",
            "https://registry/image",
            "space key",
            "nul\0key",
            "caf\u{e9}",
            &"x".repeat(129),
        ] {
            let mut config = startup_config();
            config.images = vec![image.into()];
            let error = Cluster::start(fixture.daemon.clone(), config)
                .await
                .err()
                .expect("invalid image rejected");
            assert!(
                error.to_string().contains("prepared-image key"),
                "{image:?}: {error}"
            );
        }

        let mut valid = startup_config();
        valid.images = vec![
            "ubuntu-24.04".into(),
            "base_v1".into(),
            "-runtime-accepts-leading-dash".into(),
            "x".repeat(128),
        ];
        // A second live cluster facade cannot own the same durable Home ledger.
        assert!(
            Cluster::start(fixture.daemon.clone(), startup_config())
                .await
                .is_err()
        );
        let daemon = fixture.daemon.clone();
        fixture.shutdown().await.unwrap();
        drop(fixture);
        let cluster = Cluster::start(daemon, valid).await.unwrap();
        cluster.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn empty_image_inventory_allows_node_startup() {
        let (cluster, _, _dir) = node(vec![], vec![]).await;
        assert!(cluster.images.is_empty());
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn globalized_response_drops_stale_content_length() {
        let body =
            serde_json::to_vec(&json!({"id":format!("sb-{}", uuid::Uuid::new_v4())})).unwrap();
        let length = body.len();
        let response = Response::builder()
            .header("content-length", length)
            .body(Body::from(body))
            .unwrap();
        let response = globalize_response(response, &"a".repeat(64)).await.unwrap();
        assert!(!response.headers().contains_key("content-length"));
        assert_eq!(
            to_bytes(response.into_body(), REPLY).await.unwrap().len(),
            length + 65
        );
    }
    #[tokio::test]
    async fn inventory_fetches_bounded_peer_pages_then_filters_and_pages_globally() {
        let (a, _, _ad) = node(vec![], vec!["fixture".into()]).await;
        let (b, _, _bd) = node(vec![a.system.addr()], vec!["fixture".into()]).await;
        converge(&a, &b).await;
        let mut expected = Vec::new();
        for (cluster, count) in [(&a, INVENTORY_PAGE + 3), (&b, 3)] {
            for _ in 0..count {
                let created = cluster
                    .daemon
                    .create(serde_json::from_slice(&payload()).unwrap())
                    .await
                    .unwrap();
                expected.push(format!("{}.{}", cluster.owner, created.id));
            }
        }
        expected.sort();
        let (status, list) = http(
            &a,
            "GET",
            "/v1/sandboxes?state=Running&page=2&pageSize=7",
            vec![],
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert_eq!(list["pagination"]["totalItems"], expected.len());
        let actual: Vec<_> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect();
        assert_eq!(actual, expected[7..14]);
        let (_, empty) = http(&a, "GET", "/v1/sandboxes?state=Paused", vec![], true).await;
        assert_eq!(empty["pagination"]["totalItems"], 0);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
    #[test]
    fn aggregate_inventory_budget_counts_pages_and_global_id_overhead() {
        let mut bytes = 0;
        charge_inventory(&mut bytes, REPLY - 65).unwrap();
        charge_inventory(&mut bytes, 65).unwrap();
        assert_eq!(bytes, REPLY);
        assert!(charge_inventory(&mut bytes, 1).is_err());
        assert_eq!(bytes, REPLY);
        assert!(charge_inventory(&mut bytes, usize::MAX).is_err());
    }
    #[test]
    fn rpc_body_and_envelope_budgets_agree_at_boundaries() {
        // Worst-case byte values used to turn 32 MiB into ~128 MiB decimal JSON.
        let reply = HttpReply {
            status: 200,
            body: vec![255; REPLY],
            location: None,
        };
        let message = wire(&reply).unwrap();
        let Message::Single { ref data, .. } = message else {
            panic!("single response")
        };
        assert!(data.len() <= WIRE_LIMIT);
        assert!(data.len() >= ENCODED_BODY);
        let decoded: HttpReply = decode(message).unwrap();
        assert_eq!(decoded.body, reply.body);
        drop(decoded);
        drop(reply);
        let oversized = HttpReply {
            status: 200,
            body: vec![0; REPLY + 1],
            location: None,
        };
        assert!(wire(&oversized).is_err());
        drop(oversized);
        // The same wire bound applies to metadata as well as the encoded body.
        assert!(
            wire(&HttpReply {
                status: 200,
                body: vec![],
                location: Some("x".repeat(WIRE_LIMIT))
            })
            .is_err()
        );
        assert!(decode::<HttpReply>(Message::single(WIRE, vec![b' '; WIRE_LIMIT + 1])).is_err());
    }
    #[derive(Default)]
    struct Guest {
        requests: Mutex<Vec<(String, HeaderMap)>>,
        release_response: tokio::sync::Notify,
        upload_started: tokio::sync::Notify,
    }
    async fn guest(State(guest): State<Arc<Guest>>, request: Request) -> Response {
        guest
            .requests
            .lock()
            .unwrap()
            .push((request.uri().to_string(), request.headers().clone()));
        match request.uri().path() {
            "/stream" => {
                let first = stream::once(async { Ok::<_, std::io::Error>("first\n") });
                let second = stream::once(async move {
                    guest.release_response.notified().await;
                    Ok::<_, std::io::Error>("second\n")
                });
                let mut response = Response::new(Body::from_stream(first.chain(second)));
                response
                    .headers_mut()
                    .insert("content-type", "text/event-stream".parse().unwrap());
                response
                    .headers_mut()
                    .insert("connection", "x-hop-secret".parse().unwrap());
                response
                    .headers_mut()
                    .insert("x-hop-secret", "remove-me".parse().unwrap());
                response
            }
            "/upload" => {
                let mut stream = request.into_body().into_data_stream();
                let mut data = Vec::new();
                while let Some(chunk) = stream.next().await {
                    data.extend_from_slice(&chunk.unwrap());
                    guest.upload_started.notify_one();
                }
                Response::new(Body::from(data))
            }
            "/redirect" => {
                (StatusCode::TEMPORARY_REDIRECT, [("location", "/stream")]).into_response()
            }
            _ if request.uri().path().starts_with("/escaped/") => {
                request.uri().to_string().into_response()
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }
    #[tokio::test]
    async fn e2b_cluster_remote_create_lifecycle_and_scoped_streams() {
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let guest_url = format!("http://{}", gl.local_addr().unwrap());
        let gs = Arc::new(Guest::default());
        let app = Router::new().fallback(guest).with_state(gs.clone());
        let guest_server = tokio::spawn(async move { axum::serve(gl, app).await.unwrap() });
        let al = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let aa = format!("http://{}", al.local_addr().unwrap());
        let ba = format!("http://{}", bl.local_addr().unwrap());
        let ca = format!("http://{}", cl.local_addr().unwrap());
        let (a, ar, _ad) = node_at(vec![], vec![], al.local_addr().unwrap().to_string()).await;
        let (b, br, _bd) = node_at(
            vec![a.system.addr()],
            vec!["fixture".into()],
            bl.local_addr().unwrap().to_string(),
        )
        .await;
        let (c, cr, _cd) = node_at(
            vec![a.system.addr()],
            vec![],
            cl.local_addr().unwrap().to_string(),
        )
        .await;
        br.e2b_template.store(true, Ordering::SeqCst);
        *br.endpoint_url.lock().unwrap() = Some(guest_url);
        discovered(&a, 3).await;
        discovered(&b, 3).await;
        discovered(&c, 3).await;
        let app = a.router();
        let aserver = tokio::spawn(async move { axum::serve(al, app).await.unwrap() });
        let app = b.router();
        let bserver = tokio::spawn(async move { axum::serve(bl, app).await.unwrap() });
        let app = c.router();
        let cserver = tokio::spawn(async move { axum::serve(cl, app).await.unwrap() });
        let client = a.http.clone();
        let payload = json!({"templateID":"fixture", "timeout":300, "metadata":{"team":"e2b"}, "envVars":{"VALUE":"literal"}});
        for header in ["open-sandbox-api-key", "x-access-token"] {
            let denied = client
                .post(format!("{aa}/sandboxes"))
                .header(header, KEY)
                .json(&payload)
                .send()
                .await
                .unwrap();
            assert_eq!(denied.status(), 401);
            assert_eq!(denied.json::<Value>().await.unwrap()["code"], 401);
        }
        let invalid = client
            .post(format!("{aa}/sandboxes"))
            .header("x-api-key", KEY)
            .json(&json!({"templateID":"fixture","secure":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), 501);
        let absent = client
            .post(format!("{aa}/sandboxes"))
            .header("x-api-key", KEY)
            .json(&json!({"templateID":"e2b-base"}))
            .send()
            .await
            .unwrap();
        assert_eq!(absent.status(), 404);
        let mut created = Vec::new();
        for path in ["/sandboxes", "/v2/sandboxes"] {
            let response = client
                .post(format!("{aa}{path}"))
                .header("x-api-key", KEY)
                .json(&payload)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 201);
            assert!(response.headers().contains_key("x-pvisor-task-id"));
            let value: Value = response.json().await.unwrap();
            let id = value["sandboxID"].as_str().unwrap();
            assert_eq!(split_id(id).unwrap().0, b.owner);
            assert_eq!(value["templateID"], "fixture");
            assert_eq!(value["envdVersion"], "0.10.2");
            assert_eq!(value["cpuCount"], 1);
            assert_eq!(value["memoryMB"], 64);
            created.push(value);
        }
        assert_eq!(ar.creates.load(Ordering::SeqCst), 0);
        assert_eq!(cr.creates.load(Ordering::SeqCst), 0);
        assert_eq!(br.creates.load(Ordering::SeqCst), 2);
        assert_eq!(
            b.daemon.capacity_usage().await.unwrap(),
            (2, 2000, 128 * 1024 * 1024)
        );
        let id = created[0]["sandboxID"].as_str().unwrap();
        let token = created[0]["envdAccessToken"].as_str().unwrap();
        let base = format!("{ca}/sandboxes/{id}");
        let denied = client
            .get(&base)
            .header("x-api-key", token)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 401);
        let info: Value = client
            .get(&base)
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(info["sandboxID"], id);
        assert_eq!(info["envdAccessToken"], token);
        let page = client
            .get(format!(
                "{ca}/v2/sandboxes?limit=1&metadata=team%3De2b&order=asc"
            ))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
        assert_eq!(page.headers()["x-next-token"], "1");
        let page: Vec<Value> = page.json().await.unwrap();
        assert_eq!(page.len(), 1);
        assert!(page[0].get("envdAccessToken").is_none());
        let page2: Vec<Value> = client
            .get(format!("{ca}/sandboxes?limit=1&nextToken=1&order=asc"))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_ne!(page[0]["sandboxID"], page2[0]["sandboxID"]);
        for (suffix, body, status) in [
            ("pause", json!({}), 204),
            ("pause", json!({}), 409),
            ("connect", json!({"timeout":120}), 200),
            ("timeout", json!({"timeout":60}), 204),
            ("resume", json!({"timeout":180}), 200),
        ] {
            let response = client
                .post(format!("{base}/{suffix}"))
                .header("x-api-key", KEY)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status, "{suffix}");
        }
        let resumed: Value = client
            .post(format!("{ca}/v2/sandboxes/{id}/connect"))
            .header("x-api-key", KEY)
            .json(&json!({"timeout":180}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resumed["sandboxID"], id);
        let unsupported = client
            .get(format!("{base}/metrics"))
            .header("x-api-key", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(unsupported.status(), 501);
        let (_, local_id) = split_id(id).unwrap();
        for (prefix, suffix) in [
            ("/sandboxes", "snapshots"),
            ("/sandboxes", "metrics"),
            ("/sandboxes", "renew-expiration"),
            ("/v2/sandboxes", "snapshots"),
            ("/v2/sandboxes", "pause"),
            ("/v2/sandboxes", "resume"),
            ("/v2/sandboxes", "timeout"),
        ] {
            let mut baseline = None;
            for url in [
                format!("{ba}{prefix}/{local_id}/{suffix}"),
                format!("{ba}{prefix}/{id}/{suffix}"),
                format!("{ca}{prefix}/{id}/{suffix}"),
            ] {
                // No body or Content-Type: unsupported handlers must still dispatch.
                let response = client
                    .post(&url)
                    .header("x-api-key", KEY)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 501, "{url}");
                let error: Value = response.json().await.unwrap();
                assert_eq!(error["code"], 501);
                assert_eq!(error["error_code"], "NOT_SUPPORTED");
                if let Some(expected) = &baseline {
                    assert_eq!(&error, expected, "{url}");
                } else {
                    baseline = Some(error);
                }
            }
        }
        for (prefix, suffix) in [
            ("/sandboxes", "pause"),
            ("/sandboxes", "resume"),
            ("/sandboxes", "timeout"),
            ("/sandboxes", "connect"),
            ("/v2/sandboxes", "connect"),
        ] {
            for entry in [&ba, &ca] {
                let url = format!("{entry}{prefix}/{id}/{suffix}");
                let response = client
                    .post(&url)
                    .header("x-api-key", KEY)
                    .body("{}")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 400, "missing JSON Content-Type: {url}");
            }
        }
        for bad in [KEY, created[1]["envdAccessToken"].as_str().unwrap()] {
            let denied = client
                .get(format!("{ca}/e2b/stream"))
                .header("e2b-sandbox-id", id)
                .header("e2b-sandbox-port", "49983")
                .header("x-access-token", bad)
                .send()
                .await
                .unwrap();
            assert_eq!(denied.status(), 401);
        }
        assert!(gs.requests.lock().unwrap().is_empty());
        let forbidden = client
            .post(format!("{ca}/e2b/%69nit"))
            .header("e2b-sandbox-id", id)
            .header("e2b-sandbox-port", "49983")
            .header("x-access-token", token)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert!(!forbidden.status().is_success());
        assert!(gs.requests.lock().unwrap().is_empty());
        let response = client
            .get(format!("{ca}/e2b/stream"))
            .header("e2b-sandbox-id", id)
            .header("e2b-sandbox-port", "49983")
            .header("x-access-token", token)
            .header("authorization", "Basic cm9vdDo=")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mut stream = response.bytes_stream();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "first\n"
        );
        gs.release_response.notify_one();
        assert_eq!(stream.next().await.unwrap().unwrap(), "second\n");
        assert!(stream.next().await.is_none());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let upload = stream::once(async { Ok::<_, std::io::Error>(vec![1u8; BODY + 1]) }).chain(
            stream::once(async move {
                rx.await.unwrap();
                Ok(vec![2u8; 10])
            }),
        );
        let pending = client
            .post(format!("{ca}/e2b/upload?path=%2Ftmp%2Ffile"))
            .header("e2b-sandbox-id", id)
            .header("e2b-sandbox-port", "49983")
            .header("x-access-token", token)
            .body(reqwest::Body::wrap_stream(upload));
        let pending = tokio::spawn(async move { pending.send().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), gs.upload_started.notified())
            .await
            .unwrap();
        tx.send(()).unwrap();
        let response = pending.await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().len(), BODY + 11);
        {
            let seen = gs.requests.lock().unwrap();
            assert_eq!(seen[0].1["authorization"], "Basic cm9vdDo=");
            for (_, headers) in seen.iter() {
                assert!(!headers.contains_key("x-api-key"));
                assert!(!headers.contains_key("open-sandbox-api-key"));
                assert_ne!(
                    headers.get("x-access-token").and_then(|v| v.to_str().ok()),
                    Some(KEY)
                );
            }
        }
        // Same executor and shared queue also continue accepting OpenSandbox.
        let open = client
            .post(format!("{ca}/v1/sandboxes"))
            .header("open-sandbox-api-key", KEY)
            .header("content-type", "application/json")
            .body(self::payload())
            .send()
            .await
            .unwrap();
        assert_eq!(open.status(), 202);
        let open: Value = open.json().await.unwrap();
        assert_eq!(b.daemon.capacity_usage().await.unwrap().0, 3);
        for item in created {
            let response = client
                .delete(format!(
                    "{aa}/sandboxes/{}",
                    item["sandboxID"].as_str().unwrap()
                ))
                .header("x-api-key", KEY)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 204);
        }
        assert_eq!(
            client
                .delete(format!(
                    "{ba}/v1/sandboxes/{}",
                    open["id"].as_str().unwrap()
                ))
                .header("open-sandbox-api-key", KEY)
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(b.daemon.capacity_usage().await.unwrap().0, 0);
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
        c.shutdown().await.unwrap();
        aserver.abort();
        bserver.abort();
        cserver.abort();
        guest_server.abort();
    }

    #[tokio::test]
    async fn remote_http_proxy_global_id_scoped_auth_and_streaming() {
        // Three real HTTP listeners: ingress cluster, remote owner, guest service.
        // The owner also runs a real TLS ActorSystem for discovery/lifecycle RPC.
        let guest_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let guest_url = format!("http://{}", guest_listener.local_addr().unwrap());
        let guest_state = Arc::new(Guest::default());
        let guest_app = Router::new()
            .fallback(guest)
            .with_state(guest_state.clone());
        let guest_server =
            tokio::spawn(async move { axum::serve(guest_listener, guest_app).await.unwrap() });
        let al = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let aa = al.local_addr().unwrap().to_string();
        let ba = bl.local_addr().unwrap().to_string();
        let (a, _, _ad) = node_at(vec![], vec!["other".into()], aa.clone()).await;
        let (b, runtime, _bd) =
            node_at(vec![a.system.addr()], vec!["fixture".into()], ba.clone()).await;
        *runtime.endpoint_url.lock().unwrap() = Some(guest_url);
        converge(&a, &b).await;
        let seen = Arc::new(Mutex::new(Vec::<(String, HeaderMap)>::new()));
        let capture = seen.clone();
        let owner_app = b.router().layer(axum::middleware::from_fn(
            move |request: Request, next: axum::middleware::Next| {
                let capture = capture.clone();
                async move {
                    capture
                        .lock()
                        .unwrap()
                        .push((request.uri().to_string(), request.headers().clone()));
                    next.run(request).await
                }
            },
        ));
        let ingress_app = a.router();
        let a_server = tokio::spawn(async move { axum::serve(al, ingress_app).await.unwrap() });
        let b_server = tokio::spawn(async move { axum::serve(bl, owner_app).await.unwrap() });
        let client = a.http.clone();
        let created = client
            .post(format!("http://{aa}/v1/sandboxes"))
            .header("open-sandbox-api-key", KEY)
            .header("content-type", "application/json")
            .body(payload())
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::ACCEPTED);
        let created: Value = created.json().await.unwrap();
        let id = created["id"].as_str().unwrap();
        let (_, local_id) = split_id(id).unwrap();
        let base = format!("http://{aa}/v1/sandboxes/{id}");
        let endpoint: Value = client
            .get(format!("{base}/endpoints/44772?use_server_proxy=true"))
            .header("open-sandbox-api-key", KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            endpoint["endpoint"],
            format!("{ba}/v1/sandboxes/{local_id}/proxy/44772")
        );
        assert!(endpoint.get("headers").is_none());

        // Global-ID server-proxy path, not just the direct endpoint response.
        let response = client
            .get(format!("{base}/proxy/44772/stream?q=1&q=2"))
            .header("open-sandbox-api-key", KEY)
            .header("connection", "x-client-hop")
            .header("x-client-hop", "remove")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert!(!response.headers().contains_key("x-hop-secret"));
        let mut chunks = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(3), chunks.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&first[..], b"first\n");
        assert_eq!(a.proxies.available_permits(), 127);
        assert_eq!(b.daemon.proxy_slots.available_permits(), 127);
        // The second chunk cannot exist until the first has reached this client.
        guest_state.release_response.notify_one();
        let second = tokio::time::timeout(Duration::from_secs(3), chunks.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&second[..], b"second\n");
        assert!(chunks.next().await.is_none());

        let scoped: Value = client
            .get(format!("{base}/endpoints/44772"))
            .header("open-sandbox-api-key", KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let token = scoped["headers"]["X-PVISOR-SANDBOX-TOKEN"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(scoped["endpoint"], endpoint["endpoint"]);
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(1);
        let upload = stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|chunk| (chunk, rx))
        });
        let upload_request = client
            .post(format!("{base}/proxy/44772/upload"))
            .header("x-pvisor-sandbox-token", &token)
            .body(reqwest::Body::wrap_stream(upload));
        let task = tokio::spawn(async move { upload_request.send().await.unwrap() });
        tx.send(Ok(b"upload-one".to_vec())).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            guest_state.upload_started.notified(),
        )
        .await
        .unwrap();
        // Proves request streaming: guest receives data before upload EOF.
        tx.send(Ok(b"upload-two".to_vec())).await.unwrap();
        drop(tx);
        let response = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            &response.bytes().await.unwrap()[..],
            b"upload-oneupload-two"
        );
        {
            let seen = seen.lock().unwrap();
            let (path, headers) = seen.iter().find(|(p, _)| p.ends_with("/upload")).unwrap();
            assert_eq!(
                path,
                &format!("/v1/sandboxes/{local_id}/proxy/44772/upload")
            );
            assert_eq!(headers["x-pvisor-sandbox-token"], token);
            assert!(
                !headers.contains_key("open-sandbox-api-key"),
                "scoped access must never be escalated"
            );
            assert!(!seen[0].1.contains_key("x-client-hop"));
        }
        {
            let guest_requests = guest_state.requests.lock().unwrap();
            assert_eq!(guest_requests[0].0, "/stream?q=1&q=2");
            for (_, headers) in guest_requests.iter() {
                assert!(!headers.contains_key("open-sandbox-api-key"));
                assert!(!headers.contains_key("x-pvisor-sandbox-token"));
            }
        }
        for suffix in [
            "name%20with%20spaces",
            "caf%C3%A9",
            "a%2Fb",
            "100%25.txt",
            "name%23part",
        ] {
            let escaped = format!("/escaped/{suffix}?q=a%2Fb&q=c%20d");
            let response = client
                .get(format!("{base}/proxy/44772{escaped}"))
                .header("x-pvisor-sandbox-token", &token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{escaped}");
            assert_eq!(response.text().await.unwrap(), escaped);
        }
        let before = guest_state.requests.lock().unwrap().len();
        for bad_token in [None, Some("wrong-token")] {
            let mut request = client.get(format!("{base}/proxy/44772/stream"));
            if let Some(token) = bad_token {
                request = request.header("x-pvisor-sandbox-token", token);
            }
            assert_eq!(
                request.send().await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            client
                .get(&base)
                .header("x-pvisor-sandbox-token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{base}/proxy/44772/stream"))
                .header("x-pvisor-sandbox-token", &token)
                .header("x-pvisor-sandbox-token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let (_, other) = http(&a, "POST", "/v1/sandboxes", payload(), true).await;
        let other = other["id"].as_str().unwrap();
        assert_eq!(
            client
                .get(format!(
                    "http://{aa}/v1/sandboxes/{other}/proxy/44772/stream"
                ))
                .header("x-pvisor-sandbox-token", &token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(guest_state.requests.lock().unwrap().len(), before);
        let response = client
            .post(format!("{base}/proxy/44772/redirect"))
            .header("x-pvisor-sandbox-token", &token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(guest_state.requests.lock().unwrap().len(), before + 1);
        // Fail before sending any credential to a non-loopback destination.
        let error = a
            .proxy(
                Request::builder()
                    .uri("/v1/sandboxes/local/proxy/44772")
                    .body(Body::empty())
                    .unwrap(),
                "192.0.2.1:8080",
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "INSECURE_PEER_HTTP_DISABLED");
        a_server.abort();
        b_server.abort();
        guest_server.abort();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }
    #[test]
    fn plaintext_peer_http_is_fail_closed_without_opt_in() {
        for host in ["127.0.0.1:8080", "[::1]:8080", "[::ffff:127.0.0.1]:8080"] {
            assert!(
                check_peer_http(&url::Url::parse(&format!("http://{host}/")).unwrap(), false)
                    .is_ok()
            );
        }
        for host in [
            "localhost:8080",
            "192.0.2.1:8080",
            "[2001:db8::1]:8080",
            "peer.example:8080",
        ] {
            let url = url::Url::parse(&format!("http://{host}/")).unwrap();
            assert!(check_peer_http(&url, false).is_err());
            assert!(check_peer_http(&url, true).is_ok());
        }
    }
    #[test]
    fn strict_paths() {
        let id = format!("sb-{}", uuid::Uuid::new_v4());
        assert!(split_id(&format!("{}.{}", "a".repeat(64), id)).is_ok());
        for suffix in [
            "../admin",
            "%2e%2e/admin",
            "a%2f..%2fb",
            "a\\b",
            "a%5Cb",
            "a%00b",
            "a%0Db",
            "bad%zz",
            "bad%",
        ] {
            assert!(
                validate_proxy_path(&format!("/v1/sandboxes/{id}/proxy/44772/{suffix}")).is_err()
            );
        }
    }
}
