# Daemon and independent cache entry points

Run `pvisor-daemon` directly for the OpenSandbox lifecycle API and its optional memory pool. Use `pvisor-cache` independently for OCI cache publication and access. Sandbox supervisors embed the native runtime; node resource protocols remain separate runtime facilities.

## Start the daemon directly {#daemon}

Follow [installation and startup](index.md#install), then invoke the standalone executable:

```bash
pvisor-daemon protocol
pvisor-daemon serve --help
```

`OPEN_SANDBOX_API_KEY` configures lifecycle authentication. Listener, public endpoint, persistent state and admission budgets are daemon options. The daemon does not read native `RunConfig` or `RunSpec`. Keep daemon state separate from native Job and cache storage.

VM-only supervisors retain RunHandles across API restart. Pause/resume uses acknowledged live vCPU controls on the same Attempt, not cgroup freeze or a snapshot. Add `--memory-pool` to `serve` to enable the default-off daemon-owned pool. The daemon starts or reuses a detached `pvisor-daemon memory-pool --directory DIR` component under its private state directory; the component expects the persisted pool configuration. See [pool activation and budgets](index.md#memory-pool).

## Independent cache and node runtime {#native}

```bash
pvisor-cache --help
pvisor-daemon memory-pool --help
```

`pvisor-cache prepare/publish/serve/list/stat/read` remain independent commands, not daemon subcommands. Use the [shared image cache reference](../../reference/shared-image-cache.md) for publication, backends, authentication and access.

The node runtime owns immutable environment mounts and snapshot RAM, with authorized stores, compatibility checks and connection pins. The daemon has no automatic node acquire/release adapter, RAM restore or template API. Embedded native callers retain explicit ownership; stop consumers before their backing owners.

## Supervision and external access {#supervision}

Run the daemon and cache under their owning host accounts with private state and protected credentials. Configure host restart policy and resource caps separately from sandbox admission sums. The detached pool survives API restart, not pool-process failure or host reboot. Pool loss fails dependent VMs; drain them before replacing pool state. The pool is outside individual sandbox cgroups, so reserve host headroom for its pages, indexes and process overhead.

Daemon sandbox execution requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and delegated cgroup v2 with CPU/memory/PID controllers and `cgroup.kill`. There is no host, OCI command or registry-pull fallback. Use TLS and the correct `--public-endpoint HOST:PORT` for external access. Daemon API keys do not authorize the independent cache or node runtime IPC.

Historical Controller/Worker and service-supervisor acceptance records retain their original deployment scope; they do not validate the current daemon. See [runtime boundaries](boundaries.md) and [operations](operations.md).
