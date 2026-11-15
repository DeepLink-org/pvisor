# Native Compose chaos checks

These opt-in tests exercise the supported `pvisor compose` profile against real
VMs. They do not invoke Docker Compose, emulate the VM backend or claim complete
Compose-spec compatibility. Ordinary `mise run test` checks their schedule and
frontend watchdog without starting a VM.

## Run

Use Linux x86-64 with writable KVM or Apple Silicon macOS with HVF, the in-tree
firmware, and an image with `sh`, `cat`, `sleep`, `test` and `printf`:

```sh
mise run fw -- build -j2
# For a dynamic Linux Cargo build, use the directory containing the printed file.
export PVISOR_TEST_LIBRARY_DIR=/absolute/path/to/firmware-directory
export PVISOR_DOCKER_TEST_IMAGE=mirror.gcr.io/library/alpine:3.20
export PVISOR_COMPOSE_CHAOS_SEED=1729
export PVISOR_COMPOSE_CHAOS_ROUNDS=10
# Keep evidence below the worktree even if the terminal uses an ephemeral /tmp.
mkdir -p .data/compose-chaos/fixtures
export TMPDIR="$PWD/.data/compose-chaos/fixtures"
export PVISOR_COMPOSE_CHAOS_KEEP=1
CARGO_BUILD_JOBS=2 mise run test -- pvisor-cli -- \
  --test docker_cli --run-ignored all --test-threads 1 \
  -E 'test(compose_chaos::)' --no-tests fail --no-capture
```

Prefer a manifest-digest image reference for replay. Alternatively set
`PVISOR_DOCKER_TEST_ROOTFS` to a local native Linux rootfs, without setting
`PVISOR_DOCKER_TEST_IMAGE`. The existing native fixture imports it as
`fixture:latest`. Missing virtualization, firmware or image preparation fails an
explicitly selected native test rather than silently passing or skipping it.
On macOS, the native fixture also requires the adjacent firmware dylib and signs
its copied binary with the repository's Hypervisor entitlement.

The seed is an unsigned 64-bit integer, default 1729. Rounds must be 1–1000,
default 1. Each round executes **all six actions**, ordered by a hash of the seed,
round and action name. This replays action order, not OS scheduling or crash
timing. It is a continuing stateful sequence: the same named-volume bytes and
foreign project survive between actions. No fixed-duration soak is implied by a
round count.

## Faults and independent oracles

| Action | Required result |
| --- | --- |
| Selected `stop`/`start` | Same container ID, new launch generation, retained writable upper |
| Configuration change | `--no-recreate` retains the old generation/upper; ordinary `up` replaces only the changed service, applies the new environment and discards its old upper |
| Missing bind source | Explicit admission error, old workload/upper retained; restore the valid configuration and retry without replacing the unchanged service |
| Workload exits 23 | Observe exact exit status; `up` restarts the same container with a new generation and retained upper |
| Two simultaneous `up` clients | Both complete successfully; no duplicate or pending member and no unnecessary restart |
| `down`/`up` | New container IDs, named-volume bytes preserved, old writable upper absent |

Every action additionally checks exact guest-read volume bytes, two running
members without pending replacements, and an unchanged live service in another
project in the same private store. Final `down -v --remove-orphans` must remove
only the selected project's members, owned network and volume; the foreign
project is checked before its own explicit teardown.

A separate synchronized SIGKILL test blocks a dependency health check, observes
its failed probe and the unstarted dependent, then kills **only the frontend it
spawned**. The surviving owner must observe recovery after the host gate is
restored, without another frontend. Retrying `up` must release the abandoned
project lease, reuse the dependency generation and start the admitted dependent.
Repeated `down` checks cleanup idempotence.

## Safety and evidence

- A new private temporary Docker store and a copied binary are used by each
  test; no existing user containers, volumes or networks are selected.
- At most three VMs run in the stateful test, each configured for one vCPU and
  128 MiB. Run tests serially; do not run a large round count on a pressured host.
  These are guest budgets, not host cgroup limits.
- Each supervised frontend has a 90-second deadline; observation loops have a
  15-second deadline. The native fixture's initial image preparation and direct
  Store cleanup retain their existing bounds; this is not a campaign-wide hard
  deadline.
- File-backed stdout/stderr avoid pipe backpressure and detached-owner EOF waits.
  On an assertion failure, the fixture attempts to stop its own VMs and retains
  its directory at the printed original path. Inspect any cleanup errors or
  survivors before manually removing evidence.
- `campaign.json` records seed, rounds, full schedule, copied-binary BLAKE3 and
  resolved image digest for the stateful test. `commands.jsonl` maps numbered
  stdout/stderr files to literal command arguments. `--no-capture` records results
  and the active fault. Successful fixtures are removed, so redirect the nextest
  transcript if durable passing evidence is needed, or set
  `PVISOR_COMPOSE_CHAOS_KEEP=1` to retain successful fixture directories too.

The existing `compose_acceptance` and `compose_edge_cases` native tests complement
this sequence with Redis/HTTP/DNS/port publication, failed completion jobs,
unhealthy recovery, probe timeout cleanup, file-bind authority and interrupted
replacement admission. Select those modules explicitly to run them too.

Not covered here: owner/VMM SIGKILL, host power loss, disk-full or I/O faults,
network packet loss/partition, arbitrary interruption points during promotion,
cross-platform equivalence, or multi-hour endurance. A passing bounded campaign
is evidence for the checked contracts, not a guarantee of all Compose behavior.

## Application-discovered correctness regressions — 2026-11-15

Subsequent real-application campaigns exposed five runtime defects. Their
permanent regressions live in
[`compose_edge_cases.rs`](../crates/pvisor-cli/tests/common/compose_edge_cases.rs#L1):

- Compose stop-signal admission, image-default inheritance, retained settings
  under `--no-recreate`, and explicit caller precedence.
- Standard-stream path opens reaching captured stdin/stdout/stderr through the
  existing guest pipe/PTY adapter.
- Positional mmap writeback when FUSE selects an append-open handle, without
  changing ordinary append behavior or exported descriptor flags.
- Rootfs symlink ownership surviving restart without changing its target or
  the host inode owner.
- Real GNU sed/cp preserving guest ownership and mode during atomic replacement.
  Private virtual-owner backings explicitly reject POSIX ACL xattrs; this is
  mode-preserving fallback, not full ACL support.

On the unchanged Linux/KVM source used for those campaigns, the final targeted
Rust selection passed 866 tests. The expanded native Compose/chaos selection
passed 18 tests, including two non-VM harness checks; 14 tests were outside that
selection. Four-package Clippy and VM/runtime rustdoc passed with warnings
denied. These counts do not imply full-workspace, full-Python-suite or
macOS/Windows validation.

Machine-local application reports, raw diagnostics and credentials are not part
of the regression fixtures and must not be published with a fix.

## Recorded Linux/KVM validation — 2026-11-14

The initial campaign on Fedora x86-64 used the in-tree Linux 6.12.109 firmware,
two Cargo build jobs and one test thread. No production implementation changes
were needed. The source base was `54d66cf4485e144e3e939d2adfef79d20eda86e0`
plus the new tests and documentation.

All Rust commands below used the repository's `mise run test`/nextest entrypoint:

| Selection | Result |
| --- | --- |
| `pvisor-cli -- --test compose_cli --test docker_cli --test-threads 1` | 17 passed; 16 opt-in tests skipped |
| `pvisor -- --lib --test-threads 1 -E 'test(docker::compose)' --no-tests fail` | 25 passed; 784 tests outside the selection |
| `docker_cli`, ignored tests enabled, `test(compose_chaos::)`, seed 1729 × 4 rounds | 4 passed in 28.655 s |
| `docker_cli`, ignored tests enabled, `test(compose_)`, seed 42 × 4 rounds | 13 passed in 128.417 s: 11 real-VM tests and 2 harness tests |
| `docker_cli`, ignored tests enabled, `test(compose_chaos::)`, seed 1729 × 10 rounds | 4 passed in 29.518 s; durable fixtures retained below the worktree |

The three stateful executions completed 18 rounds / 108 actions in total.
The synchronized frontend-SIGKILL case passed three times. These are repeated
executions of six action kinds and one crash case, not 111 distinct contracts.
The broader native selection also passed Redis/HTTP/DNS/publication, completion
dependencies, health failure/recovery, timed-out probe cleanup, file-bind
permissions, concurrent replacement admission and lifecycle/argv contracts.
Filtered or opt-in skips are not additional passing coverage.

`cargo clippy --locked -p pvisor-cli --test docker_cli -- -D warnings`, targeted
`rustfmt --check` and `git diff --check` passed. The firmware was built with
`mise run fw -- build -j2`; no upstream prebuilt firmware was substituted.

Final-run identities:

- Nextest ID: `6aeb56f9-c3c3-4e8b-ba60-876a53fff84b`.
- Test module SHA256: `09eb252aad636d4004eebce4bd17ec16e656a157f5d984d5a1c986fb3a2c50e1`.
- CLI SHA256: `e5be0d318d4926eaec493e1c862b907068c0ae6dc59c9f4d8f4eff63e0fe9f00`.
- Firmware SHA256: `6b8bf8378520b50ec2ce8ccc99c1a1b3722cb045bf585d7ac22881e3c9cc4bda`.
- Alpine image: `mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e`.

Machine-local transcripts, firmware receipt, final fixture metadata and audit
remain in `.data/compose-chaos/`. The final two fixtures recorded 370 supervised
frontend commands; both private container inventories were empty after teardown. An additional
`/proc` executable-path audit found no identifiable surviving fixture processes.
Four inaccessible same-UID entries remain an explicit visibility limit, not
proof of process absence. Earlier terminal-managed temporary fixture trees were
removed by the terminal environment; their nextest transcripts remain, but only
the final run retains per-command fixture files.

The Python suite, full workspace, macOS/HVF and multi-hour endurance were not run.
