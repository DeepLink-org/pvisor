# Installation

Install the native CLI with Python 3.10 or newer, then run a local command. Current wheels support Linux x86_64 and Apple Silicon macOS; installing a release package needs neither Rust nor a kernel build toolchain.

## 1. Install the tools

Use a dedicated virtual environment to avoid changing the system Python environment:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor
```

Check the command:

```bash
pvisor --version
pvisor spawn -- /bin/echo hello
```

The wheel installs `pvisor`, `pvisor-tui`, `pvisor-replay` and `pvisor-daemon` into that environment's bin directory without a Python launcher. Reactivate the environment in a new terminal, or use `~/.venvs/pvisor/bin/pvisor` directly. You can also install into an existing project virtual environment.

Upgrade in the same environment:

```bash
python -m pip install --upgrade pvisor
```

To uninstall:

```bash
python -m pip uninstall pvisor
```

Before upgrading, check release notes and deployed `PVISOR_*` settings. Local state under `.pvisor` and user caches under `pvisor/` are not migrated automatically; uninstalling the Python package also leaves this runtime data in place. Homebrew, npm and Cargo entry points and a standalone binary installer remain part of the [distribution proposal](../community/releasing.md#distribution-plan).

## 2. Check platform prerequisites

Wheel installation supports macOS and Linux and requires Python 3.10 or newer. Ordinary host Jobs write directly to the workspace; only `--safe` or `--stage` uses filesystem staging. Install macFUSE before running a staged host Job on macOS:

```bash
brew install --cask macfuse
```

macOS uses macFUSE's **FSKit backend** by default. Install macFUSE 5.4.0 or newer (older FSKit versions can corrupt small writes into zeroes), then enable it in System Settings → General → Login Items & Extensions → File System Extensions. This path loads no kernel extension and needs neither Recovery mode nor reduced boot security. Mounts use `/Volumes/pvisor-*`; data stays in the Job stage. If FSKit is unavailable, execution fails instead of switching to the kernel backend or direct writes. libkrun VM execution does not need macFUSE.

## 3. Nightly and source builds

Install a nightly build from `main` inside the activated Python environment:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

To modify or build the native programs, check out the source and prepare the toolchain using the [development instructions](../community/development.md):

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

Build and install the CLI from the prepared checkout:

```bash
mise run install-cli
```

To test a specific native build, invoke its path directly (for example `target/debug/pvisor`) or put its directory first on `PATH`. Installed native scripts ignore `PVISOR_BIN`. Editable Python installation is not a native CLI build; use `mise run build` or `mise run install-cli`.

## Install the single-node daemon {#daemon}

Linux x86_64 and macOS ARM64 wheels both include the separate `pvisor-daemon` executable. Cache serving and standalone memory pools work on both platforms; the OpenSandbox/E2B sandbox API requires Linux x86_64/KVM and delegated cgroup v2.

Prepare a trusted base with `prepare-image`; new templates default to the Rust guest engine and shared compressed image pool. The pinned SDKs' core lifecycle, command and file flows have native validation; full SDK conformance and production density remain unestablished. See the [daemon guide](../guides/daemon/index.md) for deployment, protocol scope and evidence. Stage/apply and checkpoint APIs remain on the native Job path; external schedulers handle cross-node orchestration.

## 4. Enable VM or OCI execution when needed

The default local workflow needs neither Docker nor Podman. To run an OCI image with the VM executor:

```bash
pvisor spawn --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

Without an explicit rootfs or image, Linux VM uses host `/` through virtio-fs and OverlayFS without pulling an image. macOS requires a Linux rootfs or image. `--image-store DIR` changes the content-addressed cache, `--mount SOURCE[:TARGET]:ACCESS` exposes host paths, and `--rootfs DIR` selects a prepared rootfs. Linux uses KVM; Apple Silicon uses HVF. The guest supervisor is a static musl Rust ELF built with Rust's linker; macOS does not require a C cross compiler. See [development](../community/development.md) for build prerequisites.

Once the environment is ready, follow [your first run](first-run.md) through staging, review and selective apply. See [executor boundaries](../security/executor-boundaries.md) to check which file and network controls were actually installed.
