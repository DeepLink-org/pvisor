# Daemon runtime and integration boundaries

Choose `pvisor-daemon` for node-local image sandbox lifecycle, or `pvisor run` for native Job execution and file review. Their runtime, state and capabilities are separate.

## Prepared-image contract {#images}

The current daemon backend invokes a trusted external rootless Podman executable on Linux. Images must already exist locally: creation uses `--pull=never`, with no image-auth or registry-pull API.

The image ENTRYPOINT must supervise the requested workload argv **and** real OpenSandbox 1.1.0 execd and an egress service on container ports 44772 and 18080. Requests replace the image CMD. The wrapper must execute the arguments faithfully without shell interpolation, forward signals and reap children. It must complete runtime initialization and configure service authentication itself.

The daemon does not inject `/execd`, perform its initialization handshake, or synthesize command/SSE/file responses. Readiness requires execd `/ping` and `/ready`, plus egress `/healthz`. Python SDK initialization resolves both endpoints even without a network policy request.

The pinned upstream default egress sidecar requires iptables redirects and cannot run unchanged under `cap-drop=ALL`. A genuine capability-free deployment is needed; no end-to-end image recipe has been validated. Adding `NET_ADMIN`, using privileged containers, or replacing readiness with a fake responder is not a supported workaround. Ordinary SDK creation fails readiness until the image contract is fulfilled.

## Isolation and resource limits {#isolation}

Creation installs private namespaces, no-new-privileges, `cap-drop=ALL`, and CPU/memory/swap/PID limits; container resource configuration is checked. Admission conservatively sums hard limits, including paused, failed and uncertain sandboxes. Observed idleness does not authorize implicit overcommit.

Rootless slirp4netns disables host-loopback access, but **does not deny all egress**. Requested network policies are rejected. This backend shares the host kernel, is not a VM-grade boundary and has not been security-audited. Trust the host account, Podman configuration/hooks and prepared image.

Native loopback service ports may be reachable by other local users. Daemon endpoint authentication does not protect those native mappings; use real service authentication and host network controls before considering untrusted multi-user deployment. Workload secrets are not put in Podman argv, but remain visible to trusted host/runtime owners.

## OpenSandbox profile {#profile}

Compatibility is pinned to OpenSandbox **1.1.0**, tag `release-1.1.0`, commit `b1a29cf93a823a95913f7943010febb3f29de05c`. It is a partial API profile, not full OpenSandbox or unmodified SDK end-to-end conformance.

| Capability | Current behavior |
| --- | --- |
| Image creation, list, inspect, delete | Local sandbox lifecycle; create accepts argv/env/metadata, CPU/memory limits and optional TTL |
| Pause / resume | Native cgroup freeze/unfreeze, not a VM checkpoint |
| Expiration renewal | Future RFC3339 deadline extending an existing TTL |
| Endpoints | Daemon-routed execd 44772 and egress 18080 only |
| Commands, files, health, metrics | Streamed to real upstream services in the prepared image |
| Snapshots, templates/pools, metadata mutation, hooks | Unsupported |
| Network policy, credential proxy, secure access, volumes, image auth | Unsupported creation options are rejected |
| Arbitrary application ports, signed endpoints, WebSocket, CONNECT | Unsupported |
| Stage/apply, checkpoint/fork, VM/offload | Not integrated with this daemon backend |

## Native VM, checkpoints and Gateway {#native}

Native `pvisor run --executor vm` retains its KVM/HVF and rootfs requirements. It does not use this Podman adapter. The [VM guide](../executors/vm.md) explains execution; [checkpoint and fork](../fork-checkpoint.md) covers local Job file checkpoints. Compatible owned-rootfs, no-network VM Jobs can use execution checkpoints; inspect `pvisor status --json` for capability and blockers, and follow the [CLI execution-checkpoint contract](../../reference/cli.md#full-vm-execution-checkpoints).

A daemon pause is a cgroup freeze, not a sealed RAM/CPU/device/filesystem snapshot. Daemon sandbox IDs are not Job IDs: do not pass them to `pvisor checkpoint`, `fork`, `apply` or `drop`. Native snapshot storage, node owners, shared image cache and the experimental macOS cold-page pool remain native facilities, not daemon templates or pools.

For model request routing and capture in native Jobs, use the [Gateway guide](../capture.md) and [Agent integration](../agents/index.md). The daemon has no old Worker `gateway.routes` profile or integrated pVisor credential proxy/capture contract. Prepared services and applications own any model access; external API effects cannot be undone by file review.
