# Test architecture

Run commands from the repository root. Mise owns task dependencies and the
toolchain; native runners own discovery, selection and results.

## Core principles

1. Test observable contracts. Check returned values, exact bytes, errors and
   state after failure. Match configuration structurally when its shape is the
   contract.
2. Keep tests with the owner of the behavior. Private state stays inside its
   crate; external API contracts use the public interface. Each behavior has
   one primary regression.
3. Use native runners: nextest/libtest, unittest, Node, Bats, Go and semspec.
   Let their discovery rules include new tests. Shared fixtures own resources
   and inputs, never testcase lists or verdicts.
4. Isolate effects. Each test owns its workspace and processes, bounds waits
   and output, synchronizes readiness, and cleans up on failure. Preserve
   diagnostics before removing failed resources.
5. Report coverage honestly. Mocks establish adapter behavior. Real process,
   mount, VM and device guarantees require those boundaries to run. A missing
   prerequisite in a strict acceptance gate is a failure; ignored or skipped
   tests are unexecuted coverage.
6. Keep failures meaningful. Preserve independent oracles and positive controls.
   A fix must not weaken assertions, deadlines, claims or xfail annotations.
   Semspec execution and human approval are separate.

## Layers and ownership

| Layer | Location | What it checks |
| --- | --- | --- |
| Rust implementation tests | `#[cfg(test)]` modules beside implementations | Private logic, invariants and private-state regressions |
| Rust contracts and integration | `crates/*/tests/` | Public APIs, durable records, CLI and cross-component workflows |
| Python unit | `tests/unit/` | Logic, formats, policy and adapter decisions with external operations mocked |
| Python process regression | `tests/regression/` | Real children, toolchains, PTYs and process-lifecycle boundaries |
| Script regression | `tests/tasks.toml`, `tests/tasks/` | Task arguments, prerequisite refusal, signing, publication and cleanup |
| Hardware acceptance | `tests/hardware/`, explicit native Rust gates | Real mounts, hypervisors, guest execution and restoration |
| Product scenarios | `examples/pvisor/`, `tests/podman/` | End-to-end user workflows and supported CLI compatibility |
| Semantic specifications | `docs/src/zh/cases/`, `docs/src/zh/reference/` | Executable product promises with human review |

Regression describes a test's purpose; a unit test can prevent a regression too.
Rust's `--lib` selects a Cargo target, whose private-state tests may use
processes or mounts. Related contracts can share a test binary through ordinary
Rust modules. Keep separate binaries where process-global state needs isolation.

Python uses standard unittest discovery through package `__init__.py` files.
`test-py` discovers `unit/` and `regression/`; CI runs those disjoint subsets.
`fixtures.py` and `packaging_fixtures.py` contain shared resource/input helpers.
Import helpers rather than another module's TestCase.

Workload drivers and guest probes live directly under `tests/`. Hardware modules
have explicit names outside the default `test*.py` pattern.

## Execution

| Command | Scope |
| --- | --- |
| `mise run test` | Rust, script regressions, Python, benchmark-tool tests, semspec Python tests and API doctests |
| `mise run test -- pvisor-core` | One Rust package; package arguments select Rust only |
| `mise run test -- pvisor-cli -- --test apply_cli` | One native Rust integration target |
| `mise run test -- pvisor-core -- --lib` | One library test target |
| `mise run test-py` | Python unit and process regressions |
| `mise run test-py-unit` | Python unit tests |
| `mise run test-py-regression` | Python process/integration regressions |
| `mise run test-scripts` | Task and shell regressions |
| `mise run test-doc` | Cargo doctests for the API crates |
| `mise run test-benchmark-tools` | Benchmark tooling correctness, without performance measurements |
| `mise run test-semspec` | The semspec implementation |
| `mise run test-home-demo` | Homepage explanatory model through Node |
| `mise run test-agent-plugins` | Plugin contracts through Node |
| `mise run test-agent-plugins-native` | Plugin workflows through the real CLI |
| `mise run fw -- test` | Firmware bundle and ABI checks |
| `mise run examples -- SCENARIO` | A product example |
| `mise run cases -- --suite stage` | STAGE semantic specifications |

Pass selections to the native runner:

```bash
mise run test -- pvisor-core -- --test network_contracts -E 'test(addresses::)'
mise run test-py-unit -- -k packaging -v
mise run test-py -- tests.unit.test_vm_cli_contract tests.regression.test_vm_cli_contract
```

Use direct `cargo test` for doctests or a documented special runner. Self-exec
Rust fixtures use an ignored child probe invoked by its parent. Select owning
tests explicitly; broad `--run-ignored all` can select subprocess-only helpers.

## Capability gates and coverage

| Gate | Prerequisites and boundary |
| --- | --- |
| `mise run test-isolation` | Linux user/mount/PID/network namespaces, read-write `/dev/fuse`, fusermount3; real Host isolation and prepared-rootfs admission |
| `mise run test-macos-fuse` | macOS with working macFUSE; real staging, isolation and checkpoint workflows |
| `test-hvf-cold-restore`, `test-vm-snapshot-state` | Apple Silicon HVF and signed probes; CPU/RAM and VMM-thread state |
| `test-linux-cold-restore`, `test-environment-snapshot` | Apple Silicon HVF, Linux guest and firmware; whole-machine and independent environment restoration |
| `tests/vm_cli_contract.py` | KVM, HVF or WHP; shared guest workload, authenticated exit, read-only files and cleanup |
| `tests/windows_container_contract.py` | Native Windows/WHP, matching firmware/template; container network, I/O, console and lifecycle |

Linux CI runs strict Host isolation and KVM workflows. Windows CI runs native
contracts and WHP acceptance. Hosted macOS CI runs builds and conventional tests;
real HVF and macFUSE acceptance needs a capable Mac. See
[platform requirements](../docs/src/en/reference/platforms.md).

The VM filesystem, VM terminal and ZCode Python regressions are opt-in through
`PVISOR_TEST_GUEST_FS_DIRS`, `PVISOR_TEST_VM_BIN` and `PVISOR_TEST_ZCODE`.
ConPTY is Windows-only. Their default skips do not establish acceptance.

Rust contracts and Host workflows cover selective apply, conflicts, drop,
completion publication and cancellation. The Linux isolation job runs selected
DOC cases and the USE suite; STAGE specifications have an explicit separate
entry. Passing implementation tests does not approve a semantic claim.

## CI and evidence

[CI](../.github/workflows/ci.yml) owns the required aggregate.
[Windows](../.github/workflows/windows-vm.yml) owns native WHP checks;
[documentation](../.github/workflows/docs.yml) owns the bilingual site build.
These workflows and `mise.toml` define the executed selections.

Record inputs, artifact identities, selected cases, exit/timeout results and
cleanup status. Keep logs and domain evidence in ignored output directories or
CI artifacts, outside restored build caches. The rootless gate writes native
JUnit under `$CARGO_TARGET_DIR/nextest/rootless/` and retains its task log.
Do not run independent gates against the same mutable output directory.

Test READMEs describe architecture and operation. Per-run counts, timings,
machine paths and debugging transcripts belong in execution artifacts.
Keep brief historical outcomes in [chaos campaign summaries](CHAOS_HISTORY.md):
scope, result, unresolved issues, cleanup and evidence location. Keep raw logs
out of the repository root and tracked Markdown.
Only maintainers may approve/revoke semspec or edit its review ledgers and
approved snapshots; see [the design](../tools/semspec/DESIGN.md).

## Specialized workflows

Keep resource budgets, fault oracles and cleanup instructions beside each driver:

- [Docker CLI contracts and container chaos](podman/README.md)
- [Stage/apply chaos](STAGE_CHAOS.md)
- [Daemon/SDK chaos](DAEMON_CHAOS.md)
- [Compose chaos](COMPOSE_CHAOS.md)

`network_cli_chaos.py` exercises catalog operations without VMs or packet-delivery
claims. Use its `--help` for bounded duration, workload, seed and output options.
Long-running campaigns are explicit correctness experiments, separate from the
default unit and regression suites.
