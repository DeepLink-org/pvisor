# pVisor experimental features

Register implementable, verifiable capabilities that support higher execution density and lower supervision cost. Designs, implementation plans and validation records live here. Feature code stays in existing crates, and measurements stay with the benchmark registry.

## Capability registry

| ID | Capability | Status | Goal |
|---|---|---|---|
| EXP-001 | [vCPU idle observation and automatic RAM offload](vcpu-idle-offload/index.md) | **Observation prototype stage** (M0; KVM measured, HVF not validated) | Find VM waiting windows without changing the Agent or guest kernel; offload and restore on host events/deadlines |
| EXP-002 | [Guest kernel filesystem cache and deferred backend binding](guest-stage-cache/index.md) | **Design stage** (RFC, not implemented) | Complete valid cached metadata operations and ordinary read-only open locally; retain VMM authority over stage, data supply and durability |
| EXP-003 | [Agent asynchronous I/O and completion queues](agent-io-completion/index.md) | **Restricted PoC** (M0 and host write-buffer foundation implemented; complete IOCP pending) | Host-owned downloads, model calls and pending write bytes overlap backend I/O with guest RAM offload; the Attempt owner restores and delivers results |

## CLI feature entry points

```bash
pvisor feature
pvisor feature list --json
pvisor run --executor vm --feature workload-aware-memory-offloading -- /bin/sleep 10
```

The runtime registry in `crates/pvisor/src/features.rs` defines stable names, stages, defaults and descriptions. `--feature NAME` accepts repeated or comma-separated values. Features are disabled by default; unknown names are rejected. Arguments after `--` belong to the workload. Query output `enabled` reflects registry defaults plus CLI selections; it does not load Run configuration or validate execution.

The first registered feature is `workload-aware-memory-offloading`, stage `experimental`, default `false`. It enables EXP-001 M0 observation. Automatic offload is not enabled. The effective executor must be VM; supported integration platforms are Linux x86_64/KVM and Apple Silicon macOS/HVF, with HVF runtime validation outstanding. Configuration accepts `workload-aware-memory-offloading = true` under `[features]`; CLI enable overrides configuration false, while omission preserves configuration. There is no CLI disable or persistent feature-enable operation. Existing cold RAM parameters remain outside this migration.

Feature selection travels explicitly through Host requests, RunConfig and runner spec. The native runner calls `VcpuObservationControl`. New experimental features enter the registry after real integration and support checks; empty switches are not registered. See the [CLI reference](../../reference/cli.md#features).

## Stages and promotion

- **Design stage:** defines the problem, existing foundations, gaps, scope, failures and acceptance gates; implementation remains pending.
- **Observation prototype stage:** collects opportunities and rejection reasons without automatically changing execution state.
- **Restricted PoC:** disabled by default, explicitly enabled, with a complete loop for named platforms/profiles.
- **Experimentally usable:** passes correctness and fault gates, with traceable artifacts and declared scope.
- **Stable or retired:** reviewed for API, documentation, delivery and maintenance; retired if net benefit is absent or maintenance cost is excessive.

Record each platform separately. Promotion includes source/artifact identity, commands, passes, failures, skips and uncovered cases; one platform's PASS does not establish another's support.

## Directory rules

1. Give each capability a stable ID and descriptive directory. Its single `index.md` combines current status, design, implementation plans and validation records.
2. Separate design from measurement, implemented from proposed interfaces, and heuristics from deterministic guarantees.
3. Keep features disabled by default. Unvalidated options do not enter official CLI examples or claims of delivered capability.
4. Reuse Runtime, control owners, CPU/device quiescence, storage and evidence models; preserve existing terminal-state ownership.
5. Follow `crates/pvisor-vm/README.md` for public VM APIs. State backend scope and distinguish KVM/HVF and mapping profiles.
6. Read directory READMEs before implementation. Follow `benchmark/README.md` and register or explicitly reuse the measurement protocol; store raw data in `.data/`. Manual-operation results do not establish automatic-policy benefit.
7. Follow `tools/semspec/DESIGN.md`; keep PASS separate from human approval. AI does not edit real approval ledgers/snapshots or weaken claims/checks/xfail.
8. Keep credentials, user trajectories, VM images and build artifacts outside this directory.

## First capability

EXP-001 first uses a host-driven route: backends expose vCPU observations, and the host aggregates waiting windows and arbitrates resource actions. Guest PV cooperation remains an optional extension.

EXP-001 has a default-disabled M0 observation API, backend integration points and real VM experiments. KVM measurements found no reliable waiting signal; automatic offload and wake/deadline integration remain pending. See [Initial implementation and validation](vcpu-idle-offload/index.md#validation).
