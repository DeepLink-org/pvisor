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

On unprivileged Linux x86-64/KVM, select `-E 'test(compose_native_retirement_)'`
for the post-rename cleanup fault. Each test uses one running service and a
private mode-`0500` subdirectory with a file to block deletion. One removes the
Compose file and repeats `down --project-only` after restoring permissions; the
other interrupts `up --force-recreate` and retries promotion. Both require the
exact old retirement to disappear and the named-volume bytes to survive.

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
