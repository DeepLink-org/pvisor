---
description: Install the pvisor CLI from a Python wheel on Linux x86_64 or Apple Silicon macOS, check platform prerequisites, and pick a quick start.
---

# Installation

Install pVisor on the machine that will execute tool commands: your computer for the local workflow, or a Linux/KVM node for the infrastructure example. Current release packages support Linux x86_64 and Apple Silicon macOS; installation requires neither Rust nor a kernel build toolchain.

<a id="1-install-the-tools"></a>

## 1. Install the CLI {#install-cli}

Use Python 3.10 or newer, preferably in a dedicated virtual environment:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

The command should print the pVisor version. Reactivate this environment in each new terminal. You can also install into an existing project virtual environment.

<a id="2-check-platform-prerequisites"></a>

## 2. Prepare your chosen environment {#prepare-environment}

Complete only the setup for your chosen path. Neither quick-start example requires a model account or API key.

### Local workflow {#local-prerequisites}

The example uses local tools and a workspace, with the Host executor staging file changes. No VM image, Docker or Podman is required. Prepare Git, Bash and the filesystem-mount support for your platform:

- **Linux x86_64**: FUSE mounting support is required, with access to `/dev/fuse` and a usable `fusermount3` or `fusermount` command. The example's isolation mode also requires permission to create user namespaces; see the [Host executor](../guides/executors/host.md) for platform boundaries.
- **Apple Silicon macOS**: install macFUSE 5.4.0 or newer, then enable macFUSE in System Settings → General → Login Items & Extensions → File System Extensions:

```bash
brew install --cask macfuse
```

macOS uses macFUSE's FSKit backend without loading a kernel extension or reducing boot security. If FSKit is unavailable, execution fails instead of falling back to direct writes. Staging requires `--safe` or `--stage`; ordinary Host Jobs write directly to the workspace by default.

<a id="4-enable-vm-or-oci-execution-when-needed"></a>

### Execution infrastructure {#vm-and-oci}

Prepare the **Linux x86_64 node** that will run the example:

- **KVM and isolation permissions**: the current account needs read/write access to `/dev/kvm` and permission to create user namespaces. For `setgroups` or `uid_map` errors, ask the administrator to check user namespace and AppArmor policies. KVM permissions do not replace these requirements; AppArmor rules must match the actual CLI path.
- **Caller environment**: install Git and Bash, and run the caller script in the Python environment from the previous step. The task image supplies Python, project code and test dependencies inside the VM; it does not inherit the Host virtual environment.
- **Image access**: first use requires access to Docker Hub and sufficient disk space to cache the image, but no Docker Daemon. See the [VM executor guide](../guides/executors/vm.md#prepare-a-rootfs) for offline preparation.

The example script creates the VM; you do not need to start one manually. See the [VM executor guide](../guides/executors/vm.md) for other VM platforms, rootfs and mount options.

## 3. Choose a quick start {#verify-installation}

| Your goal | Start here |
| --- | --- |
| Run commands locally, then review and accept file changes | [Local workflow: run and review](first-run.md) |
| Execute tools from a caller script and return feedback to an Agent or framework | [Execution infrastructure: tool execution integration example](infrastructure.md) |

Consult [Choose an executor](../guides/executors/index.md) when you need a different execution environment.

<a id="other-installation-methods"></a>

## Other installation and maintenance options {#installation-options}

### Upgrade and uninstall {#upgrade-and-uninstall}

Run these commands in the Python environment where pVisor is installed:

```bash
python -m pip install --upgrade pvisor-sandbox
# Uninstall when needed:
python -m pip uninstall pvisor-sandbox
```

Before upgrading, check release notes and deployed `PVISOR_*` settings. Uninstalling leaves Job state and user caches in place.

<a id="homebrew-and-native-downloads"></a>

### Homebrew and native downloads {#native-downloads}

Install from the [project Homebrew tap](https://github.com/DeepLink-org/Formula):

```bash
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor
```

The explicit repository URL is required because the tap is named `Formula`, rather than `homebrew-formula`. The tap contains package metadata; binaries come from GitHub Releases. Update with `brew update` and `brew upgrade DeepLink-org/formula/pvisor`.

To install without Python, download the native archive for your platform and `SHA256SUMS` from [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases). Verify and extract the archive, add its `bin` directory to `PATH`, and keep its files together. Replace the complete directory when upgrading.

See the [release guide](../community/releasing.md#one-time-setup) for Homebrew tap publication requirements and configuration.

Job startup requires the executable and its ancestor directories to be owned by
you or root and not writable by other users; root-owned sticky directories such
as `/tmp` are allowed. Homebrew may create its Cellar with group write permission.
If startup reports `unsafe owner executable ancestor`, inspect the named directory.
For a single-owner Homebrew installation whose Cellar is group-writable, remove
write permission for group and other users before testing:

```bash
chmod go-w "$(brew --cellar)"
brew test DeepLink-org/formula/pvisor
```

This changes that directory's permissions, without recursing into installed
packages. For a shared Homebrew installation that needs group write access,
install the wheel or native archive under your own protected directory instead.

<a id="3-install-from-source-when-needed"></a>

<a id="3-nightly-and-source-builds"></a>

### Nightly and source builds {#nightly-source}

Install a nightly build from `main` inside the activated Python environment:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

To modify or build the native programs, follow the [development instructions](../community/development.md) to obtain the source, prepare the toolchain and build the CLI.

### Single-node Daemon {#daemon}

Release packages include `pvisor-daemon`. For OpenSandbox/E2B SDK access, follow the [Daemon guide](../guides/daemon/index.md) to prepare a Linux/KVM node and delegated cgroup v2, and check support for your chosen APIs. Both quick-start examples use the CLI and require no running Daemon.
