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

## Core commands and executable extensions

`run`, `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` belong to the
core `pvisor` executable: together they cover the Job lifecycle. Help and
`extensions` introspection are built in too. A standalone core can execute,
inspect and manage Jobs without installing companion command binaries.
Session lifecycle and control/observation contracts remain shared library APIs.

`cache`, `tui`, and `replay` are independent `pvisor-NAME` executables, each
with its own argument parser and manifest. Build/install commands and wheels
ship four executables: `pvisor`, `pvisor-cache`, `pvisor-tui`, `pvisor-replay`.
`pvisor help NAME` supports core commands and extensions. Default execution
(`pvisor -- COMMAND`) stays in the core; `run --tui` and interactive `--ask`
delegate to `pvisor-tui`. New independent tools can extend the CLI without
changing the core parser. `env` has been removed.

Discovery searches the core executable directory before nonempty PATH entries.
Core command names are reserved and cannot be overridden by an extension.
Each executable extension embeds one inert JSON block:
NUL + `PVISOR_COMMAND_MANIFEST_V1` + newline, JSON, then newline +
`PVISOR_COMMAND_MANIFEST_END` + NUL. The manifest contains `schema_version`,
`name`, `version`, `description` and `session_protocol`; both protocol versions
currently equal 1. The name must match the executable suffix. JSON is limited
to 4096 bytes and executables to 256 MiB. Discovery reads bytes and never executes
a command; `--pvisor-manifest` is the extension's explicit JSON query interface.
Dispatch preserves arguments, stdio, signals and exit status via Unix `exec`.
Use `persisting_pvisor::command_manifest!` and `manifest_requested` in a Rust
extension entry point to embed and serve this manifest.
