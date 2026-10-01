# Engineering notes

Run commands from the repository root. `just` lists the supported tasks; each
workflow has one entry point.

## Repository and code ownership

The Cargo workspace is organized by product responsibility. The Python `pvisor/`
package launches the packaged Rust binary; it is not a second runtime.

| Location | Owns |
|---|---|
| `crates/persisting-pvisor/` | CLI, run orchestration, executors, image preparation and cache service |
| `crates/persisting-control/` | Shared contracts, policies, AgentCtl messages, IR and event schemas |
| `crates/persisting-gateway/` | Agent protocol forwarding, conversion, capture and projection |
| `crates/persisting-overlay-core/` | FUSE-independent overlay operations and file access enforcement |
| `crates/persisting-overlayfs/` | FUSE adapter and mounts |
| `crates/persisting-overlaynet/` | Egress policy, HTTP proxy and VM virtio-net data plane |
| `crates/persisting-replay/` | Replay planning, native agent adapters and continuation bridges |
| `pvisor/`, `setup.py`, `scripts/packaging/` | Python launcher and wheel assembly |
| `crates/*/tests/` | Rust integration tests; unit tests stay with their owning module |
| `tests/` | Python packaging and repository workflow tests |
| `examples/`, `benchmark/` | Runnable product scenarios and performance measurements |
| `scripts/ci/` | CI-specific validation and smoke runners |
| `docs/src/en/`, `docs/src/zh/` | Paired documentation pages; `docs/site/` is generated output |
| `vendor/` | Patched third-party dependencies; keep product orchestration in `crates/` |

The internal workspace dependencies are:

```text
pvisor ──> control, gateway, overlaynet, overlayfs, overlay-core, replay
gateway ──> control, overlaynet
overlaynet ──> control
overlayfs ──> control, overlay-core
overlay-core ──> control
control, replay ──> no other workspace crate
```

### pVisor source modules

```text
src/
├── lib.rs                 # Stable embedding exports
├── bin/pvisor.rs          # Binary entry point
├── cli/                   # Arguments, commands, agent presets and terminal UI
├── config.rs              # Runtime and executor configuration
├── core.rs, trace.rs       # Operation-chain execution and trace journal
├── diagnostics.rs         # Shared host logs; frontend selects the destination
├── executor/
│   ├── mod.rs             # RunExecutor and AttemptContext
│   ├── process.rs         # Host process executor
│   ├── container.rs       # Container executor
│   ├── sandbox.rs         # Host OS isolation and internal sandbox entry point
│   ├── artifact.rs        # Guest-compatible executable resolution
│   ├── delegated.rs       # Delegated run spec/result hand-off
│   └── vm/                # libkrun executor and firmware acquisition
├── image/
│   ├── oci.rs             # Registry, prepared records, blobs and extraction
│   └── cache/             # Cache CLI, protocol, server, client and lazy FUSE
├── runtime/
│   ├── run.rs             # PVisor API and run lifecycle
│   ├── agentctl.rs        # Per-run cooperative control server
│   ├── event.rs           # Runtime event publication
│   ├── bundle.rs          # Durable review summary
│   ├── checkpoint.rs      # Logical checkpoint and restore
│   ├── registry.rs        # Run identity, leases and local control endpoint
│   ├── attempt.rs         # Per-attempt driver resources and teardown
│   ├── supervisor.rs      # Capability checks and driver coordination
│   ├── plan.rs            # Typed run-plan construction
│   ├── implant.rs         # Runtime environment injection
│   ├── overlay.rs         # Staging, review, apply/discard and recovery
│   └── zcode.rs           # Process compatibility policy
└── util.rs                # Small shared file/time helpers
```

Keep CLI parsing and presentation in `cli/`, concrete execution in `executor/`,
and run-scoped resource ownership in `runtime/`. Firmware belongs to the VM
executor, while OCI preparation belongs to `image/` and is shared by direct
loads and the cache server. Durable bundles/checkpoints live beside the run
registry rather than beside individual backends. Root exports such as
`PVisor`, `ProcessExecutor`, `cache` and the internal `sandbox` entry point retain
their existing import paths.

In replay, `adapter/` owns native trajectory planning and agent launch choices;
`bridge/` owns the Claude, Codex and OpenCode protocol bridges and Claude resume
transport validation. Shared execution and journaling remain at the crate root.

### Boundaries to keep improving

This organization does not claim strict one-way layering inside pVisor:
`AttemptContext` still carries runtime-owned attachments, and runtime overlay
configuration still uses Gateway configuration types. Those need contract
changes, not just file moves. `cli/run.rs`, `runtime/overlay.rs`, and the larger
agent adapters also mix several stages; split them around actual lifecycle or
protocol boundaries when changing that behavior, rather than by line count.
Do not add another crate solely to shorten these files. Keep public exports
stable and run the affected package tests after internal moves.


## Contributor commands

| Command | What it does |
|---|---|
| `just build` / `just build release` | Build the debug/release CLI and sign it for macOS Hypervisor use |
| `just install-cli` | Install the signed release CLI under `CARGO_INSTALL_ROOT` or `~/.cargo` |
| `just wheel` / `just wheel debug` | Build a fresh wheel, verify its installation, then move it into `dist/` |
| `just check` | Type-check the product and its dependencies |
| `just fmt` / `just fmt-check` | Format Rust/Python sources or check formatting without edits |
| `just lint` | Run Clippy and the Python package lint checks |
| `just test` | Run workspace Rust tests through nextest, then Python tests |
| `just test control pvisor` | Test selected Rust packages, with short names or Cargo package names |
| `just test-py -k packaging` | Forward options to pytest |
| `just test-isolation` | Run strict Linux rootless/FUSE regressions without skipping unavailable user namespaces |
| `just smoke` | Build the debug CLI and check its command surfaces |
| `just examples` | Build the release CLI and run all examples; append scenario names to select a subset |
| `just cases --case A01,A02` | Run selected documented cases |
| `just benchmark` / `just benchmark nightly` | Run the process and Run Bundle benchmark |
| `just docs-build` | Build both documentation languages and validate links |
| `just docs-serve --port 3000` | Build, watch, and serve documentation; refresh the browser after a rebuild |
| `just ci` | Check formatting, lint, test, and build without rewriting source files |
| `just clean` | Remove build outputs; retain development environments and local Run records |

`just test` and `just test-rust` accept Cargo package names and the aliases
`pvisor`, `control`, `agentctl` (compatibility alias for Control), and `capture`
(Gateway). With arguments, `just test` runs only the selected Rust packages.
Use `just test-rust` for CI shards that should not invoke Python tests.

For individual Rust integration targets or filters, call nextest directly,
for example `cargo nextest run --locked -p persisting-gateway --test llm_fixtures`.
`cargo nextest` does not run doctests; use `cargo test --doc -p <package>` when needed.

## CI responsibilities

| Workflow | Trigger and responsibility |
|---|---|
| CI | Push/PR to `main` and `develop`: formatting, Clippy, actionlint, Python tests, benchmark harness tests, Rust tests, documented cases, and examples |
| Documentation | Documentation changes: build both languages and check links; only the upstream `main` branch deploys Pages |
| pVisor Benchmark | Runtime/build/benchmark changes: compare candidate with the PR base or previous commit and upload reports |
| Nightly Build | Daily or manual on `main`: build and verify both platform wheels, then update the nightly release |
| Publish PyPI | Stable version tags: validate versions, lockfile, and main ancestry before building and publishing; manual runs only build and verify |

The required `CI` status fails if any dependency fails, is cancelled, or is
skipped. Linux Rust coverage is split into core, Gateway, and pVisor; macOS runs
the same packages once. The separate Linux isolation job requires user
namespaces and FUSE instead of allowing those checks to skip. Filesystem examples and
documented cases share its release build and isolation prerequisites. Network
and Gateway examples run in a separate job.

The shared setup action installs Python, uv, and just. Jobs opt into Rust,
nextest, and the guest Rust target as needed. Linux Rust jobs also install cargo-zigbuild. Wheel platforms live in one reusable
workflow. PR documentation builds cannot cancel a Pages deployment.

## Build environment

The repository uses the stable toolchain from `rust-toolchain.toml`, the default
LLVM backend, and the platform linker. Install nextest `0.9.137`, or use the
repository CI setup action. The guest supervisor is a static Linux musl Rust
binary built with Rust’s bundled linker; macOS VM builds do not need Zig.

`CARGO_TARGET_DIR` selects the native build directory. The build, install,
smoke, example, and case tasks use the same location. Wheel verification uses a
fresh staging directory, so an older wheel in `dist/` cannot satisfy the check.
Linux CLI builds use static musl and embed the VM kernel. Install Zig,
`cargo-zigbuild`, and `rustup target add x86_64-unknown-linux-musl`.
Linux release wheels retain manylinux_2_28 for glibc Python installers.

Documentation tasks use an isolated uv environment with the same pinned
Zensical version as CI. They do not require a separate docs virtual environment.

See [releasing PolicyVisor](releasing.md) for publishing and
[reproducible examples](examples.md) for runtime requirements.
