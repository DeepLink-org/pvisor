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
- Each run owns a private temporary store. Cleanup removes both exact owned
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
