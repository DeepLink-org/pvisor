<div align="center">

<img src="https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-icon.png" alt="pVisor" width="88" />

# PolicyVisor

**An agent-native sandbox framework**

[English](README.md) · [简体中文](README.zh-CN.md)

[Get started](https://deeplink-org.github.io/pvisor/start/installation/) · [Documentation](https://deeplink-org.github.io/pvisor/) · [Examples](examples/pvisor/README.md) · [中文文档](https://deeplink-org.github.io/pvisor/zh/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

</div>

pVisor is a sandbox framework with two modes. Docker style sandbox runs each
container in its own VM. Agent style stagebox is pVisor's original mode for coding
agents: file changes stay staged until you review them and choose what to keep.

## Two modes

| Mode | Workflow | Boundary and results |
| --- | --- | --- |
| Docker style sandbox | Use familiar image, run, exec, stop, and remove commands | One Linux VM per container; container lifecycle and file operations |
| Agent style stagebox | Let an agent experiment, review staged changes, and choose what to keep | Native Jobs with explicit staging and review/apply/drop; isolation depends on the selected executor |

### Hands-off vibe coding.

*Coding without sandbox approvals.*

In Agent style stagebox mode, let the agent try changes in a staged workspace,
then review the files and accept what you need. Approval-free operation still
requires validation for each agent and configuration; the setup guide states the
current evidence and limits.

## Getting started

### 1. Install the CLI

With Python 3.10+:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

[Installation](https://deeplink-org.github.io/pvisor/start/installation/) covers
Homebrew, native packages, and the platform checks for each mode.

### 2. Choose a mode

#### Docker style sandbox

Prepare KVM on Linux x86_64 or
Hypervisor.framework on Apple Silicon macOS, then run:

```bash
pvisor run --rm alpine:3.20 echo hello
```

Expect `hello`. The command runs in a separate Linux VM and removes its container
after exit. First use downloads the image. Follow the
[Docker walkthrough](https://deeplink-org.github.io/pvisor/start/first-run/#docker-style)
to keep a sandbox running, execute another command, inspect it, and clean up.
This experimental command subset supports neither Docker Engine API nor Compose.

#### Agent style stagebox

For the Host stagebox example, prepare FUSE and user
namespaces on Linux x86_64, or macFUSE 5.4.0+ with FSKit on Apple Silicon macOS.
Run these commands in a new demo directory:

```bash
mkdir pvisor-demo
cd pvisor-demo
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
pvisor review last --diff
pvisor apply last --path hello.txt
pvisor drop last
cat hello.txt
```

Expect `Hello from pVisor`. The greeting is now in your project; the notes were discarded.
`last` selects the latest Job in this workspace. The
[stagebox walkthrough](https://deeplink-org.github.io/pvisor/start/first-run/#agent-style)
checks the original directory, staged view, and final file state at each step.
No model account or VM image is needed for this Host example.

### 3. Complete a task with your agent

The [Claude Code tutorial](https://deeplink-org.github.io/pvisor/guides/agents/claude-code/#repair-task)
walks through fixing failing tests in a small Python project. It includes the
task prompt and credential setup, then shows how to check staged files, accept
them, and retest in your project. You need Python, Claude Code, and model access.

> Developer preview: "Coding without sandbox approvals" describes the intended
> Agent style stagebox experience. The agent guide lists the controlled-client
> tests already recorded and the cloud-model and approval behavior still awaiting
> validation. pVisor does not automatically disable agent or IDE approvals.

`--safe` fails if required controls cannot be installed. Ordinary Host Jobs write
through by default. File acceptance does not undo remote API calls, database
writes, charges, or previously applied files. See
[security boundaries](https://deeplink-org.github.io/pvisor/security/).

## Linux preview: host tools and staged container binds

For system-tool experiments, `pvisor stage` runs host-installed tools in a Linux
VM and keeps rootfs changes for review. Apply defaults to the recorded project;
environment changes remain staged. Host files stay readable, and the live host
filesystem is not a snapshot. System writeback is a separate privileged operation,
not an OS upgrade transaction.

```bash
pvisor stage -it --stage ../pvisor-experiment
# Use the shell, then exit before reviewing.
pvisor review ../pvisor-experiment --diff
pvisor apply ../pvisor-experiment --path src --dry-run --diff
pvisor apply ../pvisor-experiment --path src
pvisor drop ../pvisor-experiment
```

Only discard remaining candidates after checking them. If all changes were
accepted, skip drop. See [stage setup and limits](crates/pvisor-cli/README.md#staged-system-experiments-linux-preview).

Container commands also support staged project binds on Linux:

```bash
mkdir project
pvisor run --name edit -v ./project:/workspace:stage alpine:3.20 \
  sh -c 'echo "Hello from a VM" > /workspace/hello.txt'
pvisor review edit --diff
pvisor apply edit --path hello.txt --dry-run --diff
pvisor apply edit --path hello.txt
pvisor rm edit
```

`:stage` is a pVisor extension; ordinary `:ro` and `:rw` mounts keep their existing
behavior. Only staged binds write results back to recorded host sources, not the
instance rootfs. Review after exit; multiple staged binds require `--mount`.
Staged binds cannot use `--rm` or mix with direct-writable mounts. Use drop for
remaining candidates when needed before removing the instance. See
[mount selection and lifecycle](docs/src/en/reference/cli.md#container-staged-binds).

## Execution and integration

pVisor also supplies execution environments for agentic inference, evaluation,
and RL post-training. Resource sharing and idle-memory reclamation help manage
machine capacity; execution records and staged changes help you review results.
Capabilities depend on the interface, executor, and platform.

- Native Jobs: use `pvisor spawn` with Host, OCI, or VM execution and explicit staging for review/apply.
- Embedded runtime: integrate the [Rust runtime](crates/pvisor/README.md).
- Platform APIs: use the [local daemon](crates/pvisor-daemon/README.md) for partial OpenSandbox/E2B SDK profiles on Linux/KVM, or the [containerd shim](crates/pvisor-shim/README.md).
- Container workflows: the experimental [Docker-style CLI](crates/pvisor-cli/README.md#docker-command-profile-experimental) runs one VM per container.

These interfaces have separate lifecycle and capability contracts. Daemon APIs do not expose native Job stage/apply semantics. Container staged
binds have their own review/apply/drop contract. Start with the
[VM tool-execution tutorial](https://deeplink-org.github.io/pvisor/start/infrastructure/)
for a caller-script example, or consult the
[integration matrix](https://deeplink-org.github.io/pvisor/guides/infrastructure/integration/).

## Documentation

- [Installation](https://deeplink-org.github.io/pvisor/start/installation/): prepare and verify your environment
- [First run](https://deeplink-org.github.io/pvisor/start/first-run/): try either mode from launch to cleanup
- [Agent guides](https://deeplink-org.github.io/pvisor/guides/agents/): command setup, credentials, and network access
- [CLI reference](https://deeplink-org.github.io/pvisor/reference/cli/): commands and configuration
- [Benchmarks](https://deeplink-org.github.io/pvisor/benchmarks/): measured workloads and conditions
- [Architecture](https://deeplink-org.github.io/pvisor/design/architecture/): execution mechanisms and ownership

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) to set up the toolchain and run targeted
checks. Ask questions in [Discussions](https://github.com/DeepLink-org/pvisor/discussions)
and report vulnerabilities through [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) for bundled third-party components.
