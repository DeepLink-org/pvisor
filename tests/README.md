# Host regression tests

Run `mise run test` for Rust, native script regressions and Python tests. A Cargo
package argument selects only Rust tests. `mise run test-py` runs Python tests;
`mise run test-scripts` runs the private tasks imported from `tests/tasks.toml`.
Python tests use standard-library `unittest`, without a pytest dependency or
configuration. CI runs both entries explicitly. Delete superseded Python wrappers
after moving their behavioral checks; do not retain a second set of recipe or shell-source
string assertions.

Declare mise routing, Bash vocabulary, firmware Make and shell installer checks in
`tests/tasks.toml`; the larger routing regression lives in `tests/tasks/`.
Test routing branches, failure handling and publication behavior;
do not repeat each one-line recipe as a mock command assertion.
Python tests cover Python contracts, structured data, PTY interaction and process
lifecycle. Do not launch mise or embed Bash checks inside a Python test runner.
Calling a product command or supervising an example process remains part of
the integration tests.

`mise run test-windows-fw-deps` uses fake privileged/package commands to verify
the Windows firmware CI setup's isolated signed mirrors, bounded downloads,
compiler selection, failure propagation and temporary cleanup without installing
anything on the local machine. Real provisioning is checked by Windows VM CI.

The opt-in [native daemon chaos driver](DAEMON_CHAOS.md) runs bounded seeded
SDK file/command traffic, cancellation and API/lifecycle faults against real VMs.
Its ordinary safety tests run without an SDK, KVM or a daemon. Application/CLI
chaos remains in `podman/`; both are correctness experiments, not benchmarks.

The opt-in [native stage chaos driver](STAGE_CHAOS.md) runs bounded, seeded
real-KVM stage/apply/drop oracles with frozen inputs and private evidence.
Its ownership, cleanup, deadline and JSON-oracle unit tests require no VM.

Use `tests.fixtures.workspace()` for an isolated canonical temporary path and an
`ExitStack` for mocks and other cleanup. Each use owns its resources; cleanup runs
before removing the directory, including when an assertion fails.

`node --test tests/home_demo.test.mjs` checks the homepage's explanatory browser
model: private writes, observed-file conflicts, stopped file forks, selective
acceptance and cleanup. It does not execute pVisor or validate runtime guarantees.
Use the native bilingual docs previews for visual and interaction checks.

`mise run test-agent-plugins` runs the Pi, DSH and Codex task-adapter fixtures
and stdio MCP tests with Node.js 22.19+. `mise run test-agent-plugins-smoke`
requires the real CLI and Host staging support; set `PVISOR_PLUGIN_BINARY` to a
built binary when it is not on PATH. It tests a disposable project through
native spawn/review/selective apply/drop, without a model or Agent installation.
These checks do not replace interactive host compatibility testing.

Semantic specifications live in user Markdown under `docs/src/zh/cases/` and
`docs/src/zh/reference/`, following `tools/semspec/DESIGN.md`. Moving conventional regression checks does not approve
specifications or change their claims, vocabulary, review ledger or snapshots.

## Docker CLI compatibility

`podman/` retains pinned upstream Podman system/e2e sources and adapted black-box
Bats/Go contracts that run the top-level pVisor container commands and check expected behavior. Docker
daemon interoperability cases are disabled with reasons. See
[podman/README.md](podman/README.md) for explicit runtime commands, prerequisites
and coverage limits. The external-runtime suites are opt-in locally and required
in the Linux CLI contract CI shards; normal Python regressions verify source integrity and
adapter behavior without a daemon.

## Network CLI chaos

`python3 tests/network_cli_chaos.py --binary target/debug/pvisor --seconds 10800`
exercises the real `network create/ls/inspect/rm` entrypoints against a new private
store without KVM or FUSE. It checks IPAM, name/ID selection, explicit rejection
without state changes, concurrent duplicate operations and SIGKILL recovery.
`--seed` replays scenario choices; process scheduling and crash timing still vary.
`--output` retains command JSONL, status and failure evidence (defaults to `/tmp`).
This covers catalog management only, not attached containers or packet delivery.
Command deadlines remain 15 seconds. Timeouts fail the campaign and retain
`timeout-PID.json` with available Linux process/thread wait and host-pressure
diagnostics, plus captured stdout/stderr. Permission-denied diagnostics are
recorded without hiding the original failure. Campaign metadata pins the initial
binary SHA256; each command also records its elapsed wall time.

For low-load runs, add `--interval 15 --workers 2`: scenarios have a 15-second
idle gap and conflict tests use two concurrent CLI processes. Input, timeout,
state-consistency and crash-recovery assertions are unchanged. A stopped run is
not a pass; SIGTERM records its interruption in the retained failure evidence.

The default `--workload generated` maintains an independent expected catalog
across a newly generated sequence of commands, rather than repeating complete
create/delete cases. It generates different names (including 1/127/128-byte
boundaries), subnets, gateways, automatic allocations, names/full IDs/short or
ambiguous ID prefixes, repeated batch selectors, partial batch failures,
force/removal aliases and list formats. Each action checks CLI output, exit
status and the resulting public catalog against the model. At most 32 user
networks remain live; create/remove/query order changes with the evolving state.
`workloads.jsonl` records each generated operation and its expected starting
state; `commands.jsonl` records the actual commands and results. Existing fixed
fault regressions remain available with `--workload regression` and are also
mixed into generated sequences without discarding the surrounding model.

## Cross-platform VM CLI contracts

`vm_cli_contract.py` drives `pvisor vm-run` on Linux/KVM, Apple Silicon/HVF and
Windows/WHP with the same `vm_guest.rs` workload. Missing virtualization,
firmware errors and incorrect guest results fail the run.

Build the guest with `rustc --target <matching-linux-musl-target> -C linker=rust-lld --edition=2021`.
Pass `--binary`, `--guest`, a new `--output` directory, and `--firmware` if the CLI
has no embedded kernel (required on Windows). Defaults are 3 rounds, seed 1729
and a 90-second command deadline; `--rounds 30` runs 120 VMs. Each round checks
1/2 CPUs and exit codes 0/7/255. The guest runs eight file workers, touches
memory, checks a timer/read-only share and renames a directory with cached children.

The host checks exact result bytes, exit code, the immutable fixture and an empty
work directory. Evidence retains stdout/stderr, commands, elapsed time, hashes
and rootfs; `report.json` records failures. Timeouts terminate and reap the command
tree. Existing output directories are refused. `test_vm_cli_contract.py` checks
supervision and false-pass rejection without virtualization.

The Linux pvisor CI shard and Windows WHP session run the driver. Hosted macOS CI
keeps builds and conventional tests; real HVF execution remains a manual check
on a capable Apple Silicon host. Ordinary unit tests do not start VMs.
Linux CI writes evidence under `RUNNER_TEMP`,
outside the cached Cargo target directory. Restored build caches must not supply
a previous run's evidence directory. See [platform support](../docs/src/en/reference/platforms.md)
and [the review record](PLATFORM_REVIEW.md).
