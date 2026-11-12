# Bounded six-hour chaos campaign: 2026-11-12

## Scope and status

- Campaign window requested by the user: 2026-11-12 22:21 +08:00 through
  2026-11-13 04:21 +08:00 (UTC 14:21–20:21). This is an ongoing campaign
  checkpoint through Round 05 (17:47Z), not six completed hours or a concurrent
  stress run. The requested end remains 20:21Z.
- Baseline: `7fe09e20de3cb7b51222c7d61d75056208da98fb`.
- Rounds 01–02 executor: the isolated child thread's `pvisor` worktree,
  `/home/reiase/workspace/pvisor/.delta/worktrees/gyv8kfjp7ksa/pvisor`.
  Parent alone owns branch/PR publication; the test worker does not publish.
  No main merge, semspec approval/revocation, reviewed ledger or approved
  snapshot changes are part of this campaign.
- First observation: 2026-11-12T14:22:10Z. **NativeHost runtime testing has not yet
  started: cold compilation was interrupted twice by resource admission.**
  Counts at the 14:28:44Z checkpoint: **0 tests started, 0 passed, 0 failed,
  0 runtime cases skipped by the test body**. Two build attempts were safely
  stopped; these are environmental pauses, not product failures.
- At the final first-round checkpoint (14:58:06.956Z), a third, smaller-budget
  compilation attempt had stopped on memory PSI. Totals remain **0 test
  bodies started, 0 passed, 0 failed; no generated seed executed**. Two
  600-second cooldown segments (20 minutes total) are pauses, not coverage.
- A final explicitly authorized parameter-change attempt ended at
  15:01:44.907Z with a **1 GiB owned-cgroup compilation OOM**, before test
  execution. Counts still remain zero. This is an environmental compilation
  budget failure, not a pVisor product failure or a host-wide OOM experiment.
  No further retry of this compilation direction is authorized/planned.
- Subsequent small-component round (15:03–15:37Z): real `pvisor-journal`
  tests passed under the 1 GiB budget, including 16.49 minutes of changing-seed
  framing tests and a final **33 passed, 0 skipped** package regression.
  NativeHost's zero-coverage limitation remains unchanged; see Round 02 below.
- Round 03 (15:46–16:21Z): Core suspension-receipt contracts passed, including
  **18 min 20.759 s** of changing-seed test-body work and a final **91 passed,
  0 skipped** Core regression. No product defect or implementation fix was found.
  No new NativeHost or OverlayNet runtime coverage was obtained.
- Round 04 (16:25–17:04Z): mount-free `pvisor-overlay-core` selective apply/drop
  lifecycle checks passed, including **22 min 51.385 s** of changing-seed work.
  Final small-package regression: **246 passed, 5 existing ignored tests skipped**.
  No product defect or production implementation change was found.
- Round 05 (17:07–17:47Z): bounded native Replay full-duplex/EOF/tail checks
  passed, including **22 min 52.570 s** of changing-seed work and a final
  **155 passed, 1 existing ignored test skipped** package regression. No product
  defect or production behavior change was found.

## Scenario and existing coverage

Read root `README.md` and `AGENTS.md`, `crates/pvisor-cli/README.md`,
`crates/pvisor/README.md`, existing root chaos reports via targeted searches,
and the current relevant integration tests before preparing changes. There
are no README/AGENT(S) files in `crates/`, `crates/pvisor-cli/tests/`, or
intervening directories, as checked by `find crates`.

The existing `run_config_cli` tests cover ordinary Host successful/nonzero
result export and a file obstructing the result's parent. `rootless_local`
covers safe abnormal exit retention and selective apply/drop separately.
The new composition uses a real Linux NativeHost safe FUSE stage whose
execution commits successfully (or with a deliberate guest exit 7/23), but
whose **frontend RunResult export fails after completion**. It then checks
that recovering external publication does not rerun or invalidate the
committed execution. This is not completion-receipt fault injection.

Prepared integration test:
`crates/pvisor-cli/tests/rootless_result_publication.rs`.

Seed contract:

- Default first seed `20261112`, default count `4`; explicit
  `PVISOR_CHAOS_FIRST_SEED` and `PVISOR_CHAOS_SEED_COUNT` allow 1–256 cases.
- Seed parity chooses a nonempty result-destination directory versus a file
  obstructing its parent. All obstruction bytes are owned sentinels.
- `seed % 7 + 1` determines 1–7 writes of seed-specific bytes before rename;
  `seed % 3` chooses guest exit 0, 7 or 23. Every case has independent state.
- Guest also renames/modifies an existing lower file and stages scratch plus
  a once-only marker. No guest networking or external service access.

Failure invariants:

1. CLI export failure is explicit (exit 1), while the Bundle/receipt retain
   the guest's actual exit and terminal state, not Unknown or invented success.
2. Lower workspace, original file and host sentinel stay unchanged before
   selective acceptance; stage payload/rename/once-marker match the oracle.
3. Offline status/review do not rerun the guest, consume the proposal or change
   immutable Bundle/completion bytes.
4. Removing just the known obstruction permits a new healthy Session to export
   a plain, identity-matching RunResult; it must have a distinct Run identity.
5. Selective apply accepts only `accepted`, drop removes remaining candidates,
   accepted bytes survive drop, and original/host files remain untouched.
6. Both owners disappear, final inventories are empty, control sockets and
   fixture mounts visible in the test's own mount namespace are absent, and
   all CLI requests have finite watchdogs. This is not a complete enumeration
   of every mount namespace on the host.

**These are prepared assertions, not passing results.** No defect has been
reproduced or fixed at this checkpoint. No assertion or existing coverage
was weakened.

The 10–20 minute useful seed campaign is deferred until the machine can
admit it safely; no sleeping or idle time is counted as testing.

## Safety boundary and exact commands

Only this child ran build/test commands, sequentially. No workspace-wide
test command. Build target is this worktree's new `target/`; no existing
user target/session/stage/cache was changed.

At 14:22:10Z available RAM was 14,288,528 KiB, disk available 256,918,756 KiB,
load1 2.73, memory PSI full avg10 0.00. Swap was already effectively full
(4 KiB free out of 8,388,604 KiB), so swap availability was not relied on.

The user-systemd probe succeeded:

```sh
systemd-run --user --wait --pipe --collect \
  -p CPUQuota=150% -p MemoryHigh=3G -p MemoryMax=4G \
  -p MemorySwapMax=0 -p TasksMax=128 \
  --unit=pvisor-chaos-20261112-probe sh -c \
  'cat /proc/self/cgroup; for f in cpu.max memory.high memory.max memory.swap.max pids.max; do cat /sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/$f; done'
```

Actual kernel values: CPU `150000 100000`, memory.high `3221225472`,
memory.max `4294967296`, memory.swap.max `0`, pids.max `128`.
There is no cgroup fallback or missing hard resource controller.

The local supervisor `.data/chaos6_supervise.py` launches one transient user
service with those properties, `RuntimeMaxSec=900`, `TimeoutStopSec=5`,
`nice -n 10 timeout --kill-after=5s 880s COMMAND`. It sets the worktree as
the service working directory and explicitly passes environment through
`--setenv`, including:

```text
CARGO_BUILD_JOBS=2  # first two build attempts; changed to 1 afterward by parent authorization
NEXTEST_TEST_THREADS=1
CARGO_NET_OFFLINE=true
CARGO_TARGET_DIR=<this worktree>/target
CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0
CARGO_INCREMENTAL=0
MISE_TRUSTED_CONFIG_PATHS=<this worktree>
TMPDIR=<this worktree>/.data/t
```

Before launching and every two seconds while the service runs, the supervisor
records available RAM, filesystem available bytes, load1 and memory PSI full
avg10. It refuses startup, or stops only its owned service, if RAM < 8 GiB,
disk < 30 GiB, load1 >= 6, or full avg10 >= 2%. Polling sleep is solely a
resource-sampling interval, not test padding.

After the two environmental stops, the parent authorized one build job and
a **10 GiB startup RAM hysteresis**, retaining the 8 GiB runtime stop.
Bounded cooldown checks every 60 seconds for at most 600 seconds and counts
no idle time as testing.

At approximately 14:57Z, after two unsuccessful cooldown segments with available
RAM stable near 9.1 GiB, the parent authorized **one** different-budget attempt:
CPUQuota=100%, MemoryHigh=768M, MemoryMax=1G, MemorySwapMax=0, one build job,
and a 9 GiB startup hysteresis. The 8 GiB runtime stop, disk/load/PSI gates,
TasksMax and watchdogs did not change. This reduced the maximum memory budget
from 4 GiB to 1 GiB rather than forcing the original allocation onto the host.
The actual cgroup values were checked inside the service before executing mise:

```text
cpu.max=100000 100000
memory.high=805306368
memory.max=1073741824
memory.swap.max=0
pids.max=128
```

An `ExecStopPost` helper captured final cgroup kernel counters before removal.
The attempt stopped after 18.2 seconds on memory PSI, with high=2221,
max=0, oom=0, oom_kill=0, memory.peak=837,963,776 bytes. No OOM occurred.
The extra 768 MiB MemoryHigh reclamation/throttling is a likely contributor;
this is **not evidence that no test can fit on the host**. No automatic
retry or budget enlargement followed. The parent requested retaining this
stopped state pending another explicit decision.

The parent then explicitly authorized one final change: CPUQuota=50%,
MemoryHigh=MemoryMax=1G, still no swap and one build job. This removed the
separate 768 MiB high threshold **without increasing the 1 GiB hard max**.
Startup required available RAM >= 9 GiB, load1 < 6, PSI full avg10 < 0.5
in two observations separated by at least 60 seconds. Runtime stops remained
8 GiB / PSI 2 / load 6 / disk 30 GiB and the service deadline remained 900 s.
The two observations qualified. The actual `cpu.max` was `50000 100000`;
memory.high and memory.max were both `1073741824`.

After 16.287 seconds of service runtime (8.172 seconds CPU), pvisor
compilation reached the cgroup memory max: systemd result `oom-kill`,
memory.events max=154, oom=1, oom_kill=1, high=0, memory.peak=1073741824.
The host's sampled global PSI remained below the runtime gate. This proves
the specific compilation did not fit the chosen 1 GiB no-swap cgroup; it does
not prove the NativeHost scenario fails, nor authorize a wider limit. No
NativeHost process or test body ran, and no retry followed.

Static safety review identified incomplete exception cleanup in the first
supervisor revision, before any test body ran. The revised supervisor uses:

- `try/finally` around launching, sampling and polling, with SIGTERM/SIGHUP
  converted to exceptions so normal interruption reaches cleanup.
- Separate bounded `systemctl stop` and all-process SIGKILL requests for only
  a unique UUID-named owned service; stop failure cannot skip the kill attempt.
- Finite wait/terminate/kill of only the owned systemd-run process handle.
- Final verification of `ActiveState=inactive` and captured cgroup
  `populated 0` or disappearance. Failure leaves `CLEANUP_BLOCKED`, forbidding
  any subsequent workload launch.
- A durable `ACTIVE_RUN` file exclusively created with `O_EXCL` and file/
  directory fsync **before** starting a service. Only confirmed cleanup removes
  it. A leftover record forbids startup and requires inspection, not automatic
  deletion. This also prevents overlap if supervisor SIGKILL bypasses finally.
- `KillMode=control-group` and the independent 900-second systemd deadline.
  This cgroup boundary covers the fixed non-adversarial fixture workload, not
  arbitrary same-UID service-management escapes.

Failed Rust cases now retain their owned fixture immediately with `keep()`;
only successful cases are removed after assertions. This preserves fake,
owned evidence on panic without deleting an uncertain live mount. Outer
service cleanup owns failure-path process termination. Receipt assertions
parse its typed RunResult identity/state/exit, and selective apply explicitly
checks that unselected staged scratch/rename/once candidates remain.

Each test CLI request has a separate 25-second watchdog with 2-second kill
grace. Fixtures use short disposable directories below owned `.data/t`, with
private HOME/XDG config/cache/state, run storage and stage siblings. No host
OOM/ENOSPC/fd pressure injection, global network mutation, sudo, or unrelated
process signalling.

After parent clarification, real selective apply is permitted: source inspection
of `TargetApplyLock::acquire_in` confirms the shared private
`/tmp/pvisor-apply-locks-UID` directory contains locks keyed by a digest of the
canonical target path. This case's unique fixture target cannot lock a real
workspace. The harness never deletes the shared directory or other lock files.
No apply command has actually run yet.

Exact validation invocations so far:

```sh
python3 .data/chaos6_supervise.py initial mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
python3 .data/chaos6_supervise.py initial2 mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 crates/pvisor-cli/tests/rootless_result_publication.rs
python3 .data/chaos6_supervise.py resume1 mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
python3 .data/chaos6_supervise.py resume2 mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
python3 .data/chaos6_supervise.py resume3 mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
python3 .data/chaos6_supervise.py bounded1g mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
python3 .data/chaos6_supervise.py bounded1g-lowcpu mise run test -- pvisor-cli -- --offline --test rootless_result_publication --no-capture
```

`--no-capture` makes nextest run serially; it reports ignoring the redundant
thread setting. `cargo nextest` receives `--locked` through the project's
`mise run test` task and `--offline` explicitly.

## Actual execution and evidence

| Phase | UTC start–end | Result |
| --- | --- | --- |
| Initial discovery | 14:22:33–14:22:43 | First `mise exec` asked for config trust; tool watchdog terminated it. No compilation/test started. Subsequent invocations explicitly trust only this worktree. |
| `initial` | approximately 14:25:37–14:25:39.849 | 2.001 s supervisor time; service inherited manager cwd, so mise could not locate task `test`. Environment-preparation failure only. Supervisor repaired to set cwd and environment explicitly. |
| `initial2` | approximately 14:26:03–14:27:11.130 | Cold dependency build, then automatic load gate stop; 68.049 s supervisor time. No test started. |
| `resume1` | approximately 14:28:05–14:28:27.386 | Reused owned partial build; compiling pvisor/pvisor-cli when RAM gate stopped it. 22.014 s supervisor time. No test started. |
| `resume2` cooldown | 14:32:54.887–14:42:54.887 | 11 admission samples over 600 s; available RAM never reached the 10 GiB startup hysteresis. Exit 75, **no service/build/test started**. |
| `resume3` cooldown | 14:45:41.061–14:55:41.062 | 11 admission samples over 600 s; maximum RAM 9.151 GiB did not meet 10 GiB hysteresis. Exit 75, **no service/build/test started**. |
| `bounded1g` | 14:57:48.737–14:58:06.956 | One 1 GiB/one-job compilation attempt; PSI gate stop before any test began. 18.212 s supervisor time, 18.198 s service time, 12.177 s CPU time. |
| `bounded1g-lowcpu` | Startup cooldown 15:00:26.851–15:01:26.855; wrapper ended 15:01:44.907 | 60.003 s qualifying pause, then 16.287 s service/8.172 s CPU. Compilation cgroup OOM before any test began; no retry. |

| Phase | Samples | Minimum available RAM | Maximum load1 | Trigger | Cgroup peak / swap |
| --- | ---: | ---: | ---: | --- | --- |
| `initial` | 3 | 9.963 GiB | 2.577 | None | 15.4 MiB / 0 |
| `initial2` | 37 | 8.603 GiB | 6.022 | load1 >= 6 | 2.2 GiB / 0 |
| `resume1` | 14 | 7.870 GiB | 4.345 | RAM < 8 GiB | 1.4 GiB / 0 |
| `resume2` cooldown | 11 | 8.979 GiB | 3.214 | Startup RAM never reached 10 GiB (maximum 9.186 GiB) | No service |
| `resume3` cooldown | 11 | 9.104 GiB | 2.798 | Startup RAM never reached 10 GiB (maximum 9.151 GiB) | No service |
| `bounded1g` | 12 | 8.166 GiB | 3.165 | PSI full avg10 3.09 >= 2 | 799.1 MiB / 0 |

Trigger and post-stop samples are distinct:

- First trigger, 14:27:11.098142Z: load1 6.02197265625, available
  9,266,561,024 bytes. Post-stop, 14:27:11.130093Z: same load, available
  9,545,089,024 bytes.
- Second trigger, 14:28:27.378299Z: available 8,450,424,832 bytes, load1
  4.31689453125. Post-stop, 14:28:27.386215Z: available 8,619,835,392 bytes,
  which passes the original 8 GiB runtime gate, not the later startup gate.
- Third trigger, 14:58:06.789712Z: available 8,854,642,688 bytes, load1
  3.1513671875, PSI full avg10 3.09. Post-stop, 14:58:06.956269Z:
  available 9,467,428,864 bytes, same PSI, still not admitted.

Post-stop memory recovery does not erase the triggering sample. PSI full
remained 0 in the first two build attempts; the third had the explicit PSI
crossing above. Disk remained well above 30 GiB.

Evidence is local and ignored, not committed large machine logs:

- `.data/chaos6/initial.log`, `initial2.log`, `resume1.log`: full exact wrapped
  commands, environment, build output, systemd CPU/memory accounting.
- `.data/chaos6/{initial,initial2,resume1}.resources.jsonl`: timestamped raw
  admission/runtime samples.
- `.data/chaos6/{resume2,resume3}.resources.jsonl`: bounded cooldown samples; no workload
  log exists because no service started.
- `.data/chaos6_supervise.py`: exact local supervisor used for these attempts.
- `.data/chaos6_entry.py`: cgroup-value verification/ExecStopPost counter helper.
- `.data/chaos6/bounded1g.log`, `bounded1g.resources.jsonl`,
  `bounded1g.cgroup-start.json`, `bounded1g.cgroup-final.json`: exact command,
  runtime samples and actual kernel configuration/counters for the third attempt.
- `.data/chaos6/bounded1g-lowcpu.{log,resources.jsonl,cgroup-start.json,cgroup-final.json}`:
  exact command, startup qualification, runtime samples and owned-cgroup OOM
  counters for the final attempt.

Both stopped services were explicitly checked afterward:
`ActiveState=inactive`, `SubState=dead`. A process listing showed no remaining
cargo/rustc or owned test worker. Since no test body ran, no NativeHost fixture,
guest process, FUSE mount, result obstruction or apply lock was created.
Only owned partial build and mise temporary script files may remain; these
are not real user state, are not counted as test output, and are not swept
through any shared directory.

For `bounded1g`, finally cleanup recorded stop exit 0, then an owned-unit kill
request reporting that the unit was already unloaded. It verified
`LoadState=not-found`, `ActiveState=inactive`, `SubState=dead` and captured
cgroup disappearance (`empty=true`, `confirmed=true`). `ACTIVE_RUN` was removed
only after that verification, and `CLEANUP_BLOCKED` is absent. There was still
no NativeHost runtime fixture to clean up.

For `bounded1g-lowcpu`, the service had already exited and been collected
before finally ran. Both stop and kill reported the unique unit was not
loaded; verification still confirmed `not-found/inactive/dead` and the
previously captured cgroup directory absent/empty. `ACTIVE_RUN` was removed
only after this confirmation. No gate record was bypassed or left unresolved.

## Next action

Do not retry this compilation under changing parameters. Retain the prepared,
not-yet-compiled test and all resource evidence. A future host/resource decision
may allow finishing the build, after which the four default seeds should run
before any longer campaign. Review any future assertion failure as harness
versus product evidence before changing implementation. Do not infer a passing
runtime check from compilation attempts or cooldown observations.

The parent initially supplied OCI hard-link source symlink handling as a
possible next-round candidate. Static inspection found canonical validation
followed by original-name extraction, but the scout subsequently withdrew the
suspected bug: Unix permits hard-linking a symlink inode, tar intentionally
preserves that type, and there is no regular-inode-only contract. This is not
a demonstrated security defect and does not justify rejecting internal symlink
sources. No diagnostic or product change was run/made for that candidate.

The parent's preferred next candidates are a real RAM-pool IPC disconnect with
more than 256 candidates (multiple cleanup batches) while another healthy
client continues RPC, or image-pool publication interruption/reopen without
advancing HEAD. Neither was executed in this first-round checkpoint.

## Round 02 — bounded journal framing and recovery

### Outcome and coverage

After the parent redirected work to a smaller crate, `pvisor-journal` compiled
and tested within the same 1 GiB/50% CPU/no-swap boundary. **No product defect
was reproduced, no implementation/API was changed, and no assertion was
weakened.** The NativeHost test above remains uncompiled/unexecuted.

Before changing the new journal test, read the crate README, API declarations,
reader/open implementation and all relevant external recovery tests. No nested
README/AGENT(S) exists under this test directory. Existing coverage already
checks graph corruption, torn records, floats, snapshot output failures and
generated append/retry sequences; these were not represented as novel findings.

New test: `crates/pvisor-journal/tests/framing_recovery.rs`, exclusively through
`pvisor_journal::api`. README architecture list updated. The scenario combines
precise framing thresholds not exhaustively tested by existing cut-point cases:

- Three committed, independent-oracle events with escaped Unicode/newline/NUL
  payloads and causal links establish durable receipts.
- Twelve cases per seed: empty clean control; non-newline byte tails of lengths
  1, 7, 8191, 8192, 8193, `MAX_EVENT_BYTES + 4096 - 1`, exact max line size,
  and max+1; a complete JSON record without LF; a random partial JSON record;
  and a complete corrupt frame followed by an otherwise repairable tail.
- Tail bytes include invalid UTF-8. Missing LF, not UTF-8 validity or JSON
  completeness, defines the repairable final frame. Max+1 is rejected unchanged.
- Complete invalid UTF-8/empty record/duplicate event/invalid scope frames must
  reject **without truncating the later tail**. Removing corruption is an explicit
  fixture repair, never claimed as automatic product repair.
- Closed read/validate must preserve bytes and mode. Successful reopen must
  preserve the full valid prefix, original receipts and LocalSync durability.
  Exact retries are quiet; a new event has offset 3 and exactly one notification.
- A retained clone keeps the writer lock through original-owner drop. After
  the final owner drops, validate/read and a second reopen preserve the result.

Seed parameters use the same explicitly forwarded environment variables as
the first test. Default first seed is `202611120001`, count 16; count is
bounded to 1–4096. Initial PRNG state is `seed | 1`: adjacent even/odd seeds can
share a PRNG sequence, while event identities/content and `seed % 4` corruption
selection still vary. The numbers below are **seed executions**, not a claim
of fully independent random trajectories.

Reviewer requested stronger oracle checks after the long A/B campaign:
initial records now match all generated events/receipt positions one-for-one,
and final `after.len() == 4` is explicit. Default seeds and the package were
rerun afterward. **A/B were not rerun with these added assertions.**

### Exact commands and actual timings

All commands used the durable single-executor gate and cgroup supervisor above,
now CPUQuota=50%, MemoryHigh=MemoryMax=1G, MemorySwapMax=0, TasksMax=128,
CARGO_BUILD_JOBS=1, NEXTEST_TEST_THREADS=1, nice 10, locked/offline and finite
900/880-second systemd/process watchdogs. Each phase first required two
qualifying >=9 GiB / PSI <0.5 samples >=60 seconds apart. Active sampling
remained every two seconds, stopping at RAM <8 GiB / load >=6 / PSI >=2 /
disk <30 GiB. Startup qualification is **not** counted as testing.

```sh
python3 .data/chaos6_supervise.py journal-admission mise run test -- pvisor-journal -- --offline --test api_contract -E 'test(interrupted_record_writes_recover_only_complete_prefixes)' --no-capture
python3 .data/chaos6_supervise.py journal-framing-default mise run test -- pvisor-journal -- --offline --test framing_recovery --no-capture
PVISOR_CHAOS_FIRST_SEED=202611120017 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py journal-framing-seeds-a mise run test -- pvisor-journal -- --offline --test framing_recovery --no-capture
PVISOR_CHAOS_FIRST_SEED=202611122065 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py journal-framing-seeds-b mise run test -- pvisor-journal -- --offline --test framing_recovery --no-capture
python3 .data/chaos6_supervise.py journal-package mise run test -- pvisor-journal -- --offline --no-capture
python3 .data/chaos6_supervise.py journal-framing-clippy mise exec -- cargo clippy --offline --locked -p pvisor-journal --test framing_recovery -- -D warnings
python3 .data/chaos6_supervise.py journal-framing-strengthened mise run test -- pvisor-journal -- --offline --test framing_recovery --no-capture
python3 .data/chaos6_supervise.py journal-package-strengthened mise run test -- pvisor-journal -- --offline --no-capture
```

| Phase | Observation start–end UTC (includes qualification) | Qualification | Actual nextest result/time |
| --- | --- | ---: | --- |
| Existing-test feasibility | 15:03:57.254–15:06:31.346 | 120.004 s | 1 passed, 22 filtered/skipped; 0.100 s; compilation 33.08 s |
| Default, original oracle | 15:09:22.577–15:10:30.609 | 60.003 s | 1 passed, 0 skipped; 4.706 s; 16 seeds / 192 cases |
| A, original oracle | 15:11:06.682–15:20:23.850 | 60.003 s | 1 passed, 0 skipped; 494.422 s; 2048 seeds / 24576 cases |
| B, original oracle | 15:20:53.161–15:30:10.331 | 60.003 s | 1 passed, 0 skipped; 494.701 s; 2048 seeds / 24576 cases |
| Package, original oracle | 15:31:05.292–15:32:17.333 | 60.003 s | 33 passed, 0 skipped; 5.921 s (includes default framing seeds again) |
| Targeted Clippy | 15:32:42.921–15:34:00.982 | 60.004 s | Passed `-D warnings`; dev compilation 17.02 s, not a test count |
| Default, strengthened oracle | 15:34:35.225–15:35:43.259 | 60.003 s | 1 passed, 0 skipped; 4.908 s; 16 seeds / 192 cases |
| Package, strengthened oracle | 15:36:04.094–15:37:12.167 | 60.009 s | 33 passed, 0 skipped; 5.903 s (includes strengthened default seeds) |

The A/B test bodies alone ran 989.123 seconds = **16 min 29.123 s** of useful
changing-seed work, without sleeps in the test. Original default+A+B:
4112 seed executions / 49344 cases. Across all six framing-test executions
(including the package/default reruns): 4160 seed executions / 49920 cases;
this includes repeated default seeds and is not 4160 unique random streams.
The seven Rust command invocations ran 71 test cases total, with the initial
22 filtered tests reported separately. Generated subcases are not relabeled
as separate nextest tests.

Each large batch separately reported 18432 bounded-tail repairs, 2048 oversized
rejections, 2048 complete-frame rejections, 24576 healthy appends and 49152
reopen checkpoints. Those oversized/corrupt cases are expected fault
rejections, not failures or skips.

### Resource samples, cleanup and limitations

| Phase | Samples | Minimum RAM GiB | Maximum load1 | Maximum PSI full avg10 | Peak cgroup MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Existing-test feasibility | 21 | 8.687 | 3.239 | 0.00 | 490.875 |
| Original default | 7 | 9.026 | 3.576 | 0.00 | 173.152 |
| A | 251 | 8.710 | 4.698 | 0.18 | 106.273 |
| B | 251 | 8.687 | 4.296 | 0.00 | 111.906 |
| Original package | 9 | 8.856 | 3.154 | 0.00 | 407.602 |
| Targeted Clippy | 12 | 8.962 | 2.458 | 0.00 | 346.465 |
| Strengthened default | 7 | 9.023 | 3.688 | 0.00 | 174.074 |
| Strengthened package | 7 | 9.282 | 3.320 | 0.00 | 108.594 |

All eight journal phases recorded `memory.events` high/max/oom/oom_kill=0,
swap=0. Minimum available disk across these phases was 244.289 GiB. No
resource gate stopped a journal phase. Their qualification pauses totaled
approximately 540 seconds and are excluded from the test durations above.

All fixtures were exclusively below this worktree's `.data/t/frame-*`; failed
cases would be retained, successful cases were removed. Final scan found
**zero `frame-*` fixtures**. This scenario creates no mounts, guest subprocesses,
host networking changes or external service state. Every phase's final cleanup
confirmed its own service inactive/unloaded and captured cgroup empty/absent;
both `ACTIVE_RUN` and `CLEANUP_BLOCKED` were absent afterward.

Full exact commands, output and timestamped samples are retained locally as
`.data/chaos6/<phase>.log`, `<phase>.resources.jsonl`,
`<phase>.cgroup-start.json`, `<phase>.cgroup-final.json`, where `<phase>` is
the supervisor phase name in the command list. These ignored logs are not
committed machine output.

Rustfmt/check and diff whitespace checks passed for the new source. Targeted
Clippy passed before the two extra oracle assertions; that is the exact scope
of its evidence. No rustdoc/doctests, other platform tests, Core/OverlayNet
package suite, full workspace suite, crash/power-loss experiment or real
ENOSPC experiment was run in this round. No public API change was made.

## Round 03 — Core suspension receipt parsing and revalidation

### Outcome, handoff and scope

This round completed a bounded non-network Core scenario; it did **not**
complete the six-hour campaign. The previous executor finished the journal
round, then a subsequent request was rejected by the agent service. OverlayNet
had no new execution or unmerged changes from that request; the rejection is
not a pVisor failure or evidence of network coverage. The parent observed no
`pvisor-chaos*` services at 15:45:40Z. This executor also read the old
`ACTIVE_RUN`/`CLEANUP_BLOCKED` paths as absent and observed no `pvisor-c6-*`
service before starting. It did not clean or write the old checkout.

The sole load executor for this round was
`/home/reiase/workspace/pvisor/.delta/worktrees/bgn8mdzanjr7/pvisor`
(Delta worktree `70cc68b2-e44d-4ee2-8b66-9d9722341bd5`). Its inherited HEAD was
`35393522f528fd8971d4f223d042f3d2988b243b`; original campaign baseline remains
`7fe09e20de3cb7b51222c7d61d75056208da98fb`. The inherited, untracked NativeHost
test was left unchanged. Parent owns review, integration and PR updates.

Read root and Core READMEs/instructions, Core execution/operation/overlay
definitions, existing operation/overlay/control contract tests, and relevant
runtime completion/Session consumers. There are no intervening or Core-test
README/AGENT(S) files. Existing tests exercise checkpoint records and operation
binding, but there was no direct Core external test of
`ExecutionSuspension::from_result`. The runtime relies on that function before
projecting Hibernated completion evidence into a Session.

Added `crates/pvisor-core/tests/suspension_receipt_recovery.rs` and documented its
scope in the Core README. **No production implementation, public interface,
existing assertion or approved semspec file was changed. No product bug was
reproduced.** This crate has not undergone API-boundary migration; the test uses
its existing owning public modules without introducing compatibility exports.

### Scenario and independent expectations

Each seed supplies a JSON-authored valid Hibernated RunResult and receipt, not
a production-object serialization used to calculate its own expected validity.
Snapshot hex content uses SplitMix64 without the previous round's `seed | 1`
alias. Run/request identities vary over 1/255/256-byte values, including Unicode;
Execution identities contain escaped quote, slash, newline and NUL characters.
Short identities can repeat across seeds, and these are not claimed to be
statistically independent streams. Both raw and compressed RAM encodings occur.

Every seed checks:

- A committed fake file is written, synced, closed and reopened. Its receipt
  must equal the separately authored JSON fields exactly, including request,
  snapshot, Run/Execution, timestamp, store and encoding.
- Every proper byte prefix of that serialized RunResult must fail JSON decoding,
  including cuts inside escapes/Unicode and delimiters. A bounded selection of
  distinct cuts is also actually written/synced/closed/reopened from another file.
- A finite 53-case mutation table checks 45 receipt rejections, one outer
  RunResult decode rejection and seven valid controls. It covers all nine
  non-Hibernated states, a fabricated state, guest exits including zero and i32
  extrema, failure evidence, absent/null/wrong-shaped receipts, blank/oversized
  request IDs, stale outer/inner Run or Execution identities, malformed digests,
  relative/oversized stores, zero timestamp, unknown RAM encoding, missing
  receipt members and strict nested unknown fields.
- Valid controls retain 256-byte ASCII/Unicode request IDs, permitted control
  bytes in request IDs, absolute root/exact-4096-byte store paths, u64 maximum
  checkpoint timestamp and explicit null exit/failure. Core does not resolve
  snapshot paths or authenticate a producer; these permitted inputs must not be
  misclassified as product defects.
- All mutation bytes and the earlier committed file remain unchanged after
  parsing/validation, including expected rejection. Caller repair is explicit:
  replace only the owned pending bytes with the known valid record, then reopen
  twice and compare exact receipt identity and bytes again.

The fixtures are fake caller storage. This tests Core's parsing, validation and
serialization after close/reopen, **not runtime atomic publication, outbox
recovery, VM termination, process-crash recovery or power-loss durability**.
No actual snapshot store, mount, socket, guest, network or shared lock is used.
Successful seed directories are deleted only after all assertions; a failed
fixture would be retained without Drop cleanup. No failed fixture occurred.

Default interval is first seed `202611120001`, count 8. The explicit count is
bounded to 1–65536 with checked interval overflow. Counts printed by the harness
are generated subscenarios, never nextest test counts.

### Safety, commands and actual results

Before copying cache, `du -sh` reported the old target as 1.7 GiB and `df -h .`
reported 245 GiB available. The only cache copy command was:

```sh
cp -a --reflink=always /home/reiase/workspace/pvisor/.delta/worktrees/gyv8kfjp7ksa/pvisor/target ./target
```

The source was read-only; copied Core artifacts had different inode numbers and
link count 1. No hard links/shared writable cache were used. All subsequent
Cargo outputs went to the current worktree's `target/`.

The inherited ignored supervisor/helper were inspected and adjusted using
`apply_patch` in this worktree only. The supervisor additionally reads the
entry helper's captured cgroup when a very short collected service disappears
before polling can capture it. It preserves exception/SIGTERM cleanup, separate
bounded stop and all-process kill of its unique UUID service, durable exclusive
`ACTIVE_RUN` identity before launch, fail-closed leftover/cleanup gates, final
inactive plus captured cgroup empty/absent verification, and independent systemd
runtime deadline. These claims concern only the fixed non-adversarial owned
tree, not arbitrary same-UID escape resistance or all-namespace mount discovery.

No budget/gate relaxation: each service used one build job, one nextest thread,
nice 10, CPUQuota 50%, MemoryHigh=MemoryMax=1G, MemorySwapMax=0, TasksMax=128,
RuntimeMaxSec=900, TimeoutStopSec=5, KillMode=control-group, and inner
`timeout --kill-after=5s 880s`. Rust debug info/incremental were disabled.
Startup required >=9 GiB available, load1 <6 and memory PSI full avg10 <0.5
in two samples >=60 seconds apart, also satisfying the disk gate. Runtime
sampling every 2 seconds stopped the owned unit at RAM <8 GiB, disk <30 GiB,
load1 >=6 or PSI >=2. All eight phases qualified in 60.00–60.01 seconds; these
approximately **480 seconds of admission pauses are not test coverage**.

Exact validation commands, in order:

```sh
python3 .data/chaos6_supervise.py core-admission mise run test -- pvisor-core -- --offline --test operation_contracts --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 crates/pvisor-core/tests/suspension_receipt_recovery.rs
python3 .data/chaos6_supervise.py core-receipts-default mise run test -- pvisor-core -- --offline --test suspension_receipt_recovery --no-capture
python3 .data/chaos6_supervise.py core-receipts-clippy mise exec -- cargo clippy --offline --locked -p pvisor-core --test suspension_receipt_recovery -- -D warnings
python3 .data/chaos6_supervise.py core-receipts-clippy-fixed mise exec -- cargo clippy --offline --locked -p pvisor-core --test suspension_receipt_recovery -- -D warnings
PVISOR_CHAOS_FIRST_SEED=202611120009 PVISOR_CHAOS_SEED_COUNT=12288 python3 .data/chaos6_supervise.py core-receipts-seeds-a mise run test -- pvisor-core -- --offline --test suspension_receipt_recovery --no-capture
PVISOR_CHAOS_FIRST_SEED=202611132297 PVISOR_CHAOS_SEED_COUNT=6144 python3 .data/chaos6_supervise.py core-receipts-seeds-b mise run test -- pvisor-core -- --offline --test suspension_receipt_recovery --no-capture
python3 .data/chaos6_supervise.py core-package mise run test -- pvisor-core -- --offline --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 --check crates/pvisor-core/tests/suspension_receipt_recovery.rs
python3 .data/chaos6_supervise.py core-final-validation sh -c 'mise exec -- cargo clippy --offline --locked -p pvisor-core --test suspension_receipt_recovery -- -D warnings && mise run test -- pvisor-core -- --offline --no-capture'
git diff --check
```

The project mise test task supplies `--locked`; offline is explicit. Nextest
`--no-capture` serializes tests and reports ignoring the redundant thread flag.
Initial Clippy failed on two new-test style lints only (`is_multiple_of` and
byte-string spelling); both were fixed without changing expectations, then
targeted Clippy passed before the long runs and again at final validation.

| Phase | Observation UTC start–end, including qualification | Actual result / nextest time |
| --- | --- | --- |
| Existing Core feasibility | 15:48:00.008–15:49:20.066 | 7 passed, 0 skipped; 0.112 s; build 18.16 s |
| Default interval | 15:51:55.653–15:52:59.686 | 1 passed, 0 skipped; 0.330 s; 8 seeds |
| Initial Clippy | 15:53:45.671–15:54:57.723 | Failed two harness style lints; not a product/test failure |
| Corrected Clippy | 15:55:21.770–15:56:23.800 | Passed `-D warnings` |
| A | 15:56:28.861–16:09:38.726 | 1 passed, 0 skipped; 727.781 s; 12288 seeds |
| B | 16:09:51.471–16:17:06.427 | 1 passed, 0 skipped; 372.982 s; 6144 seeds |
| Core package | 16:17:16.520–16:18:30.578 | 91 passed, 0 skipped; 1.034 s; build 11.37 s |
| Final Clippy + Core package | 16:19:59.576–16:21:03.608 | Clippy passed; 91 passed, 0 skipped; 1.042 s |

A/B harness test bodies alone measured 727.779104087 + 372.979876451 =
**1100.758980538 seconds (18 min 20.759 s)** of useful changing-data execution,
with no workload-padding sleep. B's count was deliberately 6144, rather than
another 12288, to keep the total useful run in the requested 15–25 minute range
after observing A's actual throughput; no resource parameter changed.

A/B covered 18432 distinct seed values, with 184320 successful receipt checks,
829440 expected receipt rejections, 153600 expected outer decode rejections
(including persisted cuts), 20173824 in-memory proper-prefix decode rejections,
and 135168 actual persisted-cut checks. These categories overlap: persisted-cut
checks are included in decode rejections and their offsets are also in the
exhaustive in-memory prefix loop; do not sum them as independent tests.
Across default, A/B and two package invocations there were 18456 seed executions,
18440 distinct seeds (the default eight repeat). Across six Rust test command
invocations: **192 nextest tests passed, 0 failed, 0 skipped**, including the
initial seven preexisting tests. Generated expected rejections are not failures.

After the long runs, the positive store-path fixture was made host-native
instead of hardcoding `/` for Windows. On this Linux host it generates the same
root/4096-byte/oversized path inputs. A/B were not rerun after that fixture-only
portability change; final Clippy and the complete Core package, including default
eight seeds, were rerun on the final source. No Windows result is claimed.

### Resource evidence, cleanup and remaining gaps

| Phase | Samples | Minimum available RAM GiB | Maximum load1 | Maximum PSI full avg10 | Cgroup peak MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Existing Core feasibility | 13 | 8.926 | 3.140 | 0.00 | 549.922 |
| Default | 5 | 17.120 | 0.773 | 0.00 | 276.934 |
| Initial Clippy | 9 | 16.873 | 0.338 | 0.00 | 376.246 |
| Corrected Clippy | 4 | 17.054 | 0.332 | 0.00 | 84.059 |
| A | 367 | 16.818 | 1.496 | 0.00 | 174.551 |
| B | 190 | 16.846 | 1.436 | 0.00 | 106.309 |
| Core package | 10 | 16.683 | 1.103 | 0.00 | 557.965 |
| Final validation | 5 | 16.850 | 0.328 | 0.00 | 173.523 |

All eight phases recorded memory high/max/oom/oom_kill counters zero, swap limit
zero and no resource-triggered stop. Minimum available disk was 244.122 GiB.
The initial build's minimum RAM below 9 GiB was during execution, not admission;
it stayed above the unchanged 8 GiB runtime stop.

All service cleanup records confirm inactive/unloaded and captured cgroup
empty/absent; by final inspection every captured cgroup directory had disappeared.
Stop/kill sometimes reported the UUID unit already collected, followed by the
explicit successful state/cgroup verification. `ACTIVE_RUN` and `CLEANUP_BLOCKED`
were absent. A final service listing found no `pvisor-c6-*` or `pvisor-chaos*`
units. Fixture scan found zero `.data/t/core-receipts-*` roots.

Ignored evidence remains in this executor's own
`.data/chaos6/<phase>.{log,resources.jsonl,cgroup-start.json,cgroup-final.json}`,
using phase names from the commands above. It is not committed machine output.
Final test SHA-256:
`bd14e792f046c257667f80c539a325f24553004f2eb1ea1819273a43215ca474`.
A/B pre-portability test SHA-256:
`a1d39f8bb5e6f5811cbedac39b9435bed52fc445deba79bf79c80beca668cd5a`.
Unchanged `crates/pvisor-core/src/operation.rs` SHA-256:
`50c723988d96bc6a5edd389945763e781c1bdb2451ded2aa35ef2bb10e6f32e4`.
Supervisor/helper SHA-256:
`af88e8182bfd7b4af87735e3123dec527e39e7ee393a968b0a62ca9050be0cb9` /
`a18db7475c8e19d4a3fad58cf21fb86a9f0c855512b42334ab6040bb896299ed`.

Rustfmt check, targeted Clippy and whitespace diff checks passed on final source.
No full workspace/Python suite, doctests/rustdoc, other platform, affected large
runtime consumer build, NativeHost/OverlayNet runtime, real VM suspend/resume,
real crash/power-loss, OOM/ENOSPC/fd exhaustion or network scenario was run.
The existing Core package's pure network unit contracts did execute as part of
the full small-package regression; that is not network transport coverage.
There is no fix to publish from this round. Parent should review/integrate this
contract test/report and assign the next bounded round before 20:21Z.

## Round 04 — mount-free selective apply, conflict retention and drop

### Outcome and scope

The small independent `pvisor-overlay-core` package fits the existing 1 GiB,
50% CPU, single-job/no-swap envelope. No large `pvisor`/NativeHost dependency was
built. This round completed its requested useful 20–30 minute generated run,
not the overall campaign: the checkpoint remains ongoing, with deadline 20:21Z.
No product bug was reproduced, no production implementation/API was changed,
and no assertion or ignored annotation was weakened.

The executor remained the same isolated `bgn8mdzanjr7/pvisor` worktree as Round03,
with inherited HEAD `35393522f528fd8971d4f223d042f3d2988b243b` and original campaign
baseline `7fe09e20de3cb7b51222c7d61d75056208da98fb`. Parent separately reviewed and
published Round03; this child did not push or merge. Already-published journal,
Core and prepared NativeHost test sources were left unchanged.

Before editing, read the full Overlay Core README, existing namespace/read
conflict/seal-publication tests, public apply entry points and relevant planning,
publication/recovery/drop implementations. No nested README/AGENT(S) exists
under this crate's tests. Existing tests already cover individual conflicts,
partial acceptance, namespace errors and private interruption states. The new
external composition adds generated ordering of multiple healthy partial accepts
after a stale successful preview, retaining two now-conflicting unselected
changes through every committed batch, reconstruction and final discard.

Added `crates/pvisor-overlay-core/tests/selective_lifecycle.rs` and a README
validation paragraph. It uses this unmigrated crate's existing public root/apply
contracts; no crate API migration or compatibility wrapper was introduced.

### Scenario and oracle

Each seed runs four variants: legacy/compact preimage journal ×
Checkpoint/Strict managed stage. All native paths are exclusively owned
`TMPDIR/sel4-<seed>-<journal>-<durability>-<random>` fixtures. Each successful case:

1. Creates six small native target files with seed-specific binary/Unicode/NUL
   contents. The stage changes three files, renames one file, deletes another,
   and creates a leaf beneath one to three new directories. Rename destinations
   alternate Unicode and non-UTF-8 Unix names.
2. Releases the actual writer, opens a new owner on the same backing, verifies
   staged data and rename/deletion visibility, then releases it before managed
   sealing. Both durability policies must produce a valid seal.
3. Checks the full candidate path/kind map against a separately maintained
   in-memory expected tree. Successful full preview is compared with an owned
   fixture snapshot and must change no names/bytes/inodes/mode/owner/mtime/ctime.
   Inspection atime is intentionally not an invariant.
4. Schedules host **content-only edits between preview and execution** to the
   two files that will remain unselected. Repeated preview and apply of the
   conflicting full selection must explicitly reject before writing intent,
   consuming candidates or changing the captured fixture. The record is loaded
   afresh between rejection attempts. This is deterministic external-editor
   interleaving, not an experiment with a writer running during publication.
5. Applies four disjoint healthy groups in a seed-selected permutation:
   one modification, caller-paired rename source/destination, one deletion,
   and the new leaf plus required new-directory ancestors. Twenty-four
   consecutive seeds exhaust all group orders. Each preview's selected,
   required and candidate sets must match the independent expectation.
6. Checks exact target names/types/bytes, remaining candidate kinds, committed
   ledger count/state/identity, first-touch baselines and both unselected upper
   payloads after **each** batch. The independent target tree is updated from
   the requested operation, never calculated from the planner's result.
7. Edits an already accepted file on the host, then invokes normal committed
   recovery. It must return no pending apply IDs and perform no replay or
   mutation of the captured fixture; the host edit and pending candidates survive.
8. Drops remaining staging, verifies the Discarded publication and absent
   upper/work, reconciles a stale pre-drop handle by calling discard again,
   then rejects apply on that reconciled terminal handle without changing bytes.

The test deliberately selects both rename endpoints. It does **not** invent an
atomic rename-selection closure contract, inject private Prepared/TargetApplied
states, kill processes, claim power-loss recovery or prove transactionality
against writers running during apply. Its zero-write comparisons cover the owned
fixture, not shared lock-file bookkeeping or atime.

Static review found no blocker. Conflict errors are checked by category, not
the exact path, so separate rejection of each of the two conflicting paths is
not established. A stale Staged handle calling apply directly, without first
reconciling through discard, is also outside this test's coverage.

Default first seed is `202611120001`, count 6. Explicit count is bounded to
1–65536, with checked interval overflow. Seed values drive the complete 24-order
permutation, 1–3 directory depth, raw/Unicode path form and bounded payload bytes.
They are deterministic scenario variations, not statistically independent
random streams. Failed fixtures would remain with their `case.txt` identity;
there were no failed fixtures.

### Exact commands and execution evidence

All build/test/Clippy commands ran sequentially through the unchanged Round03
supervisor/helper and its durable exclusive gate. Limits remained:
`CARGO_BUILD_JOBS=1`, `NEXTEST_TEST_THREADS=1`, nice 10, CPUQuota=50%,
MemoryHigh=MemoryMax=1G, MemorySwapMax=0, TasksMax=128, RuntimeMaxSec=900,
TimeoutStopSec=5, KillMode=control-group, inner timeout 880 s/kill-after 5 s,
Rust debug info off and incremental off. The actual cgroup limits were checked
inside each service. This round reused only its own independent reflink target
from Round03; there was no additional old-checkout cache copy or write.

Each phase required two startup samples >=60 seconds apart with available
RAM >=9 GiB, load1 <6 and memory PSI full avg10 <0.5, also passing disk admission.
During execution, 2-second samples retained the RAM <8 GiB / disk <30 GiB /
load1 >=6 / PSI >=2 stop conditions. All eight phases qualified in
60.00–60.01 s; their approximately **480 seconds of startup pauses are not
coverage**. No safety budget or threshold changed.

```sh
python3 .data/chaos6_supervise.py overlay-core-admission mise run test -- pvisor-overlay-core -- --offline --test read_conflicts -E 'test(rename_destination_negative_lookup_prevents_overwriting_host_creation)' --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 crates/pvisor-overlay-core/tests/selective_lifecycle.rs
python3 .data/chaos6_supervise.py overlay-selective-default mise run test -- pvisor-overlay-core -- --offline --test selective_lifecycle --no-capture
python3 .data/chaos6_supervise.py overlay-selective-clippy mise exec -- cargo clippy --offline --locked -p pvisor-overlay-core --test selective_lifecycle -- -D warnings
python3 .data/chaos6_supervise.py overlay-package-initial mise run test -- pvisor-overlay-core -- --offline --no-capture
PVISOR_CHAOS_FIRST_SEED=202611120007 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py overlay-selective-seeds-a mise run test -- pvisor-overlay-core -- --offline --test selective_lifecycle --no-capture
PVISOR_CHAOS_FIRST_SEED=202611122055 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py overlay-selective-seeds-b mise run test -- pvisor-overlay-core -- --offline --test selective_lifecycle --no-capture
PVISOR_CHAOS_FIRST_SEED=202611124103 PVISOR_CHAOS_SEED_COUNT=1024 python3 .data/chaos6_supervise.py overlay-selective-seeds-c mise run test -- pvisor-overlay-core -- --offline --test selective_lifecycle --no-capture
python3 .data/chaos6_supervise.py overlay-package-final mise run test -- pvisor-overlay-core -- --offline --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 --check crates/pvisor-overlay-core/tests/selective_lifecycle.rs
git diff --check
```

Mise supplies `--locked`; offline is explicit. `--no-capture` serializes nextest
and reports the redundant thread flag being ignored. No source or expectation
changed between default, Clippy, initial package, A/B/C and final package runs.

| Phase | Observation UTC start–end, includes qualification | Actual nextest result/time |
| --- | --- | --- |
| Existing-test feasibility | 16:25:39.940–16:26:53.996 | 1 passed, 12 filtered/skipped; 0.008 s; build 11.59 s |
| Default | 16:30:33.463–16:31:37.492 | 1 passed, 0 skipped; 1.432 s; 6 seeds / 24 variants |
| Targeted Clippy | 16:31:57.445–16:33:05.486 | Passed `-D warnings`; 6.98 s dev compilation |
| Initial package | 16:33:33.498–16:34:55.589 | 246 passed, 5 existing ignored tests skipped; 7.412 s; build 12.66 s |
| A | 16:35:25.086–16:45:32.300 | 1 passed, 0 skipped; 545.850 s; 2048 seeds / 8192 variants |
| B | 16:46:13.208–16:56:30.431 | 1 passed, 0 skipped; 554.691 s; 2048 seeds / 8192 variants |
| C | 16:56:47.935–17:02:20.558 | 1 passed, 0 skipped; 270.851 s; 1024 seeds / 4096 variants |
| Final package | 17:02:37.488–17:03:47.533 | 246 passed, 5 existing ignored tests skipped; 7.438 s |

A/B/C test bodies measured 545.847769274 + 554.688493280 + 270.848754995 =
**1371.385017549 seconds = 22 min 51.385 s** of useful changing-data work.
There is no workload-padding sleep. C used 1024 rather than 2048 seeds after
observing A/B throughput, keeping the generated campaign inside 20–30 minutes;
this is a case-count choice, not resource-limit relaxation.

A/B/C together: **5120 distinct seeds / 20480 real directory lifecycle
variants**, 40960 expected preview rejections, 40960 expected execution conflict
rejections, 81920 successful partial applies, 81920 no-pending committed recovery
checks, and 20480 terminal discard lifecycles. These are subscenario counts,
not nextest tests; recovery checks do not mean 81920 crash-recovery experiments.
Including three executions of the default six seeds (standalone plus two package
runs): 5138 seed executions, 5126 distinct seed values, 20552 lifecycle variants.
Across seven Rust test commands: **497 passed, 0 failed**. The feasibility command
filtered twelve other tests; each full package invocation separately skipped five
preexisting ignored manual measurement/screening tests.

Those five are `directory_walk_paired_benchmark`, `append_wal_screening`,
`read_observation_append_screening`, `immutable_content_receipt_screening` and
`known_observation_screening`. They were not enabled or edited; this round makes
no benchmark measurement claim. The macOS-only APFS screening test was not built
on this Linux host.

### Resources, cleanup and limitations

| Phase | Samples | Minimum available RAM GiB | Maximum load1 | Maximum PSI full avg10 | Cgroup peak MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Existing-test feasibility | 10 | 16.845 | 0.349 | 0.00 | 359.938 |
| Default | 5 | 17.052 | 0.250 | 0.00 | 199.074 |
| Targeted Clippy | 7 | 16.988 | 0.383 | 0.00 | 308.230 |
| Initial package | 14 | 16.767 | 0.420 | 0.00 | 551.762 |
| A | 276 | 16.859 | 2.385 | 0.00 | 105.586 |
| B | 281 | 16.059 | 3.143 | 1.00 | 105.637 |
| C | 139 | 16.606 | 1.864 | 0.00 | 269.625 |
| Final package | 8 | 16.687 | 1.355 | 0.00 | 118.980 |

All eight phases recorded memory high/max/oom/oom_kill counters zero and swap
limit zero. No resource stop occurred. B's global full PSI peak 1.00 was during
execution, below the unchanged 2.00 runtime stop, not a qualifying startup sample.
Minimum sampled available disk was 243.882 GiB.

Each UUID service's cleanup confirmed inactive/unloaded and its captured cgroup
empty/absent; all eight cgroup directories were absent afterward. Both gate files
were absent. Final service listing found no `pvisor-c6-*` / `pvisor-chaos*` unit;
zero `.data/t/sel4-*` fixture roots remained. Normal apply locks used only the
unique canonical fixture-target keys. Their persistent lock files in the shared
private lock namespace may remain by design; the shared directory/files were not
enumerated for deletion, removed or swept. No claim of an empty shared lock
namespace is made.

Full ignored evidence is retained in this executor's
`.data/chaos6/<phase>.{log,resources.jsonl,cgroup-start.json,cgroup-final.json}`
for the phase names above. No large machine log was committed.
Final test source SHA-256:
`ca60cd08fb93baab949dbe91ee121044bf14469faf44dce9e1db0fea0181a36e`.
Unchanged production `src/apply.rs` SHA-256:
`4d6b09fe4e1b6ab06588942b7a63dbbfef2963a8a459d4606dd4d0c911e5e669`;
unchanged `src/core.rs`:
`e154f68e87c0096fb82ffde69adeec335aed3e04464ab7f7b4ae23e314c458de`.
Supervisor SHA-256 remains
`af88e8182bfd7b4af87735e3123dec527e39e7ee393a968b0a62ca9050be0cb9`.

Final targeted rustfmt check and whitespace diff check passed. Targeted Clippy
passed on the exact unchanged test source before A/B/C. No FUSE mount, VM,
NativeHost, OverlayNet transport, process-death/power-loss, real ENOSPC/OOM/fd
exhaustion, hostile concurrent writer or real HOME/session/cache mutation scenario
was run. No Python/full-workspace/other-platform/doctest/rustdoc or affected large
runtime consumer build was attempted. The full small-package suite's existing
private in-process failure/recovery tests did execute; this is not substituted
for the omitted real-crash experiments. Parent owns review/integration/PR update
and the next bounded assignment; the six-hour end remains 20:21Z.

## Round 05 — Replay native full-duplex boundaries and healthy successors

### Outcome and existing coverage

`pvisor-replay` depends on Core/Journal and its own protocol/network libraries,
not the large `pvisor` runtime. Its existing large-stdin regression compiled and
passed within the unchanged 1 GiB envelope, although compilation reached the
hard limit and reclaimed memory (details below). The new finite native process
composition and complete package regression passed. **No product bug was found,
no production behavior was changed, and no existing assertion was weakened.**
This is another ongoing campaign checkpoint, not six completed hours.

Read the crate README/manifest, all process supervision and existing process
tests, and historical rounds 033/034 in `CHAOS_TEST_CONTINUATION.md`. Pending
stdin cancellation/EOF, early close, leader exit with inherited pipes, slow
progress/idle cancellation and a subsequent healthy process were already covered.
They are not presented as new defects or newly discovered guarantees here.
No nested README/AGENT(S) exists beneath this crate's source directory.

Added private `src/process_generated_tests.rs`, included from `src/process.rs`
only under `cfg(all(test, unix))`, and documented it in the crate README. The
only change to existing `process.rs` is that four-line test module declaration.
No public API, production test hook or process supervision algorithm changed.
The worker still used its own `bgn8mdzanjr7/pvisor` worktree, inherited HEAD
`35393522f528fd8971d4f223d042f3d2988b243b`; parent independently owns publication.

### Native fixture and independent byte oracle

Every seed runs four process lifecycles sequentially, never more than one
fixture child at a time, with **no descendants**:

1. A native re-execution of the test binary emits 131073 stdout bytes and 65537
   stderr bytes before reading stdin. It then actually closes stderr, reads
   stdin to EOF, records received bytes in its private directory, and echoes
   them to stdout. Both output draining and input delivery must make progress.
2. The same full-duplex exchange runs with stdout redirected to a private file.
3. Another native child closes stdin and exits, while 1048577 configured input
   bytes are pending. The existing expected executor-error path must reject,
   terminate/reap its child and leave no process group.
4. A new full-duplex successor, with a different input-length/tail-window index,
   must complete successfully after that error. Its redirect choice alternates.

Input lengths cover zero/one and ±1 boundaries around 4 KiB, 64 KiB, 128 KiB,
256 KiB, 512 KiB and 1 MiB (20 choices). Retained tails cover 0/1 and neighboring
4096/16384/65537-byte boundaries (11 choices). Seeds also vary bounded binary
payloads, expected ordinary exit code 0/7/23, and successor redirect mode.
Twenty consecutive default seeds cover all input sizes; longer runs cover the
combined deterministic index cycle and changing payloads. These are bounded
scenario variations, not statistically independent streams.

The parent holds exact configured input independently; the child does not read
it from an oracle file. Comparing its actual received file proves input bytes
and EOF delivery. Parent-authored stdout/stderr prelude files define the output
oracle. Exact separate-channel tails, truncation flags, status and cleanup flags
are checked, including zero retention and stderr half-close. Redirect mode also
compares the **complete** child stdout body and exact stderr log. The native
libtest startup prefix is separately bounded below 4096 bytes and excluded from
fixture-payload counts. Pipe-mode combined logs have unspecified interstream
ordering: only their total length bound, separate exact tails and input evidence
are asserted, not an invented global concatenation order. Logs must have mode 0600.
Pipe-mode full stdout/stderr bodies are not compared byte-for-byte: equal-length
front-of-stream corruption or small loss masked by the libtest-prefix allowance
would not necessarily be detected. No full-body pipe fidelity claim is made.

Each child has a five-second wall deadline, 100 ms termination/pipe graces and
an independent eight-second alarm. That alarm bounds only the child, not a
deadlocked parent reader; the outer 880/900-second watchdogs remain required.
Input is at most 1 MiB+1; one duplex child's
stdout/stderr payload is at most 1245187 bytes. The deliberate early-close child
has only native harness startup output. Child current directory, HOME and XDG
paths point into its exclusively created private fixture. No shell, interpreter,
PATH command, model, external agent, network connection or host configuration is
used by the generated check. Test code only performs read-only PID/group checks;
it never signals a PID recovered from a file.

This new matrix does not intentionally expire wall/idle deadlines. The existing
separate timeout/cancellation regressions execute in the full package run; this
is not claimed as new timer, 033/034 inherited-pipe or real replay exactly-once
coverage. Failed `replay5-*` roots would retain seed/PID/received/log evidence.
Success removes only its owned fixture after reaping/group checks and assertions.

### Commands and actual results

All seven phases used the same durable single-executor supervisor, explicit
offline/locked mise test entry, one Cargo job, one nextest thread, nice 10,
CPUQuota=50%, MemoryHigh=MemoryMax=1G, no swap, TasksMax=128, 900-second systemd
deadline, five-second stop grace/control-group kill, and inner 880-second timeout
with five-second kill-after. Rust debug info/incremental remained disabled.
Startup still required >=9 GiB / load1 <6 / PSI full avg10 <0.5 in two samples
>=60 seconds apart. Runtime sampling stayed at two seconds with the original
8 GiB RAM / 30 GiB disk / load 6 / PSI 2 stop gates.

```sh
python3 .data/chaos6_supervise.py replay-admission mise run test -- pvisor-replay -- --offline --lib -E 'test(delivers_stdin_larger_than_a_pipe_without_losing_bytes)' --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 crates/pvisor-replay/src/process_generated_tests.rs crates/pvisor-replay/src/process.rs
python3 .data/chaos6_supervise.py replay-duplex-default mise run test -- pvisor-replay -- --offline --lib -E 'test(full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes)' --no-capture
python3 .data/chaos6_supervise.py replay-duplex-clippy mise exec -- cargo clippy --offline --locked -p pvisor-replay --lib --tests -- -D warnings
PVISOR_CHAOS_FIRST_SEED=202611120021 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py replay-duplex-seeds-a mise run test -- pvisor-replay -- --offline --lib -E 'test(full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes)' --no-capture
PVISOR_CHAOS_FIRST_SEED=202611122069 PVISOR_CHAOS_SEED_COUNT=3072 python3 .data/chaos6_supervise.py replay-duplex-seeds-b mise run test -- pvisor-replay -- --offline --lib -E 'test(full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes)' --no-capture
PVISOR_CHAOS_FIRST_SEED=202611125141 PVISOR_CHAOS_SEED_COUNT=2048 python3 .data/chaos6_supervise.py replay-duplex-seeds-c mise run test -- pvisor-replay -- --offline --lib -E 'test(full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes)' --no-capture
python3 .data/chaos6_supervise.py replay-package-final mise run test -- pvisor-replay -- --offline --no-capture
MISE_TRUSTED_CONFIG_PATHS="$PWD" mise exec -- rustfmt --edition 2024 --check crates/pvisor-replay/src/process_generated_tests.rs crates/pvisor-replay/src/process.rs
git diff --check
```

The test uses default first seed `202611120001`, count 20. Explicit count is
bounded to 1–4096 with checked interval overflow. B's count 3072, compared with
A/C's 2048, was chosen after observing A throughput to obtain 20–30 minutes of
useful data with three bounded invocations. No resource threshold changed.
`--no-capture` makes nextest serial and ignores the redundant thread setting.

| Phase | Observation UTC start–end, includes qualification | Actual nextest result/time |
| --- | --- | --- |
| Existing-test feasibility | 17:07:44.046–17:09:42.220 | 1 passed, 140 filtered/skipped; 0.023 s; build 55.29 s |
| Default | 17:14:40.051–17:15:54.106 | 1 passed, 141 filtered/skipped; 3.870 s; 20 seeds |
| Clippy lib/tests | 17:16:09.202–17:17:47.331 | Passed `-D warnings`; dev compilation 37.21 s |
| A | 17:18:52.778–17:26:25.830 | 1 passed, 141 filtered/skipped; 391.767 s; 2048 seeds |
| B | 17:26:45.453–17:37:37.055 | 1 passed, 141 filtered/skipped; 589.048 s; 3072 seeds |
| C | 17:37:47.061–17:45:20.126 | 1 passed, 141 filtered/skipped; 391.765 s; 2048 seeds |
| Final package | 17:45:29.012–17:47:03.125 | 155 passed, 1 ignored test skipped; 24.867 s; build 7.78 s |

A/B/C test bodies measured 391.763937279 + 589.044768815 + 391.760878475 =
**1372.569584569 seconds = 22 min 52.570 s** of changing-data execution.
There is no workload-padding sleep. Supervision tick/pipe polling intervals
remain part of actual bounded input/output execution, not added delay.

Long-run totals: **7168 distinct seeds, 28672 native child lifecycles**:
21504 duplex children (10752 redirected), 7168 expected early-stdin-close errors,
7168 healthy successors, 9556 ordinary nonzero exits and 6561973297 successfully
delivered input bytes across duplex children. Successors are already included in
duplex counts; nonzero exits and redirects overlap those counts. The byte total
is accumulated small, deleted fixtures, not a simultaneously resident file or
throughput claim. No timer expiration or process descendant is in those totals.

Including standalone and final-package default reruns: 7208 seed executions,
7188 distinct seed values and 28832 child lifecycles. Across six Rust test
commands, **160 nextest tests passed, 0 failed**. The single final-package
ignored test is the existing approximately three-minute
`slow_progress_then_idle_cancellation_drains_and_allows_a_fresh_process`.
Its ignored status was not changed and it was not explicitly run. Filtered
counts in targeted invocations are listed separately, not claimed as coverage.

### Resources, cleanup and limits of evidence

| Phase | Samples | Minimum available RAM GiB | Maximum load1 | Maximum PSI full avg10 | Cgroup peak MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Existing-test feasibility | 32 | 16.826 | 0.933 | 0.00 | 1024.000 |
| Default | 10 | 16.527 | 0.562 | 0.00 | 603.297 |
| Clippy | 22 | 16.751 | 0.744 | 0.00 | 414.348 |
| A | 199 | 16.636 | 0.938 | 0.00 | 106.344 |
| B | 298 | 16.567 | 1.190 | 0.00 | 106.387 |
| C | 199 | 16.591 | 0.647 | 0.00 | 111.523 |
| Final package | 20 | 16.624 | 0.691 | 0.00 | 397.621 |

Initial compilation reached the exact 1 GiB cgroup limit:
`memory.events max=844`, high=0, oom=0, oom_kill=0. It succeeded after cgroup
memory-limit reclaim; **this is not evidence of spare compilation headroom**.
All later phases had high/max/oom/oom_kill=0. No host OOM, cgroup OOM kill,
resource-triggered stop, swap or parameter retry occurred. Minimum sampled
disk availability was 243.895 GiB. Seven qualifying startup pauses totaled
approximately **420 seconds**, excluded from useful execution time.

All seven owned services were confirmed inactive/unloaded with captured cgroup
empty/absent, and their directories were absent afterward. Final listing found
no `pvisor-c6-*` / `pvisor-chaos*` service; both gate files were absent. Every
new `replay5-*` fixture was removed on success. Two transient existing-suite
`.tmp*` directories surfaced during full regression and were already absent
at inspection; no manual sweep was done.

Ignored raw evidence remains under this executor's
`.data/chaos6/<phase>.{log,resources.jsonl,cgroup-start.json,cgroup-final.json}`.
New test SHA-256:
`3963a555cdcaa70e140c91035223c4f09d2eb2c12d6627624106bd903d0cf5a0`.
Final `src/process.rs`, with test-only include, SHA-256:
`2f1048794ec55a84ab03eef47a220c35625f961905fadd6be9e0862152f650d1`.
The same source was used for default, Clippy, A/B/C and final package validation.
No large machine logs were committed.

Final rustfmt and whitespace checks passed; Clippy covered `--lib --tests` with
warnings denied. No other platform, Python/full-workspace/doctest/rustdoc,
external agent/model, replay exactly-once proof, hostile process escape or
resource-exhaustion experiment was run. Existing full-package bridge tests use
their own local protocol fixtures; they are not external network acceptance.
Parent has separately authorized a possible Round06 NativeHost retry based on
the now-measured host memory surplus, with explicit new stage budget/admission
conditions. No NativeHost workload was started before this round's cleanup.
The overall campaign still ends at 20:21Z.
