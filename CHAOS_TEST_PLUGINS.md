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

### Round 3 — approval scope and edits after preview

- UTC start: 2026-11-08 01:44:13; final workload ended 01:48:09; cleanup and
  sole TEST/BUILD slot released at 01:48:17; handoff/documentation about 01:50 UTC.
  This is a short completed round, not eight hours of execution or artificial waiting.
- Read root README/AGENTS, plugin README/CONTRIBUTING and this campaign log;
  discovery found no nested plugin README/AGENT(S). No production code changed,
  and no Git commits, pushes, merges, package publication or semspec edits.
- Story: a Pi/DSH user sees a successful apply preview and a pending approval.
  Pause that exact callback with a manually resolved promise, change the real
  project file, revise the caller's selector/paths, switch the active project,
  and then approve. The original project's native apply rejects the conflict;
  the external edit survives, the other project's stage stays unpublished, and
  scratch files stay staged. Reopen both exact selectors to inspect candidates.
  Explicitly restore the baseline and obtain a new approval to accept only the
  original selected file; native drop later disposes of remaining candidates.

#### Findings and ownership contract

No incorrect acceptance was reproduced and no product repair was warranted.
Native conflict authority correctly rechecks the host after preview; the
adapter does not treat a successful preview as permission to overwrite a later
editor change. Captured initiating project and exact selection remain scoped.

Pi parses its own command JSON before awaiting confirmation. DSH's public
[tool-author contract](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/cookbook/adding-a-tool.md),
read during this round, says dispatch detaches lossless JSON and recursively
freezes arguments before policy/execution. Therefore a mutable object passed
directly to a test's injected `defineTool` identity function is not evidence of
a normal-host approval exploit. The new fixture explicitly models detached,
frozen arguments, then mutates the caller's original object. This is modeled
host-contract coverage, not execution of DSH's actual registry. The source was
upstream `master` documentation, not independently verified against the pinned
0.2.1-alpha.2 binary; that version-alignment gap remains explicit.

New files are `approval-fixture.mjs`, `approval.test.mjs` and
`approval.native.mjs` under `plugins/test/`. The fixture releases callbacks
deterministically, without readiness polling or timing sleeps, and settles
owned pending approvals in `finally`. Portable tests check approval reason,
project/selector/paths and absence of mutation calls. DOC-PLUGINS-14/15 check
real native file state and recoverability. Pi false confirmation and thrown
confirmation errors refuse mutations; DSH additionally checks rejected,
cancelled, unavailable, thrown service errors and an aborted signal followed
by an allowed-once answer. Both apply and drop are covered. No native state,
conflict logic, product recovery API or additional approval authority was added.

#### Commands and outcomes

Read round2's trust-prompt warning before execution. Every workload used:
`MISE_TRUSTED_CONFIG_PATHS="$PWD" MISE_YES=1 MISE_AUTO_INSTALL=0`,
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1`, and
`nice -n 10 timeout -k 30 120`. Setup/check tool calls were bounded at 15s.
A process-local owned PATH shim used this exact serial dispatch:

```sh
if [ "$1" = --test ]; then
  shift
  if [ -n "$ROUND3_TEST_PATTERN" ]; then
    exec /usr/bin/node --test --test-concurrency=1 --test-name-pattern="$ROUND3_TEST_PATTERN" "$@"
  fi
  exec /usr/bin/node --test --test-concurrency=1 "$@"
fi
exec /usr/bin/node "$@"
```

The guard checked MemAvailable >= 6 GiB, free disk >= 20 GiB and load1 <= 8
before and after each workload; any failing precondition refused launch.
No tools were installed and no fresh native build was needed.

| UTC interval | Exact task and scope | Result |
| --- | --- | --- |
| 01:44:27–01:44:49 | `mise run test-agent-plugins`, serial shim | Integrated readiness follow-up + EPIPE baseline: **33 passed**, 0 failed/skipped/cancelled; 22.177s. Immediately reported to coordinator |
| 01:47:35–01:47:57 | `mise run test-agent-plugins`, serial shim, four new portable approval cases | **37 passed**, 0 failed/skipped/cancelled; 22.200s |
| 01:48:06–01:48:09 | `PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor mise run test-agent-plugins-native`, serial shim with `ROUND3_TEST_PATTERN='DOC-PLUGINS-14\|DOC-PLUGINS-15'`, owned TMPDIR | **Four named native approval cases passed**, 0 failed; 2.793s. Node also reported two empty file-level pass entries (six total), not two additional executed scenarios |
| 01:48:17 | `git diff --check`; owned fixture/process/mount scan | Passed; native temporary root empty, no associated processes or current-namespace mounts |

There was no red/green production repair: the new tests passed against existing
behavior. Round2 disconnect native cases were deliberately excluded while its
worker corrected reviewed fixtures; this is not validation of those corrections.
No overlap edits to MCP, disconnect fixtures/tests or distribution tests.

#### Resources, cleanup and gaps

- All six resource boundary checks passed: MemAvailable
  19,666,026,496–19,919,679,488 bytes (~18.32–18.55 GiB), disk free
  345,225,101,312–345,313,492,992 bytes (~321.52–321.60 GiB), load1
  0.365–0.538. No concurrent test/build workload was launched.
- Bounded fixtures lived in the owned per-turn scratch root. Native tasks
  completed before approval injection; teardown called native drop before
  removing each fixture. Successful private HOMEs were removed by Backend.
  Final owned native TMPDIR was empty. No global cleanup, unrelated signals,
  host/network changes, credentials, cloud/model calls or paid services.
- Native CLI reused round1's external artifact/version/hash; its source revision
  remains unverified. These are real Host safe/deny-all Sessions and actual host
  file conflict checks, with **mock approval callbacks**, not interactive Pi/DSH
  or a loader/compiler revalidation. No Rust/Python suites, builds, Clippy,
  rustdoc, platform matrix or full native plugin suite ran in round3.
- Next scenario recommendation: concurrent *legitimate* native operations on one
  retained stage while approval is pending (a second selective apply or drop).
  Check native lock/completion behavior and clear user-facing refusal or no-op
  without inventing preview snapshot guarantees or editing private stage files.

#### Round 3 static-review follow-up — failure-path fixture ownership

Review found three actionable fixture defects despite the passing happy-path
checks above. This follow-up changes tests/helpers only. It was initially
unvalidated while round4 owned the sole TEST/BUILD slot; final follow-up
validation is recorded below. The earlier 37 portable/four native results apply
to the pre-review fixture version, not these edits.

- Native fixture now registers every run attempt before starting it, retains
  every owned operation promise, supplies five-second native task deadlines,
  and combines an owned cancellation controller with the test lifecycle signal.
  Teardown aborts and awaits owned work, then discovers and validates every
  private task record, including runs that never returned a successful result.
  It requires an existing native receipt and terminal, non-live native status,
  followed by successful uninterrupted drop before removing the root.
  Reconciliation has a 20-second operation-admission budget and individual
  two-second cancellation deadlines (supervisor kill/drain grace can extend
  settlement). Missing/unknown/live state or any cleanup failure preserves the
  root and throws `Preserved fixture ABSOLUTE_PATH: ...`. Setup mkdir failures
  are also covered by the already-registered cleanup hook.
- Approval operations attach success/error handlers immediately, including the
  retry. Waiting for the gate races early operation settlement, test
  cancellation and a five-second owned deadline. Finally releases the captured
  gate, cancels unsettled operations and awaits settlement. Added a deterministic
  early-preview-failure case for each host; neither waits for an approval that
  will never be requested.
- Conflict acceptance now records the real Backend apply call/result, requires
  exactly one apply of the original selector/paths/project, uninterrupted native
  exit 3, structured `blocked` status with the original source stage, and a
  conflict blocker. An arbitrary plugin/service error no longer satisfies it.

These review edits were initially unvalidated. Final validation is recorded
under “Round 3 final follow-up validation” below.

### Round 4 — terminal operations during pending selective approval

- UTC: 2026-11-08 01:50:22–01:55:58 for investigation/workloads/cleanup;
  report and handoff followed. Sole test/build slot explicitly released to parent
  at 01:55:58. This is about six minutes of low-load work, not eight hours.
- Read root AGENTS/README/CONTRIBUTING, plugin README/CONTRIBUTING and this
  campaign log; no nested plugin instructions/READMEs existed. Round3 approval
  files were not in this child. Added independent `concurrent-approval-*` files;
  reused round2's small selector discovery/reconcile-removal helpers without
  changing them. No production code, MCP/disconnect/distribution/round1 files,
  private stage state or frozen input ownership semantics changed.

#### Story and finding

Held Pi's actual confirmation callback or DSH's actual approval-service callback
pending after a successful real native preview of `a.txt` and `b.txt`. The
completed safe Host Session also held unselected `c.txt`. While the callback was
pending, an operator used the native CLI to apply one selected file, apply both
selected files, or drop all candidates; a fourth scenario used another explicit
plugin operation to apply `c.txt`. A fifth combined partial native apply with a
host edit to the still-pending `b.txt`. Then the user approved the original
preview. Inputs were frozen, not mutated by the host.

**No product defect demonstrated.** Ten native cases (five per host contract)
confirm the native runtime remains authoritative:

- After terminal apply of `a.txt`, stale approval safely applies only pending
  approved `b.txt`; native JSON identifies exactly that selection and one apply.
  A later user edit to already-applied `a.txt` survives. Unselected `c.txt` stays
  unpublished and reviewable.
- After terminal apply of both selected files, stale approval returns native
  success with empty `selected` and `execution: null`: an informative no-op,
  not an error or a second publication. User edits survive; `c.txt` remains.
- After terminal drop, stale approval surfaces the native discarded diagnostic;
  no candidate is resurrected and no host edit overwritten.
- After another plugin apply of `c.txt`, the pending approval publishes exactly
  `a.txt` and `b.txt`, with all three files matching their separately authorized
  contents.
- If the user edits pending `b.txt` after partially applying `a.txt`, native
  apply refuses with `target changed after staging ... b.txt; refusing to
  overwrite concurrent changes`. Both host edits survive; pending `b.txt` and
  `c.txt` remain reviewable.

Every case requires one preview, one post-approval apply, one approval request,
one Session, and no automatic retry/drop. Two portable regressions independently
require propagation of native mutation failure without retry. Host registration
and approval are callback contracts, **not real interactive Pi/DSH clients**.
Native files, previews, CLI operations and conflict decisions are real.

#### Exact commands, harness corrections and results

All workloads ran serially through process-local
`MISE_TRUSTED_CONFIG_PATHS="$PWD"`, `MISE_YES=1`, `MISE_AUTO_INSTALL=0`,
`CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=1`, `NEXTEST_TEST_THREADS=1`, and
`nice -n 10 timeout -k 30 120`. No installs or fresh build. The owned PATH shim
replaces task `node --test` with `/usr/bin/node --test --test-concurrency=1`;
scoped runs additionally insert `--test-name-pattern="concurrent approval"`.
The exact repository mise tasks were retained. Native commands set
`PVISOR_PLUGIN_BINARY=/home/reiase/workspace/pvisor/target/debug/pvisor`.
Later native runs set `PVISOR_PLUGIN_TEST_OUTPUT_DIR="$PWD/.round4-owned"`.

| UTC | Workload | Outcome |
| --- | --- | --- |
| 01:52:09–01:52:11 | `mise run test-agent-plugins-native`, scoped | Initial eight new cases: two passed, six harness failures. Incorrectly expected a stale-selection error instead of safe subset/no-op; cleanup incorrectly expected drop success after all candidates already applied. No product repair |
| 01:52:57 | Same task, single partial-apply case | Harness still expected `b.txt` absent rather than its correctly approved publication; failed. Terminal/drop cleanup succeeded |
| 01:53:18–01:53:22 | Same task, ten new cases | Six passed, four harness failures: expected literal “conflict” instead of precise native concurrent-change diagnostic, and dereferenced no-op `execution: null`. Corrected assertions to exact native semantics |
| 01:54:14–01:54:36 | `mise run test-agent-plugins`, full | **37 passed, 0 failed/skipped/cancelled**, 22.216s (35 inherited plus two new) |
| 01:54:36–01:54:59 | `mise run test-agent-plugins-native`, full | **25 passed, 1 failed**, 23.164s; all ten round4 cases passed. Existing round2 client-pipe-loss case failed at status sampling; details below. No blind rerun to erase this failure |
| 01:55:37 | `mise run test-agent-plugins`, scoped, final | **Two new portable cases passed**; Node also counted five unmatched file wrappers as passing, reporting seven overall. This is not seven exercised cases |
| 01:55:37–01:55:41 | `mise run test-agent-plugins-native`, scoped, final | **Ten new native cases passed**; two unmatched file wrappers counted by Node, reporting twelve overall. No round2 native cases re-executed |
| 01:55:41–01:55:58 | `git diff --check`; guarded owned-root reconciliation/process/mount scan/removal | Diff check passed; all retained documentation fixture Sessions terminal and explicitly dropped; owned root removed |

The final scoped run follows the final test assertion changes. The earlier
full adapter run preceded a wording/diagnostic assertion refinement in the two
new portable cases. No Rust/Python suites, compiler builds, lint, rustdoc or
platform matrix were run.

#### Preserved aggregate failure, not a round4 approval failure

The full suite failed in unchanged `plugins/test/disconnect.native.mjs:87`:

```js
const final = await f.until(async () => {
  const status = JSON.parse(ok(await reopened.operate("status", { selector })).stdout);
  // A status sampled during owner shutdown can be transiently unknown.
  // Wait for the receipt; do not use that earlier sample as authority for
  // a later operation, which independently reconciles native ownership.
  return !status.live && ["cancelled", "failed", "completed"].includes(status.state) ? status : false;
}, "native owner must reconcile after transport loss", 20_000);
```

Complete failing-test diagnostic payload (JSON pretty-printed, paths unchanged):

```text
DOC-PLUGINS-13 client pipe loss: recover a lost spawn response without rerun or acceptance (343.913663ms)
AssertionError [ERR_ASSERTION]:
{
  "selector": "/tmp/delta-terminal-.delta-fs-rb2Jwc/plugin-disconnect-QRkJUh/state/f0e7b106-147b-4847-9a59-11b22397276f/stage",
  "project": "/tmp/delta-terminal-.delta-fs-rb2Jwc/plugin-disconnect-QRkJUh/project",
  "stage": "/tmp/delta-terminal-.delta-fs-rb2Jwc/plugin-disconnect-QRkJUh/state/f0e7b106-147b-4847-9a59-11b22397276f/stage",
  "exit_code": 1,
  "signal": null,
  "interrupted": false,
  "stdout": "",
  "stderr": "Error: owning Session control endpoint unavailable\n",
  "truncated": false
}
1 !== 0
at ok (plugins/test/disconnect.native.mjs:10:10)
at plugins/test/disconnect.native.mjs:87:33
at async Object.until (plugins/test/disconnect-fixture.mjs:74:22)
at async TestContext.<anonymous> (plugins/test/disconnect.native.mjs:86:19)
at async Test.run (node:internal/test_runner/test:1409:7)
at async Test.processPendingSubtests (node:internal/test_runner/test:974:7)
generatedMessage: false, code: ERR_ASSERTION, actual: 1, expected: 0, operator: strictEqual
```

This is an unavailable control endpoint during owner shutdown, not a successfully
parsed unknown state. Round2's cleanup hook completed without a cleanup failure.
Parent was notified before any change to that area; none was made. The owning
worker must investigate harness-versus-product behavior separately. A passing
round4 filter does not supersede the **25/26 aggregate result**.

#### Resources, cleanup and gaps

- All recorded workload pre/post gates passed: MemAvailable
  19,756,699,648–19,994,103,808 bytes (18.40–18.62 GiB), workspace disk free
  345,173,458,944–345,225,367,552 bytes (~321.46–321.52 GiB), load1
  0.270–1.093. Final post-cleanup: 19,923,333,120 bytes available,
  345,199,153,152 bytes disk free, load1 0.912.
- New fixture registers teardown before attempting native run, discovers even a
  run that fails before returning its selector, and requires terminal/live=false
  status plus native drop before removal. Already-applied drop refusal is accepted
  only with explicit native applied state and zero changed files. Unknown or
  unresolvable ownership preserves the root with an error. Pending approval is
  rejected/settled before cleanup; approval readiness races operation settlement
  so early preview errors cannot leave a readiness-only wait.
- New fixture roots are bounded; private HOME/TMP is owned outside the project.
  Each successful teardown checks current-namespace mountinfo and current-UID
  process command lines for its exact root before removal. The full suite's
  output-directory option retained existing documentation fixtures; the final
  guarded cleanup separately reconciled all **13** native Sessions there
  (completed/failed/cancelled, never live), successfully dropped each, scanned
  owned processes/mounts and removed `.round4-owned`. No unrelated kill, cleanup,
  native private-state mutation, or uncertain-root recursive removal was used.
- Initial harness probes used tool-owned terminal temporary roots. Two
  already-applied fixtures were conservatively preserved by their failed cleanup;
  on the next terminal call those tool-lifetime roots were already absent, so no
  later explicit deletion was claimed for them. They were completed Sessions;
  the failure was refusing an impossible drop after all candidates were applied.
- Same compatible prebuilt CLI as round1, no rebuild/source provenance claim.
  Linux Host only. No cloud/model/credential use, host/network/system changes,
  installs, Git commit/push/merge, publication or semspec changes.
- Next scenario after investigating the inherited disconnect failure: two
  projects/tasks with overlapping relative filenames and simultaneous approvals,
  checking exact selector/project binding without a `last` fallback.

### Round 3 final follow-up validation

No workloads were launched while round4 owned the slot.

The coordinator subsequently released the sole slot for final follow-up checks.
The same exact-worktree mise trust/noninteractive settings, explicit serial
Node shim, `nice -n 10 timeout -k 30 120`, jobs2/threads1 and resource guards
were used. TMPDIR was an owned **durable worktree directory**,
`.round3-validation-20261108T015614/tmp`, so uncertain fixtures would not depend
on the lifetime of terminal scratch.

| UTC interval | Command/check | Result |
| --- | --- | --- |
| 01:56:27–01:56:49 | `mise run test-agent-plugins` | **39 passed**, 0 failed/skipped/cancelled; 22.290s, including both new early-preview-failure tests |
| 01:56:54–01:56:57 | Scoped `mise run test-agent-plugins-native` with the prior binary and `ROUND3_TEST_PATTERN='DOC-PLUGINS-14\|DOC-PLUGINS-15'` | Two approval-conflict tests failed only because the diagnostic matcher expected the word `conflict`; two refusal/recovery cases passed. Native-aware teardown safely removed all four case roots despite these assertion failures |
| 01:57:10–01:57:14 | Same scoped native task after correcting the diagnostic assertion | **Four actual approval cases passed**, 0 failed; 3.506s. Node's six passes include two empty unmatched file entries, not extra native scenarios; disconnect cases did not execute |
| 01:57:14–01:57:33 | `git diff --check`, owned processes/mounts/records scan and cleanup | Diff check passed, zero associated processes/mounts, no native task/run records or case roots remained; owned durable validation root removed |

The conflict assertion now checks the exact blocker, including the original
project's absolute `src.txt` path:
`overlay apply failed: target changed after staging at PATH; refusing to overwrite concurrent changes`.
It still requires original selector/paths/project, exactly one actual native
apply call, uninterrupted exit 3, structured `blocked` status and original
source stage. This is a stronger match to observed native conflict authority,
not acceptance of arbitrary errors.

All six workload resource checks passed: MemAvailable
19,684,761,600–19,927,715,840 bytes, disk free
345,143,517,184–345,222,062,080 bytes, load1 0.730–1.340. The first cleanup
inspection conservatively stopped on the owned Node compile-cache directory
instead of treating it as an unexpected native fixture; an explicit second
check confirmed only that cache and one retained portable fake-runtime HOME
remained, with no task/run records. Only those owned artifacts/root were then
removed. No global cleanup or unrelated processes were touched.

Sole TEST/BUILD slot released to the coordinator at **01:57:33 UTC**. No test
logic changed after the final passing checks; only this results entry changed.
The native source-provenance, upstream host-version alignment and mocked-UI
limitations above remain unchanged.

### Publication and review checkpoint

- Round 3's ownership/gate fixes passed static rereview and final execution.
  The diagnostic matcher was subsequently corrected to the exact observed
  concurrent-write blocker; no production changes were needed.
- Round 4's passing scenario evidence above predates static review findings:
  all native operations must be cancelled/joined before teardown, and portable
  approval waits must handle early settlement. Those fixture fixes are pending;
  round 4 coverage is not yet publication-ready.
- Round 5 is investigating the preserved native status failure with a
  deterministic source regression. No causal attribution to the old prebuilt
  binary is established.

#### Round 4 static-review follow-up — code ready, validation pending

Static review found two test-harness defects, not product defects. This
follow-up changes only the three round4 test/helper files and this log:

- Native fixture teardown previously waited only for the approval operation.
  Direct CLI operations had deadlines but were not fixture-cancelled/settled;
  the second plugin apply and final review were neither tracked nor given
  fixture signals. Teardown is now registered before fallible setup. Every
  native call, including capability resolution, spawn, adapter preview/apply,
  second-client apply, direct CLI and review, is registered before invocation
  with an immediate rejection handler. Signals combine fixture cancellation,
  test cancellation, caller cancellation and a per-call deadline. Teardown
  aborts the fixture, rejects approval and settles all registered work before
  terminal reconciliation/drop using fresh bounded signals. Failed setup
  removes a root only when native run was never attempted; an attempted run
  with no discoverable record preserves the root rather than guessing.
- Portable tests previously awaited only gate readiness after swallowing an
  early operation rejection. A shared approval-operation helper now races
  readiness with operation settlement, immediately observes both promises,
  and provides close/reject/settle cleanup. Portable tests use `finally` even
  on assertion failure. Added one deterministic early-preview failure case per
  host: readiness must reject with that failure, no approval may be requested,
  and no mutation may run. Native tests use the same readiness helper.

**No workloads were run for this follow-up:** round5 owns the sole Rust
test/build slot. Prior passing results describe the pre-review harness, not
this revision. Expected new portable coverage is four cases (two prior plus
two early-preview regressions); native coverage remains ten cases. Parent must
schedule validation after the slot becomes available. No production or
round1/round2/round3 files changed.

### Round 5 — native status loses owner discovery during cancellation

- Investigation began about 01:55 UTC after parent reported round4's aggregate
  native suite failure. Slot granted after round3 at 01:57:33; substantive work
  ended 02:04:39 UTC. This is an incremental repair, not eight hours elapsed.
- Original integration evidence supplied by parent: round4 full native task
  01:54:36–01:54:59, 25 passed/1 failed. DOC-PLUGINS-13 client pipe loss failed
  at `disconnect.native.mjs:87`, the strict status-result check, with exit 1,
  no signal/interruption/truncation, empty stdout, stderr exactly
  `Error: owning Session control endpoint unavailable\n`. The native cleanup
  after-hook did not fail. No rerun of that external-artifact failure was made.
- Read root/plugin context plus pvisor and pvisor-cli READMEs, relevant owner,
  status, registry, teardown and build/test-task code. No nested crate
  README/AGENT instructions were present. Scope remains one runtime status
  repair, not an API-boundary migration.

#### Source diagnosis, deterministic red and minimal repair

Status selected an `Arc<Owner>`, but filesystem sampling subsequently called
`control_ping(stage)` and `control_overlay_status(stage)`. Both independently
read the record and resolve local discovery. The actual Execution can unregister
its owner and clear `live_control` between those calls. The second lookup then
fails even though status still owns the exact selected owner.

Compared generic retry/error fallback (would hide authority failures), broader
record/lease serialization (unnecessary coordination), and using the existing
selected owner (chosen). Status now reads filesystem and optional activity
through that owner. Its existing `current_record` validation still checks exact
Session/Execution/stage identity. Offline and external owner routing are
unchanged; optional activity remains optional. No automatic retry, completion
invention, mutation authority or relaxed cancelled assertion was added.
Removed the now-unused private status-observation rediscovery helper; the
overlay rediscovery helper remains test-only for existing registry coverage.
No crate-root public API was removed.

A test-only, thread-local one-shot hook deterministically models discovery
teardown. In the red source the hook ran **after a successful ping and before
the second owner lookup**. It removed the exact registration and cleared its
recorded endpoint while status retained its selected owner. The original code
failed with the exact endpoint-unavailable diagnostic. In the repaired source
the same hook runs immediately before sampling through the retained owner.
Additional branches change Execution identity or corrupt the overlay's upper
record; both still fail rather than returning filesystem data.

This is synchronized native-runtime control-flow evidence using an owned
sleep process and synthetic overlay records, not a fully concurrent physical
owner teardown. It confirms the checkout bug independently of the earlier
external binary, whose source alignment remains unproven. Status fields are
sequential observations, not a newly promised atomic filesystem/lifecycle view.

#### Commands and actual outcomes

All workloads were serial, resource-guarded, `nice -n 10`, with two Cargo jobs,
one test thread, offline Cargo, no automatic tool installation, process-local
mise trust/noninteractive settings and `timeout -k 30 840` under an 870s tool
limit. No command approached that limit. Native tests used the explicit serial
Node shim, not ineffective argument forwarding.

| UTC | Exact command/check | Outcome |
| --- | --- | --- |
| 01:58:55–02:00:02 | `mise run test -- pvisor -- --lib -E 'test(status_retains_selected_owner_across_teardown_discovery_removal)'` | **Red: 1 failed**, 767 skipped; cold compile 66s; exact endpoint-unavailable error after deterministic ping/teardown/rediscovery ordering; owned process cancelled/reaped before assertion |
| 02:01:09–02:01:31 | `mise run test -- pvisor -- --lib -E 'test(status_retains_selected_owner_across_teardown_discovery_removal) \| test(local_queries_use_the_exact_owner_even_without_a_working_socket) \| test(runtime::session_service::tests::)'` | **20 passed**, 748 skipped; one unused private re-export warning subsequently removed |
| 02:02:46 | `mise exec -- rustfmt --edition 2024` on the four changed Rust files | Passed |
| 02:02:47–02:03:04 | `mise run test -- pvisor -- --lib -E 'test(runtime::instance_control::tests::) \| test(runtime::session_service::tests::) \| test(local_control_serves_shared_overlay_contracts_without_a_socket)'` | **44 passed**, 724 skipped, no warnings; nextest ID `d521b24e-6cf3-4742-89e8-f8a2f52f05d7` |
| 02:03:04–02:03:35 | `mise exec -- cargo build --locked --offline -p pvisor-cli --bin pvisor` | Passed, 31.41s; fresh checkout-local GNU Host CLI, not a packaged product/firmware build |
| 02:03:51–02:04:10 | `PVISOR_PLUGIN_BINARY="$PWD/target/debug/pvisor" mise run test-agent-plugins-native` | **16 passed**, zero failed/skipped/cancelled; 18.905s; real fresh-CLI stdio/native staging and strict cancellation assertions |
| 02:04:10–02:04:39 | `mise exec -- cargo clippy --locked --offline -p pvisor --lib --tests -- -D warnings` | Passed |

Final fresh-CLI faults: EOF/truncated EOF/client pipe loss cancelled; owned server
SIGKILL naturally completed after the bounded 12-second command. Retained
candidates remained reviewable; live mutations were refused. No adapter test
changes were needed, and no nonzero status results were suppressed.

#### Build provenance, resources, cleanup and remaining gaps

- Fresh binary path:
  `/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor`.
  SHA-256 `6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`.
  ELF x86-64 PIE, GNU/Linux dynamic loader, unstripped debug build. No external
  installed/debug CLI was overwritten.
- Tested source HEAD `c80b0eba298ef63cfc88b3147dd51db477f26319` plus uncommitted
  changes. SHA-256 of `git diff --` for the four runtime Rust paths:
  `c5745e80972a0d9397ff3b4ceedbce306193e9bfe755232d9c2ddd70bc725131`.
  The crate README addition documents the selected-owner read semantics.
- Every workload resource gate passed: MemAvailable
  19,121,278,976–20,409,352,192 bytes, disk free
  343,641,554,944–345,140,113,408 bytes, load1 0.891–2.213.
  Builds consumed under 2 GiB additional disk; no cache cleanup performed.
- Owned regression processes are cancelled and awaited before result assertions.
  Native fixtures use the reviewed terminal-reconciliation/drop cleanup; no
  unrelated process, mount, network or system changes were made.
- Workload slot released at 02:04:39 UTC for round4 follow-up validation.
  No commits, pushes, Git merges, publication, model calls, credentials,
  installs or semspec approval/claim changes.
- Full runtime/package/workspace tests were not run; targeted coverage was chosen
  rather than spending a known potentially hanging 900s full-runtime attempt.
  No VM boot, OCI, macOS, product packaging/firmware, rustdoc or Python coverage.
  The full native plugin suite available in this child has 16 cases, not the
  parent's later round3/round4 aggregate additions.
- Recommended next scenario: owner identity changes to a successor Execution
  while a read is in progress, checking strict refusal without stale mutation
  authority. Do not generalize this selected-owner read fix into retrying
  uncertain commands or preserving discovery after Execution completion.

Final audit at 02:05:58 UTC: `git diff --check` passed, no round5 regression or
disconnect fixture process/current-namespace mount remained. The process scan
also saw round4's newly authorized validation shell referencing the fresh binary;
it was not a round5 leak and was not signalled. Fresh artifact hash was unchanged
and binary/cache retained for that read-only reuse. No additional workloads ran
after the slot release.

#### Round 4 follow-up validation — reviewed harness, repaired native artifact

Parent granted the sole slot after round5 released it at 02:04:39 UTC. Static
rereview of the harness follow-up was clean. Validation ran 02:05:39–02:06:52;
the slot was explicitly released again at **02:06:52 UTC**. No source changes
were made after that rereview; this entry records the completed checks.

Used the round5 repaired CLI read-only, without rebuilding or overwriting:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
SHA-256: 6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

The hash was independently verified before testing. Artifact source provenance
supplied by parent: HEAD `c80b0eba298ef63cfc88b3147dd51db477f26319` plus four-file
Rust diff hash
`c5745e80972a0d9397ff3b4ceedbce306193e9bfe755232d9c2ddd70bc725131`.
This is a different artifact from the original round4 prebuilt binary.

All commands retained exact mise tasks and the explicit serial Node shim,
process-local exact-worktree mise trust/noninteractive/no-install settings,
nice 10, Cargo jobs 2, test threads 1, resource guards and
`timeout -k 30 120`. Native tasks set `PVISOR_PLUGIN_BINARY` to the path above
and `PVISOR_PLUGIN_TEST_OUTPUT_DIR="$PWD/.round4-owned"`.

| UTC | Command/check | Actual result |
| --- | --- | --- |
| 02:05:39 | `mise run test-agent-plugins`, shim adds `--test-name-pattern="concurrent approval"` | **4 exercised portable cases pass**; five unmatched file wrappers also counted by Node, nine overall; 0.208s |
| 02:05:39–02:05:43 | `mise run test-agent-plugins-native`, same filter | **10 exercised native cases pass**; two unmatched wrappers also counted, twelve overall; 4.030s |
| 02:05:53–02:06:15 | `mise run test-agent-plugins`, unfiltered | **39 passed**, zero failed/skipped/cancelled; 22.284s |
| 02:06:15–02:06:38 | `mise run test-agent-plugins-native`, unfiltered | **26 passed**, zero failed/skipped/cancelled; 23.278s. Client-pipe-loss case reconciled cancelled |
| 02:06:52 | Guarded reconciliation/drop, owned-root process/mount scan and removal; `git diff --check` | All passed |

This validates the reviewed harness revision, including both early-preview
failure regressions. The repaired artifact's 26/26 result is distinct from,
and does not erase, the earlier 25/26 failure and its preserved diagnostic.
No additional product repair or disconnect-test change was made by round4.

All workload guards passed: MemAvailable 19,116,789,760–19,473,637,376 bytes,
workspace disk free 343,297,925,120–343,306,694,656 bytes, load1 0.955–1.334.
Final cleanup post-check: 19,332,976,640 bytes available, 343,297,925,120 bytes
disk free, load1 1.0625. New fixtures joined their tracked operations and
reconciled their native Sessions before removal in both runs. The full suite's
retained documentation fixtures were separately reconciled: **13 terminal
Sessions, 13 successful explicit drops**, no owned process/current-namespace
mount remaining; `.round4-owned` removed. No uncertain root was deleted.

No installs, builds, Rust/Python suites, interactive clients, source mutation
outside the four round4 files, commits, pushes, merges or publication occurred.

### Round 6 — project windows, reopened adapters and exact task identity

- UTC: 2026-11-08 02:07:29–02:11:27 for recorded investigation and workloads; log and
  static handoff followed. Parent explicitly granted the sole TEST/BUILD slot
  after round4 release at 02:06:52. Released it at **02:11:27 UTC**; no waits
  were introduced to consume the remaining ~35-minute budget.
- Read root instructions/README/CONTRIBUTING, plugin README/CONTRIBUTING,
  this campaign and existing approval, disconnect and concurrent-approval
  fixtures. No nested plugin README/instructions existed. Did not edit
  concurrent-approval files or production code.

#### Real-user lifecycle and result

**No product defect reproduced; no production repair.** Added
`plugins/test/project-identity.native.mjs`: one bounded harness workload with two
tiny completed safe Host Sessions, separate projects, identical `shared.txt`
and `scratch.txt` names and one shared private state root. Reused the existing
`approvalHost`, `approvalOperation` and `removeAfterReconcile` helpers rather than
adding another host/gate implementation.

- Initial project is opened through a legitimate symlink; spawn records its
  canonical project. Fresh Backends reopened from the canonical cwd can recover
  status/review using each exact selector.
- Both projects reject the other's selector for all five operations, including
  drop. Absolute foreign paths and `..` paths are rejected before native apply.
  `last`, a project path and a non-normalized stage path are refused; no fallback.
- Pi and DSH approval callbacks pause after real native preview. Switching the
  active project and changing the caller-owned selector cannot redirect the old
  approval. Only the initiating project's `shared.txt` is published; the other
  project and unselected `scratch.txt` remain untouched. Native apply JSON
  identifies exactly the originating stage, one selected file and one apply.
- Repointing the user-facing project symlink to the second project while either
  host's approval is pending refuses the old task/project mismatch. It does not
  use the second project's matching relative filename. Restoring the symlink
  restores exact-selector access.
- Renaming the first project away while either approval is pending surfaces
  `ENOENT`, without publishing to the renamed project or active second project.
  Restoring the directory restores review through the original symlink.
- Renaming an owned, completed task directory or state root aside and replacing
  its old pathname with a symlink fails closed. Each original is restored in
  `finally`, and native status works again. No native staged content or record
  bytes were edited.

Added `plugins/test/project-identity.test.mjs` with **synthetic adapter records
only, no native stage**. It verifies wrong/string schema, mismatched UUID,
foreign/missing project, missing task, malformed JSON and missing record refusal;
all five operations reject identity mismatches before consulting an intentionally
missing runtime. Restoring the valid record permits a reopened Backend's task
lookup. This is adapter validation, not native or hostile-same-UID isolation.

#### Exact execution and evidence

All workloads used process-local `MISE_TRUSTED_CONFIG_PATHS="$PWD"`, `MISE_YES=1`,
`MISE_AUTO_INSTALL=0`, `CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=1` and
`NEXTEST_TEST_THREADS=1`, through `nice -n 10 timeout -k 30 120`.
An owned PATH shim preserved the exact mise tasks and invoked
`/usr/bin/node --test --test-concurrency=1 --test-timeout=120000` with a name
filter. Setup/native spawn deadlines were 15 seconds, individual operations
5 seconds, and the test 120 seconds. Every native run used the read-only binary:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
SHA256 6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

Its hash matched before and after; no build or artifact overwrite. This is the
parent's fresh round5-fix Host artifact, not a platform/source identity claim for
the older CLI.

| UTC | Exact task / filter | Actual exercised cases and outcome |
| --- | --- | --- |
| 02:09:32 | `mise run test-agent-plugins-native`, `project identity` | Initial native harness passed, two Sessions; Node reports five including four unmatched file wrappers |
| 02:10:44 | `mise run test-agent-plugins`, `project identity` | New synthetic-record case passed; Node reports eight including seven unmatched wrappers |
| 02:10:44–02:10:45 | `mise run test-agent-plugins-native`, `project identity` | Two-Session harness passed after adding exact native apply JSON and record-permission assertions |
| 02:11:26 | `mise run test-agent-plugins`, `project identity|an early preview failure|cleanup preserves fixtures` | **Four actual cases passed**: new record contract, two reused gate early-failure cases, reused uncertain-ownership cleanup case. Node reports nine including five unmatched wrappers |
| 02:11:26–02:11:27 | `mise run test-agent-plugins-native`, `project identity` | **One actual native case passed, two Sessions**, now including repointed project symlink refusals for both hosts. Node reports five including four unmatched wrappers |

No failed/red result was hidden or assertion weakened. Three native invocations
created six Sessions in total, always two within a single serial harness; each
invocation completed native disposal before the next workload.

#### Ownership, resources, cleanup and gaps

- After-hook registered before fallible setup. Entire setup and every run,
  approval operation and native call are tracked before invocation, with
  immediate rejection observation. Owned controller, test and deadline signals
  are combined. Existing gate helper races readiness against early settlement,
  releases in `finally`, and joins pending approval.
- Teardown aborts/joins all tracked work before reading records and requires
  every attempted run to have a matching recoverable UUID record, terminal
  completed/failed/cancelled status and `live: false`. Native drop must succeed;
  an already-applied refusal is allowed only with applied state and zero changed
  files. Current-UID `/proc` command lines and current-namespace mountinfo must
  not reference the fixture before recursive removal. Uncertainty preserves the
  fixture and reports its absolute path. Reused uncertain-cleanup/early-gate
  failure tests passed in the final portable check.
- Durable TMPDIR was the worktree-owned `.round6-owned/tmp`; private task HOME
  stayed outside either project. All three roots were removed by their guarded
  teardown; final `find` found no remaining fixtures. Native output is bounded
  by the shared supervisor; task output/fixtures were tiny.
- All workload resource gates passed. Available RAM
  19,299,966,976–19,690,741,760 bytes, disk free
  343,348,600,832–343,374,262,272 bytes, load1 0.660–1.030.
  Final post-workload: RAM 19,311,017,984 bytes, disk 343,362,146,304 bytes,
  load1 0.716. `git diff --check` passed.
- Linux safe Host only; Pi/DSH registration and approval are host callback
  contracts, not live interactive clients. No full adapter/native aggregate,
  Rust/Python suite, lint, rustdoc, VM/OCI/macOS or distribution check was run.
  These filtered results do not supersede earlier aggregate evidence.
- No commits, pushes, Git merges, publication, installs, host/network/system
  changes, cloud/model/credential calls, private staged-data edits or semspec
  changes. No new shared helper API was introduced.
- Next recommendation: incompatible/replaced runtime during a retained-task
  reopen, using process-local owned binary fixtures and explicit compatibility
  refusal rather than rebuilding/replacing this shared artifact. Keep real UI
  window/approval acceptance as a separate gap.
