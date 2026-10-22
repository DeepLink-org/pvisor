# Lightweight macOS container chaos

`chaos.py` is an opt-in black-box runtime driver, separate from the Podman
contract count and normal test discovery. It uses the same signed pVisor binary
and Alpine fixture as the adapted suites, without Docker, new packages or
changes to guest code. It is a bounded correctness check, not a capacity or
performance benchmark.

## Limits and execution

- At most two live VMs, each 1 vCPU and 256 MiB guest RAM; at most three concurrent
  CLI requests. Seeds run sequentially. The driver pauses 200 ms between rounds.
- Default 24 rounds and a 180-second command budget; allowed maximum 60 rounds
  and 300 seconds. Each CLI operation has a 35-second timeout, shortened by the
  remaining run budget. Cleanup has its own bounded command timeouts.
- Before starting and between rounds, stop if one-minute load exceeds 75% of the
  host logical CPU count or macOS `memory_pressure` reports less than 20% free.
  These are sampled guardrails, not a hard bound on every instantaneous host load.
- Each run owns a private temporary store. Cleanup attempts both exact owned
  names, checks the public container list is empty, then deletes its store. If
  cleanup fails, the report retains the directory for diagnosis rather than
  deleting authority needed to recover a live VM. Other stores are untouched.

Build/sign pVisor as described in the parent README, then run from the repository:

```sh
PVISOR_BINARY="$PWD/target/release/pvisor" \
PVISOR_TEST_IMAGE=alpine:3.20 \
python3 tests/podman/chaos.py --seed 11 --rounds 60 --report /tmp/chaos-11.json
```

The seed reproduces workload selection and data, not operating-system scheduling.
The first six rounds always cover all scenarios, followed by seeded choices:

| Scenario | Injection and correctness check |
| --- | --- |
| Streams | Concurrent binary stdin/stdout and distinct nonzero exits in both VMs, alongside a missing-executable failure; exact bytes and stderr must survive |
| Shared volume | Atomic file replacement observed across VMs and concurrent writes to independent files; exact contents must survive |
| Restart | Immediate stop followed by two simultaneous starts; retained upper data and unrelated VM liveness must survive |
| Disconnect | Kill an owned CLI client with a live guest exec, stop/restart its VM, then prove new exec works; clear the readiness marker before every injection |
| TCP | Immediately abandon a connection, then two byte-exact echo/half-close exchanges through the published endpoint |
| Logs | Three simultaneous readers must reproduce both streams byte-for-byte without consumption or duplication |

Application stderr is retained, including signal-handler output from BusyBox
netcat during deliberate stops. BusyBox's [signal handler](https://github.com/mirror/busybox/blob/master/networking/nc_bloaty.c)
prints `punt!` before re-raising a signal. Empty stderr is therefore not an
appropriate fixture assertion; log readers must reproduce the same stderr bytes.

## Observed defect and repair

Seed 1 failed on the third round: two simultaneous `start` clients saw different
results, one success and one `container is already running or starting`. A start
that encounters an in-progress launch now joins that generation's existing
bounded readiness/completion wait. It neither spawns another owner nor holds the
catalog lock while waiting. Existing launch failures remain visible to joiners.

The ordinary Go regression stops and concurrently starts one 1-vCPU/256-MiB VM
four times, requiring both calls to succeed and the entrypoint counter to advance
exactly once per round. A Rust contract verifies that a joining caller observes
the original generation's failure without replacing that generation.

## Local evidence, 2026-10-22

Host: Apple Silicon macOS, 10 logical CPUs, 24 GiB RAM. The fixed binary was
rebuilt and Hypervisor-signed. The image was the immutable ARM64 Alpine mirror
manifest recorded in the parent README. Seed 1 (12 rounds) and seeds 11/29/71
(60 rounds each) all passed: 192 rounds, 1,191 CLI invocations including setup and
cleanup, approximately 72.5 seconds total across the four sequential runs.

| Scenario | Completed rounds |
| --- | ---: |
| Streams | 35 |
| Shared volume | 31 |
| Restart | 31 |
| Disconnect | 34 |
| TCP | 37 |
| Logs | 24 |

Sampled maximum host load was 2.37, minimum reported memory-free percentage 77%,
and maximum combined owned owner/VMM RSS approximately 136.8 MiB. Samples showed
at most four owned owner/VMM processes (two pairs); no pressure guard tripped.
All runs confirmed empty public container lists after cleanup; a post-run process
check found no remaining owner/VMM associated with the chaos stores. These
samples do not establish an instantaneous RSS/CPU peak or whole-system leak proof.

The focused Go concurrency regression, 21 selected Rust Docker regressions and
10 Python safety/source-integrity checks passed, alongside Clippy and rustdoc.
Linux/KVM, long-duration soak, deliberate guest OOM/kernel panic and host-owner
crash injection remain untested. Cross-VM open-mapping coherence is not claimed.
