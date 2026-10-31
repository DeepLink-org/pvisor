---
description: Install pVisor step by step, check Linux or macOS file staging support, and start your first Agent style stagebox task.
---

# Installation

Install the CLI, prepare your platform, and follow the [quick start](first-run.md) for the mode you want to use.

Release packages support Linux x86_64 and Apple Silicon macOS. Python installation needs 3.10+; no Rust toolchain or Docker Daemon is needed. The Docker style path requires virtualization and image downloads. The offline Agent style demo uses your local shell without a VM image or model account.

<a id="1-install-the-tools"></a>

## 1. Install the CLI {#install-cli}

Use a regular user terminal. Check Python first:

```bash
python3 --version
```

Expect 3.10 or newer. Create a dedicated virtual environment and install:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
```

The installed command is `pvisor`; the Python distribution is `pvisor-sandbox`. In a new terminal, run `source ~/.venvs/pvisor/bin/activate` before using the CLI. If `venv` is unavailable, install your operating system's Python venv support first.

You can also install into an existing project virtual environment. For installation without Python, use [Homebrew or native packages](#native-downloads) below.

<a id="2-check-platform-prerequisites"></a>

## 2. Prepare your platform {#prepare-environment}

Follow the prerequisites for your chosen mode. Docker style uses VM support and does not need the FUSE setup used by Host staging.

| Path | Preparation |
| --- | --- |
| Docker style sandbox | [VM support](#docker-prerequisites), registry access, and cache space. |
| Offline Agent style stagebox demo | [Host file staging](#local-prerequisites) and a local shell. |

### Docker style sandbox: VM support {#docker-prerequisites}

On Linux x86_64, check architecture and your account's KVM access:

```bash
uname -m
test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
```

Expect `x86_64` and `KVM accessible`. If the device is missing or inaccessible, ask the administrator to configure virtualization and account permissions. Virtualized servers also need nested virtualization. Namespace policy can affect Linux VMM restrictions; see the [VM guide](../guides/executors/vm.md) for startup permission failures.

On Apple Silicon macOS, `uname -m` should print `arm64`. Use a release package with the Hypervisor entitlement; source builds need signing as described in [VM platform setup](../guides/executors/vm.md). This VM path does not require macFUSE for Host staging.

First use needs Docker Hub access and image-cache space. Start a VM with the [Docker tutorial](first-run.md#docker-style) to confirm that the setup works.

### Agent style stagebox: Host file staging {#local-prerequisites}

`--safe` needs to mount a staged file view. Complete only the steps for your platform.

#### Linux x86_64 {#linux-setup}

Check the architecture, FUSE device, and mount helper:

```bash
uname -m
ls -l /dev/fuse
command -v fusermount3 || command -v fusermount
```

Expect `x86_64`, an existing device, and a helper path from the final command. On Debian/Ubuntu, install `fuse3` if the helper is missing; use the equivalent package on other distributions. Ask the administrator to configure FUSE if the device is missing or inaccessible. Containers and managed servers must also allow device access and mounting.

Check basic user namespace availability:

```bash
unshare --user --map-root-user true
```

Success produces no output and exits 0. For `Operation not permitted`, `setgroups`, or `uid_map` errors, ask the administrator to check user namespace and AppArmor policies. This probe does not replace pVisor's actual startup checks; see the [Host executor](../guides/executors/host.md).

#### Apple Silicon macOS {#macos-setup}

Check the architecture:

```bash
uname -m
```

Expect `arm64`. Install macFUSE 5.4.0 or newer:

```bash
brew install --cask macfuse
```

Enable macFUSE in System Settings → General → Login Items & Extensions → File System Extensions, and complete any system prompts. pVisor uses FSKit without loading a kernel extension or reducing boot security. Staging startup fails if FSKit is unavailable.

Ordinary Host Jobs write directly to the workspace by default. The tutorial uses `--safe` for staging; keep it while resolving mount failures.

??? note "Optional: Linux VM tool execution setup"

    <a id="4-enable-vm-or-oci-execution-when-needed"></a>

    ### Execution infrastructure {#vm-and-oci}

    The [VM tool tutorial](infrastructure.md) runs on a Linux x86_64 node. In addition to the CLI, Python, Git, and Bash, check that your account can access KVM:

    ```bash
    test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
    ```

    Expect `KVM accessible`. If absent, ask the administrator to check the device and account permissions. Virtualized servers also need nested virtualization. The example requires user namespaces too; KVM access alone does not establish readiness.

    Initial task-image preparation needs access to Docker Hub and enough disk space to cache the image. The image includes Python, test tools, and project code; it does not inherit the Host virtual environment. No Docker Daemon is needed. For offline rootfs preparation, see the [VM guide](../guides/executors/vm.md#prepare-a-rootfs).

    These checks apply to the Linux VM tutorial. For VM support and prerequisites on other platforms, see the [VM executor](../guides/executors/vm.md).

## 3. Check the installation {#verify-installation}

In the terminal with your installation environment activated, run:

```bash
command -v pvisor
pvisor --version
pvisor run --help
pvisor spawn --help
```

Expect the CLI path, version number, and help for both entry points. The selected tutorial then verifies VM execution or staging with an actual run.

| Problem | Action |
| --- | --- |
| `pvisor` is missing | Reactivate the virtual environment; native-package users should add the package's `bin` to `PATH`. |
| pip reports a system-managed Python environment | Install in the virtual environment above. |
| No package matches the platform | Check Python version and system architecture; see the [platform matrix](../reference/platforms.md). |
| CLI works but task startup fails | Check FUSE, FSKit, and user namespaces under [local prerequisites](#local-prerequisites), keeping `--safe`. |

## 4. Complete your first task {#next-steps}

- VM ready: follow [Docker style sandbox](first-run.md#docker-style) through images, run, exec, stop, and removal.
- Host staging ready: follow [Agent style stagebox](first-run.md#agent-style) through an offline command, review, acceptance, and cleanup.

After the staging demo, try a real agent handoff with the [Claude Code repair task](../guides/agents/claude-code.md#repair-task). For a Python caller collecting VM tool feedback, use the [execution infrastructure example](infrastructure.md).

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
