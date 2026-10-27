# Installation

Install the native CLI with Python 3.10 or newer, then run a local command. Current wheels support Linux x86_64 and Apple Silicon macOS; installing a release package needs neither Rust nor a kernel build toolchain.

## 1. Install the tools

Use a dedicated virtual environment to avoid changing the system Python environment:

```bash
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
```

Check the command:

```bash
pvisor --version
pvisor spawn -- /bin/echo hello
```

The wheel installs `pvisor`, `pvisor-tui`, `pvisor-replay` and `pvisor-daemon` into that environment's bin directory without a Python launcher. Reactivate the environment in a new terminal, or use `~/.venvs/pvisor/bin/pvisor` directly. You can also install into an existing project virtual environment.

Upgrade in the same environment:

```bash
python -m pip install --upgrade pvisor-sandbox
```

To uninstall:

```bash
python -m pip uninstall pvisor-sandbox
```

Before upgrading, check release notes and deployed `PVISOR_*` settings. Local state under `.pvisor` and user caches under `pvisor/` are not migrated automatically; uninstalling the Python package also leaves this runtime data in place. npm, Cargo and a standalone version-selecting installer remain part of the [distribution proposal](../community/releasing.md#distribution-plan).

### Homebrew and native downloads

Add this repository as a tap, then install the stable native package:

```bash
brew tap DeepLink-org/pvisor https://github.com/DeepLink-org/pvisor.git
brew install DeepLink-org/pvisor/pvisor
brew test DeepLink-org/pvisor/pvisor
# Upgrade: brew upgrade pvisor / brew uninstall pvisor
```

The tap uses `Formula/pvisor.rb` in this source repository. The explicit URL is
required because Homebrew's short tap form assumes a `homebrew-` repository
prefix. After the initial tap, `brew update` retrieves formula updates from this
repository. Supported platforms are Apple Silicon macOS and Linux x86_64.
Homebrew does not install Python or build Rust, and does not start the daemon as a service.

Job startup requires the executable and its ancestor directories to be owned by
you or root and not writable by other users; root-owned sticky directories such
as `/tmp` are allowed. Homebrew may create its Cellar with group write permission.
If startup reports `unsafe owner executable ancestor`, inspect the named directory.
For a single-owner Homebrew installation whose Cellar is group-writable, remove
write permission for group and other users before testing:

```bash
chmod go-w "$(brew --cellar)"
brew test DeepLink-org/pvisor/pvisor
```

This changes that directory's permissions, without recursing into installed
packages. For a shared Homebrew installation that needs group write access,
install the wheel or native archive under your own protected directory instead.

For installation without a package manager, open [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) and download `pvisor-X.Y.Z-linux-x86_64.tar.gz` or `pvisor-X.Y.Z-macos-arm64.tar.gz`, together with `SHA256SUMS`. Verify the matching checksum (`sha256sum ARCHIVE` on Linux, `shasum -a 256 ARCHIVE` on macOS), extract the archive into a version-specific directory owned by you (for example under `~/.local/opt/pvisor`) and add its `bin` directory to `PATH`. Keep all files in `bin` together. Replace the complete directory when upgrading; remove it and its `PATH` entry to uninstall. Job state and user caches remain in place. The rolling `nightly` release provides the same native archive layout.

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
