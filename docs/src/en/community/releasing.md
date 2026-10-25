# Release PolicyVisor

The current stable-release workflow builds native components from version tags in GitHub Actions, packages them as Python wheels and uploads them to PyPI using Trusted Publishing. See [installation](../start/installation.md) for user commands. Wheels contain no PyO3 extensions and do not use Maturin.

Each platform wheel is tagged `py3-none-<platform>` and contains:

- The Python `pvisor` version marker, not a launcher;
- Three native scripts `pvisor`, `pvisor-tui` and `pvisor-replay`, installed directly into the environment's bin directory;
- Independently built `pvisor-daemon` in Linux x86_64 and macOS ARM64 wheels;
- `libkrunfw.5.dylib` only on Apple Silicon macOS; Linux embeds the kernel at build time.

The current release set contains Linux x86_64 and Apple Silicon macOS wheels and their corresponding firmware sources. Python source distributions (sdists) are not part of the release set.

## One-time setup

1. Create a GitHub environment named `pypi`. Do not require reviewers; restrict deployments to tags matching `v*`.
2. Add a pending Trusted Publisher in PyPI settings:
    - PyPI project: `pvisor`
    - GitHub owner: `DeepLink-org`
    - Repository: `pvisor`
    - Workflow: `release.yml`
    - Environment: `pypi`

3. Register trusted native runners with the `pvisor-release` label: Linux x86_64
   with KVM access, and Apple Silicon macOS with Hypervisor.framework access.
   Install mise's pinned Python/Rust tools and the matching Linux musl Rust target
   (`mise run setup`). Dedicate these runners to trusted release commits and
   remove previous Job state between runs. Missing runners or virtualization
   permissions block publication.

No PyPI API token is stored in GitHub. A pending publisher can create the project on the first successful upload but does not reserve its name.

Before pushing the first release tag, verify that the `pvisor` Trusted Publisher matches the current repository, workflow and environment.

## Prepare a release

1. Set the same `X.Y.Z` version in `pyproject.toml`, the workspace package section of `Cargo.toml` and `pvisor/__init__.py`.
2. Refresh local workspace versions in the lockfile without upgrading dependencies:

   ```bash
   cargo metadata --format-version 1 >/dev/null
   ```

3. Commit version changes and merge into `main`. The workflow rejects tags whose commit is not reachable from `main`.
4. Optionally run **Publish PyPI** manually. It builds/validates all wheels without publishing.
5. Create/push the matching stable tag:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

## Build and validation

The PEP 517 backend is setuptools with repository-owned `scripts/packaging/build_backend.py`. It builds the native components and stages platform payloads before wheel assembly. `setup.py` installs binary scripts without Python console-entry-point wrappers and marks wheels platform-specific while retaining Python/ABI tags `py3-none`.

Packaging builds the pinned firmware sources in `fw/`, verifies downloaded kernel
inputs and caches builds by their inputs. `PVISOR_LIBKRUNFW_PATH` can select an
existing payload. Missing firmware is a build error. Each platform exports its
corresponding firmware sources for publication alongside the wheels.

Linux CLI links fully statically with `x86_64-unknown-linux-musl` and embeds the libkrunfw kernel; the wheel does not include firmware shared libraries. The manylinux_2_28 tag remains for glibc Python installers. Building requires Zig, cargo-zigbuild and the Rust musl target; firmware is loaded only during the build.

Apple Silicon macOS uses the native Darwin linker, signs HVF entitlement and packages `libkrunfw.5.dylib`. Both platforms embed a static Linux musl Rust guest built automatically with `rust-lld`. macOS needs the `aarch64-unknown-linux-musl` Rust standard library but not Zig to build the guest.

Stable releases first run the required CI workflow at the release commit, then
build wheels. Each final wheel is installed in a fresh environment on its native
runner. Acceptance checks host exit status, refusal of strict host execution,
real VM startup, suspend/cold restore with continuing in-memory progress, stable
Job/new Attempt identity, staged writes and review. A separate fresh VM Job
checks selective apply; the restored layered Job must refuse its unsupported
apply path. It uses a
small static guest compiled from repository-owned source; unavailable hardware
fails acceptance. Logs retain wheel and installed binary SHA-256 plus command
results as `release-acceptance-*` artifacts. Run the same acceptance locally with
`mise run accept-wheel -- WHEEL LOG_JSON`.

Release-set validation requires exactly one supported wheel per platform,
matching versions, valid metadata and bounded artifact sizes. Both platform
acceptances and the complete set must pass before either publication channel.

Stable versions are immutable. A retry verifies existing GitHub/PyPI file
SHA-256 values before publishing. Identical files are retained and missing files
can be added; different bytes under an existing filename fail and require a new
version. GitHub uploads never overwrite assets, and PyPI does not blindly skip
existing filenames. Corresponding firmware sources are published on GitHub before
PyPI wheels. A configured gate is not evidence that acceptance has passed.

The Linux x86_64 daemon uses native `pvisor-vm` with KVM and delegated cgroup v2.
New templates use the shared Rust guest engine and host protocol adapters;
explicit execd/envd binaries retain their upstream backend. Wheel acceptance
covers native Jobs; daemon SDK conformance, containerd integration and density
require their own acceptance evidence. See [daemon setup](../guides/daemon/index.md).

## Nightly builds

**Nightly Build** runs daily at UTC 03:00 (11:00 Beijing) and can be triggered manually on `main`. Ordinary pushes run CI rather than rebuilding nightly wheels. Nightly/stable releases share the Linux/macOS build matrix, installation smoke tests and complete artifact-set validation. Nightly versions append `+g<run-number>.<commit>` and update only GitHub's `nightly` release. Stable tags publish validated wheels and corresponding firmware sources to GitHub Release before uploading the wheels to PyPI. Install the daemon from either platform’s nightly wheel or build it locally. Cache serving supports both; sandbox serving retains the Linux prerequisites above. Standalone daemon archives are not published.

## Distribution evolution {#distribution-plan}

!!! note "Proposal — not implemented"
    Prioritize native binary installation, the CLI and structured output, then extend package-manager channels and SDKs according to integration needs. The channel extensions, shared release manifest and installer below are not part of the current workflow.

### Channel roles

| Channel | Users | Current status and direction |
| --- | --- | --- |
| PyPI | CLI users in Python environments | Wheel workflow exists; add installation and upgrade acceptance for isolated tool environments |
| GitHub Release | CI, servers and offline installation | Wheels and firmware sources exist; propose binary archives and a versioned installer requiring neither Python nor Rust |
| Homebrew | macOS developers | Propose an initial project-owned tap reusing versioned native artifacts |
| npm | CLI users in Node toolchains | Add a native-program installation entry point when needed; deliver it separately from a TypeScript SDK |
| crates.io / Cargo | Rust embedding and source-build users | Evaluate later, after addressing internal path dependencies, build resources and public API publication boundaries |

### Shared native artifacts

After each platform's native build, wheels, binary archives and package-manager entry points reuse the same component set. A proposed release manifest records the version, commit, platform, component hashes and firmware sources; each archive has its own SHA-256. Publication uploads accepted artifacts without recompiling. Upgrade the matching CLI, companions, daemon and firmware as one installation unit to avoid incompatible partial updates.

Each new channel needs final-layout validation, including companion and firmware discovery through Homebrew symlinks and Python tool environments. The installer should support version selection, user-level directories, verification, upgrades, rollback and removal while retaining Jobs and user caches. Package-manager installations use that manager for upgrades. Installing the daemon executable remains separate from registering and starting a system service.

Keep the initial matrix at Linux x86_64 and macOS arm64. Add native acceptance before expanding platform support. Channel acceptance should cover first useful execution, the existing staged-review workflow, upgrades and removal as well as version output. VM capabilities retain real-hardware acceptance. Stable releases remain immutable, with a separate nightly entry point.

### CLI, protocols and SDKs

Running an existing Agent CLI needs no language SDK: pVisor starts the command directly, regardless of the Agent's implementation language. Platform integrations can initially use documented commands, exit codes and [JSON output](../reference/json-output.md), pinning versions according to the [compatibility boundaries](../reference/stability.md). Complete schemas and stable cross-version protocols still need work.

First settle structured contracts for startup, cancellation, status, events and file review, then choose a TypeScript or Python SDK based on actual integration users. An npm entry point can distribute only the Rust native CLI; it requires no TypeScript rewrite and does not imply an SDK exists. An SDK is not a prerequisite for the current CLI release.
