# Agent plugin low-load chaos campaign

## Campaign

- Requested window: 2026-11-08 01:04:00–09:04:00 UTC (eight hours).
- Initial revision: `84b0898a`; branch: `test/plugin-chaos-20261108`.
- Scope: realistic coding-agent tasks using pVisor's Pi, DSH and Codex adapters,
  including the native Session lifecycle they invoke.
- Tests run in subagents. The coordinator reviews and publishes each verified
  repair to the campaign PR; no merge to main or package publication is authorized.
- Existing PR #120 is a separate plugin version/publication change. Do not
  duplicate its known metadata fix or change its branch.
- This file records actual evidence, not a promise that eight hours have elapsed.
  Record interruptions, unsupported environments and failed checks explicitly.

## Safety limits

- One active build/test workload; at most two Cargo jobs and one test thread.
- Run workloads with `nice -n 10`, finite command timeouts (at most 900 seconds
  plus a 30-second kill grace), bounded subprocess output and bounded fixtures.
- Before and after each workload check available memory, load and free disk.
  Do not launch below 6 GiB available RAM or 20 GiB free disk, or above load1 8.
  Swap is already in use: do not treat available swap as workload capacity.
- No disk filling, host network changes, global cache cleanup, unrelated process
  termination, real credentials, cloud-model calls or paid services.
- Fault injection and cleanup are confined to owned disposable fixtures in the
  worktree. Track process ownership and verify teardown; do not weaken safety
  flags to bypass an unavailable platform capability.
- Use `mise run test` with targeted package names for Rust and the repository's
  plugin test task for adapter tests. Do not modify semspec approvals or claims.
- Read applicable directory READMEs and agent instructions before editing.
- Do not launch new workloads after the deadline. Distinguish elapsed wall time,
  active test time, mocked adapter checks and real runtime evidence.

## Scenario queue

Choose the next scenario from observations rather than blindly exhausting a list.

1. Agent fixes a failing test, retains changes, reviews, accepts one file and
   discards scratch output; follow dependent operations in one spawn.
2. User cancels a slow test or closes the Agent; preserve the exact selector,
   uncertain completion and recoverability without automatic rerun.
3. Agent emits large/binary/multibyte output, or a child keeps stdio open;
   bound capture, cancellation and subprocess lifetime.
4. User changes project files during review or approval; preserve conflicts,
   explicit path selection and approval scope.
5. Two tasks from different projects overlap; isolate project/task identity,
   state directories and exact-selector operations.
6. MCP receives malformed, partial or interrupted requests; recover without
   accepting mutations or confusing request/task identity.
7. Runtime is missing, incompatible, replaced or fails halfway through a task;
   fail explicitly and preserve retained work where appropriate.
8. Repeat realistic retained-stage lifecycles with bounded resources and verify
   no accumulating processes, mounts, descriptors or owned temporary roots.

## Initial observations

At 01:03 UTC: 16 CPUs, 18,907 MiB available RAM, 1,066 MiB swap used,
322 GiB free disk, load1 0.61. `/dev/fuse` and `/dev/kvm` exist; runtime
capabilities still require verification. Previous eight-hour campaigns are
recorded in `CHAOS_TEST_LOG.md` and `CHAOS_TEST_CONTINUATION.md`.

## Rounds

### Round 1 — cancelled capability probes and retained output

- UTC start: 2026-11-08 01:04:54; end: 2026-11-08 01:18:37
  (13m43s wall time, not an eight-hour result).
- Story: a coding Agent starts a staged test, the user cancels while the
  adapter checks the runtime or while the native test is running, and later
  reopens the exact selector to inspect and safely dispose of candidates.
  A faulty runtime/descendant may ignore termination or retain output pipes.
- Read root instructions, README and CONTRIBUTING, this campaign plan, and
  plugins README/CONTRIBUTING before edits. No nested plugin instructions or
  READMEs were present. No Git commits, pushes, publication or merges.

#### Confirmed bug and repair

Capability checks used a separate `execFile` path without the caller's
AbortSignal. Its ten-second timeout sent SIGTERM without escalation: a runtime
that ignored SIGTERM kept the operation pending indefinitely. Pre-aborted checks
also launched probes. The actual task supervisor already had bounded escalation
and output draining, but did not supervise these probes.

Moved that existing supervisor unchanged into internal `plugins/process.mjs`.
Backend keeps its existing `execute` export and result contract. Capability
checks now use the same supervisor, inherit cancellation from doctor/run/operate,
receive EOF on stdin, reject interrupted/truncated/nonzero probes, and never
fall back or retry. The probe deadline remains ten seconds; termination and
stream draining each have at most five additional seconds. Version checks in
doctor use this same supervisor. Capability results are not cached: existing
runtime-replacement coverage still rechecks the selected executable.

Added deterministic regressions for pre-abort, cancellation and timeout against
a SIGTERM-resistant probe, EOF on probe stdin, and real subprocess cancellation
through Pi/DSH with exact-selector recovery and no task spawn. Added
DOC-PLUGINS-12 for genuine active native cancellation after a staged write.
Updated the plugin maintenance documentation and packed-file list for the new
internal helper; no version/metadata bump from PR #120 was duplicated.

#### Commands and actual outcomes

All substantive workloads used a resource-guard wrapper around
`nice -n 10 timeout -k 30 900 COMMAND`, with `CARGO_BUILD_JOBS=2`,
`RUST_TEST_THREADS=1`, `NEXTEST_TEST_THREADS=1`, and
`MISE_AUTO_INSTALL=0`. No tools were installed. Commands ran sequentially.

The repository task is `node --test plugins/test/*.test.mjs`; for strictly
serial final runs, an owned temporary PATH shim invoked
`/usr/bin/node --test --test-concurrency=1` with the task's file arguments.
This leaves the repository task and committed tool configuration unchanged.

| UTC | Workload | Outcome |
| --- | --- | --- |
| 01:05:47 | `mise run test-agent-plugins` with attempted NODE_OPTIONS concurrency flag | Node refused that flag before running tests; exit 9 |
| 01:05:59–01:06:00 | `mise run test-agent-plugins -- --test-concurrency=1` | Baseline 25 passed, 0 failed; appended flag did not serialize file execution, so later runs used the shim above |
| 01:08:19–01:08:44 | `mise run test-agent-plugins`, new regressions before production repair | 28 tests: 25 passed, 3 failed; pre-abort unexpectedly succeeded, cancellation exceeded 7s, SIGTERM-only timeout exceeded 17s; owned stuck probes killed and verified gone |
| 01:10:25–01:10:47 | `mise run test-agent-plugins`, after repair | 31 passed, 0 failed |
| 01:10:53–01:10:58 | `PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor mise run test-agent-plugins-native` | Existing 11 native tests passed, 0 failed |
| 01:13:37–01:13:47 | `python3 reap.py node descendant.mjs` in owned scratch | Harness assertion incorrectly expected a literal escaped newline; fixture cleaned and descendant reaped |
| 01:13:54–01:14:05 | Corrected descendant-output check, same command | 1 passed: cancellation settled in 10,027ms after probe leader exited while its descendant still held stdout/stderr; exactly one probe, no retry |
| 01:14:40, 01:14:54 | `node native-cancel.mjs` in owned scratch, real CLI | Two harness-only assertion failures: assumed `record` instead of this runtime's `run` status field, then expected `failed` instead of correct `cancelled`; both tasks were cancelled and fixtures removed |
| 01:15:02–01:15:03 | Corrected active-cancellation acceptance, same command | 1 passed: CLI exit 143, interrupted=true, native state=cancelled, live=false; reopened exact-selector review/preview/drop succeeded, project unchanged, zero owned processes/mounts |
| 01:16:31–01:16:53 | Final `mise run test-agent-plugins` | **31 passed, 0 failed, 0 skipped/cancelled**; 21.956s |
| 01:16:54–01:16:59 | Final `PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor mise run test-agent-plugins-native` | **12 passed, 0 failed, 0 skipped/cancelled**, including new DOC-PLUGINS-12; 5.486s |
| 01:17:20–01:17:28 | `git diff --check`, process/mount scan and owned-fixture cleanup | Diff check passed; zero owned scratch processes/mounts. Cleanup glob initially also matched log files and raised NotADirectoryError; corrected directory-only scan verified all owned acceptance fixture directories absent |

The descendant fixture spawned a 25-second self-bounded Node child inheriting
both output descriptors, then exited its probe leader. This was a mocked
runtime/process-supervision check, not native Session acceptance. It confirms
bounded uncertain completion, **not descendant teardown**. The test explicitly
terminated its owned remaining child; a process-local Linux subreaper reaped it
(PID 755275, SIGTERM). No production process-group/global kill was added.

Native evidence used the existing compatible debug CLI, reporting `pvisor
0.3.1`, SHA-256
`a34dd0d90381df5f775af98d456ff794ec17185e307f6a53367083bd5298aad8`.
It was not rebuilt and its source revision was not established. These were real
Host safe/deny-all staged commands, real filesystem review/preview/apply/drop
in the existing suite, and real stdio MCP transport. Pi/DSH host registration
and approval callbacks were test contracts, not actual interactive clients.

#### Resources, cleanup and gaps

- Every workload's pre/post guard passed: MemAvailable 17.83–18.53 GiB,
  free disk 321.81–321.88 GiB, load1 0.33–1.07. Final check:
  18.02 GiB available, 321.81 GiB disk, load1 0.44.
- Safety deviation: the initial successful baseline did not enforce serial
  test-file execution. All red/green/final runs used the explicit serial shim;
  no two workload commands ran concurrently.
- Fixtures and bounded output lived in owned Delta terminal/scratch directories;
  native candidates were dropped or removed with their stopped disposable
  fixture. Probe tests verify their owned PIDs no longer exist. The active
  native acceptance verified no owned command/supervisor or FUSE mount remained.
  Retained failed-task private HOME directories were confined to the owned
  terminal fixture roots, whose tool lifecycle removes them. Scratch scripts
  and logs expire at turn end; durable evidence is summarized here.
- No Rust code changed, no Rust builds/checks were needed, and no
  `mise run test -- PACKAGE`, Rust formatting, Clippy, or broad lint was run.
  Node 24.22.1 was used. No real Agent UI, model/cloud calls, package publication,
  host/network changes, installs, unrelated process kills, or semspec changes.
- Next independent scenario: close or SIGTERM the MCP client/server while a
  genuinely active native task owns staged candidates, then reopen the exact
  selector and verify native reconciliation refuses unsafe apply when completion
  is uncertain. Existing MCP shutdown coverage closes after task completion;
  abrupt transport/server death during active execution remains untested here.

#### Round 1 review follow-up — fixture readiness teardown

Independent review found no production issue, but identified a P2 test-fixture
leak: `bounded()` raced the readiness polling promise without stopping its loop.
A probe exiting without its ready file, or missing the readiness deadline, could
leave that loop polling forever after fixture removal.

The fixture now owns a cancellation controller combined with the caller's
signal. Readiness polling has its own deadline, checks probe settlement, and
uses cancellable delays instead of racing an uncancelled loop. Setup failure
and teardown both cancel and await the owned probe even when no PID was
published; teardown is idempotent. Added a deterministic runtime-fixture case
that exits without readiness and must reject after completing teardown.

Only `plugins/test/distribution.test.mjs` and this review note changed in this
follow-up. Static rereview found no remaining issue. Validation waited while
round 2 owned the single workload slot; the coordinator released it at
01:41:46 UTC.

Final post-review validation, 01:42:22–01:42:44 UTC:
`nice -n 10 timeout -k 30 900 mise run test-agent-plugins`, with the same explicit
serial Node shim, Cargo jobs 2, test threads 1 and automatic installs disabled:
**32 passed, 0 failed, 0 skipped/cancelled**, 22.022s. This includes the new
exits-without-readiness regression. Pre/post guards passed: MemAvailable
18.51/18.34 GiB, free disk 321.63/321.63 GiB, load1 0.71/0.72. The owned
validation fixture root was removed after verifying no associated processes or
mounts remained.

No native suite was rerun for this test-only follow-up; its earlier result
remains 12 passed. The existing native binary's source revision remains an
explicit unverified coverage gap.

### Round 2 — active MCP disconnect and lost-response reconciliation

- UTC start: 2026-11-08 01:19:44; last workload ended 01:41:46; entry/handoff
  completed about 01:43 UTC (~23 minutes wall time, including 15 minutes blocked
  by an untrusted-mise prompt, not eight hours of execution).
- Read root instructions/README/CONTRIBUTING, plugin README/CONTRIBUTING,
  this log and prior campaign frontend-disconnection and inherited-mount topology
  hazards. Reused round1's compatible debug CLI without rebuilding or installing.
  No Rust changes, Git commits, pushes, merges, publication or semspec changes.
- Story: Codex-facing stdio MCP starts one native safe Host task, writes staged
  source-result/test-result files and runs a shell assertion, then remains active.
  Before any spawn response, close input, truncate a JSON frame and close input,
  SIGKILL only the owned MCP server, or destroy both client pipes. Discover the
  selector by validating the private task record's project/UUID, not `last`.
  Native status must observe running/live candidates before each injected fault.
  Native preview/apply/drop all refuse while live. Reopen exact-selector status,
  wait for a terminal receipt, review both candidates, preview without publication
  and explicitly drop. Original project stays unchanged; exactly one task exists.

#### Confirmed bug and repair

Client response-pipe loss caused an unhandled stdout EPIPE and MCP exit 1.
`mcp.mjs` now handles output errors: stop accepting requests, destroy input,
abort all active controllers, and suppress further sends. EPIPE and destroyed
streams are ordinary disconnects; other output errors retain a stderr diagnostic
and failure exit. No acceptance authority, retries, process-group kills or
native state manipulation were added. The separate portable fake-runtime test
closes only output, leaves request input open, triggers a ping response, and
requires the active task's SIGTERM acknowledgement plus normal server exit.
This is transport supervision evidence, not native filesystem evidence.

#### Commands, failures and red/green evidence

Workloads were sequential with `nice -n 10`, `CARGO_BUILD_JOBS=2`,
`RUST_TEST_THREADS=1`, `NEXTEST_TEST_THREADS=1`, no automatic installs and the
round1 explicit Node PATH shim adding `--test-concurrency=1`. Guarded execution
used `timeout -k 30`, initially 900s and finally 110s; outer finite tool limits
were 900s/180s/120s. Final mise calls used process-local
`MISE_TRUSTED_CONFIG_PATHS="$PWD"` and `MISE_YES=1`.

| UTC | Command/check | Actual outcome |
| --- | --- | --- |
| 01:21:09–01:36:09 | `mise run test-agent-plugins-native` | Blocked at mise trust prompt, killed at 900s; no tests or build executed. Process scan confirmed no surviving owned workload |
| 01:36:25–01:36:44 | `PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor mise run test-agent-plugins-native` | Red: 16 tests, 15 pass, client pipe loss fails because server exits 1; other three new native scenarios pass |
| 01:39–01:40 | First final-check invocation | Guard script from earlier tool scratch unavailable; Python exits 2 twice, no tests executed; recreated owned guard/shim |
| 01:40:23–01:40:45 | `mise run test-agent-plugins` | Green: 32 passed, 0 failed/skipped/cancelled, 22.145s |
| 01:40:45–01:41:04 | Native task above after EPIPE repair | 15 pass/1 new harness failure: shutdown status briefly unknown, but subsequent preview independently reconciles cancelled and succeeds. Removed stale-status assumption; no product checks weakened |
| 01:41:27–01:41:46 | Final native task above | Green: 16 passed, 0 failed/skipped/cancelled, 18.915s |
| 01:41:46 | `git diff --check`, process scan and mountinfo scan | Passed; no remaining owned fixture/native/server processes or current-namespace fixture mounts (scan matched only the checking shell) |

Final native outcomes: active EOF, truncated-frame EOF and client pipe loss
reconciled **cancelled**; owned MCP SIGKILL left native work active until the
12-second command naturally finished, then reconciled **completed**. All retained
candidates stayed reviewable, and none were automatically applied or discarded.
The SIGKILL case therefore proves bounded continued execution and live mutation
refusal, not immediate native task teardown. A transient unknown status was
observed during one owner shutdown; no stable unknown/missing-receipt fault was
injected or claimed covered.

#### Resources, cleanup, limitations and next scenario

- Recorded workload boundaries: MemAvailable 19,483,406,336–19,957,047,296 bytes
  (~18.15–18.59 GiB), disk free 345,351,458,816–345,480,900,608 bytes
  (~321.63–321.75 GiB), load1 0.258–0.978. All gates passed.
  The first timed-out guard had no POST; the next preflight supplied that check.
- Fixture roots were owned disposable terminal directories. New native fixtures
  set TMPDIR to their own non-project directory, keeping private failed-task HOME
  confined there without inherited staging mounts. Candidates were explicitly
  dropped after terminal reconciliation, then fixture roots removed. SIGKILL
  targeted only the owned child PID. No global cleanup/unrelated signals.
- Native binary is the same external debug artifact/version/hash recorded in
  round1; its source revision remains unestablished. Real native safe/deny-all
  Sessions and stdio JSON-RPC were exercised, not an interactive Codex agent,
  model call, paid service or credentials. The native "test" is a bounded shell
  assertion, not a compiler/build suite.
- Genuine recovery limitation: no plugin list/recovery API exists. Lost-response
  recovery here required explicit operator inspection of private task records.
  Multiple ambiguous tasks must not be guessed from order. README documents the
  limitation, pre-native task records, and abrupt-death continuation.
- Only `mcp.mjs` production code changed; new tests/helper are separate files.
  Round1 production/test files were untouched; README/CONTRIBUTING/log additions
  are additive. Parent was notified immediately of scope and workload release.
- No Rust/Python workspace suites, Clippy, rustdoc, broad lint, installs or
  platform matrix run. Persistent unknown and crash-before-receipt remain gaps.
- Recommended next scenario: child process retains stdio while MCP client
  disconnects, with bounded output and owned-descendant teardown; separately
  design explicit lost-response discovery rather than quietly adding a `last`
  fallback or automatic retry.

#### Round 2 static-review follow-up — code ready, validation pending

Parent's reviewer identified three P2 test gaps after the initial handoff;
the production disconnect handler was clean. Follow-up changes are test-only:

- Selector discovery now treats a missing record or partially written JSON as
  not ready within the existing polling bound. Parsed records must exactly match
  schema/project/task, with canonical project/task directories and one UUID task.
  Extra task directories and invalid completed records fail closed.
- Graceful active EOF, truncated-frame EOF and client pipe loss now require a
  **cancelled** terminal state. Only abrupt server SIGKILL may naturally complete.
  This prevents the 12-second fallback from hiding removed cancellation.
- Fixture removal now requires a native reconciliation callback after MCP closes.
  On assertion/startup failure it boundedly waits for a known terminal receipt
  and explicitly drops candidates before removal. Failure, persistent unknown,
  live ownership or an unresolvable record preserves the root and reports its
  exact path instead of recursively removing potentially live native state.
  Normal success records the already verified terminal/drop disposition.
- Added portable regression cases for missing/partial/invalid/extra task records
  and for preservation when reconciliation returns false or throws, followed by
  permitted removal only after affirmative terminal reconciliation.

No tests or builds were run for this follow-up because round3 owns the sole
workload slot. Earlier 32/32 and 16/16 results apply to the initial repair, not
these follow-up changes. Parent will schedule serial adapter/native validation
after the slot is released. No additional production or round1 files changed.
Native cleanup gives each capability/operation call an AbortSignal capped at
two seconds and the remaining overall 20-second cleanup budget. Supervisor
kill/drain grace can extend final settlement, but the ordinary 30-second command
deadline cannot silently reset that budget. Expected adapter count is 34 in
this child's inherited distribution tests, or 35 after the parent's additional
round1 exits-before-readiness regression; native count remains 16.

Final follow-up validation was authorized after round3 released the workload
slot at 01:48:17 UTC. On 2026-11-08:

| UTC | Exact task | Outcome |
| --- | --- | --- |
| 01:48:46–01:49:08 | `mise run test-agent-plugins` | **34 passed**, zero failed/skipped/cancelled; 22.196s; includes record-publication/invalid-record tests and cleanup-preservation regression |
| 01:49:08–01:49:27 | `PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor mise run test-agent-plugins-native` | **16 passed**, zero failed/skipped/cancelled; 18.993s; strict cancellation assertions and latest 2s-per-call cleanup included |
| 01:49:27 | `git diff --check`, owned fixture/runtime/MCP process scan, current-namespace mountinfo scan | Passed; no matching processes or fixture mounts |

The final runs retained the explicit serial Node shim, `nice -n 10`,
`timeout -k 30 110`, two Cargo jobs/one test thread, no installs and
process-local mise trust/noninteractive configuration. Pre/post resource gates
passed: MemAvailable 19,739,938,816–19,925,495,808 bytes, free disk
345,248,886,784–345,295,941,632 bytes, load1 0.544–0.884.
The slot was released to parent at 01:49:27 UTC.

Native outcomes again were cancelled for active EOF/truncated EOF/client pipe
loss and completed after the bounded 12-second command for server SIGKILL.
The portable regression proves fixture preservation when reconciliation returns
false or throws. A truly stuck native status/drop inside the after-hook was
**not injected**; its AbortSignal budget is code-reviewed but not fault-tested.
The 20-second reconciliation budget is not a hard wall-clock cleanup guarantee:
bounded supervisor termination/output-drain grace may extend settlement.
No builds, Rust/Python suites, publication or unrelated cleanup were performed.
