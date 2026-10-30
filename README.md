<div align="center">

<img src="https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-icon.png" alt="pVisor" width="88" />

# PolicyVisor

**An agent-native sandbox framework**

Run your tools. Review their changes. Keep what you choose.

[Quick start](https://deeplink-org.github.io/pvisor/start/first-run/) · [Documentation](https://deeplink-org.github.io/pvisor/) · [Examples](examples/pvisor/README.md) · [中文](https://deeplink-org.github.io/pvisor/zh/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE)

</div>

pVisor runs coding agents, scripts, and automation commands with local isolation,
access controls, task records, and file review. Let your tools work in a staged
workspace, then apply the changes you want to keep.

It also provides execution environments for agentic inference, evaluation, and
RL post-training. Shared resources and idle-memory reclamation address machine
capacity; staged changes and execution records help with review. Support varies
by platform and interface; see [deployment and integration](https://deeplink-org.github.io/pvisor/guides/infrastructure/).

- **Bring your tools.** Run existing Agent CLIs and shell commands.
- **Stage file changes.** Keep candidate edits separate from your workspace.
- **Apply selectively.** Accept individual paths and discard the rest.
- **Choose your environment.** Use host execution, OCI containers, or Linux VMs.
- **Control access.** Set filesystem and network policy; capture model traffic with Gateway.
- **Keep a record.** Inspect commands, results, file changes, and installed controls.

## Getting started

### Install the CLI

```bash
pip install pvisor-sandbox
```

<details>
<summary>Other installation options</summary>

```bash
# Homebrew
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor

# Rolling nightly
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

[Native downloads](https://github.com/DeepLink-org/pvisor/releases) are also available.

</details>

**Requirements:** Linux x86_64 or Apple Silicon macOS; Python 3.10+ for pip.
File staging needs FUSE and user namespaces on Linux, or macFUSE 5.4.0+ with
FSKit on macOS. Follow [platform setup](https://deeplink-org.github.io/pvisor/start/installation/#local-prerequisites)
before running the example. No Rust toolchain is needed.

### Run your first command

In a new demo directory:

```bash
mkdir pvisor-demo && cd pvisor-demo
pvisor spawn --safe -- sh -c 'echo "Hello from pVisor" > hello.txt'
pvisor review last --diff
pvisor apply last --path hello.txt
cat hello.txt
```

```text
Hello from pVisor
```

The command writes to a staged workspace. `review` shows the change; `apply`
copies the selected file into your project. Follow the
[quick start](https://deeplink-org.github.io/pvisor/start/first-run/) to try
selective apply and discarding unwanted files.

> **Developer preview.** `--safe` refuses to start if the requested isolation and
> staging cannot be installed. File review does not undo API calls or other
> external effects. See [security boundaries](https://deeplink-org.github.io/pvisor/security/).

## CLI

### Run your coding agent

```bash
pvisor spawn --safe -- claude
pvisor review last --diff
pvisor apply last --path src
pvisor drop last
```

Replace `claude` with your Agent CLI. `last` selects the latest Job for the current
workspace. Apply checks for conflicting file changes; stop concurrent writers
while applying. Drop discards remaining staged changes and leaves accepted files
in place. [Connect your agent →](https://deeplink-org.github.io/pvisor/guides/agents/)

### Run in a Linux VM

```bash
pvisor spawn --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

Requires KVM on Linux x86_64 or Hypervisor.framework on Apple Silicon macOS.
The image is downloaded and cached on first use.
[VM setup →](https://deeplink-org.github.io/pvisor/guides/executors/vm/)

### Review and apply project changes from a staged VM (Linux preview)

`stage` reuses the host's installed tools in a Linux VM and retains root-filesystem
changes for review instead of writing them directly to the host:

```bash
pvisor stage -it --stage ./experiment
pvisor review ./experiment --diff
pvisor apply ./experiment          # terminal picker; starts with nothing selected
pvisor drop ./experiment
```

It uses Docker-style `-i`, `-t`, `-e`, `-w`, `--cpus`, `-m` and `--network`
options, but creates a Job rather than a Docker container. Use `--network none`
for offline experiments. Host files remain readable; network side effects are
not reversible, and the live host lower is not a point-in-time snapshot.

Default `apply` writes only within the launch-time project, including for new
stages created by a non-root user. Outside-project changes stay staged. Pass
`--path src` to execute that selection without prompting, or use
`--all --exclude 'build/**'` for all project changes except the excluded paths.
Preview first with `pvisor apply ./experiment --path src --dry-run --diff`;
the preview does not write the target, journals, apply ledger or recovery state.
An execution with no matching changes prints `No matching staged changes` and
exits successfully.

Project writeback from a root-filesystem stage requires its launch-time project
contract; older stages without it are refused. This restricted path supports
regular files and new directories, with unsupported changes blocked before
execution. Advanced system writeback is separate: `apply --rootfs` still requires
effective root and a root-created stage in root-controlled storage. It is
**not an atomic OS upgrade or rollback**. See the
[apply reference](docs/src/en/reference/cli.md#apply) for selection, compatibility
and failure handling, and the
[stage contract](crates/pvisor-cli/README.md#staged-system-experiments-linux-preview)
for supported changes and rootfs limits.

### Integrate with your platform

Use the [Rust runtime](crates/pvisor/README.md) for embedded execution,
the [local daemon](crates/pvisor-daemon/README.md) for partial OpenSandbox/E2B SDK
profiles on Linux/KVM, or the [containerd shim](crates/pvisor-shim/README.md).
An experimental [Docker-style CLI](crates/pvisor-cli/README.md#docker-command-profile-experimental)
runs one VM per container. Each interface has its own lifecycle and capability
contract; daemon APIs do not expose native Job stage/apply semantics.

## Learn more

- [User guide](https://deeplink-org.github.io/pvisor/guides/) — agents, policies, and file review
- [CLI reference](https://deeplink-org.github.io/pvisor/reference/cli/) — commands and configuration
- [Benchmarks](https://deeplink-org.github.io/pvisor/benchmarks/) — measured workloads and conditions
- [Architecture](https://deeplink-org.github.io/pvisor/design/architecture/) — how pVisor works
- [Security](https://deeplink-org.github.io/pvisor/security/) — executor boundaries, trusted components, and recovery limits

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) to set up the toolchain and run targeted
checks. Ask questions in [Discussions](https://github.com/DeepLink-org/pvisor/discussions)
and report vulnerabilities through [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) for bundled third-party components.
