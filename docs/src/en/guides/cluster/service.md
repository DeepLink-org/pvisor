# Unified service entry and shared node resources

`pvisor service` manages the Controller, node resource service, Workers and optional cold RAM pool from one TOML. Roles retain separate processes: restarting Controller does not also terminate Workers, backing owners or pool sessions. Existing `pvisor-cluster`, `pvisor-worker` and cache commands remain available.

This page starts a resource-capped trusted host example, which provides no sandbox isolation. VMs retain the environment, task and resource contracts in the [VM guide](vm-and-gateway.md), with this node service added.

## Build and verify {#verify}

On Linux x86-64 with a working systemd user manager and cgroup v2, build from the repository root:

```bash
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 just service-build
```

This builds `pvisor`, `pvisor-cache`, `pvisor-cluster`, `pvisor-worker` and `pvisor-memory-pool`, including optional Gateway support. Install them together in a trusted directory; service does not search PATH for companions.

The explicit gate uses real HTTP/Unix services, FUSE RAM backing and delegated cgroups, without guest VMs:

```bash
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 NEXTEST_TEST_THREADS=1 just test-service
```

Missing FUSE or a user manager causes failure, rather than accepting simulation or skips. The gate checks independent Controller restart, preserved execution identity, one RAM inode across stores, private COW writes, active-pin stop protection and actual kernel CPU/memory/swap caps, plus failed-fixture unit cleanup when its proxy exits early. It does not replace native VM boot/restore hardware acceptance or S1–S5 performance experiments.

A separate gate exercises two real KVM guests, each 128 MiB/one vCPU, under separate 512 MiB/0.5-core Worker cgroups and a 2 GiB/one-core service unit. It requires `/dev/kvm`, FUSE and matching installed firmware; set its directory explicitly:

```bash
export PVISOR_TEST_LIBKRUNFW_DIR="$HOME/.cache/pvisor/firmware/5.5.0/linux-x86_64"
systemd-run --user --scope --quiet \
  -p MemoryMax=3G -p MemorySwapMax=0 -p CPUQuota=100% \
  env JUST_TEMPDIR=/tmp CARGO_BUILD_JOBS=1 NEXTEST_TEST_THREADS=1 \
  PVISOR_TEST_LIBKRUNFW_DIR="$PVISOR_TEST_LIBKRUNFW_DIR" just test-service-vm
```

This gate checks that two Workers pin one environment owner, private-upper writes stay isolated, and the original tasks finish correctly after Controller restart. It checks role caps and zero OOM/OOM-kill after execution. This validates shared environments, not physical-sharing curves for restored VM RAM. Replace the directory for another firmware version/install location; missing dependencies cause failure.

On 2026-10-05 Linux x86-64, the four no-guest gates and the separate two-real-guest gate passed. The sample TOML management workflow also passed with only private state, random port and unit-name substitutions; actual Controller/Node/Worker caps were 128/256/512 MiB and 0.25/0.5/0.5 core, with zero swap/OOM/OOM-kill. This establishes deployment and correctness, not S1–S5 performance gains.

## Service tool namespace {#tools}

Cluster clients/Controller, Worker, cache and pool now live under service, with their original arguments following the tool name. `run/status/restart/stop` manage a deployment; tool subcommands operate the corresponding protocol or independent role, retaining component resource ownership.

```bash
pvisor service cluster --help
pvisor service worker --help
pvisor service cache --help
pvisor service memory-pool --help
pvisor help service cluster submit
```

## Launch and management {#launch}

Use `examples/cluster/service.toml`. Paths are relative to the TOML; `node.state` is relative to service state and `node.socket` to node state. The example uses checkout `.pvisor/services`. Each deployment needs exclusive state and an unused port 19800.

```bash
export PVISOR_CLUSTER_TOKEN=$(openssl rand -hex 24)
export PVISOR_CLUSTER_WORKER_TOKEN=$(openssl rand -hex 24)

systemd-run --user --unit=pvisor-service-demo --collect \
  -p Delegate=yes -p MemoryMax=2G -p MemorySwapMax=0 -p CPUQuota=200% \
  --setenv=PVISOR_CLUSTER_TOKEN --setenv=PVISOR_CLUSTER_WORKER_TOKEN \
  --working-directory="$PWD" \
  "$PWD/target/debug/pvisor" service run --config "$PWD/examples/cluster/service.toml"

target/debug/pvisor service status --config examples/cluster/service.toml
target/debug/pvisor service restart --config examples/cluster/service.toml controller
```

`cgroup_root = ":self:"` requires a dedicated delegated cgroup. The supervisor moves itself into a `supervisor` subgroup, creates a subgroup per role, and installs `memory.max`, `memory.swap.max=0` and `cpu.max` before exec. Insufficient permissions/controllers cause failure, without retrying uncapped.

An exclusive delegated absolute cgroup path is also supported. Remove `cgroup_root` only for an explicitly uncapped local preview, reported as `kernel_limits: false`. Resource reservations and cache payload caps are not whole-group memory limits.

The example has one Worker and one slot: Controller gets 128 MiB/0.25 core, node service 256 MiB/0.5 core, and the Worker process tree 512 MiB/0.5 core, under a separate 2 GiB unit cap. Tasks still need memory, CPU-time, output and wall-clock limits from the existing guide. VMs use 128 MiB/one vCPU, with at most four live guests across the session, including source VMs.

Stop a role or the entire deployment:

```bash
target/debug/pvisor service stop --config examples/cluster/service.toml --role controller
target/debug/pvisor service restart --config examples/cluster/service.toml controller
target/debug/pvisor service stop --config examples/cluster/service.toml
```

Whole-deployment stop drains/stops Workers before the pool, node owners and Controller. A role failing to drain within 30 seconds returns an error and retains data owners, rather than killing them to exit the entry. A pool stop signal refuses new sessions and waits for existing sessions to disconnect. Do not force-upgrade or SIGKILL a pool with dependent VMs.

## Environment and restored backing integration {#owners}

Set `[node].cache_backend` to `"filesystem"` or `"s3"` and `cache_location` to an already-published cache. Keep `[environments] enabled = true` in the Worker profile. Managed Workers automatically receive `--node-socket`; node ownership shares immutable layer mounts while each task retains a private upper. FS/S3 native cache requires no additional cache daemon; OCI preparation/publication keeps its existing cache commands.

Restoration uses `vm.node_socket` for node RAM ownership. Managed Workers configure it automatically; standalone SDK/profiles can set it explicitly. Each acquire validates publication and compatibility. Identical sealed IDs in authorized stores reuse one read-only inode, while guest `MAP_PRIVATE` modifications remain private.

Managed Worker state is automatically an authorized snapshot root. Shared checkpoint directories or `snapshot_filesystem_pool` outside it require explicit absolute `[node].snapshot_roots` entries. A caller-supplied path does not authorize arbitrary stores.

Connection pins remain until normal native teardown. Worker failure/disconnection releases session references; lost node ownership can fail later FUSE faults, with no live reconnect/takeover support yet. Independent Controller recovery does not imply lossless Worker/node-service recovery.

## Budgets, warming and observation {#budget}

| Configuration | Bound |
|---|---|
| `max_owners` / `warm_owners` | Combined environment/RAM owner count; bounded strong warm references, retiring only idle owners under pressure |
| `max_sessions` | Active pin connections; stats does not consume pin capacity |
| `max_preparations` | Concurrent owner fetching/validation/preparation; same-key mount creation is serialized |
| `max_cache_bytes` | Aggregate retained payload of same-process image blocks, metadata-page caches and Linux decoded RAM caches |
| `limits."ROLE"` | With delegation, kernel memory/CPU caps for the role and all descendants |

Insufficient cache allowance still permits validated reads without retaining new hot data. Payload accounting excludes complete metadata, temporary read/decode buffers, external Arc holders and kernel page cache; whole-group kernel caps and observation cover those costs. macOS separate RAM pagers retain local caches outside the Node-process counter and need separate acceptance.

Status includes role PIDs/exits, `kernel_limits`, node active pins/live owners and cache bytes/limit/misses. `cache_misses` counts denied budget reservations, including retries, rather than origin-read misses. Worker readiness means its process started; confirm registration with the Controller workers API. Logs live in `STATE/logs/`. Startup saves resolved `active-config.toml`, reused by role restart; stop and run again to apply source configuration changes.

For multiple hosts, omit `[controller]` in node configuration and set Worker `url`. Control-only hosts use `[node] enabled = false`. Optional `[pool]` is Apple Silicon only, serving VM Workers from a separate process under its existing session-reference/fail-stop contract.

This integration adds no per-page WAL, automatic prefetch, dynamic cache scheduling hints, physical RAM sharing across nodes or lossless pool migration. See [service consolidation design](../../design/cluster/server-consolidation.md) for architecture and remaining acceptance.
