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
3. Use native runners: nextest/libtest, unittest, Node, Bats, Go and Scrut.
   Let their discovery rules include new tests. Shared fixtures own resources
   and inputs, not testcase catalogs or Markdown parsing. The product scenario
   fixture preserves explicit XFAIL/XPASS semantics that Scrut lacks; native
   Scrut still owns execution discovery, matching and the final test result.
4. Isolate effects. Each test owns its workspace and processes, bounds waits
   and output, synchronizes readiness, and cleans up on failure. Preserve
   diagnostics before removing failed resources.
5. Report coverage honestly. Mocks establish adapter behavior. Real process,
   mount, VM and device guarantees require those boundaries to run. A missing
   prerequisite in a strict acceptance gate is a failure; ignored or skipped
   tests are unexecuted coverage.
6. Keep failures meaningful. Preserve independent oracles and positive controls.
   A fix must not weaken assertions, deadlines, claims or xfail annotations.
   Specification execution and human approval are separate.

## Layers and ownership

| Layer | Location | What it checks |
| --- | --- | --- |
| Rust implementation tests | `#[cfg(test)]` modules beside implementations | Private logic, invariants and private-state regressions |
| Rust contracts and integration | `crates/*/tests/` | Public APIs, durable records, CLI and cross-component workflows |
| Python unit | `tests/unit/` | Logic, formats, policy and adapter decisions with external operations mocked |
| Python process regression | `tests/regression/` | Real children, toolchains, PTYs and process-lifecycle boundaries |
| Feature specifications and regressions | `tests/spec/<feature>/` | Native Scrut product contracts and their co-located executable regressions; see [specification layout](spec/README.md) |
| Script regression | `tests/tasks.toml`, `tests/tasks/` | Task arguments, prerequisite refusal, signing, publication and cleanup |
| Hardware acceptance | `tests/hardware/`, explicit native Rust gates | Real mounts, hypervisors, guest execution and restoration |
| Product scenarios | `examples/pvisor/`, `tests/podman/` | End-to-end user workflows and supported CLI compatibility |

Regression describes a test's purpose; a unit test can prevent a regression too.
Rust's `--lib` selects a Cargo target, whose private-state tests may use
processes or mounts. Related contracts can share a test binary through ordinary
Rust modules. Keep separate binaries where process-global state needs isolation.

Python uses standard unittest discovery through package `__init__.py` files.
`test-py` discovers `unit/`, `regression/` and packaged feature regressions;
`test-py-unit` includes the Docker feature's mocked command-adapter tests.
Native Scrut resource contracts run through `test-spec-fixtures`, included in
`test-scripts`, rather than silently requiring Scrut in the Python-only suite.
CI runs the unit and process subsets separately.
`fixtures.py` and `packaging_fixtures.py` contain shared resource/input helpers.
Import helpers rather than another module's TestCase.

Workload drivers and guest probes live directly under `tests/`. Hardware modules
have explicit names outside the default `test*.py` pattern.

## Execution

| Command | Scope |
| --- | --- |
| `mise run test` | Rust, script regressions, Python, benchmark-tool tests and API doctests |
| `mise run test -- pvisor-core` | One Rust package; package arguments select Rust only |
| `mise run test -- pvisor-cli -- --test apply_cli` | One native Rust integration target |
| `mise run test -- pvisor-core -- --lib` | One library test target |
| `mise run test-py` | Python unit and process regressions |
| `mise run test-py-unit` | Python unit tests |
| `mise run test-py-regression` | Python process/integration regressions |
| `mise run test-scripts` | Task and shell regressions |
| `mise run test-doc` | Cargo doctests for the API crates |
| `mise run test-benchmark-tools` | Benchmark tooling correctness, without performance measurements |
| `mise run test-spec-fixtures` | Native Scrut parsing, preserved claims, isolation, failures, cleanup and report inventory |
| `mise run test-home-demo` | Homepage explanatory model through Node |
| `mise run test-agent-plugins` | Plugin contracts through Node |
| `mise run test-agent-plugins-native` | Plugin workflows through the real CLI |
| `mise run fw -- test` | Firmware bundle and ABI checks |
| `mise run examples -- SCENARIO` | A product example |
| `mise run cases -- --suite stage` | STAGE semantic specifications |
| `mise run cases-feature FEATURE` | Discover every `S-*.md` in one feature; declared XFAIL remains visible and accepted, missing prerequisites fail |
| `mise run cases-feature-strict FEATURE` | Same inventory, every case must PASS; no XFAIL/SKIP/XPASS acceptance |
| `mise run test-feature-native docker-cli` | Cargo Docker targets, excluding Compose tests embedded in `docker_cli` |
| `mise run test-feature-native docker-compose` | Cargo `compose_cli` and Compose tests embedded in `docker_cli` |
| `mise run test-feature-native FEATURE --hardware` | Explicit ignored native VM contracts; Linux/KVM or Apple Silicon/HVF with firmware and prepared fixtures |
| `mise run test-feature-contracts docker-cli bats` | Real runtime Bats contracts with `PVISOR_BINARY` and private `CONTRACT_LOGS` |
| `mise run test-feature-contracts docker-cli go-0` / `go-1` | Discover all Go tests, assign each once by stable hash, fail on an empty shard |

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
completion publication and cancellation. Product scenarios run in independent
feature jobs rather than a historical DOC ID allowlist or a combined USE job.
Passing implementation tests does not approve a semantic claim.

## CI and evidence

[CI](../.github/workflows/ci.yml) owns the required aggregate.
[Windows](../.github/workflows/windows-vm.yml) owns native WHP checks;
[documentation](../.github/workflows/docs.yml) owns the bilingual site build.
These workflows and `mise.toml` define the executed selections.

### Product feature jobs

| Feature | CI job / task | Runner and coverage |
| --- | --- | --- |
| `config`, `docker-cli`, `docker-compose`, `replay`, `session` | `Feature <feature> / Scrut / Linux / normal`, `cases-feature <feature>` | Ubuntu; no KVM/FUSE/userns prerequisite, replay also requires its companion binary |
| `checkpoint`, `gateway`, `isolation`, `resources`, `stage` | Same independently named Scrut jobs | Linux with read/write FUSE and verified rootless namespaces |
| `network` | Same independently named Scrut job | Linux with namespaces, KVM and host rootfs; no FUSE prerequisite |
| `container` | Same independently named Scrut job | Linux with namespaces, crun/runc and the declared image; no KVM/FUSE prerequisite |
| `vm` | Same independently named Scrut job | Linux with namespaces, FUSE, KVM, matching firmware, host rootfs and guest Python/SDK driver |
| `docker-cli` | `Feature docker-cli / Cargo / <os>`, `test-feature-native docker-cli` | Ubuntu and hosted macOS; conventional non-ignored native tests only |
| `docker-compose` | `Feature docker-compose / Cargo / <os>`, `test-feature-native docker-compose` | Ubuntu and hosted macOS; Compose owns embedded `docker_cli` Compose tests as well as `compose_cli` |
| `docker-cli` | `Feature docker-cli / bats`, `/ go-0`, `/ go-1`, `test-feature-contracts` | Independent Linux/KVM black-box runtime shards using the frozen CLI artifact |
| Windows VM/container boundary | `Windows` reusable workflow: `WHP / smoke`, `/ vm-cli`, `/ containers` | Native Windows 2025/WHP with its repository-built firmware, guest and root-disk artifacts; not Unix Scrut/Bats acceptance |

The inventory job discovers feature directories containing `S-*.md`, and Scrut
task selection discovers the cases. New IDs require no workflow allowlist.
`ci-feature-matrix` declares capabilities at the feature boundary. A new
feature is discovered automatically without hardware assumptions; if its cases
need hardware, add its prerequisite declaration rather than accepting skips.
Feature Scrut concurrency is capped at three runners, native Cargo at two;
Bats and the two Go shards retain their independent 30-minute budgets.
Each Scrut job downloads the frozen Linux product built by `cli-test`, prepares
real Host/KVM prerequisites and uploads its own report, log and case resources.
The artifact includes `pvisor-replay`, the matching repository-built firmware
library and its source receipt, as well as the static CLI and daemon.
Linux VM/SDK cases explicitly use host rootfs `/` and `/usr/bin/python3`;
image cases use `mirror.gcr.io/library/ubuntu:24.04`. The `container` feature
installs both crun and runc and selects crun. J02 explicitly supplies
`/usr/bin/true` as its guest command: it checks launch/trajectory metadata,
not model conversations or an external SDK service.
The VM-control driver remains built by `cases` only when the selection needs
it, and receives the frozen artifact's firmware directory explicitly.
The required `CI` aggregate includes inventory, feature execution and native
feature jobs, alongside the independent fixture and hardware gates.

Normal gates preserve declared XFAIL, never SKIP. The USE learning-path
contracts remain all-PASS even when selected by feature. Run
`cases-feature-strict` for an all-PASS gate across all selected cases, or choose
`strict` in CI's manual `case_gate` input. Strict execution is not review
approval; `--require-reviewed` still refuses. Failure of capability setup fails
the job, rather than relabeling missing hardware as successful coverage.

Hosted macOS executes conventional Cargo and Scrut fixture tests, not product
Scrut or ignored VM/macFUSE acceptance. A capable Apple Silicon Mac can run the
explicit hardware tasks; Linux-only filesystem contracts remain Linux-only.
Windows uses its native CLI/core/VM/ConPTY contracts and real WHP drivers,
not Bash/FUSE product scenarios. Ignored Cargo hardware tests and dedicated
restore probes are unexecuted unless explicitly selected on a capable host.
`test-feature-native --hardware` is an opt-in native selection, not a claim
that default Cargo jobs exercised it; Docker staged tests need a local rootfs,
and Compose edge cases additionally need the images named in their runbook.

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
Only maintainers may approve/revoke specifications or edit historical review
ledgers and approved snapshots; see [the specification rules](spec/README.md).

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
