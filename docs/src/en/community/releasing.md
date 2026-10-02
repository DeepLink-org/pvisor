# Release PolicyVisor

GitHub Actions builds stable releases from version tags and publishes to PyPI using Trusted Publishing. Distribution remains Python wheels without PyO3 extensions or Maturin.

Each platform wheel is tagged `py3-none-<platform>` and contains:

- The Python `pvisor` package;
- Native `pvisor` CLI;
- Platform libkrun firmware payload required by pVisor.

The current release set contains Linux x86_64 and Apple Silicon macOS wheels. Source distributions are not published artifacts.

## One-time setup

1. Create a GitHub environment named `pypi`. Do not require reviewers; restrict deployments to tags matching `v*`.
2. Add a pending Trusted Publisher in PyPI settings:
    - PyPI project: `pvisor`
    - GitHub owner: `DeepLink-org`
    - Repository: `pvisor`
    - Workflow: `release.yml`
    - Environment: `pypi`

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

The PEP 517 backend is setuptools with repository-owned `scripts/packaging/build_backend.py`. It builds the Rust CLI and stages firmware before wheel assembly. `setup.py` marks wheels platform-specific while retaining Python/ABI tags `py3-none`.

Packaging fetches pinned libkrun firmware archives for Linux x86_64 and Apple Silicon macOS unless `PVISOR_LIBKRUNFW_PATH` points to an existing payload. Local wheel builds must use a supported path; missing payload is a build error rather than an incomplete wheel.

Linux CLI links fully statically with `x86_64-unknown-linux-musl` and embeds the libkrunfw kernel; firmware shared libraries are no longer distributed in the wheel. The manylinux_2_28 tag remains for glibc Python installers. Building requires Zig, cargo-zigbuild and the Rust musl target; firmware is loaded only during the build.

Apple Silicon macOS uses the native Darwin linker, signs HVF entitlement and packages `libkrunfw.5.dylib`. Both platforms embed a static Linux musl Rust guest built automatically with `rust-lld`. macOS needs the `aarch64-unknown-linux-musl` Rust standard library but not Zig to build the guest.

Every wheel has component checks and an installed CLI smoke test. Release-set validation requires exactly one supported wheel per platform, matching versions, valid metadata and bounded artifact sizes before publication.

Rerunning a partially completed tagged release skips files already accepted by PyPI and fills missing GitHub Release assets.

## Nightly builds

**Nightly Build** runs daily at UTC 03:00 (11:00 Beijing) and can be triggered manually on `main`. Ordinary pushes run CI rather than rebuilding nightly wheels. Nightly/stable releases share the Linux/macOS build matrix, installation smoke tests and complete artifact-set validation. Nightly versions append `+g<run-number>.<commit>` and update only GitHub's `nightly` release. Stable tags publish to PyPI first, then attach the same validated wheels to GitHub Release.
