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
daemon restart. Executable integration is implemented: Cargo links `pvisor` and
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
memory cap also includes host-side overhead. Controls are rechecked during
lifecycle/endpoint observations. Admission sums hard limits conservatively,
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

Creation and running observations verify real execd `/ping`, `/ready` (JSON
`initialized: true`) and egress `/healthz` through the bridges, requiring HTTP
200 and bounded bodies. Readiness is not manufactured. The daemon does not inject
`/execd`, call its initialization handshake or synthesize command/SSE/file behavior. The prepared image must complete
any runtime initialization and configure its service authentication.

**The guest bootstrap and image recipe are not supplied or end-to-end validated.**
The old container `cap-drop=ALL` constraint does not describe this VM backend;
an upstream image name alone is not a native bootstrap or vsock adapter.
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
- Runtime operations authenticate private IPC using same-UID peer credentials,
  owner, sandbox ID, generation and secret token. Durable identity binds the boot
  ID and cgroup device/inode. Sandbox IDs are never reused or relaunched; lost
  IPC is uncertainty, not Missing or proof of cleanup.
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
- The registry is bounded by configured capacity and 16 MiB, rather than an
  unbounded retained distributed task history. Mutations currently checkpoint
  the bounded registry; this cost has not been benchmarked.
- Deletion intent survives restart. A native state observation cannot overwrite
  a pending delete. Unknown deletion retains its record and reservation.
- Expiration is rechecked under the same lock as renewal, preventing stale scans
  from deleting a sandbox whose TTL was extended. Cleanup errors remain pending.
- Execd requests and responses stream (including multipart and SSE). Redirects,
  environment HTTP proxies and automatic decompression are disabled. Repeated
  response headers/query are preserved; hop-by-hop and control secret headers
  are removed. WebSocket/CONNECT are rejected.
- Endpoint resolution and connection establishment share the sandbox lifecycle
  lock, preventing daemon-managed delete/port reuse during establishment. Upload
  plus upstream-header wait is capped at 120 seconds; established response streams
  have no total timeout. Host-side external runtime manipulation is outside this
  coordination boundary.
- TTL cleanup is best effort, not a hard execution deadline: native commands,
  uploads/header waits and same-sandbox controls can delay it. New proxy requests
  are rejected after expiration. Stream/output quotas remain upstream responsibilities.
- Normal daemon shutdown leaves owned native sandboxes and records for restart.
  Maintenance resumes after restart. It does not guarantee cleanup while the
  daemon is down; configure native host supervision for that requirement.

Keep the same state directory and compatible runtime configuration across restarts.
Never delete state to fix an error: it carries native ownership and unresolved
reservations. Missing sandboxes remain visible as Failed until explicitly deleted.
Failed native creation with verified cleanup releases its reservation; uncertain
creation retains its ID in the error message so callers can inspect/delete it.

An occupied `sandboxes.json` without the native `owner.json` marker is rejected under the existing exclusive store lock: it may still own live Podman containers. Use fresh native state while preserving and cleaning up the old deployment, or delete every sandbox through the old Podman daemon and confirm cleanup before switching backends with the emptied registry. Never erase registry entries, reservations or ownership state, or fabricate a native marker to bypass this guard. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

The list API reports the last durable observation; GET reconciles native state.
The maintenance loop retries pending/expired deletion. Continuous native process
monitoring and a density-optimized event-driven reconciliation path are future work.

## Run

The flags below are implemented. This is a deployment example, not a validated
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

## Completed migration and retired Cluster

The Cargo package/directory/library are `pvisor-daemon` / `pvisor_daemon`.
The cross-package Cluster retirement is complete: Cluster and Worker integration,
the old dependency alias, `legacy-cluster` feature and `pvisor-cluster` executable
have been removed. This library exports only `daemon` and `runtime`; there is no
legacy controller/client/scheduler/storage implementation or compatibility target.
Legacy examples, measurements and integration tests have also been deleted from
this package. The new daemon's in-module tests remain.

Historical Cluster/controller benchmarks describe the retired implementation,
not this daemon's performance, admission behavior or resource density. They are
not evidence for the node-local daemon.

## Validation

Conventional tests include fake-runtime lifecycle/admission/restart/TTL regressions,
HTTP auth/schema/filter/endpoint tests and pure native identity/IPC/manifest,
quota, readiness and cleanup regressions. Fake runtime tests are not evidence of native isolation, resource density or full SDK
compatibility. No semspec approvals/ledgers were changed.

Suggested targeted checks when execution is allowed:

```sh
just test pvisor-daemon
```

The retirement cleanup was checked statically only. No compilation, tests or
product code (including native sandboxes) were run.

## Implementation ownership

| Module | Responsibility |
| --- | --- |
| `daemon/models.rs` | Fixed wire models, resource quantities, unsupported feature rejection |
| `daemon/store.rs` | Private, exclusive durable node registry |
| `daemon/mod.rs` | Admission, lifecycle, restart reconciliation, TTL and endpoint access |
| `daemon/api.rs` | OpenSandbox HTTP adapter, filters, streaming data-plane proxy |
| `runtime.rs` | Runtime trait, VM-only NativeRuntime, detached supervisor, IPC/cgroup identity and vsock bridges |
| `main.rs` | Daemon CLI/configuration, listener and maintenance lifecycle |

