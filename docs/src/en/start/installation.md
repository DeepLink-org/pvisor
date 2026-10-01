# Installation

PolicyVisor (pVisor) runs Agent CLIs, scripts, and automation commands with
policy controls and an inspectable execution record. The CLI is `pvisor`;
the Python package and core Rust crate are also named `pvisor`.
Companion crates use `pvisor-*`; environment variables use `PVISOR_*`.

## 1. Install the tools

```bash
pip install pvisor
```

Verify that the command is available:

```bash
pvisor --version
```

The wheel installs matching versions of the Python package and the `pvisor`
CLI into the active Python environment. Use a virtual environment when the
project has other Python dependencies:

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install pvisor
```

!!! tip "Start with a Run"

    Continue with [Your first Run](first-run.md). You do not need
    a separate history service to review a staged workspace.

Published wheels target Linux x86_64 and macOS arm64. Check the release artifacts before choosing another architecture.

## 2. Check platform requirements

The CLI supports macOS and Linux with Python 3.10 or newer. Ordinary host Runs
write through to the workspace; `--safe` or `--stage` uses a filesystem stage.
On macOS, install macFUSE before using staged host execution:

```bash
brew install --cask macfuse
```

macOS uses the macFUSE **FSKit backend** by default. Install macFUSE 5.4.0 or later (older FSKit releases can corrupt small writes with zero-filled data),
then enable it under System Settings → General → Login Items & Extensions →
File System Extensions. This backend does not load a kernel extension and needs
no Recovery-mode security changes. Mounts appear at `/Volumes/pvisor-*`; backing
data stays in the Run's stage directory. If FSKit is unavailable, the Run fails
closed without falling back to a kernel backend or writing through. The libkrun VM executor does not require macFUSE.

## 3. Install from source when needed

Use the nightly wheel when you need the latest build published from `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

For local development, install the Python package from a checkout:

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
pip install -e .
```

A source build of the CLI is also available:

```bash
just install-cli
```

Use `PVISOR_BIN` only when you deliberately need to test a specific
pVisor binary. Keep the Python package and CLI from the same revision when
debugging provider behavior.

## 4. Enable VM or OCI execution when needed

The default local workflow does not require Docker or Podman. To run an OCI
image through the VM executor, provide an image explicitly:

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

Without a rootfs or image option, Linux VM runs use the host `/` through virtio-fs;
macOS requires an explicit Linux rootfs or image. `--image-store DIR` changes the
local content-addressed cache, `--mount SOURCE[:TARGET]:ACCESS` exposes a path,
and `--rootfs DIR` points to a prepared Linux rootfs. Linux hosts use KVM;
Apple Silicon macOS hosts use HVF. Building the VM support from source on macOS
uses the Rust `pvisor-guest` supervisor; no C cross-compiler is needed.
Source build prerequisites are listed in [Engineering notes](../development/engineering.md).

Treat these options as a separate platform step. First complete the staged host
workflow so that you have a baseline Run Bundle to compare against.

## 5. Choose the next step

- [Your first Run](first-run.md) — stage, review, and selectively apply changes.
- [Choose a workflow](index.md) — the shortest path from install to a reviewed Run.
- [Execution environments](../guides/execution.md) — compare host, OCI, and VM boundaries.
