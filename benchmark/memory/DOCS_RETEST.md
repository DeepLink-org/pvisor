# Current-source memory benchmark qualification

## Question and outcome

Does the current physical store preserve the memory, CPU, complete-readback and
private-write behavior covered by the documentation's four-instance experiment?
This is a B-MEMORY-SCALE engineering diagnostic, not a replacement user benchmark.

The repaired client has now completed the full six-condition KSM-disabled
off/physical preflight in r6: 348/348 checks passed, no interference was observed,
all 12 physical VM logs passed publication-health checks and all owned groups
were cleaned up. The separate N=1 results show lower memory for shareable data
but extra CPU and readback cost; full private writes remove payload sharing and
can leave additional charges.
See [the repaired-pool results](REPAIRED_POOL_RESULTS.md) for levels, post-write
costs, scope and provenance. This is not a formal repeated or old/new comparison.

Six separate attempts were retained on 2026-11-15. The first (`r1`) did not enter
VM sampling: its 180-second quiet admission deadline expired while foreign VMs
and build/test processes remained active.

The second (`r2`) entered real VM sampling and stopped after two of the six
planned conditions. Both workers passed their 58 full data/lifecycle checks;
only the first condition passed observation qualification. Foreign build/test
processes appeared during the second condition. Both owned product groups were
confirmed quiescent. Four remaining conditions were not executed.

| r2 condition | Data/lifecycle checks | Observation qualification |
|---|---|---|
| off / unique-random | 58/58 passed | Passed |
| physical / shared-random | 58/58 passed | Failed: foreign build/test activity in 132 of 199 guard observations |
| Remaining four conditions | Not executed | Not available |

Both early cohorts remain `complete=false`. Their two executed conditions use
different payload patterns and cannot be compared against each other. They do
not contribute to the later successful r6 comparison. Historical documentation
numbers and figures were not replaced.

All four physical workers also logged `publication_disabled=true`. The log does
not timestamp that transition, so it cannot be assigned to a specific phase.
No pool RPC error was logged and all data/lifecycle checks still passed. The
investigation below reproduced a client-side queue-overflow defect with a healthy
publisher. The old r2 logs cannot prove the trigger of every individual message.
A stopped publisher's lower CPU is not evidence of an optimization.

## Publication-disable diagnosis and repair

A deterministic subprocess test acquires 16,384 valid references, corresponding
to a 64 MiB Linux payload, while retaining one separate live mapping. It returns
references in 4,096-entry bursts, with one optional release flush between bursts.
The real publisher stays alive throughout.

Before repair, this test failed with the same `publication_disabled=true` log:
the queue allowed only 4,096 entries, while an optional flush removed at most 256.
Queue saturation called `disable`, closed the healthy session and discarded the
pending notifications. The same-generation reconnect safety rule then prevented
resuming that uncertain session. Linux reference maintenance can return up to
4,096 COW references per pass; one complete free report can also return a larger
burst. This is a mismatch between admission ownership and deferred notification
capacity, not evidence that the publisher died.

The repair reserves one FIFO token slot per possible reference before acquiring
authority, and tracks acquired references until release acknowledgement. The
publisher's existing reference budget bounds this bookkeeping; a separate fixed
release-queue ceiling no longer disconnects healthy sessions. Metadata reservation
failure limits new publication instead of invalidating existing RAM. Mandatory
release performs no RPC or queue growth. Optional flushes still send at most
256 tokens, and FIFO removal does not shift the remaining backlog.

Four regression cases now pass:

- Repeated 4,096-reference release bursts preserve the healthy session and a live
  mapping, drain in bounded frames, and allow subsequent publication.
- A single whole-payload release performs no RPC or queue growth.
- Impossible bookkeeping reservations preserve existing references and mappings.
- A release transport failure quarantines the old tokens; reconnecting to a new
  generation cannot replay them.

The related runtime storage/pager suite passes all 94 selected tests. The daemon
pool and user-service integration suites pass all 17 tests. Targeted runtime and
daemon Clippy, plus warnings-denied helper Clippy, pass. New disable logs include
operation, error kind, Unix timestamp and queued/outstanding counts so future
events can be distinguished.

These are correctness results, not a new memory/CPU benchmark. The r1/r2 frozen
release binaries predate this repair and remain unchanged. New repaired binaries
were built for r3 and reused, after verification, by the independent r4 condition
below. Native macOS recovery has not been measured here.

## Repaired four-VM qualification {#repaired-four-vm}

The r3 probe and daemon were rebuilt from the repaired source as static musl
release artifacts under the same bounded build settings. Neither old product
binary was reused. This cohort also adds publication-health qualification:
physical conditions must retain four hash-bound stderr logs, and any
`publication_disabled=true` event fails the condition, even after successful
reconnect. Analysis rechecks those hashes; old cohorts keep their old contracts.

| Attempt and condition | Data/lifecycle checks | Physical publication | Strict sampling |
|---|---|---|---|
| r3 off / unique-random | 58/58 passed | Not exercised | Failed: interference in 169 of 194 guard observations |
| r4 physical / shared-random | 58/58 passed | Four worker logs checked, zero disable events | Failed: interference in 3 of 198 guard observations |
| r5 six-condition attempt | No worker executed | Not exercised | Admission failed: foreign VMs in 153 of 173 observations |
| r6 complete off/physical subset | 348/348 passed | 12 physical VM logs checked, zero disable events | Passed: zero interference in 1,166 guard observations |

The six-condition r3 cohort stopped after its first condition. To exercise the
actual repair rather than repeatedly stopping in an off control, r4 was planned
as a separate single-condition diagnostic. It does not resume, fill or complete
r3. The r4 workload retained four 256 MiB / one-vCPU VMs, each with 64 MiB of
shared-random data, the 60-second idle window, full readback, 25%/100% private
writes, peer checks, 30-second post-write window and independent exit/survivor
checks. Its worker resource/placement checks passed and the owned unit was
confirmed quiescent.

The focused audit records `worker_correctness_passed=true` and
`physical_publication_healthy=true`, separately from
`cohort_complete=false`, `strict_condition_qualified=false` and
`performance_comparison_available=false`. The three interfered observations
remain in the evidence; they were not discarded to obtain PASS. These data
support the repair's behavior in this single workload, not a CPU/memory benefit,
long-term stability, complete matrix or production-density conclusion.

After the user reported interference had been removed, r5 made a new strict
six-condition attempt with the same verified repaired artifacts. It did not
obtain the required quiet window within 180 seconds and stopped before creating
a product unit or starting any measurement VM. Multiple distinct foreign pVisor
processes appeared during admission; at least one recorded path belonged to
another worktree. A subsequent descriptor inspection observed an external
`/bench-artifacts/pvisor` process holding `anon_inode:kvm-vm`, confirming this was
not merely executable-name matching. A later instantaneous inspection found no
matching process, but that does not establish an uninterrupted sampling window.
No failure was suppressed and no new performance values were produced.

After the user paused the foreign task, r6 started from an instantaneous guard
with zero foreign sessions and retained the full admission and continuous
checks. All six conditions passed, including the complete read/COW/exit sequence,
resource limits, curve coverage, log hashes and cleanup. A separate audit and
plot render rechecked the recorded evidence. Its qualified N=1 data are kept
separate from every incomplete attempt above.

## Alignment with the documentation

The reference is the [VM memory chapter](../../docs/src/zh/benchmarks/vm-memory/index.md)
and its [technical analysis](../../docs/src/zh/lab-notes/vm-memory-performance-analysis.md).
The registered protocol is described in [REGISTRY.md](../REGISTRY.md#b-memory-scale).

| Documentation item | Retest configuration or qualification |
|---|---|
| Four simultaneous instances, each with 64 MiB of checked data | Verified in all six repaired-source r6 conditions |
| VM 256 MiB / one vCPU | Unchanged |
| Complete product group: four CPU quota units, 2 GiB, zero swap, CPU 0–3 | Repaired worker budget/placement checks passed |
| Charged memory and PSS | Same 50 ms / 500 ms observers; all r6 observation/coverage checks passed |
| 60-second ready window and complete first readback | Passed in all six r6 conditions |
| 25% / 100% private writes, unchanged peers, independent exit | 58/58 checks per r6 condition, 348 total |
| Post-write behavior | Full additional 30-second window verified in all r6 conditions |
| Repeated, shared-random and unique-random data | Complete off/physical N=1 subset in r6, with no earlier data pooled |
| Seven-arm static comparison including KSM, Firecracker, Docker and compression | Not qualified: host KSM is disabled; the registered KSM-off subset permits only off/physical |
| Shared-library hints and explicit DAX comparison | Not executed; requires its independent common-userspace/reference-runtime preparation and full KSM policy |
| Daemon failure during a running guest workload | Not injected by this memory protocol; existing process/mapping tests do not substitute for this acceptance |
| macOS or 2–3K VM operation | Not measured |

The host is Linux x86_64 / Ryzen 7 9700X, kernel
`7.3.3-300.fc45.x86_64`, with KSM `run=0`, `pages_to_scan=100`,
`sleep_millisecs=20`, `smart_scan=1`. No host settings were changed and no foreign
process was stopped. The retained embedded firmware has a different provenance
from the historical documentation cohort; even a future successful run must
remain separate, not a historical speedup percentage.

## Entry-point fixes and completed checks

- The physical `pool_memory_case` helper now uses the runtime's authenticated
  `PhysicalClient`, including exact-backing ownership and deferred release,
  instead of measuring the legacy raw client adapter.
- `pool_compare.py --prepare` now preserves both `--without-ksm` and
  `--embedded-fw` in the semspec command. The regression test checks policy
  preservation and that preparation does not start a trial.
- `mise run test-py -- discover -s benchmark/memory`: r1 had 65 passed/one
  skipped; r3 added publication-health/audit cases and had 68 passed/one skipped.
- `cargo clippy --locked -p pvisor --example pool_memory_case -j 1 -- -D warnings`:
  passed through `mise exec`.
- The r1 source snapshot's static musl release probe and daemon: built successfully,
  with one build job under a four-GiB / zero-swap / two-CPU scope.
- Repaired static musl release probe and daemon: rebuilt successfully for r3,
  source-verified before both r3 and r4; no build ran during sampling.
- Source hashes were unchanged across the build and matched the worktree at
  measurement launch. Firmware and worker bytes were frozen and verified.
- `git diff --check`: passed.

The preparation checks establish the build and harness behavior. The focused r4
audit verifies the repaired publication path independently of its failed timing.
The subsequent complete r6 audit additionally qualifies the six-condition N=1
observations, not a formal repeated or version-regression result.

## Retained evidence and continuation

The new evidence directory is
`benchmark/memory/.data/docs-retest-20261115-r1/`. It retains the plan, frozen
product source, source manifest, build controls/logs, firmware provenance,
release binaries and the failed cohort under `curves/`. The runtime copy is
`.data/dm151/`; its original semspec input and command failure log remain beside
it. Neither location is a measurement to resume or overwrite.

| Evidence | SHA-256 |
|---|---|
| Input receipt | `c4da69a0e40eb30e023b41cb75de20c33e4a0876678fa0b4aa25b99e07517a1e` |
| Product source manifest | `667002b7ab25b698e339dc008df60344048d99ab21b1e469e63755b8435dfaf9` |
| Product source archive | `81362a52d84f0d20f84c43d1aeac0c85c500cde1b3a7addaa4edc10992259829` |
| Failed r1 cohort report | `6c175b393430749d536ffb28530f78c329390d2babb0d9355d8f2b2a2de60aeb` |
| Incomplete r2 cohort report | `d5af4609ea7a53cd2b6d16e2d740b390e560701ce5c339959692c6f5e6d4d928` |
| Repaired r3 input receipt | `b47d8049b91c352b501399567d97a585c9936f77d5a32da5455bad7e6208812c` |
| Repaired product source manifest | `ae2c2bb9b2beac44c4b5331081443da7e8500fc84c5475ff944b05950f6f7518` |
| Incomplete r3 cohort report | `00961a2247714bf11c03de242200fbb08767cb755633cbdd2b8b452ccd8d5e9e` |
| Focused r4 cohort report | `0601b05655ce9f28e48df9bca7ea00fd1d32cb6630b8365e9c95416be23f0160` |
| Focused r4 qualification | `74ad07a1319ad43da8ee1e751b135ec528706f3e0f5860ba204aab4926c840aa` |
| Admission-blocked r5 cohort report | `ca61d50dcc6dcc44008761b014374afb809a736d0540709c094126534b2801c6` |
| Complete r6 cohort report | `fa7d77fbccfbb234c6b740a77a7bae6773a988bc1f9a16f0e746a3abcadca03e` |
| Complete r6 audit | `7fdbdcd063b39c0f9754b492b8478d873af3244aafc8bacd2772b223fc691084` |

The source identity is commit `177c8a66` plus the exact archived uncommitted
changes, not that commit alone. The first attempt retained 173 admission guard
observations over 180 seconds; no test VM or product cgroup was created in r1.
The second attempt is independently retained under
`benchmark/memory/.data/docs-retest-20261115-r2/`, with runtime copy `.data/dm152/`.
It reused the verified frozen binaries without rebuilding. Each condition passed
its own 30-second quiet admission, but continuous guards detected later
interference in the physical condition. All original reports, observations,
worker logs and failure statuses remain unchanged.
The repaired build and incomplete six-condition attempt are retained under
`benchmark/memory/.data/docs-retest-20261115-r3/`, runtime `.data/dm153/`.
The focused condition and separate `qualification.json` are retained under
`benchmark/memory/.data/docs-retest-20261115-r4/`, runtime `.data/dm154/`.
The fifth attempt is retained under
`benchmark/memory/.data/docs-retest-20261115-r5/`, runtime `.data/dm155/`.
Its frozen inputs were reused only after matching the current repaired source
manifest and every artifact hash; it contains no measurement worker report.
The complete sixth attempt is retained under
`benchmark/memory/.data/docs-retest-20261115-r6/`, runtime `.data/dm156/`.
Measurement and independent audit passed. The original plotting dependency
failure remains recorded; a separate workspace-local render retry succeeded.
No previous failure status or raw measurement was changed.

For a new strict attempt, first arrange a window without foreign VMs or
build/tests. Use a new short workspace-owned output path. KSM may remain disabled
for the registered off/physical subset; the full seven-arm and library cohorts
require administrator-configured KSM and their separately frozen inputs.
Do not disable interference checks, reuse historical samples or promote this
failed cohort to PASS. A busy-host diagnostic requires an explicit user request
and remains ineligible for benchmark conclusions.
