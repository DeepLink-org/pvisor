# pVisor daemon

A **single-node sandbox manager**, not a distributed scheduler. The default
`pvisor-daemon` executable owns sandbox admission, persistent intentions, native
lifecycle, expiration and API access. It has no Controller/Worker registration,
placement, business DAG, distributed lease or remote completion outbox.

## Protocol baseline

Compatibility is pinned to **OpenSandbox 1.1.0**, tag `release-1.1.0`, commit
`b1a29cf93a823a95913f7943010febb3f29de05c`. See
[`opensandbox.lock.json`](opensandbox.lock.json). This is a **partial API profile**,
not a claim of complete OpenSandbox or unmodified SDK end-to-end conformance.
Upgrades require explicit review of schemas, SDK behavior and conventional tests;
never follow upstream `main` or an image's `latest` tag automatically.

| Interface | Support |
| --- | --- |
| `POST /v1/sandboxes` | Image-based creation, argv, env, metadata, CPU/memory limits, optional TTL; 202 JSON |
| `GET /v1/sandboxes` | Repeated state filters, SDK-encoded metadata, page/pageSize |
| `GET /v1/sandboxes/{id}` | Reconcile local native state; 200 JSON |
| `DELETE /v1/sandboxes/{id}` | Confirm native deletion before releasing reservation; 204 |
| `POST /v1/sandboxes/{id}/pause` and `/resume` | Acknowledged live vCPU pause/resume on the same Attempt; 202 empty body |
| `POST /v1/sandboxes/{id}/renew-expiration` | Future RFC3339 deadline, must extend existing TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | Actual execd (44772) and egress (18080) publication |
| Command, file, health and metrics data plane | Stream to the prepared image's real upstream service; not reimplemented |
| Snapshots, templates/pools, metadata mutation, lifecycle hooks | Unsupported; rejected, not simulated |
| Network policy, credential proxy, secure access, volumes, image auth | Unsupported creation options are rejected |
| Arbitrary application ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| pVisor stage/apply, checkpoint/fork, offload | Not wired into this daemon backend |

Lifecycle authentication is `OPEN-SANDBOX-API-KEY`. Successful and failed API
responses receive `X-Request-ID`. Errors use `{code, message}`. Supported create
resources are exactly `cpu` and `memory`; unsupported quantities/controls fail
before admission. Timeout omission/null means manual cleanup; a configured TTL
is at least 60 seconds. The default maximum is one day, with a configuration
ceiling of one year.

Both endpoint modes return a daemon-routed authority without a URL scheme:

- `use_server_proxy=true`: callers authenticate with the lifecycle API key.
- Default mode: endpoint `headers` contains a random, sandbox-scoped
  `X-PVISOR-SANDBOX-TOKEN`. SDKs must preserve endpoint-provided headers.

This does not expose a supervisor's loopback mapping as a public endpoint. A
sandbox token does not authorize lifecycle operations or another sandbox.
Control credentials are removed before forwarding to the guest. Guest execd
access tokens, if supplied by the caller, are not replaced by the lifecycle key.
Signed endpoint expiration is unsupported and explicitly rejected.

## Runtime boundary: native VM and prepared images

Native `pvisor` forwards `pvisor service daemon ...` arguments unchanged to the
separately installed `pvisor-daemon`; it does not depend on this crate. The
VM-only `NativeRuntime` in `runtime.rs` embeds `pvisor::PVisor` and `VmExecutor`
in a detached `native-supervisor` subprocess that holds the RunHandle and survives
daemon restart. Execution starts through pVisor's `RuntimeJobService`; live
status/pause/resume/termination dispatch through the shared in-process
`AttemptService`. The supervisor disables pVisor's default HostVm endpoint with
`instance_control(false)`, retaining only its authenticated `control.sock` rather
than two management endpoints for one VM. It still owns readiness, boot/generation
identity, cgroup reconciliation, deletion fencing and confirmed teardown. Shared
termination only requests cancellation; it cannot release a reservation or replace
cgroup absence proof. Cargo links `pvisor` and
`pvisor-core`; synchronous `main` calls `pvisor::run_krun_internal_if_requested()`
before argument parsing or Tokio, then dispatches the hidden
`native-supervisor --sandbox-dir ABSOLUTE_PATH` command. `serve` constructs
`NativeRuntime` with the required `--images-dir` and `--cgroup-root` flags.

The only runtime backend is **native VM execution**, with no host, OCI command or
registry-pull fallback. It requires Linux x86_64, usable `/dev/kvm`, trusted
absolute runtime paths and a writable delegated cgroup v2 hierarchy with enabled
CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller
writes and the KVM API. Each sandbox cgroup caps the entire supervisor/VMM/helper
process tree: 10–8000 millicores of aggregate CPU rate, hard memory, zero swap,
`pids.max=512` and group OOM. vCPU count is rounded up from the CPU quota (up to
8), not equivalent to quota; guest RAM is rounded down to MiB and the enclosing
memory cap also includes host-side overhead. Full controls/readiness are rechecked
at create, Inspect and resume, not on each data-request endpoint lookup. Admission
sums hard limits conservatively,
including paused/failed/uncertain records; this is not a measured whole-node
physical-memory budget.

The launch callback joins the identity-bound cgroup **before exec**, using a pre-opened `cgroup.procs` FD under the owner lock and checking started/deletion/tombstone markers. Supervisor startup verifies membership instead of moving an already-running Tokio process; supervisor/Tokio allocations and subsequent VM/helper children are charged inside the sandbox budget. Direct hidden-command invocation outside that cgroup fails closed.

Images are trusted local `images_dir/<key>.json` manifests, not registry
references. Fields are `rootfs` (absolute independently provisioned Linux tree,
never host `/` or overlapping daemon state), `entrypoint` (absolute guest
bootstrap argv), optional `cmd`, optional `env`, and optional absolute
`library_dir` for firmware. The bootstrap receives requested workload argv, or
`cmd` if empty, appended to `entrypoint`; request env overrides manifest env,
without inheriting host env or shell interpolation. The immutable rootfs is
served through the native VM path with private writes.

The bootstrap must supervise real OpenSandbox **1.1.0 execd**, egress and the
workload, initialize/authenticate services, forward signals and reap children.
It must provide byte-transparent guest AF_VSOCK listeners on **CID 3**, ports
**44772/18080**, bridged to those real services. Supervisor loopback TCP
publications connect through private Unix sockets and native vsock forwarding.
A stock distribution rootfs or a sleeping process does not satisfy this contract.

Create, Inspect of a Running VM and resume readiness checks verify real execd
`/ping`, `/ready` (JSON `initialized: true`) and egress `/healthz` through the
bridges, requiring HTTP
200 and bounded bodies. Readiness is not manufactured. The daemon does not inject
`/execd`, call its initialization handshake or synthesize command/SSE/file behavior. The prepared image must complete
any runtime initialization and configure its service authentication.

Endpoint lookup authenticates the live supervisor, requires the current RunHandle
Running state, checks deletion fences and resolves a live publication. It does
not repeat service health probes or full cgroup-limit reconciliation per data
request; connection failures belong to the API adapter. Native `observation.json`
caches are neither written nor used as liveness proof.

**The guest bootstrap and image recipe are not supplied or end-to-end validated.**
An upstream image name alone is not a native bootstrap or vsock adapter.
Python SDK initialization resolves both endpoints even without a network policy.
There is no full SDK-conformance or density evidence.

Native OverlayNet supplies outbound VM networking; requested OpenSandbox network
policies remain unsupported and rejected. Do not infer deny-all egress from a VM
label or health checks. Trust the host account, daemon/firmware and prepared image;
private state and same-UID IPC do not defend against hostile host-UID/root code.
Loopback publications may be reachable by other local users: use real service
authentication and host controls. Workload secrets stay out of supervisor argv
and host environment, but persist in private records accessible to trusted owners.
No security audit or hostile multi-user assurance is claimed.

## State, concurrency and failure behavior

- One daemon exclusively locks a private state directory. Owner identity and
  sandbox records are atomically persisted before native creation/control.
- Runtime operations use core `AgentCtlHostRequest`/`AgentCtlHostResponse` version-1
  envelopes over newline-delimited JSON (maximum 1 MiB payload, no length prefix).
  Framing and same-effective-UID peer authentication reuse pVisor's public
  `host_transport` implementation, including bounded encoding and sync/async frame
  interoperability; envelope and owner/token/generation validation remain explicit
  supervisor responsibilities. Responses must correlate the request ID and echo
  the authenticated owner/target.
  Private IPC authenticates same-UID peer credentials, namespace owner, sandbox Job
  ID, explicit Attempt ID, generation and secret token. Missing generation or
  Attempt cannot resolve to the current instance. The token remains private and
  separate from host API, lifecycle and guest credentials; cooperative guest
  AgentCtl never grants supervisor authority. Durable identity binds the boot
  ID and cgroup device/inode. Sandbox IDs are never reused or relaunched; lost
  IPC is uncertainty, not Missing or proof of cleanup.
- Runtime pause/resume return the confirmed live `RuntimeState` from the
  authenticated native acknowledgement, not command acceptance. The daemon keeps
  its pre-control inspection and durable Pausing/Resuming intention, validates the
  returned state and commits it without another inspection. Native state, cgroup
  limit, deletion-fence and resume service-readiness checks remain in the supervisor.
  Failed/lost acknowledgements retain the intention and reservation for reconciliation;
  they do not prove the control was unapplied. A control retry that inspects the
  desired live state commits that observation before succeeding, without requiring
  a GET. Pending deletion, expiration and uncertain storage still fence control.
- Delete persists monotonic intent and uses the supervisor's exclusive lock to
  fence late launch. Cleanup uses identity-bound `cgroup.kill`, never a persisted
  PID or PID-based kill, and confirms an empty cgroup plus released owner lock
  before allowing reservation release. Replaced/missing same-boot cgroups do not
  authorize fabricated absence without durable tombstone proof.
- Cleanup does not depend on the original rootfs/firmware or launch validation.
  After absence proof and exclusive ownership, it publishes `tombstone.json`,
  removes the empty owned cgroup and reclaims private run/RAM/temp storage,
  sockets and secret-bearing records. The tombstone, owner lock and lifecycle
  markers remain as an ID-reuse fence; interrupted reclamation retries from this
  proof, and errors retain reservations.
- HTTP disconnects do not cancel accepted lifecycle operations.
- Different sandboxes have independent lifecycle locks. There is no global runtime
  mutex spanning slow creation or readiness checks.
- The registry is bounded by configured capacity and an aggregate 16 MiB logical
  v1-equivalent budget, excluding repeated owner envelopes. Mutations persist
  only the target record, not a whole-registry checkpoint; performance has not
  been benchmarked.
- Deletion intent survives restart. A native state observation cannot overwrite
  a pending delete. Unknown deletion retains its record and reservation.
- Expiration is rechecked under the same lock as renewal, preventing stale scans
  from deleting a sandbox whose TTL was extended. Cleanup errors remain pending.
- Execd requests and responses stream (including multipart and SSE). Redirects,
  environment HTTP proxies and automatic decompression are disabled. Repeated
  response headers/query are preserved; hop-by-hop and control secret headers
  are removed. WebSocket/CONNECT are rejected.
- Proxies take shared admission on the sandbox lifecycle gate; lifecycle operations
  take exclusive access. Concurrent proxies do not serialize, and a queued lifecycle
  writer prevents new proxies from entering. Admission retains the port-reuse fence
  through endpoint resolution, upload and upstream headers, capped together at
  120 seconds. Reqwest does not expose a separate connection establishment boundary,
  so lifecycle operations can still wait up to the remaining admission timeout for
  already-admitted proxies; shared admission does not preempt them. Established
  response streams hold no admission and have no total timeout. Idle upstream connection pooling is disabled to avoid
  reusing connections across recycled loopback ports. Host-side external runtime
  manipulation is outside this coordination boundary.
- TTL cleanup is best effort, not a hard execution deadline: native commands,
  uploads/header waits and same-sandbox controls can delay it. New proxy requests
  are rejected after expiration. Stream/output quotas remain upstream responsibilities.
- Normal daemon shutdown leaves owned native sandboxes and records for restart.
  Maintenance resumes after restart. It does not guarantee cleanup while the
  daemon is down; configure native host supervision for that requirement.

### Incremental registry and migration

The v2 `sandboxes.json` is a version/owner header with an empty `sandboxes` map,
not an empty inventory. Private `records/` (0700) contains `meta.json` with version
2 and the same owner, plus one owner-wrapped `sb-<uuid>.json` file (0600) per
sandbox. Opening validates header/metadata/record ownership, IDs, resources and
credentials; activated v2 records cannot fall back to a v1 snapshot on corruption.
A missing header with an existing records tree is rejected.

Each mutation clones/serializes only its target record. Replacement writes and
fsyncs a private `.record-*` temporary, atomically renames it over the target and
fsyncs `records/`; deletion unlinks the target and fsyncs the directory. Ordinary
mutations do not rewrite the root header or metadata. A separate commit lock
serializes admission decisions and mutations; the registry reader lock is
released during blocking-pool disk I/O, exposing the last committed inventory.
The in-memory target updates only after commit success. An uncertain rename/unlink
or subsequent fsync error retains the existing fail-stop storage latch: do not
continue from stale memory; preserve state, repair storage, restart and reconcile.
The aggregate budget counts one header/owner, sandbox keys and record contents as
a v1-equivalent logical snapshot, not physical directory bytes, so a valid v1
snapshot at the limit remains migratable.

V1 `sandboxes.json` remains authoritative until activation. `Daemon::open` first
holds the exclusive store lock and asks the runtime factory to accept the owner;
only then does Store initialize/migrate. It writes/fsyncs metadata and records in
private `.records-*` staging, syncs the directory, renames it to `records/` and
syncs the state directory, then atomically replaces/fsyncs the root v2 header.
That header activates the committed records tree; native preflight follows.
Interrupted, unactivated copies are disposable, not recovery prerequisites:
staging and `.records-retired-*` directories are reclaimed only after validating
private ownership, reserved names and regular-file types without following
symlinks or requiring incomplete payloads to deserialize. An uncertain activation
fails the open; the next open uses the surviving header.

The supervisor accepts newline-delimited version-1 Host envelopes, not private
length-prefixed `Request`/`Reply` frames. There is no legacy framing fallback.
Before upgrading a deployment that uses length-prefixed frames, delete its sandboxes
through that deployment's daemon and confirm cleanup; do not erase identity/registry
records to bypass uncertainty. Wire compatibility is separate from durable identity
and runtime state compatibility; their persisted representations remain unchanged.

Keep the same state directory and compatible runtime configuration across restarts.
Never delete state to fix an error: it carries native ownership and unresolved
reservations. Missing sandboxes remain visible as Failed until explicitly deleted.
Failed native creation with verified cleanup releases its reservation; uncertain
creation retains its ID in the error message so callers can inspect/delete it.

An occupied version-1 `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

Any v2 header without the native `owner.json` marker is also rejected, even with
its empty map. Preserve the original header, native marker and records together;
do not erase reservations or fabricate ownership to bypass the guard.

The list API reports the last durable observation; GET reconciles native state.
The maintenance loop retries pending/expired deletion. Continuous native process
monitoring and a density-optimized event-driven reconciliation path are future work.

## Run

This is a deployment example, not a validated
bootstrap/image or end-to-end SDK recipe. Supply a real delegated cgroup
hierarchy, not an ordinary filesystem directory:

```sh
pvisor-daemon serve \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

Keep state paths short and absolute: per-sandbox `control.sock` must be shorter
than 104 bytes, and vsock Unix socket paths also have length limits. Preserve the
state across daemon restart; `/run/user/1000/pvd` does not promise persistence
across logout/reboot, and a VM cannot survive host reboot.

Set `OPEN_SANDBOX_API_KEY` to a strong random secret (at least 32 bytes); avoid
passing it in argv. Use TLS in a trusted reverse proxy before exposing the service
outside loopback. Supply `--public-endpoint` for externally routed or wildcard
listeners. A `--listen ...:0` development binding also requires an explicitly
correct public endpoint; automatic endpoint publication is not implemented.

`pvisor-daemon protocol` prints the pinned baseline. Connect SDK version 1.1.0
with the chosen domain and protocol, and use a prepared image satisfying the
contract above. Invoke `pvisor-daemon` directly or use the native CLI's passthrough,
`pvisor service daemon serve ...`; both address the same separately installed
daemon executable.

## Optional daemon-owned memory pool

Add `--memory-pool` to `pvisor-daemon serve` to enable the shared cold-page
pool. It is off by default. The daemon starts its own `memory-pool` component
in a private state subdirectory and records its socket in each new sandbox
identity. VM RAM remains private; on Linux, independently reclaimable 4 KiB pages are compressed into
content-addressed pool objects, and identical chunks reuse one object. A
userfaultfd miss restores bytes into the requesting VM's private RAM.

The detached pool component survives API-process restart and is reused only
with the same configuration. Connections authenticate the host UID; individual
connection references are released on disconnect. Default limits are 512 MiB
of encoded payload, 32,768 objects, 32 connections and 32,768 references per
connection. Indexes, threads and allocator memory are additional overhead.
Linux requires the existing kernel-fault userfaultfd permission.

Reserve host memory for the pool separately from sandbox admission: its process
is outside individual sandbox cgroups and their hard limits. Startup preserves sparse guest RAM. First-touch faults allocate one 4 KiB
zero page; the scanner samples resident pages without filling holes. A fault restores only the requested page; adjacent cold pages remain in the pool.
Budget for the actual working set and restoration peaks.
The pool must stay alive while dependent VMs run; loss fails those VMs, and a
stale socket is not silently replaced. State-directory retention supports API
restart, not pool-process or host-reboot recovery. The VM/pool benchmark includes
the pool component in one group; it does not establish full API/SDK conformance
or production density.

## Package and evidence scope

The Cargo package/directory/library are `pvisor-daemon` / `pvisor_daemon`.
The library exports `daemon` and `runtime` for single-node sandbox management;
distributed scheduling belongs to external orchestrators.

Cluster/controller measurements are not evidence for this node-local daemon's
performance, admission behavior or resource density.

## Validation

Conventional tests include fake-runtime lifecycle/admission/restart/TTL regressions,
HTTP auth/schema/filter/endpoint tests and pure native identity/IPC/manifest,
quota, readiness and cleanup regressions. Fake runtime tests are not evidence of native isolation, resource density or full SDK
compatibility.

Run targeted checks with:

```sh
just test pvisor-daemon
```

These conventional tests do not launch native sandboxes or establish prepared-image,
SDK, isolation or density validation.

## Implementation ownership

| Module | Responsibility |
| --- | --- |
| `daemon/models.rs` | Fixed wire models, resource quantities, unsupported feature rejection |
| `daemon/store.rs` | Private per-record registry, atomic publication/unlink, logical budget and v1 activation |
| `daemon/mod.rs` | Admission/commit serialization, lifecycle, restart reconciliation, TTL and endpoint access |
| `daemon/api.rs` | OpenSandbox HTTP adapter, filters, streaming data-plane proxy |
| `runtime.rs` | Runtime trait, VM-only NativeRuntime, detached supervisor, IPC/cgroup identity and vsock bridges |
| `memory_pool.rs` | Daemon-owned bounded cold-page pool, private socket and restart reuse |
| `main.rs` | Daemon CLI/configuration, listener and maintenance lifecycle |

