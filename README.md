<div align="center">

<img src="docs/overrides/assets/logos/pvisor-logo.png" alt="PolicyVisor" width="420" />

# PolicyVisor

**A container runtime optimized for agent workloads**

[English](README.md) · [简体中文](README.zh-CN.md)

[Get started](https://deeplink-org.github.io/pvisor/start/installation/) · [Documentation](https://deeplink-org.github.io/pvisor/) · [Examples](examples/pvisor/README.md) · [中文文档](https://deeplink-org.github.io/pvisor/zh/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

</div>

pVisor supports a subset of Docker CLI commands and adds commands to review and accept an agent's changes.

- **Stronger isolation**: Each container has [its own Linux kernel](docs/src/en/security/executor-boundaries.md), keeping the agent's tools separate from host processes and other containers.
- **Lower memory use**: With [page sharing](docs/src/en/benchmarks/vm-memory/index.md) enabled, agents using the same tools and libraries can share one copy of identical pages across VMs.
- **Faster tool startup**: [Lazy images](docs/src/en/benchmarks/lazy-image-startup.md) load content on demand, so an agent can start a tool before the whole image downloads.

On Linux, mount a project with `:stage` to keep file changes out of your working tree. After the container exits, use `review` to see the diff, `apply` to keep selected paths, and `drop` to discard the rest.

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
Homebrew, native packages, and the platform checks for each example.

### 2. Try the CLI

#### Run a container

Prepare KVM on Linux x86_64 or
Hypervisor.framework on Apple Silicon macOS, then run:

```bash
pvisor run --rm alpine:3.20 echo hello
```

Expect `hello`. The command runs in a separate Linux VM and removes its container
after exit. First use downloads the image. Follow the
[Docker walkthrough](https://deeplink-org.github.io/pvisor/start/first-run/#docker-style)
to keep a sandbox running, execute another command, inspect it, and clean up.
The experimental command subset includes image-based
[`pvisor compose`](crates/pvisor-cli/README.md#compose-command-profile-experimental);
it does not implement the Docker Engine API or the full Compose specification.

For the supported Unix command profile, `alias docker=pvisor` uses the same
Session runtime: root `inspect` reports metadata, `kill` signals the current
execution, and `export` writes a stopped container rootfs tar. Unknown commands
fail rather than executing a host workload. Native tasks use explicit `spawn`
or `pvisor [SESSION_OPTIONS] -- COMMAND`; options-only `--config` / `--spec`
invocations remain supported. `inspect --files` selects a task filesystem view.
A shell alias does not redirect SDK Docker sockets or subprocess executable lookup.

#### Review an agent task with native commands

If your tools are already installed locally, `pvisor spawn --safe` starts a Host Session with staged writes and no VM. Prepare FUSE and user
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
`last` selects the latest Session in this workspace. The
[staged task walkthrough](https://deeplink-org.github.io/pvisor/start/first-run/#agent-style)
checks the original directory, staged view, and final file state at each step.
No model account or VM image is needed for this Host example.

### 3. Complete a task with your agent

The [Claude Code tutorial](https://deeplink-org.github.io/pvisor/guides/agents/claude-code/#repair-task)
walks through fixing failing tests in a small Python project. It includes the
task prompt and credential setup, then shows how to check staged files, accept
them, and retest in your project. You need Python, Claude Code, and model access.

> Developer preview: approval-free agent operation still needs validation
> for cloud models and approval behavior. The agent guide lists the recorded
> controlled-client tests. pVisor does not automatically disable agent or IDE approvals.

`--safe` fails if required controls cannot be installed. Ordinary Host Sessions write
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

- Native Sessions: use `pvisor spawn` with Host, OCI, or VM execution and explicit staging for review/apply.
- Agent plugins: [Pi, DSH and Codex task adapters](plugins/README.md) use native `spawn → review → apply/drop`; built-in Agent tools remain separate.
- Embedded runtime: integrate the [Rust runtime](crates/pvisor/README.md).
- Platform APIs: use the [local daemon](crates/pvisor-daemon/README.md) for partial OpenSandbox/E2B SDK profiles on Linux/KVM, or the [containerd shim](crates/pvisor-shim/README.md).
- Container workflows: the experimental [Docker-style CLI](crates/pvisor-cli/README.md#docker-command-profile-experimental) runs one VM per container.

These interfaces have separate lifecycle and capability contracts. Daemon APIs do not expose native Session stage/apply semantics. Container staged
binds have their own review/apply/drop contract. Start with the
[VM tool-execution tutorial](https://deeplink-org.github.io/pvisor/start/infrastructure/)
for a caller-script example, or consult the
[integration matrix](https://deeplink-org.github.io/pvisor/guides/infrastructure/integration/).

### Try the experimental Rust guest kernel

On Linux x86_64 with KVM, use a `pvisor` build that embeds the Rust guest kernel
(`mise run build` builds one from this checkout). Select it for a single run:

```bash
PVISOR_GUEST_KERNEL=rust-native pvisor run --rm --cpus 2 --memory 16m alpine:3.20 echo hello
```

Expect `hello`. The environment setting applies only to this command. Linux
remains the default; if you exported the variable, run `unset PVISOR_GUEST_KERNEL`
to switch back. The Rust profile on x86_64 supports 1–64 vCPUs with at least
16 MiB or 1.5 MiB per vCPU rounded up, whichever is larger. Workloads may need
more RAM. Snapshot and restore are not supported. See the
[Rust guest kernel README](pvisor-kernel/README.md#experimental-selection) for details.

## Documentation

- [pVisor 0.4.0 release notes](docs/src/en/community/changelog.md#v040)
- [Installation](https://deeplink-org.github.io/pvisor/start/installation/): prepare and verify your environment
- [First run](https://deeplink-org.github.io/pvisor/start/first-run/): run a container or review a staged task from launch to cleanup
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
