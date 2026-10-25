# Docker-style application chaos

`application_chaos.py` is an opt-in Linux x86_64/KVM black-box test of applications using
the supported Docker CLI profile. It does not need a Docker daemon and adds no
Podman contract count. It is a correctness experiment, not a performance benchmark.

The fixture is an Alpine-hosted HTTP application, a network client and a read-only
shared-volume observer. A fourth VM is allowed only for an isolated-network or
short-lived container/build check. All containers use one vCPU and 384 MiB guest
RAM inside independent systemd user scopes with 512 MiB host MemoryMax, zero swap,
one CPU quota, TasksMax=256 and inherited affinity CPU 4–7. The controller uses
CPU 8–9. Linux cgroup v2, a delegated user systemd manager, KVM, Python 3.10+ and
a native pVisor executable with its firmware are required.

Every run owns a new private store and two named networks. The base image must be
a pinned native Alpine reference. The runner pulls it, builds an application image
with COPY-only instructions, freezes provenance and a seeded schedule, then runs
every scenario once before randomized repeats. Runtime failures are never skipped
or retried as successful rounds. Explicit readiness waits have bounded deadlines.
Commands, streams, HTTP results, cgroup samples and owner diagnostics are retained
on both success and failure. Cleanup attempts every exact owned name and network;
it never deletes the store or hides a cleanup error.

```sh
rustc --edition=2024 -O --target x86_64-unknown-linux-musl -C linker=rust-lld \
  tests/podman/application_http.rs -o /absolute/path/to/app-http
python3 tests/podman/application_chaos.py \
  --binary /absolute/path/to/frozen/pvisor \
  --http-binary /absolute/path/to/app-http \
  --image mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e \
  --output /absolute/new/evidence-directory --seed 11 --rounds 36
python3 tests/podman/audit_application_chaos.py /absolute/new/evidence-directory
```

The seed reproduces scenario selection and payloads, not operating-system
scheduling. Upper state and entrypoint counters must survive stop/start and
owner/VMM crashes; named-volume state must also survive removal/recreation.
An acknowledged write is checked after the application has closed and renamed
the file; no database transaction, host power-loss or open-mapping durability is
claimed. HTTP and CLI streams are compared byte-for-byte. Network and read-only
checks include positive controls. Expected faults are explicitly distinguished
from an unexpected command or content failure.

For a longer local campaign, `--product-slice TEST_OWNED.slice` places every
instance scope under an already configured test-owned systemd slice. Set its
aggregate memory/CPU/swap limits before starting; per-instance limits still apply.
The runner does not configure or stop that parent slice. Keep the controller and
evidence-size/host-memory monitor bounded separately, and retain failed stores.

The existing `chaos.py` retains its independent lightweight macOS profile.

## Scenarios and failure rules

The first 15 rounds always cover every scenario, followed by seeded selections.
A continuous host HTTP reader runs alongside the controller. Only connection
errors that overlap a declared restart/crash/recreate epoch are expected;
content corruption is always a failure. After each transition, exact current
content, image pin, retained upper state and network attachments are checked.
Concurrent readers during atomic replacement may observe only the whole old or
whole new body. Background responses must match a known published SHA-256.

| Scenario | Required behavior |
|---|---|
| Streams | Concurrent binary stdin/stdout, distinct exits 17/29 and missing executable 127 remain correctly routed |
| Atomic volume | Variable-length atomic replacement is visible through HTTP and a second VM; read-only mutation attempts fail |
| Restart | Four simultaneous starts join one generation; entrypoint counter advances exactly once and host port stays fixed |
| VMM death | Kill only the verified owned VMM through pidfd; healthy peers survive and acknowledged state remains after restart |
| Owner death | Owner lifetime-pipe closure terminates its VMM; port is released and a new generation recovers |
| Recreate | Same name/IP/port and named volume recover data; writable upper and entrypoint count start fresh |
| Client disconnect | Partial HTTP requests are reset; an exec CLI is abandoned; subsequent requests and TCP write-half close still work |
| Logs | Four readers reproduce both streams without consumption or cross-routing |
| Conflicts | IP and published-port collisions report their specific cause; the existing application remains reachable |
| Isolated network | Cross-network name/IP requests fail with a same-network positive control |
| Auto-removal | Exact binary output and exit 23 survive `--rm`; the same name can immediately be reused |
| Image replacement | Failed RUN reports status 7; tag replacement affects new containers while an existing VM retains its pinned image |
| In-flight exec | Stop/restart terminates the old exec; fresh concurrent execs retain their own streams and statuses |
| Slow HTTP reader | A reader stops consuming a 512 KiB response while independent HTTP and guest requests still finish |
| Guest permissions | Root reads a synchronized `000` file before/after a cold VM restart; an unprivileged guest user remains denied. Permitted unprivileged write/truncate clears setuid/setgid and retains exact bytes/zero extension. Guest-owned new directories/files preserve the creator's UID/GID and mode |

Expected errors are checked by cause as well as status, so an unrelated startup
125 or build failure is not accepted as a collision or deliberate RUN failure.
Scope disappearance accepts only ENOENT/ENODEV; permission and other I/O errors
remain failures. Cleanup also checks every recorded owned cgroup for survivors.
The read-only audit verifies saved rounds, payload digests, error causes, limits,
resource events, HTTP results, half-close checks and cleanup without starting VMs.
Host HTTP probes bind source address `127.0.0.2`, keeping it distinct from the
published `127.0.0.1` endpoint during outages. This avoids Linux TCP self-connect
artifacts. The fixture requires complete Content-Length headers; EOF inside a
header block is a transport failure, even though `http.client` may accept it as
an empty 200 response. Complete responses still require exact known content.

## Observed runtime defects and repair

The original runtime failed seed 11 at zero-based round 48 (`abort_clients`): a
normal HTTP request received a disconnect after repeated reset clients. A
separate minimal real-VM probe reproduced normal-request rejection after 32
aborted connections, with service returning after the guest's read timeout.
Two socket regressions failed before repair, preserving that evidence.

The host publication relay returned from its input pump on RST without cancelling
the guest tunnel, keeping downstream reads and the 32-slot publication budget
occupied. The guest relay could also return after TCP EOF while a detached input
reader retained a cloned control descriptor. Host input errors now shut down the
tunnel; both endpoints cancel companion I/O before joining their pumps. An
orderly write-half close still waits for the complete application response.
No connection budget, timeout or retry allowance was increased.

The fixed real-VM probe completed 96 aborted connections with a successful normal
HTTP request after each group of eight. Socket regressions and a 131,077-byte
half-close response check passed. The application runs also verify real-guest
HTTP half-close after reset traffic.

## Linux evidence, 2026-10-24

The runtime was frozen from revision `3ff39c8905e80491b94989e2e7989a92e4a7b16a`
plus the host/guest tunnel fixes. The fixed GNU release CLI SHA-256 is
`48bff5d2be67a92946834fe5ab5a55b9cbe9de5b2d72f05a36ecd0902c6fcc2d`;
the firmware SHA-256 is
`c08140d24ee7e202830257f9ee20a8d904f0c5b5d80d122019117d823623c0bc`.
The pinned AMD64 Alpine reference is the one in the command above. Frozen source,
worker, source changes, binaries and full receipts remain beside the raw data.

| Independent seed | Completed rounds | Archived command records | Verified HTTP responses | Expected disconnects during injected faults | Maximum sampled instance cgroup peak MiB |
|---|---:|---:|---:|---:|---:|
| 11 | 60 | 589 | 604 | 133 | 67.14 |
| 29 | 60 | 616 | 569 | 190 | 66.31 |
| 71 | 60 | 605 | 583 | 228 | 67.28 |

All three independent runs and their read-only audits passed: 180 scenario
executions, 1,756 verified HTTP responses (including 1,387 background responses),
zero unexpected HTTP failures, content errors or observed OOM/swap/budget events.
The 551 disconnects in declared fault windows are expected availability effects,
not successful requests. Every run observed at most four live instances. Command
records include scoped helpers; a concurrent-start helper contains four actual
start clients, and deliberately abandoned CLI clients have separate diagnostics.
No latency or throughput claim is made from these correctness runs.

The executions included 21 owner crashes, 12 VMM crashes, 15 ordinary concurrent
restarts, five restarts with an in-flight exec, 14 removals/recreations and 14
reset/half-close scenarios. The sampled traffic windows were approximately
28, 29 and 33 seconds respectively; these are short fault-combination tests,
not a long-duration application soak or production reliability result.

Descriptor growth was separately investigated: after 300 HTTP requests, the
owned web scope went from 115 descriptors to 415, then returned to 115 after a
six-second quiet period. The VMM's existing five-second deferred vsock reaper
explains transient descriptor retention. This bounded observation does not prove
absence of all leaks or safe operation at arbitrary connection churn rates.

Validation also passed 59 selected runtime/CLI/guest tests, 16 Python safety
tests, targeted Clippy with warnings denied, library rustdoc with warnings denied,
Rust/Python formatting and diff checks. The existing `argv[0]` rustdoc text was
quoted to repair a broken intra-doc link. macOS was not exercised by this run.

Failed preparation/diagnostic cohorts remain separate: the fixed Alpine lacks
`httpd` (the fixture was replaced with the static HTTP application); one script
run mishandled ENODEV while a short-lived cgroup disappeared; and the original
runtime failed the repeated-reset scenario. None is counted as a passing run or
merged into the repaired runtime's results. All owned instances, recorded scopes
and test networks were cleaned; stores and evidence are retained.

Raw evidence is under [`.data/app-20261024/`](.data/app-20261024/):
[`outcome.json`](.data/app-20261024/outcome.json),
[failed original runtime](.data/app-20261024/validation-v2/seed-11/failure.json),
[original reset probe](.data/app-20261024/reset-baseline/diagnostic.json),
[fixed reset probe](.data/app-20261024/reset-fixed/diagnostic.json),
[three fixed-run audits](.data/app-20261024/fixed-audit.json), and
[descriptor reclamation](.data/app-20261024/fd-reclaim-fixed/diagnostic.json).
