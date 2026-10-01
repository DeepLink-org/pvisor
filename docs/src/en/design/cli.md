# pVisor command model

The Job is the CLI's primary object: a managed command, its execution evidence,
and any staged changes. `pvisor run` starts a Job; `status`, `kill`, `inspect`,
`fork`, `apply`, and `drop` act on that Job directly. The commands stay flat.
`replay` creates a Job from an existing trajectory. Internally, a Job is stored as a Run record; `RunConfig`
remains the configuration type. A configuration file is an explicit input,
never an implicit project policy.

## Start with `run`

The short form is intentionally equivalent to the explicit form:

```bash
pvisor -- codex
pvisor run -- codex
```

Use `--stage` when filesystem changes must remain available for review. Ordinary
host Jobs without it write through to the workspace; `--safe` creates a temporary
stage that pVisor drops when the Job ends. The selected host, container, or VM provider records its effective
capabilities and limitations in the Run Bundle.

Common controls are grouped by purpose:

| Purpose | Options | Result |
| --- | --- | --- |
| Filesystem | `--safe`, `--stage`, `--mount SOURCE[:TARGET]:read\|stage\|write`, `--access PATH-GLOB:deny\|ask\|read` | stage changes when requested and declare path access |
| Runtime | `--executor host\|container\|vm`, `--rootfs`, `--container-image` | select the execution provider and root filesystem |
| Network | `--overlaynet-deny-all`, `--overlaynet-allow`, `--overlaynet-limit` | request deny, allowlist, or rate-limit policy |
| Gateway | `--gateway-mode`, `--gateway-route`, `--gateway-level` | route and optionally capture model traffic |
| Limits | `--timeout`, `--memory`, `--max-processes`, `--max-open-files` | constrain the Attempt where the provider supports it |
| Configuration | `--config`, `--spec`, `--name`, `--pass-env` | provide a RunConfig or prepared RunSpec, identity, and explicit environment |

Provider selection does not change the Run contract. It changes the mechanism
used to enforce each capability dimension, and the resulting evidence is
reported separately.

## Inspect and decide

A completed Job remains a record until its staged effects are explicitly
accepted or discarded:

```bash
pvisor status --review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# or: pvisor drop last
```

`status --review` explains the Run Bundle and staged changes. `inspect` executes a
read-only command against the Job view. `apply` commits a selected path set and
keeps the remainder staged; `drop` discards the stage. Neither operation
rewrites a live Job. A reset creates a new stage generation so stale metadata
cannot replace a newer decision.

## Checkpoints and forks

Checkpoints are stopped-consistent filesystem and AgentCtl safe points. They do
not claim to capture process memory:

```bash
pvisor fork last -- codex
```

`fork` snapshots the stopped Job before starting a new attempt. Embedded callers
can use the cooperative AgentCtl protocol to quiesce participating sessions
before checkpointing.

## Configuration precedence

`--config` accepts a TOML `RunConfig` and `--spec` a prepared JSON `RunSpec`.
Explicit scalar options override file values. Repeated list options replace the complete list,
and the command after `--` replaces `run.command`. `--container-image` and
`--rootfs` may infer the matching executor; an explicit `--executor` remains
clearer in automation.

Keep the public workflow small: start a Job, inspect its evidence, then make an
explicit decision about staged effects. Detailed provider behavior belongs to
[execution environments](../guides/execution.md), while the complete option
surface belongs to the [CLI reference](../reference/cli.md).

## Core commands and companion tools

`pvisor` contains `run`, `status`, `kill`, `inspect`, `fork`, `apply`, and `drop`,
plus help and `extensions`. The standalone core manages the full Job lifecycle.

`pvisor-tui` belongs to `persisting-tui`; `pvisor-replay` belongs to
`persisting-replay`. They depend on the core, which does not depend on them.
The cache frontend remains in the core package because executors still use OCI
and lazy image caching. Wheels install all four executables together.

A static table defines the three first-party companion names and descriptions.
Discovery checks only the core's installation directory, never PATH. It does
not scan manifests, hash executables or pass launcher evidence. Installation
directories and regular executable files must still be owned by the current
user or root and must not be group/world writable; symlinks are rejected.
Unix `exec` preserves arguments, stdio, signals and exit status. Companions
cannot override core commands. `pvisor help NAME` supports companions;
`run --tui` and interactive approvals delegate to the TUI.

Default core builds omit Gateway. Enable capture with `--features gateway`;
wheel builds enable this feature. Plain explicit proxy authorization and
forwarding remain in OverlayNet. Requesting uncompiled capture or Gateway
debug functionality reports an error.

The lifecycle type is `Session`; `ExecutorSession` and `AttemptContext` aliases
were removed. Use existing `RunHandle` status, cancellation, checkpoint and
event APIs. There is no test-only Hook/Control protocol. AgentCtl retains
workload cooperation. Terminal status follows driver teardown, durable results
and terminal event commit. Unknown append outcomes still suppress conflicting
replacement terminal events.
