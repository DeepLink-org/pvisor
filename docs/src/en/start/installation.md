# Installation

PolicyVisor (pVisor) runs Agent CLIs, scripts, and automation commands with
policy controls and an inspectable execution record. The CLI is `pvisor`;
the Python package is also named `pvisor`. Existing repository URLs,
crate names, and `PERSISTING_*` environment variables retain their current names.

If you previously installed `persisting`, run `python -m pip uninstall persisting`
before installing `pvisor` (including nightly wheels). Both distributions install
the same CLI path, so they should not coexist in one environment.

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

The CLI supports macOS and Linux with Python 3.10 or newer. A normal host Run
works without a filesystem extension. On macOS, install macFUSE before using a
host-process staged Run (`pvisor run --stage …`):

```bash
brew install --cask macfuse
```

Approve the macFUSE system extension when macOS asks. Without `--stage`, the
command may write the real project tree. With `--stage`, if the required mount
capability is unavailable, the Run fails closed rather than silently writing
the workspace without COW. The libkrun VM executor does not require macFUSE.

## 3. Install from source when needed

Use the nightly wheel when you need the latest build published from `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

For local development, install the Python package from a checkout:

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e .
```

A source build of the CLI is also available:

```bash
just install-cli
```

Use `PERSISTING_PVISOR_BIN` only when you deliberately need to test a specific
pVisor binary. Keep the Python package and CLI from the same revision when
debugging provider behavior.

## 4. Enable VM or OCI execution when needed

The default local workflow does not require Docker or Podman. To run an OCI
image through the VM executor, provide an image explicitly:

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

`ubuntu:latest` is also the default VM image. `--image-store DIR` changes the
local content-addressed cache, `--overlayfs-path` selects the guest workspace,
and `--rootfs DIR` points to a prepared Linux rootfs. Linux hosts use KVM;
Apple Silicon macOS hosts use HVF. Building the VM support from source on macOS
also requires Zig:

```bash
brew install zig
```

Treat these options as a separate platform step. First complete the staged host
workflow so that you have a baseline Run Bundle to compare against.

## 5. Choose the next step

- [Your first Run](first-run.md) — stage, review, and selectively apply changes.
- [Choose a workflow](index.md) — the shortest path from install to a reviewed Run.
- [Execution environments](../guides/execution.md) — compare host, OCI, and VM boundaries.
