# pVisor daemon

A **node-local service manager** for sandbox lifecycle, optional image caching
and a shared memory pool. The `pvisor-daemon` executable owns component startup
and shutdown, sandbox admission, persistent intentions, native lifecycle,
expiration and API access. Distributed placement and scheduling belong to external
orchestrators.

## Composite service

`pvisor-daemon serve` owns the selected service components. Cache is opt-in:

```sh
# Portable cache service only; no sandbox API key, KVM or cgroup required.
pvisor-daemon serve --cache --cache-listen unix:///tmp/pvisor-cache.sock
# Shared memory pool only (Apple Silicon macOS or Linux); no sandbox API key.
pvisor-daemon serve --memory-pool --state /tmp/pvisor-pool
# Sandbox and cache together (OPEN_SANDBOX_API_KEY must be set).
pvisor-daemon serve --sandbox --images-dir /srv/pvi --cgroup-root /sys/fs/cgroup/pvd --cache
```

`--cache-image-store` overrides OCI staging. `--memory-pool` can run independently
or alongside cache/sandbox serving. `--state` belongs to sandbox or memory-pool
serving. Select sandbox serving explicitly
with `--sandbox`; sandbox-specific options require that selection. With no component
selected, startup fails. Cache-specific options require `--cache`. Cache and sandbox
retain separate endpoints, storage and authentication; the sandbox API key does not
authorize cache requests.

The daemon prepares sandbox resources first, then waits up to ten seconds for the
cache worker's post-bind readiness acknowledgement before serving sandbox requests.
A cache startup failure aborts startup; an unexpected component exit fails the
daemon. SIGINT/SIGTERM stops API admission, allows up to ten seconds for HTTP
requests to drain, then terminates/reaps the owned cache worker. Cache requests
in flight may fail; clients must reconcile uncertain preparation results. The
worker is an internal implementation component, not a public service command.
An owner pipe stops it even if the daemon crashes. A stale Unix socket can remain;
the existing lock and socket checks allow safe restart without deleting cache data.
Detached native VM supervisors and the optional shared pool retain their existing
restart contracts; stopping the service manager does not delete live sandboxes.

## Protocol baseline

Compatibility is pinned to **OpenSandbox 1.1.0**, tag `release-1.1.0`, commit
`b1a29cf93a823a95913f7943010febb3f29de05c`. See
[`opensandbox.lock.json`](opensandbox.lock.json). This is a **partial API profile**.
The pinned, unmodified Python SDK runs against real native VMs for the core
lifecycle, command and file profile described below; full API conformance is not claimed.
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
| Command, file, health and metrics data plane | Native guest engine with host wire adapters by default; explicit upstream templates retain byte-transparent proxying |
| Snapshots, templates/pools, metadata mutation, lifecycle hooks | Unsupported; rejected, not simulated |
| Network policy, credential proxy, secure access, volumes, image auth | Unsupported creation options are rejected |
| Arbitrary application ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| pVisor stage/apply, checkpoint/fork, offload | Not wired into this daemon backend |

Lifecycle authentication is `OPEN-SANDBOX-API-KEY`. Successful and failed API
responses receive `X-Request-ID`. HTTP/1 error responses close the connection
explicitly, so early rejected request bodies cannot race with a reused connection. Errors use `{code, message}`. Supported create
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
Otherwise the proxy supplies a private per-sandbox execd token. The managed
runtime retains it in reserved `PVISOR_EXECD_ACCESS_TOKEN`, which the daemon
overwrites before admission and persists across restart. It is separate from
lifecycle, endpoint and supervisor tokens. For managed images the runtime removes
the raw token from guest launch inputs and uses official `/internal/init` to bind
its SHA-256 hash, sandbox ID and generation. Readiness verifies that binding and
both authenticated context-inventory access and unauthenticated rejection. Public proxies
reject runtime initialization, including percent-encoded paths.
Managed loopback mappings also require the private execd token before forwarding
any HTTP request. Their guest-side transport is a private Unix/vsock socket, so
connecting directly to a loopback port cannot bypass API authentication or reset
the runtime binding. Request/response bodies, multipart and SSE still stream.
Signed endpoint expiration is unsupported and explicitly rejected.

## Native guest execution (default for new templates)

`prepare-image` without `--execd`/`--envd` prepares a **native guest engine**
template. Add `--e2b` for the pinned E2B JSON/Connect profile:

```sh
pvisor-daemon prepare-image --name ubuntu --image ubuntu:24.04 --images-dir /srv/pvi
pvisor-daemon prepare-image --name e2b-base --image ubuntu:24.04 --e2b --images-dir /srv/pvi
```

Prefer digest-pinned bases. Neither command injects execd, envd or a second guest
supervisor. Both keep the shared compressed pool, per-VM private writes and
confirmed cleanup. Existing manifests keep their selected backend; preparation
publishes `native_guest: true` atomically and does not convert running sandboxes.
No automatic fallback to upstream services or host execution occurs.

The detached supervisor translates OpenSandbox HTTP/SSE or E2B JSON/Connect into
bounded, host-CID-only guest control frames on vsock 1025. `pvisor-guest`'s existing
launcher, process-group registry and single PID-1 reaper own every process. The
main entrypoint runs directly; its exit terminates the sandbox. Guest readiness
checks the actual sandbox/generation binding before publication; credentials
remain in the host supervisor and never enter guest workload environment. API
restart retains the same supervisor, VM, process inventory and output journals.
Supervisor loss is not recoverable process replay or a checkpoint.

The native core supports foreground/background commands, separate stdout/stderr,
stdin/EOF, nonzero exits, env/cwd/user overrides, process listing/reconnect,
TERM/KILL, E2B PTYs and resizing. OpenSandbox background logs use byte cursors.
Completion closes the stream after the final exit event without a fixed sleep.
Files are streamed through guest-local operations: read/write, stat, immediate
listing, mkdir, move, remove and chmod. Metrics sample actual guest CPU ticks and
memory. Files use root identity; non-root file requests, ownership changes,
xattrs, gzip uploads, recursive listing, watchers, notebook/session kernels and
OpenSandbox WebSocket PTYs are explicitly unsupported. The E2B adapter accepts
JSON/Connect (the pinned Python SDK's format), not binary protobuf. Unsupported
operations return 501; supported operations still return guest filesystem errors.

Bounds per sandbox: 64 HTTP response streams, 16 active managed execs, 64 retained jobs, 4 MiB output or
16,384 output events per job, 64 MiB per upload, 128 upload files and 1,024 entries
per directory listing. Exceeding output limits kills that process and reports a
terminal error; completed jobs are evicted oldest-first. Frame size remains 1 MiB.
Slow/disconnected clients do not create unbounded buffers. Foreground OpenSandbox
stream cancellation kills its process; E2B processes and explicit background
commands remain connectable within those retention limits. These limits and the
partial protocol profile apply to native templates; legacy upstream services
retain their own behavior. Real SDK tests below select templates by name and can
validate either backend. Startup improvements require a same-budget A/B; changing
the engine alone is not evidence of a speedup or full SDK conformance.

## Upstream OpenSandbox templates (explicit compatibility backend)

Prepare a template once, then create native VMs directly through the official
SDK. Preparation uses the same compressed content pool as `pvisor docker`:
equal file chunks share storage, while each VM has private writes. Create opens
a pinned pool revision and retains its lazy projection owner until VM teardown;
it performs no registry pull or complete rootfs extraction. Preparation may use
temporary unpacked build storage, which is outside the create path.
Per-VM projection scratch lives under the owned run directory, so confirmed
cgroup cleanup also reclaims it after a supervisor crash. Pool objects remain shared.

Build the runtime-performance daemon with `just build performance daemon` (the
binary is staged as `target/performance/pvisor-daemon`). Build the trusted
**1.1.0** execd once from the pinned upstream commit:

```sh
git clone --branch release-1.1.0 --depth 1 https://github.com/opensandbox-group/OpenSandbox /tmp/opensandbox
git -C /tmp/opensandbox rev-parse HEAD  # b1a29cf93a823a95913f7943010febb3f29de05c
(cd /tmp/opensandbox/components/execd && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags='-s -w' -o /tmp/execd .)
pvisor-daemon prepare-image --name ubuntu --image ubuntu:24.04 \
  --execd /tmp/execd --images-dir /srv/pvi
```

Prefer a digest-pinned base in deployments. `prepare-image` checks Linux x86-64
ELF/static linkage, embeds the daemon-built static vsock supervisor, builds into
the shared pool, and atomically publishes `<name>.json` after success. The operator
is responsible for the trusted execd version/source; ELF checks do not authenticate
its provenance. Build/pre-publication failures preserve the previous manifest;
pulled/published cache objects can remain. An error during rename/directory sync
can leave publication uncertain: inspect the manifest before retrying. Builds require
the `x86_64-unknown-linux-musl` Rust stdlib for the embedded guest adapter.
Use a base with `/bin/sh`, `tail` and the tools required by your workloads. No
Python interpreter or notebook kernel is injected into an Ubuntu base.

Start the daemon with the delegated cgroup and API key in the Run section, then:

```python
from opensandbox.config import ConnectionConfig
from opensandbox.sandbox import Sandbox

config = ConnectionConfig(domain="127.0.0.1:8080", protocol="http",
                          api_key="YOUR_API_KEY", use_server_proxy=True)
sandbox = await Sandbox.create("ubuntu", connection_config=config)
try:
    result = await sandbox.commands.run("printf 'hello from a VM'")
    print(result.logs.stdout[0].text)
finally:
    await sandbox.kill()
    await sandbox.close()
```

Install SDK `opensandbox==1.1.0`. Both server-proxy and endpoint-header modes
work. The managed adapter supervises the real execd with `--init` and bridges
guest CID 3 port 44772 without interpreting command/SSE/file traffic. Execd
starts gated on the supervisor's runtime-init handshake; create waits for real `/ping` and bound, initialized `/ready`,
as well as acknowledged native controls. Parent startup polling is 5 ms rather
than a fixed 100 ms; no check is bypassed and no warm VM is handed to another user.

The managed profile uses a **native capability adapter** on port 18080, not
upstream egress. Its `/healthz` checks actual guest execd readiness; all policy
requests return 501. Native OverlayNet provides the existing outbound profile.
The current firmware lacks upstream egress's required netfilter support, so
network policy, credential proxy and secure-access requests remain rejected.
The official SDK can create and use sandboxes without those features. Snapshot,
template/pool and arbitrary-port APIs remain unsupported. This is a local
replacement for the tested core profile, not every OpenSandbox deployment.

The creation design removes registry/unpack work and reduces polling delay.
The [paired native/Docker experiment](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/opensandbox/REPORT.md)
found faster first creation after API restart and slower steady creation. These
results use the `performance` build and prepared images, not registry cold pulls.
VM isolation and Docker container isolation are different contracts; their full
process-tree memory accounting is also different.
Private `startup.json` records bounded cumulative supervisor phase times in
microseconds for diagnosis; it contains no credentials and is never liveness proof.

## Local E2B SDK interface

The same daemon also accepts the official **E2B Python SDK 2.59.0**, both sync
and async, using the native guest adapter by default for new `--e2b` templates, or real
**envd 0.10.2** for explicitly prepared `--envd` templates. Source commits and
protocol blobs are pinned in [`e2b.lock.json`](e2b.lock.json). This is a partial
local profile, tested with unmodified SDKs, not full E2B service conformance.
OpenSandbox and E2B templates can coexist in one daemon and share the compressed
image pool, admission limits, persistent registry and confirmed native cleanup.
Native templates share the guest engine; their protocol/resource profile remains
explicit. An OpenSandbox image is not an E2B template.

| E2B interface | Local behavior |
| --- | --- |
| `POST /sandboxes`, `POST /v2/sandboxes` | Local template key, envs, metadata, positive timeout; 201 after native readiness |
| `GET /sandboxes`, `GET /v2/sandboxes` | Running/paused records, SDK metadata/state/template/time filters, sort and `x-next-token` pagination |
| `GET /sandboxes/{id}` | Reconcile real native state; return template resources and sandbox-scoped envd token |
| `POST /sandboxes/{id}/timeout` | Replace deadline relative to now, including shortening; expired records cannot be revived |
| `POST /sandboxes/{id}/connect`, `/v2/sandboxes/{id}/connect`, `/sandboxes/{id}/resume` | Resume the same live VM if paused, then replace TTL under the same lifecycle lock |
| `POST /sandboxes/{id}/pause` | Acknowledged live vCPU pause; repeated pause returns 409 as expected by the SDK |
| `DELETE /sandboxes/{id}` | Confirm native teardown, then release admission; 204 |
| `/e2b/*` | Native JSON/Connect adapter or explicit real envd proxy: commands, stdout/stderr, stdin, signals, process inventory and files |
| Template builds, snapshots/fork, filesystem-only pause, auto-pause/resume, IAM, MCP, volumes, network policy and internet denial | Rejected with 501; no substitute behavior |
| Historical metrics, arbitrary application ports and WebSocket | Unsupported |

E2B lifecycle requests use `X-API-Key` and numeric `{code, message, error_code}`
errors. They use the same configured secret as OpenSandbox's lifecycle API.
`serve` resolves `--api-key`, then `OPEN_SANDBOX_API_KEY`, then `E2B_API_KEY`.
Use environment credentials rather than command-line secrets. E2B data requests
require exactly one `E2b-Sandbox-Id`, `E2b-Sandbox-Port: 49983` and
sandbox-scoped `X-Access-Token`; that token grants no lifecycle or other-sandbox
access. The proxy preserves Basic authorization for envd's guest user selection,
removes lifecycle credentials, and blocks init/upgrade/freeze/unfreeze control
paths, including encoded paths and the OpenSandbox proxy route.
The raw loopback mapping also authenticates before every request. Only the private
supervisor initializes envd, checks its actual version, environment binding and
unauthenticated rejection. The raw bootstrap token is absent from workload envs.

Prepare a trusted envd from the pinned official source and publish a local template:

```sh
git clone https://github.com/e2b-dev/runtime /tmp/e2b-runtime
git -C /tmp/e2b-runtime checkout --detach 3996d14cf2ee9f938508b70d1f0f71a4fc6a5235
(cd /tmp/e2b-runtime/packages/envd && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -trimpath -ldflags='-s -w' -o /tmp/envd .)
pvisor-daemon prepare-image --name e2b-base --image ubuntu:24.04 \
  --envd /tmp/envd --envd-version 0.10.2 --cpu-millis 1000 --memory-mib 512 \
  --images-dir /srv/pvi
```

The pinned envd requires Go 1.26.10. Prefer a digest-pinned base and keep the
trusted binary/source provenance; static ELF validation does not authenticate it.
Preparation uses the shared compressed pool and atomic manifest publication.
`native_e2b: true` selects the embedded guest supervisor and private init handshake;
`e2b` retains the template's resource/version profile. Create opens the pinned lazy
projection, with private scratch reclaimed on confirmed deletion. It performs no
image pull or full extraction. The guest starts real envd with `--isnotfc` and
`--no-cgroups`: host delegated cgroups still cap the entire native process tree;
per-process guest cgroup management is not provided. Default guest user/workdir are
`root` and `/root`; other users must already exist in the operator's base image.
The base must contain `/bin/bash` and the tools/languages needed by workloads.
No Jupyter/code-interpreter kernel is injected.

Start sandbox serving as in the Run section. For a local daemon on port 8080:

```sh
pip install 'e2b==2.59.0'
export E2B_API_KEY='YOUR_DAEMON_API_KEY'
export E2B_API_URL='http://127.0.0.1:8080'
export E2B_SANDBOX_URL='http://127.0.0.1:8080/e2b'
```

```python
from e2b import Sandbox

sandbox = Sandbox.create("e2b-base", timeout=300)
try:
    print(sandbox.commands.run("printf 'hello from pVisor'").stdout)
    sandbox.files.write("/tmp/hello.txt", "shared image, private writes")
    print(sandbox.files.read("/tmp/hello.txt"))
finally:
    sandbox.kill()
```

Explicit `api_url`, `sandbox_url` and `api_key` arguments work as well.
`E2B_SANDBOX_URL` must include `/e2b`; it lets the SDK route several sandboxes by
headers without wildcard DNS or debug mode. Keep this SDK version pinned because
those URL overrides are deprecated upstream. Only envd port 49983 is published;
`get_host()`/signed file URLs and arbitrary web applications are outside this profile.

Pause retains the live VM and its resource reservation, with no durable snapshot.
**TTL keeps counting while paused**, and expiry deletes paused VMs too. Connect
cannot revive an expired sandbox. A paused VM survives daemon restart, not host
reboot; configure timeouts accordingly. This differs from E2B's snapshot-backed
pause storage. The shared pool removes extraction from create; no E2B-cloud
startup, density or security comparison is claimed.

With an isolated daemon and at least 2 CPU/1 GiB admission capacity, run the opt-in
real SDK acceptance test:

```sh
uv run --no-project --with e2b==2.59.0 python \
  crates/pvisor-daemon/tests/e2b_sdk.py --api-url http://127.0.0.1:8080
```

It covers sync/async creation, two concurrent VMs, streaming, nonzero exits,
large environment variables, stdin, background process signals, UTF-8 and >3 MiB
binary files, private writes, pagination/metadata, TTL shortening, live pause/connect,
scoped authentication, private route rejection and deletion. Additional real KVM
verification restarts the daemon and reconnects to the same VM, retained file and
running background process, then runs the existing OpenSandbox SDK test against
the same service. Conventional target tests include E2B wire/auth/state/restart
contracts and protected loopback initialization. These checks do not establish
full SDK conformance or hostile multi-user isolation.

## Runtime boundary: native VM and prepared images

Invoke `pvisor-daemon` directly. The native `pvisor` application does not depend
on this crate. The VM-only `NativeRuntime` in `runtime.rs` embeds `pvisor::PVisor` and `VmExecutor`
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
references. Legacy fields are `rootfs` (absolute independently provisioned Linux tree,
never host `/` or overlapping daemon state), `entrypoint` (absolute guest
bootstrap argv), optional `cmd`, optional `env`, and optional absolute
`library_dir` for firmware. The bootstrap receives requested workload argv, or
`cmd` if empty, appended to `entrypoint`; request env overrides manifest env,
without inheriting host env or shell interpolation. The immutable rootfs is
served through the native VM path with private writes.

For independently provisioned legacy manifests, the bootstrap must supervise real OpenSandbox **1.1.0 execd**, egress and the
workload, initialize/authenticate services, forward signals and reap children.
It must provide byte-transparent guest AF_VSOCK listeners on **CID 3**, ports
**44772/18080**, bridged to those real services. Supervisor loopback TCP
publications connect through private Unix sockets and native vsock forwarding.
A stock distribution rootfs or a sleeping process does not satisfy this contract.

Create, Inspect of a Running VM and resume readiness checks verify real execd
`/ping`, `/ready` (JSON `initialized: true`) and egress `/healthz` through the
bridges, requiring HTTP
200 and bounded bodies. Readiness is not manufactured. For legacy images the daemon does not inject
`/execd`, call its initialization handshake or synthesize command/SSE/file behavior. The prepared image must complete
any runtime initialization and configure its service authentication.

Endpoint lookup authenticates the live supervisor, requires the current RunHandle
Running state, checks deletion fences and resolves a live publication. It does
not repeat service health probes or full cgroup-limit reconciliation per data
request; connection failures belong to the API adapter. Native `observation.json`
caches are neither written nor used as liveness proof.

`prepare-image` supplies the managed bootstrap described above. Its manifest adds
`pooled_image` (the owning Docker API's pinned image contract) and
`native_opensandbox: true`; `rootfs` then names its private Docker store, not an
unpacked directory. The backend is lazy and is not advertised as immutable DAX
storage. Existing directory-root manifests retain their original upstream-egress
contract. Python SDK initialization resolves both endpoints even without a
network policy. There is no full SDK-conformance or density evidence.

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
monitoring and event-driven reconciliation are not implemented.

## Run

After preparing a template, supply a real delegated cgroup
hierarchy, not an ordinary filesystem directory:

```sh
pvisor-daemon serve --sandbox \
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
listeners. A loopback `--listen 127.0.0.1:0` development binding publishes its
actual allocated authority automatically; the listening address is logged.

`pvisor-daemon protocol` prints the pinned baseline. Connect SDK version 1.1.0
with the chosen domain and protocol, and use a prepared image satisfying the
contract above. Invoke the separately built `pvisor-daemon` executable directly.
`pvisor cache` provides `prepare`, `publish`, `prepare-rootfs`,
`prepare-workspace`, `list`, `stat` and `read`. The daemon owns cache serving
through `pvisor-daemon serve --cache --cache-listen ENDPOINT --cache-image-store DIRECTORY`.
Cache serving supports Linux and macOS and does not require KVM or cgroups.
It preserves `PVISOR_CACHE_LOCATION` / `PVISOR_CACHE_SERVER`, `PVISOR_IMAGE_STORE`
and `PVISOR_CACHE_TOKEN`; `--cache-listen` and `--cache-image-store` override their defaults.
TCP requires a token. Filesystem/S3 clients need no cache daemon. Node runtime
protocols belong to `pvisor`; the daemon has no automatic acquire/release
adapter.

## Optional daemon-owned memory pool

Run `pvisor-daemon serve --memory-pool --state /tmp/pvisor-pool` independently,
or add `--memory-pool` to `serve --sandbox`. It is off by default. Standalone
serving creates private state, prints the canonical socket in a JSON readiness
record and monitors the detached pool. SIGINT/SIGTERM stops the manager while
preserving the pool for live VMs; manager restart reuses that same service.
Unexpected pool loss fails the manager. Stale sockets/configuration conflicts
fail closed; neither a restart nor another manager replaces live RAM ownership.
Stop dependent VMs before terminating the pool component. The pool's startup
lock serializes independent managers as well as sandbox startup.

Apple Silicon macOS defaults to `--memory-pool-mode compressed`, using the
existing HVF cold-page backend and the same daemon lifecycle, budgets and
same-UID authorization. Cold 64 KiB blocks are compressed and equal contents
share one pool object; access restores private VM RAM. This is compressed-state
deduplication, not Linux physical-page sharing. macOS does not use memfd,
userfaultfd, a FUSE RAM mount or guest zram. Explicit `physical` selection fails
on macOS; old macOS component records labelled `physical` retain their legacy
compressed backend when reopened directly with the same configuration.
**The macOS pool must stay alive: its heap can hold the only copy of cold RAM,
and service loss terminates dependent VMs.** Linux sealed-local recovery does
not apply. Only trusted, same-user workloads should share this experimental
pool. No net physical-memory saving or density guarantee is implied by object
counts or encoded payload size.

Use a fresh state directory and `--memory-pool-mode physical-pages` for native
physical sharing on Apple Silicon (also accepted on Linux):

```sh
pvisor-daemon serve --memory-pool --memory-pool-mode physical-pages --state /tmp/pvisor-physical
pvisor run -d -m 512m --memory-pool /tmp/pvisor-physical/memory-pool/pool.sock alpine sleep 3600
```

This mode reuses the daemon's bounded content index, full-byte duplicate checks,
slot references and same-user transport. macOS stores native 16 KiB pages in a
private, unlinked file, gives clients only a read-only descriptor, and registers
a kqueue process-exit watch before granting references. Disconnect alone never
releases live mappings. Slots remain immutable while pinned; collection punches
unreferenced slots before reuse. Unlike Linux memfd size seals, fixed size is an
owner contract on macOS. The owner and same-user workloads must be trusted.
The temporary filesystem must support file mapping and hole punching; backing
uses file cache and disk, not locked RAM. A maximum of 32,768 objects bounds the
default Apple Silicon pool to 512 MiB, with 32,768 references per VM. Indexes,
scan buffers and per-page state add memory beyond payload limits.

The HVF worker merges equal resident pages while the guest runs; reads stay
shared and CPU/device writes COW. It rechecks every byte after publication under
the CPU/device barrier, and releases replaced references outside that barrier.
COW pages are detached from the source shadow before old slots can be reused.
Unique pages stay private. The current 4 KiB guest pages do not always align into
identical 16 KiB host pages across VMs, including for identical random heaps.
This mode does not compress, reconnect, or support snapshots/offload. Pool loss
terminates dependent VMs; state records do not recover RAM after owner loss.
Legacy macOS `physical` records keep their compressed meaning; use the explicit
new `physical-pages` mode, never change an active state's configuration.

Standalone readiness also advertises `shared_kernel_base` on Apple Silicon.
The private `<state>/memory-pool/shared-kernel` directory stores atomically
published, verified, read-only kernel baselines for
`pvisor run --shared-kernel-base DIR`. Equal initial firmware bytes map one inode
privately, so unchanged kernel pages can remain physically shared and writes
use COW. This is a cold-boot kernel baseline, not a warmed guest template or
dynamic application-page merging. Mappings retain their file ownership after
daemon exit or path removal. Only containers that also select `--memory-pool`
depend on the live compressed pool. Kernel cache files/page-cache overhead are
outside compressed payload limits, with no automatic kernel-cache GC. Do not
modify/truncate cache files or expose the directory to guests.

Connect ordinary Jobs using `pvisor spawn --vm-memory-pool SOCKET -- COMMAND`,
or containers using `pvisor run --memory-pool SOCKET IMAGE COMMAND`. With the
example state above, SOCKET is `/tmp/pvisor-pool/memory-pool/pool.sock` (the JSON
record includes its canonical macOS `/private/tmp` spelling). No OpenSandbox
bootstrap, API key, KVM or cgroup is required for this macOS path.

On Linux, physical sharing remains the default. The VM scans resident 4 KiB pages and publishes
immutable content-addressed pages into a bounded daemon memfd. Only candidates observed in different VM sessions enter the pool; candidates
hold hashes without retaining page payloads. Equal bytes reuse one slot. After CPU/device quiescence and a live-byte recheck, the VM maps that
slot privately: reads retain the shared physical page, while writes use kernel
COW. This path does not require userfaultfd and does not compress unique pages.
Local cold-RAM compression remains a separate userfaultfd mode.

The physical pager uses UKSM-inspired region scheduling: productive regions
receive larger scan quotas, COW/unstable regions lose priority, and every region
retains exploration work. Residency queries cover only selected regions. See
[the scanner contract](../../docs/src/en/design/memory-optimization/deduplication.md#adaptive-scan).

Select `--memory-pool --memory-pool-mode compressed` for the Linux sealed
compressed pool instead. Equal decoded 64 KiB blocks share an object across VM
sessions; unique eligible blocks use fill/zstd compression. Each object is a
read-only memfd sealed against writes and size changes. The client validates
the descriptor, complete decoded bytes and identity before the pager rechecks
and discards RAM. CPU/device accesses restore private RAM locally through the
existing kernel-fault userfaultfd pager. Missing userfaultfd authority fails VM
startup without fallback or host-policy changes. Physical sharing and compressed
eviction are separate modes, fixed in persisted configuration.

The daemon starts or reuses the detached
`pvisor-daemon memory-pool --directory DIR` component with configuration-bound
private state under `<state>/memory-pool/`. This component expects the daemon's
persisted pool configuration. The component survives API restart with the
same configuration.
Connections authenticate the host UID; Linux clients receive a read-only file descriptor.
References pin slots until explicit release after unmapping; on disconnect,
pidfd-confirmed peer exit is required before releasing mapped references.
Default limits are 512 MiB of page storage, 32,768 objects, 32 connections and
32,768 references per connection. At 4 KiB per object, the object ceiling bounds
the default pool to 128 MiB of distinct pages and each VM to 128 MiB of references.
Indexes, VM mappings, threads and allocator memory add overhead. Budget rejection
keeps the original page resident. An older compressed pool is not reused by the
physical-sharing client; drain old VMs and use fresh pool state when upgrading.

Compressed-mode byte accounting includes frame headers and 4 KiB file-page
rounding. Admission requires at least 1/8 savings after rounding plus a
conservative 256-byte metadata allowance. The same object/connection/reference
limits apply. Each object and delivered reference consumes a file descriptor;
OS descriptor limits may also reject publication while preserving original RAM.
Restore and local release issue no IPC. Bounded release batches flush during
publication/stats, including idle pager maintenance. Disconnected pins remain
budgeted at the publisher until pidfd confirms peer exit. After service loss,
new publication stops permanently while retained handles still decode locally.
Stats then count local pinned bytes/objects with cross-session sharing unknown
(reported as zero), and emit `pvisor-sealed-pool publication_disabled=true
accounting_scope=local-pins`. Recovery can still queue behind an in-progress
publication's store lock; recovery scheduling and latency acceptance are unvalidated.

Reserve host memory for the pool separately from sandbox admission: its process
is outside individual sandbox cgroups and their hard limits. Scanning skips
nonresident RAM and leaves untouched capacity sparse. Shared reads do not restore
private RAM; COW writes and scanning consume memory and CPU; scanning releases obsolete
pool references after writes. Budget for the actual
working set and startup/write peaks.
The physical pool must stay alive while dependent VMs run; its loss fails those
VMs. Compressed clients retain already delivered backing after service loss,
but neither mode reconnects or silently replaces a stale socket. State-directory
retention supports API restart, not pool-process or host-reboot recovery.
The VM/pool benchmark includes
the pool component in one group; it does not establish full API/SDK conformance
or production density.

## Package and evidence scope

The Cargo package/directory/library are `pvisor-daemon` / `pvisor_daemon`.
The library exports `daemon` and `runtime` for single-node sandbox management;
distributed scheduling belongs to external orchestrators.

Cluster/controller measurements are not evidence for this node-local daemon's
performance, admission behavior or resource density.

## Validation

Standalone pool tests cover same-user compressed object sharing, budgets,
manager shutdown/reopen, unavailable owners and stale-socket refusal. On Apple
Silicon, the opt-in native container case also checks cold-page publication in
both VMs, cross-session objects, post-idle exec, private writes, stop/start,
pool-loss termination and refusal to restart without the configured pool. It
also uses one shared kernel inode across cold starts, verifies that boot/exec
leave the baseline unchanged and checks a baseline-only container survives pool
loss and cache-file unlink. The lower-level native HVF COW gate is in the VM README:

```sh
# Build/sign the CLI first; ROOTFS needs native sh, cat and sleep.
PVISOR_MEMORY_POOL_TEST_PVISOR=/absolute/path/to/signed/pvisor \
PVISOR_DOCKER_TEST_ROOTFS=/absolute/path/to/rootfs \
just test pvisor-daemon -- --test memory_pool \
  -E 'test(macos_container_vms_publish_and_restore_through_daemon_pool)' --run-ignored only
```

This is a correctness gate, not a net-memory or performance measurement.

The opt-in four-VM physical acceptance uses a static native probe (Rust musl
stdlib installed, no C cross-compiler required):

```sh
rustc --edition 2021 --target aarch64-unknown-linux-musl -C linker=rust-lld -O \
  crates/pvisor-daemon/tests/fixtures/shared_memory_guest.rs -o /tmp/pvisor-shared-guest
mkdir -m 700 /tmp/pvisor-four-evidence
PVISOR_MEMORY_POOL_TEST_PVISOR=/absolute/path/to/signed/pvisor \
PVISOR_DOCKER_TEST_ROOTFS=/absolute/path/to/native/rootfs \
PVISOR_SHARED_TEST_GUEST=/tmp/pvisor-shared-guest \
PVISOR_SHARED_KERNEL_TEST_OUTPUT=/tmp/pvisor-four-evidence \
just test pvisor-daemon -- --test memory_pool \
  -E 'test(macos_four_512m_vms_dynamically_share_and_cow_application_pages)' --run-ignored only --no-capture
```

Run again with `PVISOR_SHARED_TEST_CONTROL=1` and a new empty private evidence
directory for the same workload with sharing disabled. Each arm starts four
independent 512 MiB/one-vCPU Linux VMs. The 25-second RAM inventories query native
resident object/offset identities under each VM's own CPU/device barrier;
`vmmap` summaries, raw worker logs and complete payload results are retained.
The source oracle checks every byte before and after reads, quarter/full writes,
and peer exit. Read-only probes do not rewrite the payload. Cleanup removes only
the test's own containers and component. Failed evidence is retained too.

Native four-instance results and limits are recorded in [the acceptance report](tests/MACOS_SHARED_MEMORY.md).

This is one static group per arm, not randomized repeated performance sampling.
Resident object/offset counts describe mapped guest RAM backing, not PFNs or
whole-host memory; per-process footprint is only a proxy and pool/metadata/cache
cost is additional. Startup and write peaks, scan CPU and density are not measured.
The separate `macos_four_512m_containers_share_resident_kernel_pages` gate checks
common cold-boot baselines without the dynamic pool.

Conventional tests include fake-runtime lifecycle/admission/restart/TTL regressions,
HTTP auth/schema/filter/endpoint tests and pure native identity/IPC/manifest,
quota, readiness and cleanup regressions. Fake runtime tests are not evidence of native isolation, resource density or full SDK
compatibility.

Run targeted checks with:

```sh
just test pvisor-daemon
```

Pool tests launch the actual component, check sharing/budgets and local recovery
after killing the service, and retain disconnected pins until client exit.
They do not exercise guest userfaultfd discard/refault or prove physical savings.

The separate `compressed_pool_case` correctness driver starts at most two
256 MiB/one-vCPU native VMs and a 32 MiB pool. It waits for known shared and
per-guest payload blocks to have external pool references, terminates the pool,
then checks full guest digests, private writes and virtio-fs file round trips.
It uses the same pool component but bypasses the OpenSandbox API/image bootstrap.
It needs KVM, kernel-fault userfaultfd authority and local guest firmware:

```sh
cargo build --locked -p pvisor-daemon --example compressed_pool_case
PVISOR_CASE_VM_LIBRARY_DIR=/absolute/firmware \
  PVISOR_EXPERIMENTAL_MEMORY_METRICS=1 \
  target/debug/examples/compressed_pool_case /tmp/pv-cmp-check
```

The output path must be fresh and short; `result.json` records success/failure.
`PVISOR_CASE_ROOTFS` and `PVISOR_CASE_VM_PYTHON` can select an alternate Linux
rootfs/Python. On 2026-10-18, the probe passed on Linux x86_64/KVM after
userfaultfd access was granted: both guests completed, with positive discard
counters and increasing restoration counters after service loss. It writes
through virtio-fs before full payload digest reads, then verifies private writes
and a second file round trip. The result includes per-guest recovery counters.
This establishes scoped correctness, not physical savings or latency acceptance.

Append `--physical` to run the same two-VM driver against the raw-page pool.
This mode keeps the service alive, verifies known payload sharing and independent
writes/file I/O, then requires each guest's diagnostics to show adaptive scan,
successful merge and COW observations. The Linux x86_64/KVM probe passed with
the adaptive region scheduler on 2026-10-18. It does not compare scan throughput,
CPU cost or memory savings against linear scanning or UKSM.

The opt-in native SDK test launches two real sandboxes concurrently, exercises
both endpoint modes, streaming output/nonzero exit, env/cwd, transport-secret
filtering, UTF-8/binary files, private writes, metrics, renewal, pause/resume,
reconnect and cross-sandbox authentication. It also requires explicit policy 501
and deletes its own sandboxes. With an isolated daemon and a managed `ubuntu`
template (at least 2 CPU/1 GiB admission capacity), run:

```sh
uv run --no-project --with opensandbox==1.1.0 python \
  crates/pvisor-daemon/tests/opensandbox_sdk.py --domain 127.0.0.1:8080
```

Keep `OPEN_SANDBOX_API_KEY` in the environment. These tests establish only the
listed native core profile; isolated-session/bwrap, notebook languages, arbitrary
applications, network-policy enforcement, hostile-user isolation and production
density require their own validation.

## Implementation ownership

| Module | Responsibility |
| --- | --- |
| `daemon/models.rs` | Fixed wire models, resource quantities, unsupported feature rejection |
| `daemon/store.rs` | Private per-record registry, atomic publication/unlink, logical budget and v1 activation |
| `daemon/mod.rs` | Admission/commit serialization, lifecycle, restart reconciliation, TTL and endpoint access |
| `daemon/api.rs` | OpenSandbox HTTP adapter, filters, shared streaming data-plane proxy |
| `daemon/e2b.rs` | Pinned E2B lifecycle schemas, SDK routes, TTL/connect and envd authentication |
| `runtime.rs` | Runtime trait, VM-only NativeRuntime, detached supervisor, IPC/cgroup identity and vsock bridges |
| `memory_pool.rs` | Daemon-owned bounded cold-page pool, private socket and restart reuse |
| `main.rs` | Daemon CLI/configuration, listener and maintenance lifecycle |
| `prepared_image.rs` | Operator-side static execd/envd checks, pool template preparation and atomic manifest publication |
| `sandbox_init.rs` | Embedded guest-only supervisor/bridge for explicitly selected upstream services |
| `native.rs`, `native/` | Host OpenSandbox/E2B adapters, bounded process journals and guest RPC; never host execution |

The native API-restart SDK regression accepts an unused state directory and a
real delegated cgroup. It verifies unchanged Run/Attempt identity, absence of
injected Go services, binary files and a live stdin process across API restart:

```sh
uv run --no-project --with e2b==2.59.0 python \
  crates/pvisor-daemon/tests/native_sdk_restart.py \
  --daemon "$PWD/target/performance/pvisor-daemon" \
  --images-dir /srv/pvi --template e2b-base \
  --cgroup-root /sys/fs/cgroup/pvd --state /tmp/pvisor-sdk-restart
```
