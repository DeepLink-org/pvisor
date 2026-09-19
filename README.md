# <img src="docs/src/assets/logos/pvisor-icon.png" alt="PolicyVisor" width="72" /> PolicyVisor (pVisor)

**Policy-governed, reviewable execution.**  
**策略约束下的可审查执行。**

[![CI](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/Persisting/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**PolicyVisor**, abbreviated **pVisor**, is an execution layer for Agent CLIs,
scripts, and automation commands. Keep your existing tools; use policies to
define their authority, inspect the controls installed by the selected executor,
and review staged file changes before applying them to your project.

The **p** stands for **Policy**. Each Run connects requested capabilities,
effective runtime controls, and an inspectable execution record.

![PolicyVisor execution and review workflow](docs/src/assets/diagrams/persisting/system-products.svg)

## Define, execute, review

- **Define the boundary:** select an executor and the filesystem, network, and
  other capability requirements for the task.
- **Execute with evidence:** retain the command, outcome, installed controls,
  warnings, and observed effects in a local Run Bundle.
- **Review staged changes:** enable `--stage`, inspect file changes, then apply
  selected paths or discard the remaining changes.

Host, container, and libkrun VM executors provide different boundaries. A
requested policy is not proof of enforcement; inspect the evidence for the Run.

## Install

```bash
pip install pvisor
pvisor --version
```

PolicyVisor uses `pvisor` for both its Python package and CLI. Wheel filenames
use `pvisor-<version>-py3-none-<platform>.whl`. Existing repository URLs,
Rust crate names, and `PERSISTING_*` environment variables retain their current names.

If you installed the earlier `persisting` distribution, uninstall it with
`python -m pip uninstall persisting` before installing `pvisor`: both packages
provide the same CLI path.

The rolling nightly build installs the same command without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/Persisting/en/start/installation/)
for platform requirements and executor setup.

## Run a command and review its changes

```bash
pvisor run --stage ../task-stage-001 -- /bin/sh -c 'printf "hello\n" > hello.txt'
pvisor review last
pvisor apply last --path hello.txt   # or: pvisor drop last
```

Run this from your project directory after completing the platform setup in
the installation guide. Use a fresh stage directory outside the project for
each Run. Replace the command after `--` with your script or installed Agent CLI:

```bash
pvisor run --stage ../agent-stage-001 -- codex
pvisor review last
```

`--stage` creates a copy-on-write workspace view for review; without it the
command may write the real project tree. The exact boundary is
platform-dependent and recorded with the Run—consult the
[execution guide](https://deeplink-org.github.io/Persisting/en/guides/execution/)
before treating it as a security boundary.

`apply` and `drop` govern staged files. They cannot undo remote API calls,
database writes, or messages already sent. Model-traffic capture is optional;
logical checkpoints preserve staged filesystem state, not process memory.

## Current maturity

| Capability | Status |
|---|---|
| Local execution records, staged workspace review, selective apply, and logical checkpoints | Implemented |
| Gateway capture and cooperative proxy policy | Implemented |
| Container/libkrun executors and transparent network boundaries | Platform-dependent; see the pVisor and OverlayNet docs |

## Documentation

- [Choose a workflow](https://deeplink-org.github.io/Persisting/en/start/) — the path from install to a reviewed Run
- [Your first Run](https://deeplink-org.github.io/Persisting/en/start/first-run/) — the run-review-apply loop
- [PolicyVisor model](https://deeplink-org.github.io/Persisting/en/concepts/policyvisor/) — policy, controls, and evidence
- [Project architecture](https://deeplink-org.github.io/Persisting/en/design/) — ownership and delivery boundaries
- [中文文档](https://deeplink-org.github.io/Persisting/zh/start/) — 从安装到策略约束下的可审查执行

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
