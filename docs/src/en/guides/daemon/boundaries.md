# Daemon runtime and integration boundaries

Choose `pvisor-daemon` for node-local native VM sandbox lifecycle, or `pvisor run` for the public Job/file-review workflow. The daemon embeds native execution but keeps separate API identities, state and capabilities.

## Prepared-image contract {#images}

Images are trusted local `images_dir/<key>.json` manifests, not registry references. Fields are absolute independent Linux `rootfs` (never host `/` or overlapping daemon state), absolute guest bootstrap `entrypoint` argv, optional `cmd`, optional `env` and optional absolute firmware `library_dir`. Requested workload argv (or `cmd` if empty) is appended to `entrypoint`; request env overrides manifest env, without host environment inheritance or shell interpolation.

The long-lived guest bootstrap must supervise workload, real OpenSandbox 1.1.0 execd and egress, initialize/authenticate services, forward signals and reap children. It must expose byte-transparent AF_VSOCK listeners on guest **CID 3**, ports **44772/18080**, bridging to real services. Supervisor loopback TCP publications connect through private Unix sockets and native vsock forwarding. A stock rootfs or sleeping process is insufficient.

Create, Inspect of a Running VM and resume readiness checks require genuine HTTP 200 execd `/ping`, `/ready` with JSON `initialized: true`, and egress `/healthz` through the bridges, with bounded bodies. The daemon does not inject/initialize execd or synthesize command/SSE/file responses. Python SDK initialization resolves both endpoints even without network policy.

The bootstrap and image recipe are **not supplied or end-to-end validated**. The old container `cap-drop=ALL` restriction does not describe this native VM backend; an upstream image name is not a native bootstrap/vsock adapter. No fake readiness, SDK-conformance or density evidence is provided.

## Isolation and resource limits {#isolation}

Requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and a writable delegated cgroup v2 hierarchy with enabled CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller writes and the KVM API; there is no host, OCI command or registry-pull fallback.

The native supervisor embeds `pvisor::PVisor` with only `VmExecutor` and holds its RunHandle in a detached subprocess. The sandbox cgroup caps the entire supervisor/VMM/helper tree: aggregate CPU rate **10–8000 millicores**, hard memory, zero swap, `pids.max=512` and group OOM. vCPU count rounds quota up to whole CPUs (at most 8); guest RAM rounds down to MiB, while the hard memory cap also includes host overhead. Full limit/readiness checks remain at create, Inspect and resume; endpoint lookup authenticates live Running state and deletion fences without repeating full health/cgroup checks per data request. See [service access](../../design/daemon/lifecycle.md#endpoints). Paused, failed and uncertain records retain conservative admission charges.

The launch callback joins the identity-bound cgroup **before exec**, using a pre-opened `cgroup.procs` FD under the owner lock and checking started/deletion/tombstone markers. Supervisor startup verifies membership instead of moving an already-running Tokio process; supervisor/Tokio allocations and subsequent VM/helper children are charged inside the sandbox budget. Direct hidden-command invocation outside that cgroup fails closed.

Native OverlayNet supplies VM outbound networking; OpenSandbox network-policy requests remain unsupported and rejected, not deny-all egress. Trust the host account, daemon/firmware and prepared image; private state and same-UID IPC do not protect against hostile host-UID/root code. Other local users may reach loopback publications, so real service authentication and host controls remain necessary. Secrets stay out of supervisor argv and host environment but persist in private records. No security audit or hostile multi-user assurance is claimed.

Private IPC authenticates same-UID peers plus owner, sandbox ID, generation and secret token; durable identity binds boot ID and cgroup device/inode. IDs are never reused or relaunched. Lost IPC is uncertainty, not Missing or cleanup proof. Durable deletion intent and the supervisor exclusive lock fence late launch; cleanup uses identity-bound `cgroup.kill`, never a persisted PID or PID-based kill, and confirms an empty cgroup plus released lock before capacity release. Replaced or missing same-boot cgroups without durable tombstone proof do not prove absence.

## OpenSandbox profile {#profile}

Compatibility is pinned to OpenSandbox **1.1.0**, tag `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`. It is a partial API profile, not full OpenSandbox or unmodified SDK end-to-end conformance.

| Capability | Current behavior |
| --- | --- |
| Image creation, list, inspect, delete | Local sandbox lifecycle; create accepts argv/env/metadata, CPU/memory limits and optional TTL |
| Pause / resume | Acknowledged live vCPU pause/resume on the same Attempt, not a checkpoint |
| Expiration renewal | Future RFC3339 deadline extending an existing TTL |
| Endpoints | Daemon-routed execd 44772 and egress 18080 only |
| Commands, files, health, metrics | Streamed to real upstream services in the prepared image |
| Snapshots, templates/pools, metadata mutation, hooks | Unsupported |
| Network policy, credential proxy, secure access, volumes, image auth | Unsupported creation options are rejected |
| Arbitrary application ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| Stage/apply, checkpoint/fork, offload | Not integrated with this daemon backend |

## Native VM, checkpoints and Gateway {#native}

Native `pvisor run --executor vm` retains its KVM/HVF and rootfs requirements. The daemon uses the native VM executor, but does not expose these Job APIs. The [VM guide](../executors/vm.md) explains execution; [checkpoint and fork](../fork-checkpoint.md) covers local Job file checkpoints. Compatible owned-rootfs, no-network VM Jobs can use execution checkpoints; inspect `pvisor status --json` for capability and blockers, and follow the [CLI execution-checkpoint contract](../../reference/cli.md#full-vm-execution-checkpoints).

A daemon pause is acknowledged vCPU pause on the same live Attempt, not a sealed RAM/CPU/device/filesystem snapshot. Daemon sandbox IDs are not Job IDs: do not pass them to `pvisor checkpoint`, `fork`, `apply` or `drop`. Native snapshot storage, node owners, shared image cache and the experimental macOS cold-page pool remain native facilities, not daemon templates or pools.

For model request routing and capture in native Jobs, use the [Gateway guide](../capture.md) and [Agent integration](../agents/index.md). The daemon has no old Worker `gateway.routes` profile or integrated pVisor credential proxy/capture contract. Prepared services and applications own any model access; external API effects cannot be undone by file review.
