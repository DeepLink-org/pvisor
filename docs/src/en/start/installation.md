---
description: Install the pvisor CLI from a Python wheel on Linux x86_64 or Apple Silicon macOS, check platform prerequisites, and pick a quick start.
---

# Installation

Install pVisor on **Linux x86_64** or **Apple Silicon macOS**, then run your first command. Release packages need no Rust or kernel build toolchain.

<a id="1-install-the-tools"></a>

## 1. Install the CLI {#install-cli}

Use **Python 3.10+** and a dedicated virtual environment:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

Reactivate the environment in each new terminal. An existing project virtual environment works too.

<a id="2-check-platform-prerequisites"></a>

## 2. Prepare your environment {#prepare-environment}

Both quick starts use the CLI, with no model account, API key or running Daemon.

### Host: local commands {#local-prerequisites}

The [run-and-review example](first-run.md) uses your shell and local tools. It needs no VM image, Docker or Podman.

- **Linux x86_64:** FUSE mounting support, access to `/dev/fuse`, a usable `fusermount3` or `fusermount`, and permission to create user namespaces. See the [Host executor](../guides/executors/host.md) for platform boundaries.
- **Apple Silicon macOS:** macFUSE **5.4.0+**, enabled in System Settings → General → Login Items & Extensions → File System Extensions. To install it with Homebrew:

```bash
brew install --cask macfuse
```

macOS uses macFUSE's FSKit backend; no kernel extension or reduced boot security is needed. If FSKit is unavailable, execution fails instead of falling back to direct writes. **Use `--safe` or `--stage` to stage files**; ordinary Host Jobs write directly to the workspace.

??? note "VM: tool execution prerequisites"

    <a id="4-enable-vm-or-oci-execution-when-needed"></a>

    <a id="vm-and-oci"></a>

    The [VM tool example](infrastructure.md) requires a **Linux x86_64 node** with:

    - **KVM and isolation permissions:** read/write access to `/dev/kvm` and permission to create user namespaces. For `setgroups` or `uid_map` errors, ask the administrator to check user namespace and AppArmor policies. KVM access alone is insufficient; AppArmor rules must match the actual CLI path.
    - **Caller tools:** Git, Bash and the Python environment installed above. The task image supplies Python, project code and test dependencies inside the VM; it does not inherit the Host virtual environment.
    - **Image access:** Docker Hub access on first use and disk space to cache the image. No Docker Daemon is needed. See [offline rootfs preparation](../guides/executors/vm.md#prepare-a-rootfs).

    The script creates the VM for you. For Apple Silicon macOS VM support, other rootfs options and mounts, see the [VM executor guide](../guides/executors/vm.md).

## 3. Run an example {#verify-installation}

[Run and review](first-run.md) to create two staged files and keep only one. To return tool results to an Agent or framework, try [running tools in a VM](infrastructure.md).

Use [Choose an executor](../guides/executors/index.md) when you need another execution environment.

<a id="other-installation-methods"></a>

## Other installation options {#installation-options}

### Upgrade and uninstall {#upgrade-and-uninstall}

In the environment where pVisor is installed, upgrade with:

```bash
python -m pip install --upgrade pvisor-sandbox
```

Check release notes and deployed `PVISOR_*` settings first. To uninstall:

```bash
python -m pip uninstall pvisor-sandbox
```

Uninstalling leaves Job state and user caches in place.

<a id="homebrew-and-native-downloads"></a>

<a id="native-downloads"></a>

??? note "Homebrew and native downloads"

    Install from the [project Homebrew tap](https://github.com/DeepLink-org/Formula):

    ```bash
    brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
    brew install DeepLink-org/formula/pvisor
    ```

    The explicit URL is required because the repository is named `Formula`, not `homebrew-formula`. The tap holds metadata; binaries come from GitHub Releases. Upgrade with `brew update`, then `brew upgrade DeepLink-org/formula/pvisor`.

    For a native archive, download your platform's package and `SHA256SUMS` from [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases). Verify and extract it, add its `bin` directory to `PATH`, and keep the files together. Replace the complete directory when upgrading.

    Tap publication requirements are in the [release guide](../community/releasing.md#one-time-setup).

??? warning "Homebrew troubleshooting: unsafe owner executable ancestor"

    Job startup requires the executable and ancestor directories to be owned by you or root and not writable by other users. Root-owned sticky directories such as `/tmp` are allowed.

    If startup reports `unsafe owner executable ancestor`, inspect the named directory first. **Only for a single-owner Homebrew installation with a group-writable Cellar**, remove group and other write permissions, then test:

    ```bash
    chmod go-w "$(brew --cellar)"
    brew test DeepLink-org/formula/pvisor
    ```

    This changes only the Cellar directory, not packages recursively. For a shared installation that needs group write access, use a wheel or native archive in your own protected directory instead.

<a id="3-install-from-source-when-needed"></a>
<a id="3-nightly-and-source-builds"></a>

<a id="nightly-source"></a>

??? note "Nightly and source builds"

    For a nightly from `main`, download and inspect the [nightly installer](https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh) before running it in the activated Python environment. Running the installer executes code from that repository on your machine.

    To modify or build the native programs, follow the [development instructions](../community/development.md) for source and toolchain setup.

<a id="daemon"></a>

??? note "Single-node Daemon: OpenSandbox/E2B SDK access"

    Release packages include `pvisor-daemon`. Follow the [Daemon guide](../guides/daemon/index.md) to prepare a Linux/KVM node and delegated cgroup v2, and check support for your chosen APIs. Neither quick start needs the Daemon.
