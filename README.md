# <img src="docs/src/assets/logos/persisting-icon.png" alt="Persisting" width="72" /> Persisting

**Reviewable execution for Agents.**

[![CI](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/Persisting/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

<img src="docs/src/assets/logos/pvisor-with-text.png" alt="pVisor" width="220" />

**`pvisor`** runs an existing Agent command inside a controlled execution
environment. It stages filesystem effects, records the controls that were
actually installed, and lets you review changes before they reach the project.

![Current Persisting workflows](docs/src/assets/diagrams/persisting/system-products.svg)

## Install

```bash
pip install persisting
pvisor --version
```

The rolling nightly build installs the same command without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/Persisting/installation/)
for platform requirements and executor setup.

## Run one Agent and review its changes

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --all   # or: pvisor drop last
```

`--stage` creates a copy-on-write workspace view for review; without it the
Agent may write the real project tree. The exact boundary is
platform-dependent and recorded with the Run—consult the
[execution guide](https://deeplink-org.github.io/Persisting/pvisor/guides/execution/)
before treating it as a security boundary.

## Current maturity

| Capability | Status |
|---|---|
| pVisor host execution, review, checkpoints, and transactional workspace | Implemented |
| Gateway capture and cooperative proxy policy | Implemented |
| Container/libkrun executors and transparent network boundaries | Platform-dependent; see the pVisor and OverlayNet docs |

## Documentation

- [Choose a workflow](https://deeplink-org.github.io/Persisting/overview/) — the path from install to a reviewed Run
- [Run your first Agent](https://deeplink-org.github.io/Persisting/pvisor/get-started/) — the run-review-apply loop
- [Project architecture](https://deeplink-org.github.io/Persisting/system-design/) — ownership and delivery boundaries

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
