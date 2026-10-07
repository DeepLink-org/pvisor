# Engineering guide

Run commands from the repository root. `just` lists supported tasks, with one entry per workflow.

## Repository layout and ownership

The Cargo workspace has 13 crates organized by product responsibility. Its default member is `pvisor-cli`. Python `pvisor/` is an installable version marker, not a launcher or runtime implementation. Wheels install native executable scripts directly into the environment's bin directory; the old Python launcher and its binary override are obsolete.

| Directory | Responsibility |
| --- | --- |
| `crates/pvisor/` | Embeddable runtime, Session/Attempt orchestration, executors, durable Job service, image preparation and cache mechanisms |
| `crates/pvisor-cli/` | Four application binaries, CLI commands, terminal frontends and replay/TUI companion dispatch |
| `crates/pvisor-vm/` | Native VM runtime, portable API, private VMM/platform implementations, embedded guest and kernel/firmware integration |
| `crates/pvisor-daemon/` | Linux x86_64 sandbox lifecycle API, detached native VM supervisors and optional daemon-owned pool |
| `crates/pvisor-core/` | Operation, Placement, policies, external interactions and Event contracts |
| `crates/pvisor-journal/` | Shared fact Journal storage and readers |
| `crates/pvisor-gateway/` | Agent protocol forwarding/conversion, capture and projections |
| `crates/pvisor-overlay-core/` | FUSE-independent OverlayFS operations and file access controls |
| `crates/pvisor-overlayfs/` | FUSE adaptation and mounts |
| `crates/pvisor-overlaynet/` | Egress policy, HTTP proxy and VM virtio-net path |
| `crates/pvisor-guest/` | Linux PID 1 supervisor and shared VM launch contract |
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
vm ──> overlay-core
daemon ──> pvisor, core
replay ──> core, journal
shim ──> guest, overlay-core
shim --features vm ──> vm
gateway ──> core, journal, overlaynet
overlaynet ──> core
overlayfs ──> core, overlay-core
overlay-core ──> core, journal
journal ──> core
core, guest ──> no other workspace crates
```

`pvisor` has no CLI or Clap normal dependency; Clap is a development dependency for examples only. The `pvisor-replay` engine has no normal dependency on `pvisor` or Clap. The `pvisor-tui` crate has been removed; its executable name is unchanged.

### pVisor source modules

```text
crates/pvisor-cli/src/
├── lib.rs                 # Frontend modules, not runtime re-exports
├── bin/                   # Four: pvisor, pvisor-cache, pvisor-tui, pvisor-replay
├── cli/                   # Arguments, commands and shared terminal utilities
│   ├── cache.rs           # Cache argument parsing and rendering
│   └── features.rs        # Runtime feature listing frontend
├── companions.rs          # Same-installation companion lookup/dispatch

└── tui/                   # TUI PTY runtime, renderer, review panels and keymap

crates/pvisor/src/
├── lib.rs                 # Runtime exports and explicit frontend/embedding APIs
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
│   └── vm/                # VM executor adapter and Run resource/control integration
├── image/
│   ├── oci.rs             # Registry, prepared records, blobs and unpacking
│   └── cache/             # Cache protocol/server/client and lazy FUSE
├── runtime/
│   ├── run.rs             # PVisor API and run lifecycle
│   ├── job_service.rs     # Durable RuntimeJobService
│   ├── job_execution.rs   # Job execution mechanisms
│   ├── host_transport.rs  # Typed Host transport
│   ├── instance_control.rs # Local instance control exchange
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

CLI arguments/display and Host listener/workers belong in `pvisor-cli`; execution mechanisms belong in `pvisor`'s `executor/`, and Run resource ownership and the durable Job service belong in `runtime/`. The VM executor adapts Run/Attempt lifecycle to `pvisor_vm::api`; `pvisor-vm` owns the VMM, platform mechanisms, embedded guest and kernel/firmware integration. OCI preparation belongs in `image/` and is shared by direct loading/cache service. Cache storage and the authenticated server remain in the runtime; cache command parsing/rendering belongs in `pvisor-cli/src/cli/cache.rs`. Bundles and checkpoints belong with run records rather than one backend. `pvisor-daemon/src/memory_pool.rs` owns pool startup/reuse and the detached `memory-pool` component; `serve --memory-pool` enables it. Node protocols in `pvisor/src/node.rs` and `node/` remain runtime facilities; the removed CLI node supervisor is not a daemon adapter. `pvisor-cache` retains independent preparation, publication, serving and reads.

Existing public runtime imports, including `PVisor`, `ProcessExecutor`, `cache` and the internal `sandbox` entry, retain their paths. Explicit frontend/embedding APIs export filesystem access types, `GatewayProfile`, `DelegatedRunOutput`, `rootless_runtime_available`, overlay selection/inspection and Run lookup/control helpers, Linux Run leases, `audit`, `checkpoint`, `job_execution` and startup/private-JSON helpers. Runtime implementation modules remain private; these exports do not establish an API stability promise.

In replay, `adapter/` owns native trajectory planning and launch selection; `bridge/` owns Claude/Codex/OpenCode protocol bridges and Claude resume transport validation. Shared execution and journal remain at the crate root.

### Core implementation boundaries

Core defines Operation, Event and shared policies; pvisor implements admission, actual rewrites, Placement and scheduling. Session owns Attempt resources/terminal states; executors execute and return observations; OverlayCore owns file application/recovery. AgentCtl and approval socket I/O remain in pvisor. See [Core architecture](../design/architecture.md) for ownership and [Operation and Event](../design/operations-events.md) for fields/order.

## Core reduction budget

CI checks the default runtime and application dependency boundaries before the capture-enabled distribution. `scripts/ci/check_core_budget.py` rejects the CLI, Gateway, replay, Clap, TUI and terminal dependencies in the normal `pvisor` closure. The default `pvisor-cli` application includes the replay engine and integrated TUI, while Gateway remains optional. It records toolchain, runtime/application dependency counts, runtime-closure source lines, public Core declarations and application binary bytes. The script owns budget/measurement definitions; CI reports contain measured results. Compare on the same platform/toolchain.

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
| `just test core pvisor cli` | Selected Rust packages: shared contracts, runtime and application |
| `just test cli` / `just test pvisor-cli` | Executable/frontend tests; `just test pvisor` selects runtime tests |
| `just test pvisor-vm` | VM-owner tests; macOS signs Hypervisor entitlement before nextest |
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
| `just docs-serve` / `just docs-serve en` | Native Zensical preview with live reload; Chinese on port 3000, English on port 3001 |
| `just ci` | Check format/lint/tests and build without rewriting source |
| `just clean` | Remove build artifacts, preserving development environment/local Run records |

`just test`/`just test-rust` accept Cargo package names and aliases `pvisor`, `cli` (`pvisor-cli`), `core`, `control`/`agentctl` (Core compatibility aliases), `capture` (Gateway) and `shim` (`pvisor-shim`). With package arguments, `just test` runs only those Rust tests. CI shards use `just test-rust` without additionally running Python tests.

Runtime-only Rust tests remain in `crates/pvisor/tests/`. The 19 executable/frontend integration test files, including mixed runtime/command tests, live in `crates/pvisor-cli/tests/`; mixed files retain their runtime-only cases in `pvisor`. Native VM and environment-dependent tests keep their existing prerequisites and skip/ignore gates; compile checks do not validate real guests.

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

The Python package, installed CLI and runtime Rust crate use `pvisor`; the application crate is `pvisor-cli`, and companion crates use `pvisor-*`; environment variables use `PVISOR_*`. Wheel names follow `pvisor-<version>-py3-none-<platform>.whl`.

## Build environment

Use the stable toolchain from `rust-toolchain.toml`, default LLVM backend and platform linker. Install nextest `0.9.137` or use the CI setup action. Rust's bundled linker builds the guest supervisor as a static Linux musl ELF; macOS VM builds do not require Zig. On Apple Silicon run `rustup target add aarch64-unknown-linux-musl` before the first build. CI installs only the current architecture's guest target; workspace configuration does not download unrelated cross-compilation targets.

| Artifact | Linux | Apple Silicon macOS |
| --- | --- | --- |
| Host CLI | Static Linux musl ELF | Native Darwin executable with HVF entitlement |
| Embedded guest | Static Linux musl ELF | Static Linux musl ELF |
| `pvisor-vm` | Single Rust runtime crate | Single Rust runtime crate |
| Guest kernel | Embedded at build time | Runtime-loaded `libkrunfw.5.dylib` |

`CARGO_TARGET_DIR` selects native build output shared by build/install/smoke/examples/cases. Wheel verification uses a fresh staging directory so old `dist/` packages cannot be mistaken for new artifacts. Linux CLI links musl statically and embeds the VM kernel. Building requires Zig, cargo-zigbuild and `rustup target add x86_64-unknown-linux-musl`. Linux wheels retain manylinux_2_28 for glibc Python installers.

Documentation tasks use an isolated uv environment with the same pinned Zensical as CI; no separate docs virtual environment is required.

See [Release process](releasing.md) and [Reproducible examples](examples.md) for releases/runtime requirements.


The private runtime modules use a libkrun 1.19.3 base with selected upstream backports. See the [upstream synchronization ledger](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/provenance/libkrun/UPSTREAM.md) for source commits, local adaptations and validation limits; the version number does not imply a complete 1.19.6 or 2.0 upgrade.

## VM API boundary

`pvisor_vm::api` is the sole external runtime interface. It declares portable structs and trait signatures with no conditional compilation or method bodies. Private modules implement the contracts; consumers import `VmConfiguration`, `VmRuntime`, `VmControl` and the snapshot/RAM traits they use. Platform services use `RuntimeSupport` on `VmPlatform`.

CLI, shim, examples and the init benchmark use this interface. Low-level register/device tests and hardware probes belong inside `pvisor-vm`. Static kernel extraction/packing lives in `crates/pvisor-vm/build_kernel.rs`; its existing build environment variables remain compatible. Firmware data ABI and OS FFI remain private. Historical trace/receipt identifiers and benchmark evidence retain their original names.

Run `just test pvisor-vm`; macOS signs its test executables with the existing Hypervisor entitlement. Real VM tests require host HVF/KVM access; Linux VM-creation tests require `/dev/kvm`.
