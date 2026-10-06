# PolicyVisor (pVisor)

**Scaling autonomous agent execution.**

PolicyVisor (pVisor) manages execution for Agent CLIs, scripts, and automation
commands. The **p** stands for **Policy**: connect requested capabilities,
effective runtime controls, and reviewable results.

Owns one Job, its internal Run record and Attempts, capability admission,
filesystem Effects, execution placement, and the host CLI (`pvisor`). It can
place Jobs on host, container, and libkrun VM executors while preserving one
Run contract.
It is not an Agent framework, an OCI runtime, or an operating system.

OverlayFS, OverlayNet, Gateway, and AgentCtl are pVisor runtime drivers.
`pvisor-core` defines Operations, Events and cross-component contracts. This crate
owns Session lifecycle, scheduling, policy adaptation and execution. Job lifecycle
commands are built into `pvisor`; local node lifecycle, cache and memory-pool
tools are grouped under `pvisor service`, while TUI and replay are optional Job
frontends found beside it. The old Cluster Controller/Worker control plane and
`pvisor-worker` executable have been retired from this crate.
Guest injection uses the core `pvisor` execution runtime.

```mermaid
flowchart TD
    Entry[CLI / PVisor API] --> Session[Session: one Attempt]
    Core[pvisor-core contracts and policies] -.-> Session
    Session --> Executor[Host / OCI container / libkrun VM]
    Session --> Drivers[OverlayFS / OverlayNet / optional Gateway]
    Session --> Records[Run record / Run Bundle / optional Event Journal]
    Records --> Review[status / inspect / apply / drop]
```

| Product area | Current responsibility |
| --- | --- |
| Run lifecycle | One logical `Run`, currently one `Attempt` per execution, cancellation, deadlines, terminal publication, and parent lineage |
| Agent control | Optional authenticated AgentCtl v1 for Sessions, client state, directives, and cooperative quiescence |
| Capabilities | Models, tools, filesystem read/write, network, secrets, subprocess, and resources, with evidence recorded per dimension |
| Filesystem effects | Copy-on-write staging, classified review, logical checkpoint/fork, repeated selective apply, terminal apply/drop, and an apply ledger |
| Network and model access | Gateway capture plus OverlayNet policy; enforcement strength depends on executor and is never inferred from a product label |
| Execution placement | Host process, native OCI container executor, or libkrun VM using an OCI image, prepared rootfs, or Linux host rootfs |
| Evidence | Run Bundle, lifecycle events, capability enforcement, filesystem changes, network counters, AgentCtl observations, output, and artifact references |

With `--stage PATH`, the product loop is `RunSpec → admission → Attempt →
RunResult + private Run Bundle + staged Effects → review/apply/drop`. Ordinary
host Jobs without staging write through to the workspace. `--safe` and `--ask`
retain the workspace stage in Job storage by default; `--stage PATH` selects
another location. HOME and VM rootfs writes have separate lifetimes; see
[staging and storage](../../docs/src/en/reference/cli.md#staging-and-storage).
Capture is a Gateway capability, not a second product.

## Local service boundary

`pvisor service run --config service.toml` supervises local resource owners;
`status`, `restart ROLE` and `stop [--role ROLE]` manage their lifecycle. The
`node` role shares immutable image mounts and snapshot RAM while retaining
same-user authorization, compatibility checks and active-pin restart/stop
fences. The optional `pool` role serves the experimental Apple Silicon cold-page
pool. `service cache` and `service memory-pool` dispatch their installed tools.

A minimal configuration is:

```toml
state = '.pvisor/services'

[node]
cache_backend = 'filesystem'
cache_location = 'cache'
snapshot_roots = ['snapshots']
```

Paths resolve relative to the configuration file; node state/socket paths resolve
under service state. Linux deployments can set `cgroup_root = ':self:'` **before
`[node]`**, using a real delegated unified cgroup v2 hierarchy. Service limits
remain installed before child execution, with positive per-role `[limits.node]`
(and optional `[limits.pool]`) budgets. Without delegation, process ownership is
not evidence of kernel-enforced limits. Resource owners are not transparently
recoverable after a crash; drain pins before restart/stop. Node pin checks do not
prove the pool has no direct VM clients. Drain those clients separately: SIGINT
puts the pool into draining, and a stop timeout can leave it rejecting new clients
while retaining existing data owners.

`controller`, `worker` and `workers` configuration keys are rejected, including
empty legacy sections/lists. There is no implicit migration to a new scheduler.
`pvisor service daemon ...` passes arguments unchanged to a trusted, separately
installed `pvisor-daemon` beside `pvisor`; it does not add a daemon role to this
configuration, link a daemon dependency, or imply native executor integration.

Cluster-only tests and fixtures are retired. Local service tests retain delegated
limits, cleanup, shared image pins and shared RAM/COW ownership fences. The
mixed service gate's two-guest private-write checks now use native local Jobs
with retained node image pins, without a Controller or Worker. Native
Job checkpoint/fork/suspend/resume tests and snapshot/cache/rootless tests remain;
`native_cpu_qos` preserves the real anchor lifecycle gate using `pvisor` itself.

### Build and validation boundary

The current root `service-build` already selects local binaries and builds the
daemon separately; the old `test-service` / `test-service-vm` recipes are absent.
The ignored local gates need explicit selection in a future root test recipe;
their environment requirements remain in the test annotations. Install
`pvisor-daemon` beside `pvisor` if companion dispatch is desired. The root
Cluster alias and daemon legacy feature have been removed; the daemon builds
independently, without a Rust dependency on this crate. Packaging includes both
executables, but common installation does not establish native backend integration.
See the [daemon boundary](../pvisor-daemon/README.md).

## Develop

```bash
just build release          # release build + macOS Hypervisor signing
just build    # debug build + macOS signing
just test pvisor
just examples
```

On macOS, source builds that use HVF must be signed. `just build release` does this;
the equivalent entitlements file is `macos-hypervisor.entitlements`. The embedded
`pvisor-guest` supervisor is built as a static Linux musl ELF
with Rust's bundled linker. On Apple Silicon, install its stdlib once with
`rustup target add aarch64-unknown-linux-musl`. It launches workloads directly,
without a shell helper, and reports their exit codes through libkrun's root filesystem ioctl.

The vendored libkrun is built only as an `rlib` and statically linked into
`pvisor`; no `libkrun.so` or `libkrun.dylib` is required. The separate guest
kernel is embedded in Linux static musl builds; macOS loads `libkrunfw.5.dylib`
at runtime. Linux source builds require Zig, `cargo-zigbuild`, and
`rustup target add x86_64-unknown-linux-musl`. Use `just build` so target
selection and kernel preparation match release builds.

## Links

- [Operation and Event](../../docs/src/zh/design/operations-events.md): operation requests, actual rewrites,
  VM/Overlay placement, execution facts and boundary observations.
- [The PolicyVisor model](../../docs/src/zh/start/what-is-pvisor.md)
- [Get started](../../docs/src/en/start/first-run.md)
- [Isolation architecture](../../docs/src/zh/design/isolation.md)
- [Gateway architecture](../../docs/src/zh/design/gateway.md)
- [OverlayNet architecture](../../docs/src/zh/design/overlaynet.md)
- [pVisor CLI](../../docs/src/en/reference/cli.md)
- [System architecture](../../docs/src/zh/design/architecture.md)
- [`pvisor-overlayfs`](../pvisor-overlayfs/README.md)
- [`pvisor-overlaynet`](../pvisor-overlaynet/README.md)
- [`pvisor-gateway`](../pvisor-gateway/README.md)
- [`pvisor-core`](../pvisor-core/README.md)
