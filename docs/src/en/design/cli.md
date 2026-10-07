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

Top-level commands follow the object they operate on: flat Job/workspace operations. Sandbox lifecycle and optional pool ownership use the separate `pvisor-daemon` executable; OCI caching uses independent `pvisor-cache`. `replay` creates Jobs from trajectories, and `tui` is an optional interactive frontend. Root help groups commands under Jobs, Filesystems and Extensions, with installed optional companions shown.

| Responsibility | Entry |
|---|---|
| Job lifecycle and branches | `run`, `status`, `kill`, `suspend`, `resume`, `fork` |
| Inspect, review and accept file changes | `inspect`, `review`, `apply`, `drop` |
| Immutable Job checkpoints | `checkpoint` |
| Trajectory replay and interactive frontend | `replay`, `tui` |
| Local sandbox lifecycle | Direct `pvisor-daemon serve` and OpenSandbox-profile HTTP API |
| Immutable image cache | `pvisor-cache prepare/publish/serve/list/stat/read` |
| Optional daemon-owned shared pages | `pvisor-daemon serve --memory-pool`; detached `memory-pool --directory DIR` component |

`status --review` is a shortcut; `review` is the detailed review entry. `run --tui` is the primary interactive path, with top-level `tui` for explicit frontend invocation. Job operations are top-level commands, without a `job` command layer.

Resource tools use their own executables; the old service supervisor and node/pool role TOML are removed. Native execution capture, restoration and storage management use Job commands, with support determined by the VM profile. Other unknown names follow ordinary default execution rules. Direct `pvisor ctrl`, `pvisor ctrl --help` and `pvisor help ctrl` explicitly reject with migration guidance instead of default-running a workload; `pvisor run -- ctrl` remains explicit workload intent, not a control API alias.

```bash
pvisor-daemon serve --help
pvisor-daemon protocol
pvisor-cache --help
pvisor-daemon memory-pool --help
```

Use `pvisor-daemon` directly for sandbox lifecycle and optional pool ownership. The cache, replay and TUI remain separate from Job commands; replay/TUI companion lookup reports missing tools explicitly. The daemon CLI constructs the native VM runtime and dispatches supervisors, with synchronous internal VM dispatch before Tokio. Node resource protocols remain in the runtime, but the old CLI node supervisor has not migrated into the daemon. Packaging does not expose staging/checkpoint APIs or automatically acquire node resources.

Tools come from a static table and only a trusted installation directory; discovery neither searches PATH nor executes companions. The directory/executables belong to the current user or root, must not be group/world writable, and reject symlinks. Unix `exec` preserves argv, stdio, signals and exit codes. Tool `--help`/`--version` arguments pass through unchanged, and tools cannot shadow Job commands. See [daemon operations](daemon/operations.md) for sandbox deployment and [responsibility convergence](daemon/responsibility-convergence.md) for separate native resource budgets.

The default core build excludes Gateway. Use `--features gateway` for capture; wheel builds enable it. Without capture, OverlayNet still authorizes and forwards ordinary explicit proxy traffic. Requesting uncompiled capture or Gateway debug capabilities returns an error.

Built-in Job CLI operations cross the on-demand persistent Host AgentCtl listener as typed requests; the frontend launches authorized request workers rather than reconstructing shell commands in the listener. Ordinary persisted Jobs need no endpoint arguments. Live VM addressing uses the global `--vm-socket`, `--vm-job-id` and `--vm-attempt-id` options on `status`, `suspend --vm-pause` / `--vm-offload`, and `resume --vm-load`. See the [CLI reference](../reference/cli.md#vm-instance-control).

Embedded callers use `RunHandle` for status, cancellation, checkpoints and Event subscriptions. Session owns Attempt lifecycle; Guest AgentCtl retains workload cooperation duties, isolated from Host authority. See [Host and Guest AgentCtl](architecture.md#host-agentctl) for protocol compatibility, cancellation and upgrade limits. See [Core architecture](architecture.md) for execution/terminal handling and [Operation and Event](operations-events.md) for records/failures.

## Implementation ownership {#implementation-ownership}

`cli/host.rs` owns typed Job dispatch and live VM option validation; `cli/host_service.rs` owns listener admission, compatibility and worker authorization. `cli/host_fds.rs`, `cli/host_cancel.rs` and `cli/host_process.rs` implement descriptor transfer, cancellation and process ownership; `cli/host_image.rs` implements macOS loaded/disk Mach-O UUID verification before hashing. `runtime/host_transport.rs` owns shared host-authority paths, identical bounded async/sync newline JSON framing and peer checks, including Job service/internal-worker frames; FD marker bytes remain separate, non-JSON transport records. The internal version-1 handshake checks the Job ticket schema and exact package/content-build compatibility. `JobCommand` embeds internal CLI DTOs and is not a stable public API; Core owns pure shared Host envelope/supervisor contracts and validation, not CLI DTOs or transport.

`cli/run.rs` owns new Job configuration and startup. `cli/run/lifecycle.rs` owns workspace branches and native execution restoration. `runtime/job_execution.rs` owns durable Job state, request receipts and native terminal acknowledgement; it does not infer suspension from a saved filesystem.

Lazy image ownership separates host FUSE mounts from VM direct backend attachments. VM preparation returns a direct attachment; host mount teardown cannot run on that attachment. Gateway persists canonical events and excludes drafts before actor dispatch; it has no live Markdown compatibility option or retired draft projection command.
