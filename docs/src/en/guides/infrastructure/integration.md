# Choose a pVisor integration

Choose the CLI for native Jobs and file review, the Runtime when a Rust application needs to own execution lifecycle, the daemon API for HTTP sandbox management, or the shim for an existing containerd call chain. Decide the calling protocol and state owner before checking the capabilities required.

## Entry points and ownership {#entry-points}

| Entry point | Intended caller | Identity and lifecycle managed | Prerequisites |
| --- | --- | --- | --- |
| `pvisor` CLI | Developers, scripts, CI, and evaluators integrating through subprocesses | Native Job/Attempt; commands manage execution, review, and state operations | Prepare the selected host, OCI, or VM executor; associate tasks through machine-readable output |
| Rust `pvisor` Runtime | Rust applications requiring direct execution and cleanup control | `PVisor`, `RunHandle`, and Job service; the application owns the runtime and completes waiting and cleanup | Pin crate versions, configure executors/drivers, and follow lifecycle and persistence contracts |
| `pvisor-daemon serve --sandbox` | Platform services using an HTTP lifecycle API | Daemon sandbox ID; local admission, TTL, supervisors, and persistent state | Linux x86_64, KVM, delegated cgroup v2, trusted prepared images, and guest service bridges |
| `containerd-shim-pvisor-v2` | containerd / CRI call chains | containerd task / exec identity and the corresponding task lifecycle | Linux and containerd configuration; VM execution also needs the `vm` build feature and KVM |

The CLI is a calling interface; an executor is an execution environment. Choosing the CLI does not limit execution to host processes. API identities are not interchangeable: daemon sandbox IDs cannot be passed to native Job commands, and containerd task IDs do not automatically become Job selectors.

## Current capability matrix {#capabilities}

Conditional support requires checking the executor, profile, and configuration. A runtime mechanism is not necessarily exposed by every frontend.

| Capability | Native CLI | Rust Runtime | Daemon Sandbox API | containerd shim |
| --- | --- | --- | --- | --- |
| Execution and cancellation | Job commands | Application owns execution handles and lifecycle | Create, inspect, delete, and TTL | Task/exec lifecycle |
| File staging, review, apply/drop | Available with explicit staging | Integrate through configuration and Job service | Not exposed | Native Job review interface not exposed |
| Workspace checkpoint/fork | Subject to support conditions | Integrate through the Job service contract | Not exposed | Checkpoint not implemented |
| Full execution checkpoint/resume/fork | Supported VM profiles | Native execution-state services; the same profile limits apply | Not exposed; pause/resume controls live vCPUs only | Checkpoint and pause/resume not implemented |
| Model communication capture | Explicit Gateway configuration | Embedding application configures the Gateway driver | API not exposed | API not exposed |
| Outcomes and evidence | Job records, Bundles, and JSON output | Collect through the selected Session/Job service contract | Lifecycle state and upstream data plane; no Run Bundle export | Task events, exit status, and I/O; no native Bundle interface |

Native execution checkpoints currently require supported VM profiles with no networking and private RAM. Restoration requires the same host boot, binary, and firmware; shared pools and other configurations have separate restrictions. See the [CLI reference](../../reference/cli.md#full-vm-execution-checkpoints) for complete conditions and capability queries.

The daemon implements a partial OpenSandbox 1.1.0 profile. The project has not supplied a bootstrap/image recipe, completed end-to-end SDK conformance validation, or established full daemon density evidence. Underlying VM/memory-pool tests do not replace those validations. The [daemon guide](../daemon/index.md#profile) owns interface and delivery requirements.

## Native Jobs and embedded applications {#native}

Validate the local environment with an [offline task](../../start/first-run.md), then decide whether to embed the Runtime. Scripts or an existing trainer can call the CLI and use [machine-readable output](../../reference/json-output.md) to retain associations between tasks and execution results.

Rust applications directly manage executors, drivers, waiting, and cleanup. Cancellation requests, process exit, resource reclamation, and terminal publication occur at different points; retain the corresponding owners according to the [execution model](../../design/execution-model.md). See the [Runtime README](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor/README.md) for API entry points and [stability](../../reference/stability.md) for interface compatibility.

## containerd integration {#containerd}

The shim connects containerd task/exec operations to host execution or an explicitly selected VM path. It is a separate entry point from the native CLI's OCI executor, which invokes `runc`/`crun` through `pvisor run --executor container` and retains the Job workflow.

VM shim CPU/RAM annotations configure VM shape. Some host OCI constraints are accepted as no-ops, so their presence does not establish enforcement inside the guest. See the [shim README](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-shim/README.md) for registration, guest exec, mounts, and support scope. After registration, validate the controls and data paths required by the workload before deployment.

## Image caching is a shared service {#cache}

Clients manage content through `pvisor cache prepare/publish/list/stat/read`. The service entry point is `pvisor-daemon serve --cache`, which can run alongside `--sandbox`. The cache service supports Linux x86_64 and macOS ARM64; cache-only operation does not require KVM or sandbox cgroup prerequisites.

Cache endpoints, storage, and authentication are independent of the sandbox API. Preparing a cached image does not create the prepared-image manifest and guest bootstrap required by the daemon. See the [shared image cache reference](../../reference/shared-image-cache.md) for configuration and backend contracts.

## Hand execution back to the surrounding system {#handoff}

The caller pins inputs, associates business and execution identities, and defines timeouts, result acceptance, and retries. When waiting disconnects or an error leaves the outcome uncertain, reconcile state through the selected interface before repeating execution, scoring, or cleanup.

Validate success, failure, cancellation, and resource reclamation with the same version and environment before increasing concurrency. [RL rollouts](../rl-rollouts.md) demonstrates scoring handoff through native Jobs; the [infrastructure overview](index.md) connects inference, state reuse, and capacity evaluation.
