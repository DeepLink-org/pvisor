# Installation

Once `pvisor` is installed, you can run Agent CLIs, scripts, and automation inside a policy boundary and get checkable execution records. The Python package, CLI, and core Rust crate are all named `pvisor`; other crates use `pvisor-*` and environment variables use `PVISOR_*`.

When upgrading, update deployed `PVISOR_*` settings as well. Local state defaults to `.pvisor` and user caches to `pvisor/`; existing data is not migrated automatically.

## 1. Install the tools

```bash
pip install pvisor
```

Check the command:

```bash
pvisor --version
```

The wheel installs a matching Python package and CLI in the current Python environment. If your project has other Python dependencies, use a virtual environment:

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install pvisor
```

Published wheels target Linux x86_64 and macOS arm64. Check release artifacts or build from source for other architectures.

## 2. Check platform prerequisites

The CLI supports macOS and Linux and requires Python 3.10 or newer. Ordinary host Jobs write directly to the workspace; only `--safe` or `--stage` uses filesystem staging. Install macFUSE before running a staged host Job on macOS:

```bash
brew install --cask macfuse
```

macOS uses macFUSE's **FSKit backend** by default. Install macFUSE 5.4.0 or newer (older FSKit versions can corrupt small writes into zeroes), then enable it in System Settings → General → Login Items & Extensions → File System Extensions. This path loads no kernel extension and needs neither Recovery mode nor reduced boot security. Mounts use `/Volumes/pvisor-*`; data stays in the Job stage. If FSKit is unavailable, execution fails instead of switching to the kernel backend or direct writes. libkrun VM execution does not need macFUSE.

## 3. Install from source when needed

Use the nightly wheel for the latest `main` build:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

For local development, install the Python package from a checkout:

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
pip install -e .
```

Or build the CLI from source:

```bash
just install-cli
```

Set `PVISOR_BIN` only when you are explicitly testing a specific pVisor binary. When diagnosing provider behavior, keep the Python package and CLI on the same revision.

## Install the single-node daemon separately {#daemon}

For an OpenSandbox-compatible lifecycle API on one Linux host, follow [daemon installation and startup](../guides/daemon/index.md). The new `pvisor-daemon` is a separate source-installed executable; do not assume an existing Python wheel ships it or any ready-to-use sandbox image. Its partial OpenSandbox 1.1.0 profile has VM-only NativeRuntime embedding pVisor on Linux x86_64/KVM with delegated cgroup v2; the executable is integrated with native runtime construction and synchronous internal VM dispatch before Tokio.

A working sandbox needs a trusted local manifest/rootfs and genuine execd/egress through guest CID 3 vsock bridges. Bootstrap and image recipe are not supplied or end-to-end validated; starting the API does not establish SDK conformance or density. Stage/apply and checkpoint APIs are not implemented, and node sharing is not automatically acquired. Controller/Worker and the Cluster SDK are retired; external schedulers own cross-node orchestration.

## 4. Enable VM or OCI execution when needed

The default local workflow needs neither Docker nor Podman. To run an OCI image with the VM executor:

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

Without an explicit rootfs or image, Linux VM uses host `/` through virtio-fs and OverlayFS without pulling an image. macOS requires a Linux rootfs or image. `--image-store DIR` changes the content-addressed cache, `--mount SOURCE[:TARGET]:ACCESS` exposes host paths, and `--rootfs DIR` selects a prepared rootfs. Linux uses KVM; Apple Silicon uses HVF. The guest supervisor is a static musl Rust ELF built with Rust's linker; macOS no longer needs a C cross compiler. See [development](../community/development.md) for build prerequisites.

Treat these as separate platform steps: first complete a staged host workflow, then compare executor evidence in Run Bundles.
