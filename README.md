<div align="center">

<img src="https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-icon.png" alt="pVisor" width="88" />

# PolicyVisor

**An agent-native sandbox framework**

Run your tools. Review their changes. Keep what you choose.

[Getting started](https://deeplink-org.github.io/pvisor/start/first-run/) · [Documentation](https://deeplink-org.github.io/pvisor/) · [Examples](examples/pvisor/README.md) · [中文](https://deeplink-org.github.io/pvisor/zh/)

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE)

</div>

**pVisor** runs coding agents, scripts, and automation commands with local isolation,
access controls, task records, and file review. Run a command in a staged workspace,
inspect its changes, and apply only the files you want to keep.

It also provides execution environments for agentic inference, evaluation, and
RL post-training. Shared resources and idle-memory reclamation address machine
capacity; staged changes and execution records help with review. Support varies
by platform and interface; see [deployment and integration](https://deeplink-org.github.io/pvisor/guides/infrastructure/).

- **Bring your tools.** Use existing Agent CLIs and shell commands. No Agent SDK required.
- **Stage file changes.** With staging enabled, candidate edits stay separate from your workspace.
- **Apply selectively.** Review diffs, accept paths, and discard the rest. Conflicting files are refused.
- **Choose your environment.** Run on the host, through an OCI runtime, or in a Linux VM.
- **Control access.** Set filesystem and network policy; capture model traffic with Gateway.
- **Keep a record.** Inspect the command, results, file changes, and controls that actually installed.

## Getting started

### Install the CLI

With Python 3.10+ in an activated virtual environment:

```bash
pip install pvisor-sandbox
```

<details>
<summary>Homebrew and native downloads</summary>

```bash
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor
```

[GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) also provides native archives.
See [installation](https://deeplink-org.github.io/pvisor/start/installation/) for virtual environment setup, nightly builds, upgrades, and troubleshooting.

</details>

**Supported hosts:** Linux x86_64 and Apple Silicon macOS. The staged example below
needs FUSE and user namespaces on Linux, or macFUSE 5.4.0+ with FSKit enabled on
macOS. [Prepare your platform →](https://deeplink-org.github.io/pvisor/start/installation/#local-prerequisites)

### Run, review, and keep a file

No model account, Agent installation, or VM image is needed:

```bash
mkdir pvisor-demo && cd pvisor-demo
pvisor spawn --safe --overlaynet-deny-all -- sh -c 'echo "Hello from pVisor" > hello.txt'
```

The command has finished, but `hello.txt` is still staged. Review it, then accept it:

```bash
pvisor review last --diff
pvisor apply last --path hello.txt
cat hello.txt
```

```text
Hello from pVisor
```

`last` selects the latest Job registered for this workspace.
[Continue the walkthrough →](https://deeplink-org.github.io/pvisor/start/first-run/)

> **Developer preview.** `--safe` refuses to start if the requested isolation or
> staging cannot be installed. Apply is not a multi-file transaction, and drop
> does not undo accepted files or external effects. [Security boundaries →](https://deeplink-org.github.io/pvisor/security/)

## CLI

### Run your coding agent

From your project, use an installed Agent CLI in place of the shell command:

```bash
pvisor spawn --safe -- claude
pvisor review last --diff
pvisor apply last --path src --dry-run --diff
pvisor apply last --path src
pvisor drop last
```

`--path src` accepts changes under `src/`; `drop` discards what remains staged.
Stop concurrent writers before applying. [Agent setup →](https://deeplink-org.github.io/pvisor/guides/agents/)

### Run an image in a VM

```bash
pvisor run --rm alpine:3.20 echo 'Hello from a VM'
```

Requires KVM on Linux or Hypervisor.framework on Apple Silicon macOS.
The first run downloads the image; later runs reuse the cache. No Docker daemon
is required. This is a Docker-style CLI subset, not the Docker Engine API.
[Supported commands →](crates/pvisor-cli/README.md#docker-command-profile-experimental)

### Try host tools without writing back (Linux preview)

```bash
pvisor stage -it --stage ./experiment
# Use the shell, then exit to review its changes.
pvisor review ./experiment --diff
pvisor apply ./experiment --path src --dry-run --diff
pvisor apply ./experiment --path src
pvisor drop ./experiment
```

`stage` runs host-installed tools in a Linux VM. Apply defaults to the recorded
project; environment changes remain staged. Host files stay readable and the live
host filesystem is not a snapshot. System writeback is a separate privileged
operation, not an OS upgrade transaction.
[Stage setup and limits →](crates/pvisor-cli/README.md#staged-system-experiments-linux-preview)

### Stage a container's project edits (Linux only)

```bash
mkdir project
pvisor run --name edit -v ./project:/workspace:stage alpine:3.20 \
  sh -c 'echo "Hello from a VM" > /workspace/hello.txt'
pvisor review edit --diff
pvisor apply edit --path hello.txt --dry-run --diff
pvisor apply edit --path hello.txt
pvisor drop edit
pvisor rm edit
```

`:stage` is a pVisor extension; ordinary `:ro` and `:rw` mounts are unchanged.
Only staged binds can write results back to their recorded host sources—not the
instance rootfs. Review after exit; multiple staged binds require `--mount`.
Staged binds cannot use `--rm` or be mixed with direct-writable mounts.
[Mount selection and lifecycle →](docs/src/en/reference/cli.md#container-staged-binds)

## Guides

- **[Run your first Job](https://deeplink-org.github.io/pvisor/start/first-run/)** — create two files, keep one, discard the other.
- **[Connect an Agent](https://deeplink-org.github.io/pvisor/guides/agents/)** — configure tools and model credentials.
- **[Review and apply](https://deeplink-org.github.io/pvisor/guides/review-apply/)** — select changes and handle conflicts.
- **[Execute tools from a worker](https://deeplink-org.github.io/pvisor/start/infrastructure/)** — run a test task in a VM and collect feedback.
- **[Deployment and integration](https://deeplink-org.github.io/pvisor/guides/infrastructure/)** — choose the Rust runtime, local daemon, or containerd shim. Each has its own lifecycle and capability contract.
- **[Benchmarks](https://deeplink-org.github.io/pvisor/benchmarks/)** — measured workloads, configurations, and limitations.

Use `pvisor --help` or `pvisor COMMAND --help` for command help.
The [CLI reference](https://deeplink-org.github.io/pvisor/reference/cli/) covers the full supported interface.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for local setup and targeted checks.
Ask questions in [Discussions](https://github.com/DeepLink-org/pvisor/discussions)
and report vulnerabilities through [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) for bundled third-party components.
