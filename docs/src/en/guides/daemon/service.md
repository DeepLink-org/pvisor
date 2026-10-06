# Daemon and native service entry points

Run `pvisor-daemon` independently for the OpenSandbox lifecycle API. Keep native node, image-cache and memory-pool services with the native pVisor Jobs that use them. Sandbox supervisors embed the native runtime, but API state and node resource ownership remain separate.

## Start the daemon directly {#daemon}

Follow [installation and startup](index.md#install), then invoke the standalone executable:

```bash
pvisor-daemon protocol
pvisor-daemon serve --help
```

`OPEN_SANDBOX_API_KEY` configures lifecycle authentication. Listener, public endpoint, persistent state and admission budgets are daemon options; `RunConfig`, `RunSpec`, native service TOML and old Worker profiles are not daemon configuration. Keep its state separate from native Job/node/cache/pool state.

VM-only supervisors embed pVisor and retain RunHandles across daemon restart. Pause/resume uses acknowledged live vCPU controls on the same Attempt, not cgroup freeze or a snapshot. Node owners/cold pools are not automatically acquired. Cargo and the executable integrate native runtime construction and hidden supervisor dispatch; synchronous internal VM dispatch runs before Tokio. See [startup](index.md#start).

## Native node, cache and memory pool {#native}

The native service tools remain available:

```bash
pvisor service --help
pvisor service daemon --help
pvisor service cache --help
pvisor service memory-pool --help
```

The native supervisor's `run/status/restart/stop --config FILE` manages its configured `node` and optional `pool` roles. Its TOML accepts `state`, `node`, `pool`, `cgroup_root` and per-role `limits`; it has no daemon role, and old Controller/Worker fields are rejected. The node resource service owns shared immutable environment/RAM backing for native callers; it is not a replacement Controller or a global scheduler. Retain authorized snapshot roots, pin ownership and resource budgets when configuring it. Native consumers must explicitly use the appropriate node socket/profile; this is not automatic daemon integration.

Use the [shared image cache reference](../../reference/shared-image-cache.md) for OCI cache publication and access. The [memory-sharing integration](../../design/memory-optimization/proof-of-concept.md#v1-integration) describes the experimental Apple Silicon pool. Keep owners/pools alive while dependent VMs hold pins; losing an owner can fail later faults, and stopping a pool can fail dependent VMs. Stop consumers before their data owners rather than killing owners to force a supervisor exit.

`pvisor service daemon` is the companion entry point for builds with the new CLI wiring and an installed adjacent `pvisor-daemon`. Check that build's `pvisor service --help`; installing a standalone daemon does not add a subcommand to an older CLI. The direct executable remains the deployment path above. Do not use retired `service cluster` / `service worker`, `[controller]` / `[[workers]]` configuration or Cluster tokens for the new daemon.

## Supervision and external access {#supervision}

Run each service under its owning host account with private persistent state and protected credentials. Install trusted companions alongside the matching native CLI when using companion dispatch; do not assume PATH discovery or wheel inclusion. Configure service restart policy and host resource caps separately from sandbox admission sums.

Requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and a writable delegated cgroup v2 hierarchy with enabled CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller writes and the KVM API; there is no host, OCI command or registry-pull fallback. Use TLS and the correct `--public-endpoint HOST:PORT` for external access. Daemon keys do not authorize node/cache/pool services.

Old Controller/Worker restart, drain, registration and VM-sharing acceptance records describe the retired deployment, not validation of this daemon or its companion wiring. See [runtime boundaries](boundaries.md) and [operations](operations.md) for the new supported scope.
