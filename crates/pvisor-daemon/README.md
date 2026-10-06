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
| `POST /v1/sandboxes/{id}/pause` and `/resume` | Native cgroup freeze/unfreeze, confirmation; 202 empty body |
| `POST /v1/sandboxes/{id}/renew-expiration` | Future RFC3339 deadline, must extend existing TTL |
| `GET /v1/sandboxes/{id}/endpoints/{port}` | Actual execd (44772) and egress (18080) publication |
| Command, file, health and metrics data plane | Stream to the prepared image's real upstream service; not reimplemented |
| Snapshots, templates/pools, metadata mutation, lifecycle hooks | Unsupported; rejected, not simulated |
| Network policy, credential proxy, secure access, volumes, image auth | Unsupported creation options are rejected |
| Arbitrary application ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| pVisor stage/apply, checkpoint/fork, VM/offload | Not wired into this daemon backend |

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

This does not expose a container's loopback mapping as a public endpoint. A
sandbox token does not authorize lifecycle operations or another sandbox.
Control credentials are removed before forwarding to the guest. Guest execd
access tokens, if supplied by the caller, are not replaced by the lifecycle key.
Signed endpoint expiration is unsupported and explicitly rejected.

## Runtime boundary: prepared images, not a replacement execution kernel

The cross-package migration is complete. Native `pvisor` forwards
`pvisor service daemon ...` arguments unchanged to the separately installed
`pvisor-daemon` executable; it does not depend on this crate. The daemon likewise
has no Rust dependency on the native executor crate, so there is no reverse
crate dependency or dependency cycle.

This package provides a `Runtime` adapter and one **external rootless Podman
backend**. It is **not** the pVisor VM executor or stage/apply path. Plain host
execution is never used as a fallback.

The backend requires Linux, a trusted **absolute** Podman executable path,
rootless operation, cgroup v2 and delegated CPU/memory/PID controllers. Startup
checks these before binding the API. Creation installs CPU/memory/swap/PID limits,
private namespaces, no-new-privileges and cap-drop=ALL; resource settings are
checked in the container configuration. Admission sums hard limits conservatively,
including paused/failed/uncertain sandboxes; observations do not authorize
implicit overcommit. This is not proof of an independently measured node-wide
physical-memory budget: daemon/helpers/cache also consume resources.

Images are **preprovisioned locally** (`--pull=never`). The image ENTRYPOINT must
supervise the requested workload argv plus real OpenSandbox **1.1.0 execd and
an egress service**, listening on container ports 44772/18080. Requests replace
the image CMD; the wrapper must execute those arguments faithfully, without shell
interpolation, forward signals and reap children. Merely running `tail` in a
stock distribution image does not provide a working SDK sandbox.

Creation verifies real execd `/ping`, `/ready` and egress `/healthz`; it does not
manufacture readiness. The daemon does not inject `/execd`, call its initialization
handshake or synthesize command/SSE/file behavior. The prepared image must complete
any runtime initialization and configure its service authentication.

**Important upstream constraint:** the pinned upstream default egress component
installs iptables redirects and cannot run unchanged under cap-drop=ALL. A genuine
capability-free deployment is required; the ordinary upstream sidecar is not
supported by this adapter. No end-to-end image recipe has been validated here.
Until that image contract is fulfilled, ordinary `Sandbox.create()` SDK readiness
will fail rather than return a pretend working sandbox. Python SDK initialization
resolves both service endpoints even when no network policy was requested.

Rootless slirp4netns disables host-loopback access but is **not deny-all egress**.
This backend does not enforce requested network policies and rejects them. It
shares the host kernel, has no VM-grade boundary, and has not been security-audited.
The host account, Podman configuration/hooks and prepared image are trusted.
Native published loopback ports may be reachable by other local users; do not
use this deployment as an untrusted multi-user isolation boundary without real
service authentication and host network controls. Workload environment secrets
are not placed in Podman argv, but are still visible to trusted host/runtime owners.

## State, concurrency and failure behavior

- One daemon exclusively locks a private state directory. Owner identity and
  sandbox records are atomically persisted before native creation/control.
- Every runtime operation validates native owner/sandbox labels. Random sandbox
  IDs are never reused; names/labels do not protect against the same host UID.
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

The list API reports the last durable observation; GET reconciles native state.
The maintenance loop retries pending/expired deletion. Continuous native process
monitoring and a density-optimized event-driven reconciliation path are future work.

## Run

This command is a deployment example, **not a tested installation recipe**:

```sh
pvisor-daemon serve \
  --podman /usr/bin/podman \
  --listen 127.0.0.1:8080 \
  --state .pvisor/daemon \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592
```

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
HTTP auth/schema/filter/endpoint tests and pure runtime parsing/argv tests. Fake
runtime tests are not evidence of native isolation, resource density or full SDK
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
| `runtime.rs` | Runtime trait and external rootless Podman lifecycle |
| `main.rs` | Daemon CLI/configuration, listener and maintenance lifecycle |

