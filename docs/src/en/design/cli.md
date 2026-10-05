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

Top-level commands follow the object they operate on: flat Job/workspace operations, with deployments, cluster tasks and node resources grouped under `service`. `replay` creates Jobs from trajectories, and `tui` is an optional interactive frontend. Root help groups commands under Jobs, Filesystems and Extensions. Jobs includes `checkpoint`; Extensions contains `service`, `replay` and `tui`, with optional companions shown when installed. `extensions` has been removed.

| Responsibility | Entry |
|---|---|
| Job lifecycle and branches | `run`, `status`, `kill`, `suspend`, `resume`, `fork` |
| Inspect, review and accept file changes | `inspect`, `review`, `apply`, `drop` |
| Immutable Job checkpoints | `checkpoint` |
| Trajectory replay and interactive frontend | `replay`, `tui` |
| Deployment role lifecycle | `service run/status/restart/stop --config FILE` |
| Cluster tasks/controls and execution nodes | `service cluster`, `service worker` |
| Immutable environments and experimental cold pages | `service cache`, `service memory-pool` |

`status --review` remains an existing shortcut; `review` remains the detailed review entry. `run --tui` is the primary interactive path, with top-level `tui` retained for explicit frontend invocation. No additional `job` command layer is introduced, avoiding duplicate syntax for the same Job operations.

The four resource commands are removed from the top level and keep their arguments under service. Retired forms return explicit migration errors instead of becoming host workloads through default execution. The standalone `snapshot` frontend is removed; native full capture, restoration and storage management now use Job commands, with support determined by the VM profile.

```bash
pvisor service --help
pvisor service cluster --help
pvisor service worker --help
pvisor service cache --help
pvisor service memory-pool --help
pvisor help service cluster submit
```

Seven installed artifacts are `pvisor`, `pvisor-cluster`, `pvisor-worker`, `pvisor-cache`, `pvisor-memory-pool`, `pvisor-replay` and `pvisor-tui`. Resource companions implement service subcommands. The standalone core still manages Jobs; a missing companion yields an explicit service-tool error. Supervisor/data roles retain separate processes, so CLI consolidation does not merge failure boundaries.

Tools come from a static table and only a trusted installation directory; discovery neither searches PATH nor executes companions. The directory/executables belong to the current user or root, must not be group/world writable, and reject symlinks. Unix `exec` preserves argv, stdio, signals and exit codes. Tool `--help`/`--version` arguments pass through unchanged, and tools cannot shadow Job commands. See the [unified service guide](../guides/cluster/service.md) for deployment and budgets.

The default core build excludes Gateway. Use `--features gateway` for capture; wheel builds enable it. Without capture, OverlayNet still authorizes and forwards ordinary explicit proxy traffic. Requesting uncompiled capture or Gateway debug capabilities returns an error.

Embedded callers use `RunHandle` for status, cancellation, checkpoints and Event subscriptions. Session owns Attempt lifecycle; AgentCtl retains workload cooperation duties. See [Core architecture](architecture.md) for execution/terminal handling and [Operation and Event](operations-events.md) for records/failures.
