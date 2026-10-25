# Engineering guide


Run commands from the repository root. `mise tasks ls` lists public tasks; `mise run ci` runs the local check sequence, and `mise run ci -- check "test pvisor-core"` runs selected tasks in order and stops on failure. Internal tasks stay callable but are hidden from the default help.

Install [mise](https://mise.jdx.dev/installing-mise.html) 2026.8.5 or newer, then prepare the development environment:

```bash
mise trust
mise install --locked
mise run setup
mise run doctor
```

`mise.toml` pins Rust (including rustfmt and Clippy), Python 3.12, uv, Ruff, actionlint, jq, Zig, nextest and cargo-zigbuild. `setup` installs the current architecture's Linux musl guest target and syncs development dependencies into `.venv` from `uv.lock`; it neither installs pVisor itself nor builds firmware. The Python product still supports 3.10+; development tools use 3.12.

CI and wheel builds use mise 2026.10.18 with the committed tool versions and lockfiles. For compatible local tools already on `PATH`, set `disable_tools = ["python", "uv", "jq"]` in the `[settings]` table of the ignored `mise.local.toml`; disabled tools must be provided by the caller. Python development tasks require 3.12 or newer, and `UV_PYTHON` falls back to `python3` when Python is disabled. Select other mise-managed versions in that file's `[tools]` table and check them with `mise ls --current`. Release builds and recorded benchmark comparisons use a clean checkout without personal overrides or disabled tools.

`mise run` selects the project tools and environment without shell activation. For direct Cargo or Python commands, use `mise exec -- cargo …` / `mise exec -- python3 …`, or activate your shell following mise's shell setup instructions. Separate task arguments from mise with `--`; Rust tests use a second `--` between package names and nextest options, for example `mise run test -- core -- --offline`.

Platform package managers still provide native dependencies: Linux build libraries, FUSE, KVM and cgroup permissions, or macOS Xcode Command Line Tools, LLVM/GNU tools and macFUSE. See Build environment below and the executor guides. Machine-specific overrides belong in ignored `mise.local.toml`; keep personal paths and credentials out of shared configuration. Update tool versions in `mise.toml`, use CI's mise 2026.10.18 to run `mise lock --platform linux-x64,macos-arm64` and commit `mise.lock`; after changing Python dependencies, run `mise exec -- uv lock` and commit `uv.lock`.

## Repository layout and ownership

The Cargo workspace has 13 crates organized by product responsibility. Its default member is `pvisor-cli`. Python `pvisor/` is an installable version marker, not a launcher or runtime implementation. Wheels install native executable scripts directly into the environment's bin directory; the old Python launcher and its binary override are obsolete.

| Directory | Responsibility |
| --- | --- |
| `crates/pvisor/` | Embeddable runtime, Session/Attempt orchestration, executors, durable Job service, image preparation and cache mechanisms |
| `crates/pvisor-cli/` | Three application binaries, CLI commands, terminal frontends and replay/TUI companion dispatch |
| `crates/pvisor-vm/` | Native VM runtime, portable API, private VMM/platform implementations, embedded guest and kernel/firmware integration |
| `crates/pvisor-daemon/` | Linux x86_64 sandbox API, native guest protocol adapters and cross-platform cache/pool components |
| `crates/pvisor-core/` | Operation, Placement, policies, external interactions and Event contracts |
| `crates/pvisor-journal/` | Shared fact Journal storage and readers |
| `crates/pvisor-gateway/` | Agent protocol forwarding/conversion, capture and projections |
| `crates/pvisor-overlay-core/` | FUSE-independent OverlayFS operations and file access controls |
| `crates/pvisor-overlayfs/` | FUSE adaptation and mounts |
| `crates/pvisor-overlaynet/` | Egress policy, HTTP proxy and VM virtio-net path |
| `crates/pvisor-guest/` | Linux PID 1, shared launch contracts and native process/file control engine |
| `crates/pvisor-shim/` | containerd Runtime v2 shim; optional VM execution |
| `crates/pvisor-replay/` | Replay planning, native agent adapters and continuation protocol bridges |
| `pvisor/`, `setup.py`, `scripts/packaging/` | Python version marker and native-script wheel packaging |
| `crates/*/tests/` | Rust integration tests; unit tests stay with their modules |
| `tests/` | Python packaging and repository workflow tests |
| `examples/`, `benchmark/` | Executable product scenarios and performance measurements |
| `scripts/ci/` | CI checks and smoke-test entry points |
| `docs/src/zh/`, `docs/src/en/` | Documentation source; `docs/site/` is generated |
| `vendor/` | Patched third-party dependencies; product orchestration belongs in `crates/` |

Direct normal workspace dependencies (including target-specific edges; names below omit the `pvisor-` prefix):

```text
cli ──> pvisor, core, journal, replay, overlaynet, overlay-core, vm
pvisor ──> core, journal, overlaynet, overlayfs, overlay-core, guest, vm
cli --features gateway ──> gateway, pvisor/gateway
pvisor --features gateway ──> gateway
vm ──> overlay-core, guest
daemon ──> pvisor, core, guest
replay ──> core, journal
shim ──> guest, overlay-core
shim --features vm ──> vm, overlaynet, core
gateway ──> core, journal, overlaynet
overlaynet ──> core
overlayfs ──> core, overlay-core
overlay-core ──> core, journal
journal ──> core
core, guest ──> no other workspace crates
```

`pvisor` has no CLI or Clap normal dependency; Clap is a development dependency for examples only. The `pvisor-replay` engine has no normal dependency on `pvisor` or Clap. The `pvisor-tui` crate has been removed; its executable name is unchanged.

### pVisor source modules

| Implementation entry | Ownership |
| --- | --- |
| `pvisor-cli/src/cli/mod.rs` | Build parsing/dispatch for the selected command; root help describes command groups |
| `pvisor-cli/src/cli/host*.rs`, `run/` | Frontend/owner startup, terminal, signals and Job command adaptation |
| `pvisor/src/runtime/job_service/` | Durable Job lifecycle, workspace, forks, capture, views and policy |
| `pvisor/src/runtime/attempt_service.rs` | In-process live Attempt status/pause/resume/termination shared by Jobs and daemon |
| `pvisor/src/session*`, `runtime/attempt.rs` | Per-Attempt driver preparation, execution and finalization |
| `pvisor/src/runtime/completion.rs` | Commit and validate completion binding Bundle bytes to terminal results |
| `pvisor/src/docker.rs`, `image/` | Per-container VMs, image catalog and shared compressed image pool; ordinary OCI execution remains separate |
| `pvisor-daemon/src/daemon/`, `runtime.rs` | APIs, admission, records, supervisors and cgroup/VM ownership |
| `pvisor-daemon/src/native/`, `prepared_image.rs` | OpenSandbox/E2B guest protocol adaptation and template publication |
| `pvisor-replay/src/adapter/`, `bridge/` | Trajectory planning, agent selection and native continuation bridges |

The CLI owns arguments/display; the runtime owns execution and durable Jobs. VM, filesystem and network implementations stay in their owning crates. Core defines shared policies and wire DTOs. Jobs and daemon reuse in-process control while retaining their own authentication, admission, storage and confirmed cleanup.

See [Core architecture](../design/architecture.md#api-boundaries) for public interfaces and migration status. Only VM, OverlayFS and Journal currently use a sole `api` entry. Other crates retain their actual exports; this logical table does not imply unified Rust visibility or stability.

### Core implementation boundaries

Core defines Operation, Event and shared policies; pvisor implements admission, actual rewrites, Placement and scheduling. Session owns Attempt resources/terminal states; executors execute and return observations; OverlayCore owns file application/recovery. AgentCtl and approval socket I/O remain in pvisor. See [Core architecture](../design/architecture.md) for ownership and [Operation and Event](../design/operations-events.md) for fields/order.

## Core reduction budget

CI checks the default runtime and application dependency boundaries before the capture-enabled distribution. `scripts/ci/check_core_budget.py` rejects the CLI, Gateway, replay, Clap, TUI and terminal dependencies in the normal `pvisor` closure. The default `pvisor-cli` application includes the replay engine and integrated TUI, while Gateway remains optional. It records toolchain, runtime/application dependency counts, runtime-closure source lines, public Core declarations and application binary bytes. The dependency count, closure source lines and Core declarations ratchet against a committed baseline (`scripts/ci/core-budget.json`): any metric above the baseline fails CI, so no absolute caps are hand-maintained in the CI config. A change that intentionally grows the budget regenerates the baseline with `mise run update-core-budget` in the same commit, keeping the exact delta reviewable; regenerating after a reduction tightens the baseline to reality. The script owns budget/measurement definitions; CI reports contain measured results. Compare on the same platform/toolchain.

## Contributor commands

| Command | Purpose |
| --- | --- |
| `mise run build` / `mise run build -- release` | Build all workspace executables supported on this platform; sign Hypervisor entitlement on macOS |
| `mise run install-cli` | Install signed release CLI to `CARGO_INSTALL_ROOT` or `~/.cargo` |
| `mise run wheel` / `mise run wheel -- debug` | Build a fresh wheel; place it in `dist/` after installation validation |
| `mise run fw -- build` / `mise run fw -- test` | Build firmware or run bundle/ABI regressions |
| `mise run doctor` / `mise run doctor -- test` | Tool/platform diagnostics; mise installs missing tools first |
| `mise run check` | Compilation checks for the product and its dependencies |
| `mise run fmt` / `mise run ci -- "fmt-rust --check" "fmt-py --check"` | Format Rust/Python or check formatting |
| `mise run lint` | Clippy, Python and workflow lint |
| `mise run test` | Workspace Rust tests via nextest, followed by Python tests |
| `mise run test -- core pvisor cli` | Selected Rust packages: shared contracts, runtime and application |
| `mise run test -- cli` / `mise run test -- pvisor-cli` | Executable/frontend tests; `mise run test -- pvisor` selects runtime tests |
| `mise run test -- pvisor-vm` | VM-owner tests; macOS signs Hypervisor entitlement before nextest |
| `mise run test-py -- -k packaging` | Filter Python unittest checks in both test directories |
| `mise run test-py -- discover -s benchmark/support` | Benchmark tool unittest checks; also included in default Python tests |
| `PVISOR_TEST_VM_BIN=target/release/pvisor mise run test-py -- tests/test_vm_terminal.py` | Real VM terminal/TUI interaction regressions |
| `PVISOR_TEST_ZCODE=1 mise run test-py -- tests/test_zcode_integration.py` | Explicit integration requiring Linux rootless, FUSE3 and zcode |
| `mise run test-isolation` | Strict Linux rootless/FUSE regressions; missing user namespaces do not skip checks |
| `mise run smoke` | Build debug CLI and check main commands |
| `mise run examples` | Build release CLI and run all examples; append names for a subset |
| `mise run cases -- --case S-DOC-001,S-DOC-002` | Selected documentation cases |
| `mise run benchmark` / `mise run benchmark -- nightly` | Process and Run Bundle benchmarks |
| `mise run docs-build` | Build bilingual documentation and check links |
| `mise run docs-serve` / `mise run docs-serve -- en` | Native Zensical preview with live reload; Chinese on port 3000, English on port 3001 |
| `mise run ci` | Check format/lint/tests and build without rewriting source |
| `mise run clean` | Remove build artifacts, preserving development environment/local Run records |

`mise run test`/`mise run test-rust` accept Cargo package names and aliases `pvisor`, `cli` (`pvisor-cli`), `core`, `control`/`agentctl` (Core compatibility aliases), `capture` (Gateway) and `shim` (`pvisor-shim`). With package arguments, `mise run test` runs only those Rust tests. CI shards use `mise run test-rust` without additionally running Python tests.

Runtime-only Rust tests remain in `crates/pvisor/tests/`. Executable/frontend integration test files, including mixed runtime/command tests, live in `crates/pvisor-cli/tests/`; mixed files retain their runtime-only cases in `pvisor`. Native VM and environment-dependent tests keep their existing prerequisites and skip/ignore gates; compile checks do not validate real guests.

Default unittest discovery includes `tests/` and `benchmark/`. Rust tests in `pvisor-core` verify shared Operation/Overlay contracts. Benchmark tests requiring `/proc` and Linux rootfs tools run only on Linux.

VM filesystem checks run inside the Linux guest and require root, Python and tar. From the repository run `PVISOR_TEST_GUEST_FS_DIRS=/var/tmp:. mise run test-py -- tests/test_vm_filesystem.py` to check guest root/workspace filesystems separately. Checks skip without directory configuration; explicitly enabled failures are errors.

For a specific Rust integration test/filter, pass nextest arguments after `--`, for example `mise run test-rust -- pvisor-gateway -- --test llm_fixtures`. nextest excludes doctests; use `cargo test --doc -p <package>` when needed.

## CI responsibilities

| Workflow | Trigger and responsibility |
| --- | --- |
| CI | Push/PR to `main`/`develop`: format, Clippy, actionlint, Python/benchmark-tool/Rust tests, documentation cases and examples |
| Documentation | Documentation changes: bilingual build/link checks; only upstream `main` deploys Pages |
| pVisor Benchmark | Runtime/build/benchmark changes: compare with PR base/previous commit and upload reports |
| Nightly Build | Daily or manually on `main`: build/validate two-platform wheels and update nightly release |
| Publish PyPI | Stable tag: check version, lockfile and main ancestry before publishing; manual runs build/validate only |

Keep required status `CI`: any failed, canceled or skipped dependency fails it. Linux Rust tests shard into core/Gateway/pVisor; macOS runs the same package set once. The separate Linux isolation job requires user namespaces/FUSE and cannot skip isolation checks. Filesystem examples/documentation cases share its release build/isolation environment. Network/Gateway examples run separately.

The shared setup action installs the same mise tools and Python development dependencies as local development. Its `native-firmware` input prepares macOS native firmware tools, and `static-musl` installs Linux system build libraries; tool versions come from `mise.toml`. A reusable workflow owns the two-platform wheel matrix, including mise with the same configuration inside manylinux. PR documentation builds do not cancel Pages deployments.

## VM guest startup

`pvisor-guest` provides the shared `GuestConfig` library and the `pvisor-guest` executable. With `pvisor-vm`'s `init-blob` feature, `crates/pvisor-vm/build.rs` uses Rust's `rust-lld` to build a release Linux musl ELF for the VM architecture. Separate `target/pvisor-guest/` avoids competing with the outer Cargo artifact lock. `pvisor-vm` embeds the ELF as `/init.krun`, which becomes guest PID 1.

CLI/shim inject `/.pvisor-guest.json` containing argv, environment, cwd, workspace mounts, limits, optional networking and shim agent arguments. The supervisor initializes guest filesystems/console I/O, mounts the workspace, configures networking, launches the workload directly and reaps children. On exit, it reports the workload exit code using libkrun's private rootfs ioctl `0x7602`, then syncs/reboots. Nonzero exit fails the Attempt; normal VM shutdown without a reported code fails with 125.

See [Guest init comparison](https://github.com/DeepLink-org/pvisor/blob/555e750105e8072e0b85030dfb36226c66dae206/benchmark/pvisor/README.md#guest-init-comparison-apple-silicon) for measurements and scope.

## Packaging and names

The Python package, installed CLI and runtime Rust crate use `pvisor`; the application crate is `pvisor-cli`, and companion crates use `pvisor-*`; environment variables use `PVISOR_*`. Wheel names follow `pvisor-<version>-py3-none-<platform>.whl`.

## Build environment

Use the Rust toolchain pinned in `mise.toml`, the default LLVM backend and platform linker. `mise run setup` installs the current architecture's guest target. Rust's bundled linker builds the guest supervisor as a static Linux musl ELF; macOS VM builds do not require Zig.

| Artifact | Linux | Apple Silicon macOS |
| --- | --- | --- |
| Host CLI | Static Linux musl ELF | Native Darwin executable with HVF entitlement |
| Embedded guest | Static Linux musl ELF | Static Linux musl ELF |
| `pvisor-vm` | Single Rust runtime crate | Single Rust runtime crate |
| Guest kernel | Compressed and embedded at build time | Compressed and embedded at build time |

`CARGO_TARGET_DIR` selects native build output shared by build/install/smoke/examples/cases. Wheel verification uses a fresh staging directory so old `dist/` packages cannot be mistaken for new artifacts. Linux CLI links musl statically and embeds the VM kernel. The mise installation and `setup` provide Zig, cargo-zigbuild and the Linux musl target. Linux wheels retain manylinux_2_28 for glibc Python installers.

Documentation tasks use an isolated uv environment with the same pinned Zensical as CI; no separate docs virtual environment is required.

See [Release process](releasing.md) and [Reproducible examples](examples.md) for releases/runtime requirements.

The private runtime modules use a libkrun 1.19.3 base with selected upstream backports. See the [upstream synchronization ledger](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/provenance/libkrun/UPSTREAM.md) for source commits, local adaptations and validation limits; the version number does not imply a complete 1.19.6 or 2.0 upgrade.

## VM API boundary

`pvisor_vm::api` is the sole external runtime interface. It declares portable structs and trait signatures with no conditional compilation or method bodies. Private modules implement the contracts; consumers import `VmConfiguration`, `VmRuntime`, `VmControl` and the snapshot/RAM traits they use. Platform services use `RuntimeSupport` on `VmPlatform`.

CLI, shim, examples and the init benchmark use this interface. Low-level register/device tests and hardware probes belong inside `pvisor-vm`. Static kernel extraction/packing lives in `crates/pvisor-vm/build_kernel.rs`; its existing build environment variables remain compatible. Firmware data ABI and OS FFI remain private. Historical trace/receipt identifiers and benchmark evidence retain their original names.

Run `mise run test -- pvisor-vm`; macOS signs its test executables with the existing Hypervisor entitlement. Real VM tests require host HVF/KVM access; Linux VM-creation tests require `/dev/kvm`.
