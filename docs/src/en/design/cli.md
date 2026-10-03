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

## Core commands and companion tools

`pvisor` includes `run`, `status`, `review`, `checkpoint`, `kill`, `inspect`, `fork`, `apply`, `drop`, help and `extensions`. Installing the core alone supports ordinary runs and file acceptance. `suspend/resume` provide capability checks; full execution checkpoints for ordinary Jobs are not yet connected.

`pvisor-tui` and `pvisor-replay` belong to their respective crates and depend on core; core does not depend on them. The `pvisor-cache` frontend remains in the core package because executors use OCI and lazy caches. Wheels install four binaries together.

Companion names/descriptions come from a static table. Discovery checks only the three first-party tools beside the core binary, without searching PATH, scanning manifests, computing binary digests or passing launcher evidence. The installation directory and executables must belong to the current user or root, must not be group/world writable, and must not be symbolic links. Unix `exec` preserves arguments, stdio, signals and exit codes. Companion tools cannot override core commands. `pvisor help NAME` supports companions; `run --tui` and interactive approval delegate to TUI.

The default core build excludes Gateway. Use `--features gateway` for capture; wheel builds enable it. Without capture, OverlayNet still authorizes and forwards ordinary explicit proxy traffic. Requesting uncompiled capture or Gateway debug capabilities returns an error.

Embedded callers use `RunHandle` for status, cancellation, checkpoints and Event subscriptions. Session owns Attempt lifecycle; AgentCtl retains workload cooperation duties. See [Core architecture](architecture.md) for execution/terminal handling and [Operation and Event](operations-events.md) for records/failures.
