# <img src="docs/src/assets/logos/pvisor-icon.png" alt="PolicyVisor" width="72" /> PolicyVisor (pVisor)

**Policy-governed, reviewable execution.**  
**策略约束下的可审查执行。**

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**PolicyVisor**, abbreviated **pVisor**, is an execution layer for Agent CLIs,
scripts, and automation commands. Keep your existing tools; use policies to
define their authority, inspect the controls installed by the selected executor,
and review staged file changes before applying them to your project.

The **p** stands for **Policy**. A **Job** is one managed command, its effective
runtime controls, and an inspectable execution record. `pvisor run` starts a
Job; the flat `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` commands
act on it. On disk, Jobs retain the existing Run record and `run-*` IDs.

![PolicyVisor execution and review workflow](docs/src/assets/diagrams/pvisor/system-products.svg)

## Define, execute, review

- **Define the boundary:** select an executor and the filesystem, network, and
  other capability requirements for the task.
- **Execute with evidence:** retain the command, outcome, installed controls,
  warnings, and observed effects in the Job's Run Bundle.
- **Review staged changes:** pass `--stage PATH` to retain the copy-on-write
  changeset, then apply selected paths or discard the remainder. Without it,
  the temporary stage is discarded when the Job ends.

Host, container, and libkrun VM executors provide different boundaries. A
requested policy is not proof of enforcement; inspect the evidence for the Job.

## Install

```bash
pip install pvisor
pvisor --version
```

PolicyVisor uses `pvisor` for its Python package, CLI and core Rust crate.
Companion crates use `pvisor-*`; environment variables use `PVISOR_*`.
Wheel filenames use `pvisor-<version>-py3-none-<platform>.whl`.

The rolling nightly build installs the same command without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/pvisor/en/start/installation/)
for platform requirements and executor setup.

## Run a command and review its changes

```bash
pvisor run --stage ../task-stage-001 -- /bin/sh -c 'printf "hello\n" > hello.txt'
pvisor status --review last
pvisor apply last --path hello.txt   # or: pvisor drop last
```

Run this from your project directory after completing the platform setup in
the installation guide. Use a fresh stage directory outside the project for
each Job. Replace the command after `--` with your script or installed Agent CLI.
For Codex, `--safe` stages its home state separately from project files; see
the [CLI reference](docs/src/en/reference/cli.md) for what each stage retains:

```bash
pvisor run --safe --stage ../agent-stage-001 -- codex
pvisor status --review last
```

Ordinary host Jobs write through to the workspace. `--safe` stages workspace
and home changes and discards its temporary stage when the Job ends; `--stage`
retains a changeset for review.
Explicit writable mounts and application state outside the workspace can still
write through to the host. The exact boundary is
platform-dependent and recorded with the Job—consult the
[execution guide](https://deeplink-org.github.io/pvisor/en/guides/execution/)
before treating it as a security boundary.

`apply` and `drop` govern staged files. They cannot undo remote API calls,
database writes, or messages already sent. Model-traffic capture is optional;
logical checkpoints preserve staged filesystem state, not process memory.

## Current maturity

| Capability | Status |
|---|---|
| Staged workspace review, selective apply, and logical checkpoints | Implemented with an explicit retained stage |
| Run Bundle retained after an auto-dropped temporary stage | Not yet implemented; supply `--stage PATH` for durable audit |
| Gateway capture and cooperative proxy policy | Implemented |
| Container/libkrun executors and transparent network boundaries | Platform-dependent; see the pVisor and OverlayNet docs |

## Documentation

- [Choose a workflow](https://deeplink-org.github.io/pvisor/en/start/) — the path from install to a reviewed Job
- [Your first Job](https://deeplink-org.github.io/pvisor/en/start/first-run/) — the run-review-apply loop
- [PolicyVisor model](https://deeplink-org.github.io/pvisor/en/concepts/policyvisor/) — policy, controls, and evidence
- [Project architecture](https://deeplink-org.github.io/pvisor/en/design/) — ownership and delivery boundaries
- [中文文档](https://deeplink-org.github.io/pvisor/zh/start/) — 从安装到策略约束下的可审查执行

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
