# Installation

`pvisor` runs an Agent inside a reviewable execution boundary. The default
install is that CLI.

## 1. Install the tools

```bash
pip install persisting
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
pip install persisting
```

!!! tip "Start with a Run"

    Continue with [Run your first Agent](pvisor/get-started.md). You do not need
    a separate history service to review a staged workspace.

## 2. Check platform requirements

The CLI supports macOS and Linux with Python 3.10 or newer. A normal host Run
works without a filesystem extension. On macOS, install macFUSE before using a
host-process staged Run (`pvisor run --stage …`):

```bash
brew install --cask macfuse
```

Approve the macFUSE system extension when macOS asks. Without `--stage`, the
Agent may write the real project tree. With `--stage`, if the required mount
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
pvisor run --image ubuntu:latest -- COMMAND
```

`ubuntu:latest` is also the default VM image. `--image-store DIR` changes the
local content-addressed cache, `--overlayfs-target` selects the guest workspace,
and `--vm-rootfs DIR` points to a prepared Linux rootfs. Linux hosts use KVM;
Apple Silicon macOS hosts use HVF. Building the VM support from source on macOS
also requires Zig:

```bash
brew install zig
```

Treat these options as a separate platform step. First complete the staged host
workflow so that you have a baseline Run Bundle to compare against.

## 5. Choose the next step

- [Run your first Agent](pvisor/get-started.md) — stage, review, and selectively apply changes.
- [Choose a workflow](overview.md) — the shortest path from install to a reviewed Run.
- [Execution environments](pvisor/guides/execution.md) — compare host, OCI, and VM boundaries.
