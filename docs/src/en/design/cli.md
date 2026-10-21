# pVisor command model

Job is the CLI's central object. Commands stay flat: `pvisor run` creates a Job; `status`, `kill`, `inspect`, `fork`, `apply` and `drop` operate on it directly; `replay` creates a Job from a trajectory. See [Execution model](execution-model.md) for Job, Run and Attempt. Internal records still use Run and configuration still uses `RunConfig`. Configuration files must be passed explicitly; they are not implicit project policies.

## Start with run

Short and full forms are equivalent:

```bash
pvisor -- codex
pvisor run -- codex
```

`--stage PATH` retains the changeset in that directory. An ordinary host Job without staging writes directly to the workspace. `--safe` and `--ask` retain staged workspace changes by default; see [Staging and storage](../reference/cli.md#暂存与存储). The selected host, container or VM provider records actual capabilities and limits in the Run Bundle.

Common controls grouped by purpose:

| Purpose | Options | Result |
| --- | --- | --- |
| Filesystem | `--safe`, `--stage`, `--mount SOURCE[:TARGET]:read\|stage\|write`, `--access PATH-GLOB:deny\|ask\|warn` | Stage changes when requested and declare path permissions |
| Runtime | `--executor host\|container\|vm`, `--rootfs`, `--container-image` | Select provider and rootfs |
| Network | `--overlaynet-deny-all`, `--overlaynet-allow`, `--overlaynet-limit` | Request deny, allowlist or rate limits |
| Gateway | `--gateway-mode`, `--gateway-route`, `--gateway-level` | Configure routes and optionally capture model traffic |
| Limits | `--timeout`, `--memory`, `--max-processes`, `--max-open-files` | Limit an Attempt where supported by its provider |
| Configuration | `--config`, `--spec`, `--name`, `--pass-env` | Supply RunConfig/prepared RunSpec, identity and explicit environment variables |

Provider selection preserves the Run contract while changing each capability's enforcement mechanism. Final evidence records dimensions separately.

## Inspect and decide

A completed Job remains a record. Staged effects require explicit acceptance or discard:

```bash
pvisor review last
# Compatibility entry: pvisor status --review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# 或：pvisor drop last
```

`review` and `status --review` explain the historical Run Bundle's execution evidence and reread staged changes in the selected workspace; `inspect` runs a read-only command in the Job view; `apply` commits selected paths and retains the rest; `drop` discards remaining file changes while preserving the Job and checkpoints. Both require an explicit Job and a confirmed stopped terminal state. Reset creates a new stage generation so old metadata cannot overwrite a new decision.

## Checkpoint and fork

By default, `fork` creates a logical filesystem checkpoint of a stopped Job before launching the child; see [Execution model](execution-model.md) for scope:

```bash
pvisor checkpoint create last --request-id before-refactor --json
pvisor checkpoint list last --json
pvisor fork last --state workspace --stage ./stage/branch -- codex
```

Embedded callers can use the cooperative AgentCtl protocol to quiesce participating sessions before checkpointing.

## Configuration precedence

`--config` accepts TOML `RunConfig`; `--spec` accepts prepared JSON `RunSpec`. Explicit scalar options override file values; repeated list options replace the complete list; the command after `--` replaces `run.command`. `--container-image` and `--rootfs` can infer a matching executor, though automation should specify `--executor` explicitly.

The public workflow is simple: start a Job, inspect evidence, then decide what to do with staged effects. See [Executors](../guides/executors/index.md) for provider behavior and [CLI reference](../reference/cli.md) for all options.

## Core commands and service boundaries

Top-level commands follow the object they operate on: flat Job/workspace operations. Sandbox lifecycle and optional pool ownership use the separate `pvisor-daemon` executable; OCI cache commands use `pvisor cache`, with serving owned by `pvisor-daemon serve --cache`. `replay` creates Jobs from trajectories, and `tui` is an optional interactive frontend. Root help stays concise, grouping commands under Execution (`run`, `status`, `kill`), Changes (`review`, `apply`, `drop`, `inspect`), Checkpoints (`checkpoint`, `suspend`, `resume`, `fork`) and Tools (installed `replay`/`tui` companions, `cache`, `feature`, `help`). It includes run-review-apply examples and points to `pvisor help COMMAND` for details.

| Responsibility | Entry |
|---|---|
| Job lifecycle and branches | `run`, `status`, `kill`, `suspend`, `resume`, `fork` |
| Inspect, review and accept file changes | `inspect`, `review`, `apply`, `drop` |
| Immutable Job checkpoints | `checkpoint` |
| Trajectory replay and interactive frontend | `replay`, `tui` |
| Local sandbox lifecycle | Direct `pvisor-daemon serve` and OpenSandbox-profile HTTP API |
| Immutable image cache | `pvisor cache prepare/publish/list/stat/read`; `pvisor-daemon serve --cache` |
| Optional daemon-owned shared pages | `pvisor-daemon serve --sandbox --memory-pool`; detached `memory-pool --directory DIR` component |

`status --review` is a shortcut; `review` is the detailed review entry. `run --tui` is the primary interactive path, with top-level `tui` for explicit frontend invocation. Job operations are top-level commands, without a `job` command layer.

Resource tools use their own executables. Native execution capture, restoration and storage management use Job commands, with support determined by the VM profile. Other unknown names follow ordinary default execution rules. Direct `pvisor ctrl`, `pvisor ctrl --help` and `pvisor help ctrl` explicitly reject with migration guidance instead of default-running a workload; `pvisor run -- ctrl` remains explicit workload intent, not a control API alias.

```bash
pvisor-daemon serve --help
pvisor-daemon protocol
pvisor cache --help
pvisor-daemon memory-pool --help
```

Use `pvisor-daemon` directly for sandbox lifecycle and optional pool ownership. The cache, replay and TUI remain separate from Job commands; replay/TUI companion lookup reports missing tools explicitly. The daemon CLI constructs the native VM runtime and dispatches supervisors, with synchronous internal VM dispatch before Tokio. Node resource protocols belong to the runtime and have no daemon acquire/release adapter. Packaging does not expose staging/checkpoint APIs or automatically acquire node resources.

Tools come from a static table and only a trusted installation directory; discovery neither searches PATH nor executes companions. The directory/executables belong to the current user or root, must not be group/world writable, and reject symlinks. Unix `exec` preserves argv, stdio, signals and exit codes. Tool `--help`/`--version` arguments pass through unchanged, and tools cannot shadow Job commands. See the [daemon architecture](daemon/index.md) and [operations](daemon/operations.md) for sandbox deployment and native resource budgets.

The default core build excludes Gateway. Use `--features gateway` for capture; wheel builds enable it. Without capture, OverlayNet still authorizes and forwards ordinary explicit proxy traffic. Requesting uncompiled capture or Gateway debug capabilities returns an error.

`run` starts a terminal frontend and one Job owner. Live controls execute in that
owner through its recorded private socket; stopped operations execute in the
caller under `job.lock`. Ordinary commands discover endpoints from the Job.
`--vm-socket` is a debug override and requires the two explicit identity flags.
Live VM flags remain command-local; pause/offload conflict and `--vm-ram-file`
requires offload. Live resume continues the same Attempt, while stopped resume
restores a sealed checkpoint. See the [CLI reference](../reference/cli.md#vm-instance-control).

Embedded callers use `RunHandle` for status, cancellation, checkpoints and Event subscriptions. Session owns Attempt lifecycle; Guest AgentCtl retains workload cooperation duties, isolated from Host authority. See [Host and Guest AgentCtl](architecture.md#host-agentctl) for protocol compatibility, cancellation and upgrade limits. See [Core architecture](architecture.md) for execution/terminal handling and [Operation and Event](operations-events.md) for records/failures.

Help, version and feature queries return without startup logs. Execution
commands emit `process.entry` and `cli.parsed` only after parsing, before Job
request dispatch. The marker names do not establish a measurement of complete
process-entry or argument-parsing overhead.

## Implementation ownership {#implementation-ownership}

`cli/host.rs` owns typed owner bootstrap and live VM option validation;
`cli/host_service.rs` owns frontend/owner lifecycle, record-based routing and
protocol compatibility. `cli/host_process.rs` owns process trees and terminal
handoff; `cli/host_image.rs` retains macOS loaded/disk Mach-O UUID checks.
`runtime/host_transport.rs` shares private paths, bounded newline JSON and peer
checks. Instance control uses `pvisor-instance-control-1`, independently of caller
binary digest. Internal `JobCommand` DTOs currently cover all commands, but only
run, fork and resume bootstrap the same-binary owner; other commands execute directly or send
instance-protocol commands to it.
Core owns shared Host envelopes and supervisor definitions; daemon admission is
unchanged. See [owner lifecycle](architecture.md#host-job-service) and
[existing internal channels and cleanup order](architecture.md#owner-internal-control).

`cli/run.rs` owns new Job configuration and startup. `cli/run/lifecycle.rs` owns workspace branches and native execution restoration. `runtime/job_execution.rs` owns durable Job state, request receipts and native terminal acknowledgement; it does not infer suspension from a saved filesystem.

Lazy image ownership separates host FUSE mounts from VM direct backend attachments. VM preparation returns a direct attachment; host mount teardown cannot run on that attachment. Gateway persists canonical events and excludes drafts before actor dispatch; it has no live Markdown compatibility option or retired draft projection command.
