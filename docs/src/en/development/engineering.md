# Engineering notes

Run commands from the repository root. `just` lists the supported tasks; each
workflow has one entry point.

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
| CI | Push/PR to `main`: formatting, Clippy, actionlint, Python tests, benchmark harness tests, Rust tests, documented cases, and examples |
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
nextest, and macOS Zig only as needed. Wheel platforms live in one reusable
workflow. PR documentation builds cannot cancel a Pages deployment.

## Build environment

The repository uses the stable toolchain from `rust-toolchain.toml`, the default
LLVM backend, and the platform linker. Install nextest `0.9.137`, or use the
repository CI setup action. macOS VM builds also require Zig.

`CARGO_TARGET_DIR` selects the native build directory. The build, install,
smoke, example, and case tasks use the same location. Wheel verification uses a
fresh staging directory, so an older wheel in `dist/` cannot satisfy the check.
Linux release wheels use manylinux_2_28 (glibc 2.28).

Documentation tasks use an isolated uv environment with the same pinned
Zensical version as CI. They do not require a separate docs virtual environment.

See [releasing PolicyVisor](releasing.md) for publishing and
[reproducible examples](examples.md) for runtime requirements.
