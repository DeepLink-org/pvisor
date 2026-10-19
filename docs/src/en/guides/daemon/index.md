# Run the single-node daemon


Cache is an optional component of the same daemon: add `--cache` to sandbox serving, or run `pvisor-daemon serve --cache` alone. `--cache-listen` and `--cache-image-store` configure its separate endpoint and store. Cache startup failure or unexpected exit fails the daemon; daemon shutdown/crash stops its cache worker. HTTP shutdown drains for at most ten seconds; in-flight cache requests may fail. Native VM supervisors and the shared pool retain their existing restart lifecycle.
Install `pvisor-daemon` to manage image sandboxes on one Linux host through an API partially compatible with OpenSandbox 1.1.0. The daemon owns node-local admission, lifecycle, persistent state and expiration cleanup.

Cross-node scheduling, global DAGs and distributed leases belong to Kubernetes, Ray or your application. The daemon contains no Controller or Worker.

Use `pvisor-daemon` for node-local native VM sandbox lifecycles; use `pvisor run` for the public Job and file-review workflow.

!!! warning
    The project **has not supplied a guest bootstrap or image recipe and has not performed end-to-end validation**; SDK-conformance validation and density measurements are also unavailable. Every step below requires you to prepare images and vsock bridging yourself.

## Prerequisites {#prerequisites}

You need Linux x86_64, a usable `/dev/kvm`, trusted absolute paths, and a writable, delegated cgroup v2 hierarchy with the CPU/memory/PID controllers and `cgroup.kill` enabled.

Preflight checks perform real controller writes and KVM API calls. There is no host, OCI-command or registry-pull fallback.

An image is a trusted local `images_dir/<key>.json` manifest, not a registry reference. Fields include an absolute-path standalone Linux `rootfs` (neither the host `/` nor overlapping daemon state), an absolute-path guest bootstrap `entrypoint` argv, optional `cmd`, optional `env`, and an optional absolute-path firmware `library_dir`. The requested workload argv (or `cmd` when empty) is appended to `entrypoint`; request env overrides manifest env, does not inherit the host environment, and receives no shell interpolation.

The long-running guest bootstrap must supervise the workload, a real OpenSandbox 1.1.0 execd and egress; initialize/authenticate the services itself; forward signals and reap children. It must provide byte-transparent AF_VSOCK listeners on ports **44772/18080** of guest **CID 3**, bridging the real services. The supervisor's loopback TCP publications reach the guest over a private Unix socket and native vsock forwarding. A plain rootfs or a sleep-only process is not enough. Create, Inspect on a Running VM and resume readiness checks require, through the bridge, a real HTTP 200 from execd `/ping`, JSON `initialized: true` from `/ready`, and egress `/healthz`, with bounded response bodies. The daemon neither injects/initializes execd nor synthesizes command/SSE/file responses.

## Install and start {#install}

Linux x86_64 and macOS ARM64 wheels install `pvisor-daemon` alongside `pvisor`. Cache serving works on both platforms; the sandbox service below requires Linux x86_64:

```bash
pip install pvisor
pvisor-daemon --help
pvisor-daemon protocol
```

For the latest `main` build, use the [nightly wheel installer](../../start/installation.md). `protocol` prints the pinned OpenSandbox version and commit. Pin the source revision, SDK 1.1.0 and image contents together. The local source-install path (`cargo install --locked --path crates/pvisor-daemon`) is not verified.

`serve` constructs the `NativeRuntime` from the implemented and required `--images-dir` and `--cgroup-root`:

```bash
export OPEN_SANDBOX_API_KEY="$(openssl rand -hex 32)"
pvisor-daemon serve --sandbox \
  --images-dir /srv/pvi \
  --cgroup-root /sys/fs/cgroup/pvd \
  --listen 127.0.0.1:8080 \
  --state /run/user/1000/pvd \
  --max-sandboxes 32 \
  --cpu-millis 4000 \
  --memory-bytes 8589934592 \
  --max-timeout-seconds 86400
```

Runtime paths are absolute and must stay unchanged across daemon restarts. Keep the state path short, for example `/run/user/1000/pvd`: each sandbox's `control.sock` must be shorter than 104 bytes, and vsock Unix sockets also have path-length limits. Retain the state; this `/run` example does not guarantee persistence across logout/reboot, and VMs do not survive a host reboot. `/sys/fs/cgroup/pvd` must be a real delegated hierarchy, not a plain directory.

Choose a private state directory outside the checkout. Generate the API key once, keep it in protected service secret storage, and reuse the same value across restarts. The example admission budget is 32 records, four CPU units and an 8 GiB total hard memory limit. Whole-node physical occupancy is unmeasured; leave headroom for daemon/cache/host and configure host supervision separately.

Query the lifecycle API from a second shell with the same protected API key:

```bash
curl --fail-with-body --config - <<EOF
url = "http://127.0.0.1:8080/v1/sandboxes"
header = "OPEN-SANDBOX-API-KEY: ${OPEN_SANDBOX_API_KEY}"
EOF
```

Expect a JSON sandbox list, empty on a fresh state. This query validates API access; sandbox creation, native resource enforcement and SDK data-plane readiness need separate validation. Do not publish the key or paste credentialed logs into issues.

## OpenSandbox interface scope {#profile}

The compatibility baseline is pinned to OpenSandbox **1.1.0**, tag `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`. Support is limited to the partial API below; full OpenSandbox and unmodified-SDK end-to-end compatibility are unvalidated.

| Capability | Current behavior |
| --- | --- |
| Image create, list, get, delete | Node-local sandbox lifecycle; create accepts argv/env/metadata, CPU/memory limits and an optional TTL |
| Pause / resume | Confirmed live vCPU pause/resume on the same Attempt, not a checkpoint |
| Renew | A future RFC3339 time that extends an existing TTL |
| Endpoints | Only daemon-routed execd 44772 and egress 18080 |
| Commands, files, health, metrics | Streamed to the real upstream services in the prepared image |
| Snapshots, templates/pools, metadata modification, hooks | Unsupported |
| Network policies, credential proxy, secure access, volumes, image auth | Unsupported create options are rejected |
| Arbitrary app ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| Stage/apply, checkpoint/fork, offload | Not wired into this daemon backend |

## Query, delete and expiration {#operations}

| Request | Meaning |
| --- | --- |
| `GET /v1/sandboxes` | Most recently persisted observations; supports repeated state filters, SDK-encoded metadata and page/pageSize |
| `GET /v1/sandboxes/{id}` | Reconcile this sandbox against node-local native state |
| `DELETE /v1/sandboxes/{id}` | Releases the reservation only after native deletion is confirmed; success returns 204 |
| `POST /v1/sandboxes/{id}/pause` | Acknowledges a live vCPU pause; 202 with an empty body |
| `POST /v1/sandboxes/{id}/resume` | Acknowledges a live vCPU resume; 202 with an empty body |
| `POST /v1/sandboxes/{id}/renew-expiration` | Extends an existing TTL with a future RFC3339 `expiresAt` |

Replace `{id}` with the sandbox ID returned by create/list. An HTTP disconnect does not cancel an accepted operation. Each sandbox has its own lifecycle lock; control and connection establishment for one sandbox are serialized.

After losing a create response, do not blindly retry creation: this API is not the old task-ID idempotency protocol. Check retained state first. When create reports an uncertain native result, keep the sandbox ID from the error message and query/delete it. Admission is released only when creation failed and cleanup is confirmed complete; uncertain cleanup retains the record and reservation.

Create `timeout` is optional; omit/null means manual cleanup. A configured TTL is at least 60 seconds and at most `--max-timeout-seconds` (default one day, configurable up to one year). Renewal must extend an existing deadline. Expiration cleanup is **best effort**, not a hard execution deadline; new proxy requests are rejected after expiration. Stopping the daemon normally preserves native sandboxes and persistent records for restart; cleanup is not guaranteed while the daemon is down. For hard deadlines or downtime cleanup, configure separate native host supervision.

## Endpoint authentication {#endpoints}

Resolve execd through `GET /v1/sandboxes/{id}/endpoints/44772` and egress through port `18080`. The returned authority carries no URL scheme; traffic goes through the daemon, not directly to the supervisor's loopback publication.

- With `use_server_proxy=true`, routed requests authenticate with the lifecycle `OPEN-SANDBOX-API-KEY` header.
- The default mode provides a random, sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` in the endpoint `headers`. Clients must preserve these headers; the token authorizes neither lifecycle operations nor other sandboxes.
- Control credentials are removed before forwarding to the guest. A caller-provided guest execd access token is not replaced by the lifecycle key.

Signed endpoint expiration, arbitrary app ports, WebSocket and CONNECT are unsupported. Requests/responses stream, including multipart and SSE. Upload plus waiting for upstream headers is capped at 120 seconds; established response streams have no total timeout. Output/traffic quotas remain the upstream services' responsibility.

Loopback is the default listener. Before external access, configure TLS at a trusted reverse proxy and set `--public-endpoint HOST:PORT` to the externally routed authority, without scheme or path.

## Enabling the daemon memory pool {#memory-pool}

Add `--memory-pool` to the `serve` command to connect new sandboxes to the daemon-held physical shared-page pool; it is off by default. The runtime scans resident 4 KiB pages, preferring cross-instance duplicate content, and the daemon retains one physical page. VM reads stay shared; writes get private pages through kernel COW. This Linux path needs no userfaultfd and does not compress unique pages. Actual occupancy depends on duplication, scanning and writes; see the [memory benchmark](../../benchmarks/vm-memory/index.md#linux-lifecycle).

The pool component is independent of the API process and retains pages across API restarts. After a disconnect, mapping references are released only once a pidfd confirms the VM process exited. Losing the pool process fails dependent VMs and does not replace the live pool automatically. Default limits are 512 MiB of page data, 32,768 objects, 32 connections and 32,768 references per connection. The 4 KiB object cap bounds the default pool to 128 MiB of distinct pages and each VM to 128 MiB of references. Indexes, mappings, threads and the allocator add overhead; the pool sits outside per-sandbox cgroups and admission quotas, so reserve host headroom. When upgrading an old compressed pool, drain old VMs first and use a new state directory.

For the experimental compressed mode, add `--memory-pool --memory-pool-mode compressed` instead. Identical decoded 64 KiB blocks share sealed fill/zstd objects, and unique compressible content can also enter the pool. VM startup requires kernel-fault userfaultfd permission. After pool loss, already-held handles remain usable while new publication stops. The mode binds to persistent configuration; switching requires draining old instances and a new state directory. Process-level sharing and service-exit tests pass, and a two-VM probe verified guest UFFD eviction/refault and file I/O after pool termination. See the [sealed compressed-pool contract](../../design/memory-optimization/compression-pool.md). These VM/pool tests do not equal full OpenSandbox SDK end-to-end validation.

## Restart and state ownership {#restart}

One daemon exclusively owns its private state; owner identity is persisted before native creation/control. Private IPC verifies same-UID peer, owner, sandbox ID, generation and a secret token; persistent identity binds the boot ID and cgroup device/inode. IDs are never reused or restarted. IPC loss means unknown, not Missing or cleanup evidence. Persistent delete intent and the supervisor's exclusive lock block late starts; cleanup uses identity-bound `cgroup.kill`, never stores PIDs or kills by PID, and releases capacity only after the cgroup is empty and the owner lock is released. A replaced or lost cgroup without persistent tombstone evidence under the same boot does not prove the object is absent.

Pending delete intent survives restarts and is not overridden by native observations. Unknown deletion state retains the reservation. A natively lost sandbox still shows Failed until explicitly deleted. Do not delete state to fix errors, and never share it between two daemons. The registry is bounded by the configured capacity and a 16 MiB total logical v1-equivalent budget; it is neither an infinitely retained distributed-task archive nor a Run Bundle/artifact store.

A version-1 `sandboxes.json` with existing records but no native `owner.json` marker is rejected under the existing store's exclusive lock: it may still hold live Podman containers. Use fresh native state and retain/clean the old deployment, or delete all sandboxes through the old Podman daemon, confirm cleanup, and switch backends with the emptied registry. Do not delete registry entries, reservations or ownership state, and do not forge a native marker to bypass the check. The native daemon neither adopts those containers as Missing nor silently releases their reservations.

The current v2 state uses a small `sandboxes.json` owner/header with an empty map plus private per-sandbox `records/`. A missing native `owner.json` still rejects v2 startup; an empty header does not prove no sandbox exists. Ownership and record state must be retained together. Native v1 storage migrates only after the runtime factory accepts the owner; see [storage migration](../../design/daemon/storage.md#migration).

Deletion/reconciliation can continue tombstone-based cleanup when the original rootfs or firmware is missing, reclaiming empty owned cgroups and private run/RAM/temp/socket/secret records while keeping minimal ID barriers. Reclamation errors retain the reservation for retry; do not delete tombstones or registry state yourself. See [storage](../../design/daemon/storage.md#gc).

## Isolation and trust boundaries {#isolation}

The native supervisor embeds `pvisor::PVisor` in a separate child process outside the daemon's lifecycle, configures only `VmExecutor`, and holds the RunHandle. The sandbox cgroup limits the whole supervisor/VMM/helper tree: total CPU rate **10–8000 millicores**, hard memory, zero swap, `pids.max=512` and group OOM. vCPU count rounds the quota up (at most 8); guest RAM rounds down to MiB; the hard memory limit also covers host-side overhead. Paused, Failed and uncertain records keep their conservative reservations.

The startup callback checks started/delete/tombstone markers under the owner lock and joins the identity-bound cgroup **before exec** through a pre-opened `cgroup.procs` FD. Invoking the hidden command outside that cgroup fails closed.

Native OverlayNet provides VM egress networking. OpenSandbox network-policy requests are unsupported and rejected; a rejected request does not switch egress to deny-all.

The host account, daemon/firmware and prepared images must be trusted. Private state and same-UID IPC do not stop hostile same-UID or root code. Other local users may reach loopback publications; configure service authentication and host access controls. Secrets stay out of supervisor argv and the host environment but are kept in private records. No security-audit results exist; hostile multi-user isolation is out of scope.

## Relationship to native Jobs, the cache and the retired Cluster {#native}

A daemon sandbox ID is not a Job ID; do not pass it to `pvisor checkpoint`, `fork`, `apply` or `drop`. A daemon pause is a confirmed vCPU pause on the same live Attempt, not a sealed RAM/CPU/device/filesystem snapshot. Native Job file review, checkpoints and model capture continue through the `pvisor run` path: execution steps are in the [VM guide](../executors/vm.md), node-local Job file checkpoints in [checkpoints and forks](../fork-checkpoint.md), and model-request routing/capture in the [Gateway guide](../capture.md). A compatible standalone-rootfs, network-free VM Job can use execution checkpoints; check capability and blockers with `pvisor status --json`.

`pvisor cache prepare/publish/list/stat/read` remain independent commands, not daemon subcommands; see the [shared image cache reference](../../reference/shared-image-cache.md) for publication, backends, authentication and access. Daemon state is stored separately from native Jobs and cache stores; the Daemon API key authorizes neither the standalone cache nor node-runtime IPC.

The retired Cluster task/client SDK, Controller/Worker registration, placement, DAG, lease renewal, completion outbox and artifact retirement commands are not daemon interfaces. Do not feed old task JSON, `worker.toml`, Controller credentials or journals to the daemon; there is no automatic path from distributed task history to sandbox state. Retain old results before retiring a deployment, and use fresh state for the OpenSandbox profile.

## Troubleshooting {#troubleshooting}

| Symptom | Check |
| --- | --- |
| Startup fails before bind | Linux x86_64/KVM, short absolute state path, trusted manifests and a real delegated cgroup v2 |
| A v1 registry with records lacks the native `owner.json` | Retain old state/containers; use fresh native state, or clean all sandboxes through the old Podman daemon before switching |
| A v2 header lacks the native `owner.json`, even with an empty map | Restore the original ownership state; do not delete `records/`, reservations or forge markers |
| State directory busy | Another daemon holds the exclusive lock; do not delete locks or state to bypass |
| Image unavailable | Prepare the image locally first; automatic pull and image auth are unsupported |
| SDK create never becomes ready | Real execd initialization with `/ping`/`/ready`, egress `/healthz` and the 44772/18080 vsock bridge on guest CID 3; a plain rootfs is insufficient |
| External endpoint unreachable | TLS proxy routing, domain/protocol, `--public-endpoint` and the endpoint-provided headers |
| Admission still full after failure/pause | Failed/uncertain/paused retain hard reservations; query and confirm deletion |
| Expired sandbox still exists | Cleanup is best effort; check daemon uptime, the lifecycle lock and native cleanup errors |
| Policy/template/volume rejected | Unsupported options are rejected, never silently installed or emulated |

API errors carry `{code, message}` and responses carry `X-Request-ID`. Retain the HTTP status, request ID and redacted diagnostics; API success is not the command exit status. See [exit codes and errors](../../reference/exit-codes.md#daemon).
