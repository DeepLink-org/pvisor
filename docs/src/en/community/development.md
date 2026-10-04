# Engineering guide

Run commands from the repository root. `just` lists supported tasks, with one entry per workflow.

## Repository layout and ownership

The Cargo workspace follows product responsibilities. Python `pvisor/` only launches the bundled Rust binary; it is not another runtime implementation.

| Directory | Responsibility |
| --- | --- |
| `crates/pvisor/` | CLI, orchestration, executors, image preparation and cache service |
| `crates/pvisor-core/` | Operation, Placement, policies, external interactions and Event contracts |
| `crates/pvisor-gateway/` | Agent protocol forwarding/conversion, capture and projections |
| `crates/pvisor-overlay-core/` | FUSE-independent OverlayFS operations and file access controls |
| `crates/pvisor-overlayfs/` | FUSE adaptation and mounts |
| `crates/pvisor-overlaynet/` | Egress policy, HTTP proxy and VM virtio-net path |
| `crates/pvisor-guest/` | Linux PID 1 supervisor and shared VM launch contract |
| `crates/pvisor-tui/` | Standalone terminal frontend `pvisor-tui` |
| `crates/pvisor-replay/` | Replay planning, native agent adapters and continuation protocol bridges |
| `pvisor/`, `setup.py`, `scripts/packaging/` | Python launcher and wheel packaging |
| `crates/*/tests/` | Rust integration tests; unit tests stay with their modules |
| `tests/` | Python packaging and repository workflow tests |
| `examples/`, `benchmark/` | Executable product scenarios and performance measurements |
| `scripts/ci/` | CI checks and smoke-test entry points |
| `docs/src/zh/`, `docs/src/en/` | Documentation source; `docs/site/` is generated |
| `vendor/` | Patched third-party dependencies; product orchestration belongs in `crates/` |

Actual workspace dependencies:

```text
pvisor ──> core, journal, overlaynet, overlayfs, overlay-core, guest
pvisor --features gateway ──> gateway
tui, replay ──> pvisor
gateway ──> core, overlaynet
overlaynet ──> core
overlayfs ──> core, overlay-core
overlay-core ──> core, journal
journal ──> core
core, guest ──> no other workspace crates
```

### pVisor source modules

```text
src/
├── lib.rs                 # Stable embedded API exports
├── bin/pvisor.rs          # Binary entry
├── cli/                   # Arguments, commands and shared terminal utilities
├── session/               # Attempt lifecycle and completion
├── session.rs             # Session owner
├── config.rs              # Runtime and executor configuration
├── trace.rs               # Shared fact Journal re-exports
├── diagnostics.rs         # Shared host diagnostics; frontend chooses destination
├── executor/
│   ├── mod.rs             # RunExecutor and execution output contract
│   ├── process.rs         # Host process executor
│   ├── container.rs       # Container executor
│   ├── sandbox.rs         # Host OS isolation and internal sandbox entry
│   ├── artifact.rs        # Guest-compatible executable resolution
│   ├── delegated.rs       # Delegated spec/result handoff
│   └── vm/                # libkrun executor and firmware acquisition
├── image/
│   ├── oci.rs             # Registry, prepared records, blobs and unpacking
│   └── cache/             # Cache CLI/protocol/server/client and lazy FUSE
├── runtime/
│   ├── run.rs             # PVisor API and run lifecycle
│   ├── agentctl.rs        # Per-run cooperative control server
│   ├── agentctl_client.rs # Synchronous AgentCtl client
│   ├── audit.rs           # Approval socket transport/cache
│   ├── event.rs           # Run event publication
│   ├── bundle.rs          # Persistent review summary
│   ├── checkpoint.rs      # Logical checkpoints and restoration
│   ├── registry.rs        # Run identity, liveness lock and local control endpoints
│   ├── attempt.rs         # Per-attempt driver resources/cleanup
│   ├── supervisor.rs      # Capability checks and driver coordination
│   ├── operation.rs       # Operation and observation construction
│   ├── implant.rs         # Runtime environment injection
│   ├── overlay.rs         # Stage/review/apply/drop/recovery
│   └── zcode.rs           # Process compatibility policies
└── util.rs                # Small shared file/time utilities
```

CLI arguments/display stay in `cli/`; execution mechanisms belong in `executor/`; Run resource ownership belongs in `runtime/`. Firmware belongs to the VM executor. OCI preparation belongs in `image/` and is shared by direct loading/cache service. Bundles and checkpoints belong with run records rather than one backend. Root exports such as `PVisor`, `ProcessExecutor`, `cache` and internal `sandbox` retain their import paths.

In replay, `adapter/` owns native trajectory planning and launch selection; `bridge/` owns Claude/Codex/OpenCode protocol bridges and Claude resume transport validation. Shared execution and journal remain at the crate root.

### Core implementation boundaries

Core defines Operation, Event and shared policies; pvisor implements admission, actual rewrites, Placement and scheduling. Session owns Attempt resources/terminal states; executors execute and return observations; OverlayCore owns file application/recovery. AgentCtl and approval socket I/O remain in pvisor. See [Core architecture](../design/architecture.md) for ownership and [Operation and Event](../design/operations-events.md) for fields/order.

## Core reduction budget

CI builds the default core independently before the capture-enabled distribution. `scripts/ci/check_core_budget.py` rejects Gateway, replay, TUI and their terminal dependencies in the default core. It records toolchain, dependency count, source lines, public Core declarations and binary bytes. The script owns budget/measurement definitions; CI reports contain measured results. Compare on the same platform/toolchain.

## Contributor commands

| Command | Purpose |
| --- | --- |
| `just build` / `just build release` | Build debug/release CLI and sign Hypervisor entitlement on macOS |
| `just install-cli` | Install signed release CLI to `CARGO_INSTALL_ROOT` or `~/.cargo` |
| `just wheel` / `just wheel debug` | Build a fresh wheel; place it in `dist/` after installation validation |
| `just check` | Compilation checks for the product and its dependencies |
| `just fmt` / `just fmt-check` | Format Rust/Python or check formatting |
| `just lint` | Clippy and Python package lint |
| `just test` | Workspace Rust tests via nextest, followed by Python tests |
| `just test core pvisor` | Selected Rust packages, using aliases or Cargo package names |
| `just test-py -k packaging` | Pass options to pytest |
| `just test-benchmark` | Benchmark tool tests via pytest; also included in default Python tests |
| `just test-py --vm-bin target/release/pvisor` | Real VM terminal/TUI interaction regressions |
| `just test-py tests/test_zcode_integration.py --zcode-integration` | Explicit integration requiring Linux rootless, FUSE3 and zcode |
| `just test-isolation` | Strict Linux rootless/FUSE regressions; missing user namespaces do not skip checks |
| `just smoke` | Build debug CLI and check main commands |
| `just examples` | Build release CLI and run all examples; append names for a subset |
| `just cases --case S-DOC-001,S-DOC-002` | Selected documentation cases |
| `just benchmark` / `just benchmark nightly` | Process and Run Bundle benchmarks |
| `just docs-build` | Build bilingual documentation and check links |
| `just docs-serve --port 3000` | Build/watch/preview docs; refresh manually after rebuild |
| `just ci` | Check format/lint/tests and build without rewriting source |
| `just clean` | Remove build artifacts, preserving development environment/local Run records |

`just test`/`just test-rust` accept Cargo package names and aliases `pvisor`, `core`, `control`/`agentctl` (Core compatibility aliases) and `capture` (Gateway). With package arguments, `just test` runs only those Rust tests. CI shards use `just test-rust` without additionally running Python tests.

Default pytest collection includes `tests/` and `benchmark/pvisor/`. Rust tests in `pvisor-core` verify shared Operation/Overlay contracts. Benchmark tests requiring `/proc` and Linux rootfs tools run only on Linux.

VM filesystem checks run inside the Linux guest and require root, Python, pytest and tar. From the repository run `python3 -m pytest -q tests/test_vm_filesystem.py --guest-fs-dir /var/tmp --guest-fs-dir .` to check guest root/workspace filesystems separately. Checks skip without directory arguments; explicitly enabled failures are errors.

For a specific Rust integration test/filter, call nextest directly, for example `cargo nextest run --locked -p pvisor-gateway --test llm_fixtures`. nextest excludes doctests; use `cargo test --doc -p <package>` when needed.

## CI responsibilities

| Workflow | Trigger and responsibility |
| --- | --- |
| CI | Push/PR to `main`/`develop`: format, Clippy, actionlint, Python/benchmark-tool/Rust tests, documentation cases and examples |
| Documentation | Documentation changes: bilingual build/link checks; only upstream `main` deploys Pages |
| pVisor Benchmark | Runtime/build/benchmark changes: compare with PR base/previous commit and upload reports |
| Nightly Build | Daily or manually on `main`: build/validate two-platform wheels and update nightly release |
| Publish PyPI | Stable tag: check version, lockfile and main ancestry before publishing; manual runs build/validate only |

Keep required status `CI`: any failed, canceled or skipped dependency fails it. Linux Rust tests shard into core/Gateway/pVisor; macOS runs the same package set once. The separate Linux isolation job requires user namespaces/FUSE and cannot skip isolation checks. Filesystem examples/documentation cases share its release build/isolation environment. Network/Gateway examples run separately.

The shared setup action installs only Python, uv and just by default. Rust, nextest and guest Rust targets are opt-in. Linux static CLI/shim builds enable Zig/cargo-zigbuild via `static-musl`; Rust checks/unit tests do not need them. A reusable workflow owns the two-platform wheel matrix. PR documentation builds do not cancel Pages deployments.

## VM guest startup

`pvisor-guest` provides the shared `GuestConfig` library and the `pvisor-guest` executable. With `pvisor-vm`'s `init-blob` feature, `crates/pvisor-vm/build.rs` uses Rust's `rust-lld` to build a release Linux musl ELF for the VM architecture. Separate `target/pvisor-guest/` avoids competing with the outer Cargo artifact lock. `pvisor-vm` embeds the ELF as `/init.krun`, which becomes guest PID 1.

CLI/shim inject `/.pvisor-guest.json` containing argv, environment, cwd, workspace mounts, limits, optional networking and shim agent arguments. The supervisor initializes guest filesystems/console I/O, mounts the workspace, configures networking, launches the workload directly and reaps children. On exit, it reports the workload exit code using libkrun's private rootfs ioctl `0x7602`, then syncs/reboots. Nonzero exit fails the Attempt; normal VM shutdown without a reported code fails with 125.

See [Guest init comparison](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md#guest-init-comparison-apple-silicon) for measurements and scope.

## Packaging and names

The Python package, CLI and core Rust crate use `pvisor`; companion crates use `pvisor-*`; environment variables use `PVISOR_*`. Wheel names follow `pvisor-<version>-py3-none-<platform>.whl`.

## Build environment

Use the stable toolchain from `rust-toolchain.toml`, default LLVM backend and platform linker. Install nextest `0.9.137` or use the CI setup action. Rust's bundled linker builds the guest supervisor as a static Linux musl ELF; macOS VM builds no longer need Zig. On Apple Silicon run `rustup target add aarch64-unknown-linux-musl` before the first build. CI installs only the current architecture's guest target; workspace configuration does not download unrelated cross-compilation targets.

| Artifact | Linux | Apple Silicon macOS |
| --- | --- | --- |
| Host CLI | Static Linux musl ELF | Native Darwin executable with HVF entitlement |
| Embedded guest | Static Linux musl ELF | Static Linux musl ELF |
| pvisor-vm | Single Rust runtime crate | Single Rust runtime crate |
| Guest kernel | Embedded at build time | Runtime-loaded `libkrunfw.5.dylib` |

`CARGO_TARGET_DIR` selects native build output shared by build/install/smoke/examples/cases. Wheel verification uses a fresh staging directory so old `dist/` packages cannot be mistaken for new artifacts. Linux CLI links musl statically and embeds the VM kernel. Building requires Zig, cargo-zigbuild and `rustup target add x86_64-unknown-linux-musl`. Linux wheels retain manylinux_2_28 for glibc Python installers.

Documentation tasks use an isolated uv environment with the same pinned Zensical as CI; no separate docs virtual environment is required.

See [Release process](releasing.md) and [Reproducible examples](examples.md) for releases/runtime requirements.


The merged private runtime modules use a libkrun 1.19.3 base with selected upstream backports. See the [upstream synchronization ledger](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/provenance/libkrun/UPSTREAM.md) for source commits, local adaptations and validation limits; the version number does not imply a complete 1.19.6 or 2.0 upgrade.

## VM API boundary

`pvisor_vm::api` is the sole external runtime interface. It declares portable structs and trait signatures with no conditional compilation or method bodies. Private modules implement the contracts; consumers import `VmConfiguration`, `VmRuntime`, `VmControl` and the snapshot/RAM traits they use. Platform services use `RuntimeSupport` on `VmPlatform`.

CLI, shim, examples and the init benchmark use this interface. Low-level register/device tests and hardware probes belong inside `pvisor-vm`. Static kernel extraction/packing now lives in `crates/pvisor-vm/build_kernel.rs`; its existing build environment variables remain compatible. Firmware data ABI and OS FFI remain private. Historical trace/receipt identifiers and benchmark evidence retain their original names.

Run `just test pvisor-vm`; macOS signs its test executables with the existing Hypervisor entitlement. Real VM tests require host HVF/KVM access; Linux VM-creation tests require `/dev/kvm`.
