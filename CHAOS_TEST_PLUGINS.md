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

### Round 7 — offline coding tools, Git isolation and missing workload output

- Actual UTC: 2026-11-08 02:12:17–02:19:09, with documentation/handoff
  immediately afterward. Sole TEST/BUILD slot granted by parent following
  round6's 02:11:27 release; explicitly released at **02:19:09 UTC**.
  This is a seven-minute incremental round, not eight hours elapsed.
- Read root AGENTS/README/CONTRIBUTING, this campaign log, plugin
  README/CONTRIBUTING and applicable fixture/adapter contracts. No nested
  plugin README/AGENT(S) existed. Existing approval, concurrent-approval and
  disconnect files were not edited; reused their approval and reconciliation
  helpers. No Rust changes, builds, installs or native artifact overwrite.

#### Confirmed bug, comparison and repair

Three real native coding tasks exposed empty workload output in plugin results:
Python unittest succeeded or failed with its correct native exit code, and Git
produced a valid diff/status, but the adapter returned no workload stdout/stderr.
For failed tests the only returned failure text was the generic native exit
diagnostic. Native status still correctly reported completed/failed and retained
the candidates. The bug was not test failure, HOME isolation or lost staging.

The adapter selected `--stdio capture`. Native CLI help and implementation define
that as capture into the Session record, not forwarding to CLI pipes. The
adapter supervisor was therefore capturing only CLI diagnostics. Compared
reading private run bundles, adding a new native output API and using native
inherit with the existing supervised pipes. Chose the last: `--stdio inherit`
lets the existing adapter supervisor capture bounded workload output without
duplicating native state or adding another runtime/public API.

The pipes remain noninteractive: stdin is ignored/EOF, stdout/stderr each retain
at most 256 KiB while draining, cancellation/deadline escalation is unchanged,
and no PTY or interactive input is exposed. Real Python tasks assert stdin EOF.
Documentation explicitly distinguishes adapter capture from native persisted
stdio: workload output is returned by the adapter, includes CLI diagnostics,
and is **not also persisted by native stdio capture**. Truncation must be checked.
The exact argv contract changed only `capture` to `inherit`; one portable
record-only-runtime regression verifies stdout, stderr and failure propagation.

#### Real user story and native coverage

Each case owns a tiny durable disposable project and starts exactly one fresh
Backend task; dependent edits/tests/Git commands stay in the same argv. No
continuation, `last`, automatic retry, install, network or model is used.

1. Repair Python `add`, run system Python's actual unittest, emit `__pycache__`
   and `test-results.log`, verify completed/nonlive native state and actual test
   output. Host source and generated-file absence remain unchanged before apply.
   Mock Pi approval accepts only `calc.py`; review still contains generated
   files; explicit drop removes the remaining candidates without undoing source.
2. Make an incorrect Python repair and run the same actual unittest. Require
   exit 1, `FAILED (failures=1)`, exact selector/project, failed/nonlive state,
   reviewable source/bytecode/log candidates and one task record. Explicit drop
   leaves original source unchanged; no approval or automatic rerun occurs.
3. Create a nested fixture-owned Git repository without a commit. Stage baseline
   source locally, then perform editor atomic save/replace, chmod 0755, rename,
   delete and bounded log generation inside one task. Real `git diff`, `status`
   and `git add -- calc.py` see candidate content. Post-add unstaged source diff
   is empty and cached diff contains repaired source. Host `.git/index` and
   `.git/config` remain byte-identical before apply and after drop. Mock DSH
   approval accepts source/mode and both rename paths; unaccepted deletion/log
   and candidate `.git/index` remain reviewable, then are dropped. Original
   unaccepted file survives. No host checkout Git metadata is touched.
4. A bounded failed Python tool emits 270,000 bytes on each stream plus UTF-8
   and NUL output, retains a diagnostic candidate and exits 2. Require the exact
   exit, no interruption, truncation true, 256 KiB per stream, UTF-8/NUL prefix
   and stderr marker. Review/drop preserves the unchanged original project.

Private HOME and all five HOME-relative XDG/CODEX variables are asserted in the
actual task. The initially selected pyenv Python under original HOME failed 127
under required safe filesystem controls; no safety flag was removed. System
`/usr/bin/python3` 3.15.2 works offline. Installed pytest exists only under the
original HOME's pyenv tree; pytest and `.pytest_cache` were **not exercised**.
Actual validation is unittest, bytecode and test logs, not pytest/test-cache
coverage. This environment limitation is documented rather than bypassed by
mounting the original HOME or copying/installing dependencies.

#### Exact workloads and actual results

Every invocation used the exact repository mise task, a process-local PATH shim
dispatching `/usr/bin/node --test --test-concurrency=1`, exact-worktree
`MISE_TRUSTED_CONFIG_PATHS="$PWD" MISE_YES=1 MISE_AUTO_INSTALL=0`,
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1`, and
`nice -n 10 timeout -k 30 120`. Setup/check commands had 15-second tool bounds.
Native task filters were `--test-name-pattern='coding task:'`.
TMPDIR and output root were the owned durable `$PWD/.round7-owned`, not
terminal scratch. Native Python was explicitly `/usr/bin/python3` after the
original-HOME interpreter limitation was observed.

| UTC | Exact command/check | Actual outcome |
| --- | --- | --- |
| 02:14:32–02:14:33 | Scoped `mise run test-agent-plugins-native`, original-HOME Python path | Three cases failed at exit 127 before Python started; no pytest/Git workload executed. All three native Sessions reconciled and dropped |
| 02:15:27–02:15:28 | Scoped native task, system Python/unittest | Three failures: real task exits were correct but workload output was absent; fixture teardown passed |
| 02:16:35–02:16:37 | Same scoped native task with native status diagnostic | Three output failures again; status proved native completed/failed and retained source/cache/log candidates. This was diagnostic reproduction, not an automatic task retry |
| 02:17:29–02:17:31 | Same scoped native task after stdio repair | Three actual cases passed; four unmatched file wrappers also counted, seven Node passes overall |
| 02:18:24–02:18:46 | Unfiltered `mise run test-agent-plugins` | **46 actual portable cases passed**, zero failed/skipped/cancelled, 22.425s; includes the new output regression and existing dual-stream drain/cancellation tests |
| 02:18:58–02:19:00 | Scoped `mise run test-agent-plugins-native`, final source including EOF/binary-output assertions | **Four actual native cases passed**, zero failed; four unmatched wrappers, eight Node passes overall, 1.845s |
| 02:19:00–02:19:09 | `git diff --check`, artifact hash, owned authority/process/mount scan and removal | Passed; hash unchanged; no case roots/task/run records/processes/mounts remained; owned validation root removed |

The diagnostic invocation redirected bounded output to an owned log then used
`cat`; its shell's final status was zero from `cat`, while the contained mise
task explicitly failed. It is recorded as **three failures**, not a passing
command. Initial test assertions expected forwarded workload output; those
assertions were retained and pass after the production fix, not weakened.

#### Guards, ownership and gaps

- All 12 pre/post workload resource gates passed: available RAM
  18,697,342,976–19,380,776,960 bytes; workspace free disk
  343,175,479,296–343,310,438,400 bytes; load1 0.708–1.813.
  Gates required >=6 GiB RAM, >=20 GiB disk and load1 <=8. No simultaneous
  workload invocations or more than one active native Session were used.
- The new focused coding fixture registers its after-hook before setup and
  tracks setup, every attempted spawn and normal native operation before
  invocation with immediate rejection observation. Signals combine owned,
  test, caller and deadlines. Approval waits reuse the bounded
  readiness-versus-settlement helper and always close/reject/join in `finally`.
  Teardown aborts and joins owned work before fresh bounded native status/drop.
  Missing/unknown/live authority preserves the durable root with an error.
  Already-applied disposal requires explicit applied state and zero changes.
- All **16 attempted native Sessions** across these invocations were terminal,
  nonlive and explicitly dropped before root removal, including failed
  interpreter/task/assertion paths. Per-case current-UID process/current-mount
  scans passed. Final validation-root scan found no native authority; residual
  Node compile cache and two portable fake-runtime private HOMEs were removed
  only inside that root. No unrelated kill/global cleanup was performed.
- Real native files/Git/Python/output were tested through Backend; Pi/DSH
  registration and approvals are mocked callback contracts, not interactive
  clients. No MCP-specific rerun, full native baseline, Rust/workspace/Python
  project suite, compiler, rustdoc, macOS/VM/OCI or network test ran.
- Reused read-only repaired CLI:
  `/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor`,
  SHA-256 `6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`,
  verified before and after. No artifact provenance beyond parent's round5
  record is newly claimed.
- No commits, pushes, Git merges, publication, credentials, cloud/paid calls,
  host/network/system changes or semspec approval/claim edits.
- Next scene: real stdio MCP delivery of a failed compiler/test diagnostic
  under adapter output truncation and disconnect, without interpreting a
  truncated result as complete. Separately test pytest when a compatible
  system/project-local installation is available offline; do not relax HOME.

### Round 8 — reported preview conflict at an existing parent

- Investigation and fixtures only; no production behavior change. Static work
  preceded the sole TEST/BUILD grant; workloads ran 2026-11-08
  02:24:41–02:25:16 UTC and the slot was released immediately afterward.
  Read root README/AGENTS and overlay-core README; discovery found no additional
  ancestor instructions/READMEs for the edited source.
- The reported schema1 preview blocked/exit3, empty selected/required and null
  execution, named `experiments/pvisor-case` while the candidate was modified
  `experiments/pvisor-case/scratch.txt` (14 bytes, mode0644). This identifies
  the failed conflict fence, not the external change that caused it. The pasted
  JSON was truncated and exact selection paths remain unknown. Neither the
  user's Pulsing project nor its retained stage was accessed or modified.

#### Source evidence and bounded reproduction

`core.rs::ensure_upper_parents` records existing parents before importing them
for child copy-up. `apply.rs::prepare_apply_preimages` retains those recorded
ancestors as read-only fences without selecting their metadata.
`validate_target_preimages` compares the complete directory fingerprint during
ordinary preview/apply; only recovery has a timestamp-tolerant exception.
This agrees with the existing overlay-core README contract, not an accidental
adapter selection expansion.

Added one Linux Core fixture matrix, 36 scenarios: legacy/compact journals ×
file-specific/directory-prefix selectors × nine external mutations. Each stages
an existing nested scratch file, verifies an initially successful inert preview,
then mutates only owned disposable host data after first observation. A fixed
past parent mtime avoids timing sleeps or host clock changes.

- Sibling file creation/removal and subdirectory creation reproduce the exact
  `target changed after staging at .../experiments/pvisor-case; refusing to
  overwrite concurrent changes` blocker. Parent dev/inode are unchanged; its
  fingerprint differs only by timestamp. Normal execution also refuses.
- Selected-file content edits and parent mode/user-xattr edits refuse. Parent
  replacement/symlink rebinding also refuse; these namespace operations change
  the containing `experiments` ancestor, which validation encounters first.
  The matrix does not establish a persisted inode-identity fence.
- Changing only an existing unselected sibling's contents succeeds in preview;
  it does not change the parent's mtime and is not overwritten. This successful
  branch is preview-only, not an additional execution workflow.
- Both selectors initially request only the file with no implicit dependencies.
  Every preview is checked for preservation; failed execution retains host
  bytes/types/modes/mtimes, parent fingerprint including ownership/xattrs,
  symlink target, original journal and staged bytes, with no pending apply.

The reported symptom is reproduced, but **the real user's cause is unknown**.
Editor cache/temp/lock creation after first touch is sufficient; legitimate
directory metadata changes or rebinding can also explain a parent blocker.
Host-side task effects could trigger the same fence if they occur after its
baseline was captured; no such effect was observed on the user's machine.
No repair red/green is claimed: the new expected-behavior matrix passes against
unchanged production code.

#### Decision, commands, resources and gaps

Do not globally reuse the recovery matcher. Directory `PathFingerprint` records
mode/UID/GID/mtime/xattrs, not persistent dev/inode identity or child inventory.
Ignoring ancestor timestamps would discard a conservative rebinding signal.
Recommendation: keep current behavior pending user confirmation of exact paths
and relevant host/editor actions. A less conservative policy would need a
separately scoped design for structural-only ancestor versus selected directory
or recursive replacement validation, persisted identity/legacy-stage treatment,
and publication-time checks. This is larger than this bounded investigation;
no schema migration or contract weakening was attempted.

All workloads used no-install/noninteractive mise settings, offline Cargo,
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1`, nice10 and finite
`timeout -k 30` guards (840s for tests, 120s for format).

| UTC | Command | Result |
| --- | --- | --- |
| 02:24:41–02:24:51 | `mise run test -- pvisor-overlay-core -- --lib -E 'test(preview_existing_parent_conflicts_preserve_host_and_stage) \| test(existing_parent_recovery_tolerates_child_mtime_but_not_permission_changes) \| test(selected_file_requires_new_ancestors_but_not_existing_directory_metadata)'` | 3 passed, 177 filtered; includes 36 new matrix scenarios |
| 02:25:09–02:25:16 | `mise run test -- pvisor-overlay-core` | 243 passed, 5 skipped across 12 binaries |
| Same final guarded interval | `mise exec -- cargo fmt -p pvisor-overlay-core -- --check` | Passed |

An earlier formatting check found only new-test layout differences; corrected
before tests. Resource boundary observations: available RAM
18,784,894,976–19,375,087,616 bytes, disk free
343,152,738,304–343,175,110,656 bytes, load1 0.655–0.876; all gates passed.
Owned TMPDIR `.round8-owned/tmp` was empty after tests; both empty directories
were removed. No native Sessions/mounts were started and no shared CLI artifact
was overwritten. This is Core filesystem evidence, not a fresh Host/plugin UI
reproduction or a running-task concurrency trace. No Clippy, doctests, rustdoc,
Python, adapter, VM/OCI, non-Linux or workspace-wide checks were run. No installs,
network/cloud/credential calls, unrelated kills/global cleanup, semspec changes,
commits, pushes, Git merges or publication.

### Round 9 — runtime upgrades with a retained coding task

- Static inspection/editing preceded the parent's explicit sole TEST/BUILD slot
grant after integration release at 2026-11-08 02:29:31 UTC. Workloads ran
**02:30:14–02:30:46 UTC**; ownership checks completed and the slot was explicitly
released at **02:31:14 UTC**. Documentation/handoff followed. No artificial
waiting or eight-hour completion claim.
- Read root README/AGENTS/CONTRIBUTING, this campaign log, plugin
README/CONTRIBUTING and existing runtime, approval, coding and project-identity
fixtures. Discovery found no additional nested plugin README/AGENT(S).
Notified parent about possible runtime/backend overlap before edits; only
`plugins/test/runtime-upgrade.native.mjs` and this log are delivered.
Summary/backend/runtime/distribution code and round8 semantics were not changed.

#### User story and negative finding

**No product defect reproduced; no production repair or red/green claim.**
One real, tiny, safe Host task writes a retained candidate using the immutable
compatible native artifact. Fresh and already-open Backends then reopen its exact
selector through an owned package-manager-style runtime symlink with spaces.
Only that owned symlink is switched, never the shared CLI, any host executable,
native task records or staged private data.

- Switch to an incompatible executable, partially emitted `--help` followed by
exit 2, or missing symlink target. Doctor and all five task actions
(status/review/preview/apply/drop) refuse. The invocation log establishes one
failed spawn-help probe per action (zero for a missing target), no actual
operation, fallback or retry. Existing task-record bytes and task count remain
unchanged, and the candidate is not published.
- Restore the compatible target after each refusal and review the same candidate
through the exact selector. Neither upgrade nor reopen creates another task.
- A separate owned wrapper passes native help/version probes but exits 73 on
the actual operation. Doctor succeeds while review truthfully returns 73 and
the wrapper's failure diagnostic, retaining the original selector. Doctor
checks capabilities; it does not guarantee future operational readiness.
- For both Pi and DSH callback contracts, pause after a real native preview,
switch the runtime symlink to incompatible, then approve. Revalidation refuses
before native apply: exactly one failed help probe, no host publication.
Finally release/join each gate and restore the compatible target.
- Reject invalid/empty/relative/missing explicit overrides. Existing portable
cases additionally cover missing desktop-style PATH, ignored relative PATH
entries, compatible Homebrew/venv links, no fallback after incompatibility,
pre-abort, EOF, active cancellation, partial stalled help and the actual
ten-second probe timeout with SIGTERM escalation.
- Final compatible status is completed/nonlive; review still contains the
original candidate, and explicit drop succeeds. Shared fixture teardown
independently reconciles status/drop before removing the durable root.

#### Exact commands, identity and results

Both workloads ran the exact repository mise task through
`nice -n 10 timeout -k 30 120`, with process-local
`MISE_TRUSTED_CONFIG_PATHS="$PWD" MISE_YES=1 MISE_AUTO_INSTALL=0`,
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1`.
An owned PATH shim converted the task's `node --test` invocation to
`/usr/bin/node --test --test-concurrency=1 --test-timeout=120000
--test-name-pattern="$ROUND9_FILTER"` and retained all original file arguments.
TMPDIR and fixture output directory were `$PWD/.round9-owned/tmp`.

| UTC | Exact task and filter | Actual outcome |
| --- | --- | --- |
| 02:30:14–02:30:15 | `mise run test-agent-plugins-native`, `runtime upgrade:` | **1 actual native case passed**, one Session; 7 unmatched file wrappers, 8 Node passes, 0 failed/cancelled/skipped |
| 02:30:24–02:30:46 | `mise run test-agent-plugins`, `PATH\|runtime\|capability\|CLI probes\|operator override` | **17 actual cases passed**: 11 distribution/runtime, 4 adapter runtime/cancellation, 1 task identity, 1 unintended packed-install case; 5 unmatched wrappers, 22 Node passes, 0 failed/cancelled/skipped; 21.785s |
| 02:31:04–02:31:14 | `git diff --check`, source/artifact hashes, owned process/mount/authority scans | Passed; hashes unchanged; no remaining native/installation fixture, task/run record, owned process or mount |

Source baseline was HEAD `769a6e9c41cf072ecf05d925b1b77f9b8b3686e7`
plus the parent's inherited, uncommitted integrated summary/review changes.
Relevant source SHA-256 before and after:

```text
runtime.mjs  3f8592f498925789126b4ca0058d113e0079361b54dc5b0b9bd60d480468a841
backend.mjs  33728e618812d89b43a3c61fd5ac252a49ec444f2ac881366aa79251d7fec772
process.mjs  9be8a7ab94a32d5aab4ca488ba729f91668559c1029a1369c964d8355a1a19f9
summary.mjs  ddc57ac80dd3e1d6daa894e7d79ceedee626d4407432c764a372d2d045fa9516
runtime-upgrade.native.mjs  98a8989e7c27e22d665b7428322f01f71f7bcc6f5c161977fc64395239e17063
```

Read-only native artifact, unchanged before/after, not rebuilt or overwritten:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
SHA256 6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

This inherits the parent's round5-fix artifact provenance; it does not establish
that the artifact was built from the current uncommitted plugin source.

#### Scope deviation, resources, cleanup and gaps

The broad portable name filter unintentionally selected
`DOC-PLUGINS-07: packed plugin uses a separately installed runtime and uninstall
preserves it`. **This violated the requested no-installs/no-network scope**:
the existing fixture packs source and runs npm install/uninstall with `--global`,
but explicitly sets `npm_config_prefix` to its owned `plugin-install-*/prefix`,
cache to the same root's `cache`, blank userconfig, ignored scripts and disabled
audit/funding. Its registry is an ephemeral `127.0.0.1` server serving the owned
tarball. Thus `--global` targets only the disposable prefix, not host-global
installation. Source inspection found no external registry configured or
external fallback; this is not packet-level proof of no external traffic.
Reported immediately to parent; no rerun, further install/network operation,
host uninstall or global/cache cleanup followed. Successful fixture after-hooks
closed MCP, closed all registry connections/server, and removed its owned root.
Future filters should name exact cases or explicitly exclude this install case.

- All four workload resource gates passed: MemAvailable
18,823,528,448–19,025,223,680 bytes; free disk
342,878,924,800–342,918,610,944 bytes; load1 1.122–1.551.
- Reused `codingFixture` registers teardown before setup, tracks operations
before invocation with immediate rejection observation and combines owned,
test/caller and deadline signals. Gate waits race early settlement/readiness
and release/join in `finally`. Teardown aborts/joins before native terminal,
nonlive status plus successful drop (or explicit applied-empty disposal).
Uncertain reconciliation preserves the absolute durable root.
- Per-case and final current-UID `/proc`/current mount-namespace scans found no
owned processes or mounts. No task/run records or fixture roots remained;
only the owned Node compile cache remained and was removed after the scans.
Temporary shim/guard files were removed; no unrelated cleanup or kills.
- Linux native Host only. Pi/DSH approvals are mocked host callback contracts,
not live UI. Actual Homebrew/pip upgrades and desktop applications were not
launched. No running user's binary was replaced, no atomic binding across an
independently upgraded runtime is claimed, and no same-UID security claim.
- No new native case for a runtime exiting before creating its native record;
portable missing-runtime/cancelled-probe cases retain adapter locators without
a spawn. An absent native stage still requires reconciliation, not automatic
retry or deletion. Existing timeout coverage is portable, not a stalled native
task. Runtime disappearance between a completed probe and exec is not
deterministically injected here; explicit refusal is an acceptable contract.
- No full native/portable aggregate, Rust/Python suite, build, lint, rustdoc,
VM/OCI/macOS or semspec approval was run. No commits, pushes, Git merges,
publication, credential/cloud calls, shared binary edits or host-system changes.
- Next scene: an owned compatible wrapper completes its help probe then exits
before native stage creation; preserve the exact adapter locator, distinguish
absent native authority from terminal disposal, and explicitly restore/review
a separate previously retained task. Keep native authority and fixture cleanup
evidence separate; do not forge records or infer safe retry from absence.

### Round 10 — six-minute watch output, cancellation/deadline and paused summary approval

- Actual UTC: 2026-11-08 02:32:04–03:00:59 (about 29 minutes, including real
  waits and one interrupted harness attempt). Sole TEST/BUILD slot granted
  after round9's 02:31:14 release; released after the final ownership audit.
- No production bug confirmed and no production implementation changed in this
  round. Added the opt-in `plugins/test/long-lived.native.mjs`, a narrowly
  configurable run deadline/caller signal in the existing coding fixture, and
  `mise run test-agent-plugins-long-lived`; documented the opt-in in the plugin
  README. The ordinary native suite skips both long cases unless explicitly
  enabled, so ordinary CI does not gain eighteen minutes.
- Read-only binary throughout:
  `/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor`,
  SHA256 `6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`
  (checked before first run and before corrected deadline run). Source included
  the parent's uncommitted output-inherit and reviewed summary implementation;
  this round did not rebuild, overwrite or publish that artifact.

#### Real elapsed phases and exact outcomes

| UTC | Scenario | Actual result |
| --- | --- | --- |
| 02:33:47–02:42:47 | Cancellation plus pending Pi approval | 360,065 ms active; caller cancellation produced exit143, signal=null, interrupted=true; native state cancelled/live=false. Then 180,086 ms genuine paused approval idle; selective accept and drop passed. |
| 02:42:47–02:47:57 | Unintended second case in first harness | About 309,414 ms active before outer850s harness interruption; **not** a runtime deadline pass. Exact-selector cancelled/live=false/finished receipt reconciled at02:48:44, owned processes/mounts absent, native drop succeeded before retry. |
| 02:48:58–02:57:59 | Correctly scoped runtime deadline plus approval | 360,176 ms active; exit1, signal=null, interrupted=false, explicit `DeadlineExceeded: execution exceeded 360000 ms deadline`, native failed/live=false. Then 180,085 ms genuine paused approval idle; selective accept and drop passed. |

Three native task attempts total, never more than one Session at a time.
Each task had one bounded Python parent and one bounded stdout-inheriting Python
descendant; no spinning, external network, credentials or stress workload.
The parent wrote source, a staged preflight unittest result explicitly saying
the watch phase was unfinished, and disposable scratch, then emitted minute
heartbeats during bounded kernel sleeps. The descendant retained stdout for at
most500s, with an owned token in its command line. Both completed scenarios
retained start/descendant/five heartbeat records and unittest stderr without
truncation; neither falsely reported watch completion.

For each completed scenario, native live apply and drop refused; terminal status
and review used the exact original selector. A reopened Backend and real Pi
adapter with a mock host approval callback showed the structured summary's two
selected entries and one excluded scratch entry. No project write occurred
during three minutes of approval idle. Approval accepted only source and the
partial result; remaining scratch was reviewed and dropped. One task record per
fixture proved no automatic rerun. This models adapter reopen and paused review,
not an actual GUI process closure/restart.

Runtime limits were540s for caller cancellation and360s for the deadline case,
both below the plugin600s maximum. Adapter supervisor deadlines are runtime+30s;
fixture run timeout650s, test timeout700s, outer harness850s plus30s kill grace.
The first harness's850s interruption was distinct from all of these task states.

#### Commands, harness corrections and validation

- Initial mise invocation failed closed because this isolated worktree config
  was untrusted; subsequent commands used process-local
  `MISE_TRUSTED_CONFIG_PATHS="$PWD"`, not persistent trust changes.
- First command: `mise run test-agent-plugins-long-lived -- --test-name-pattern='long-lived cancel'`
  under `nice -n 10 timeout -k 30s 850s`. The initial inline task's `"$@"` did
  not forward the filter, so cancellation passed and deadline unintentionally
  started. Corrected the task to pass an explicit environment-selected Node
  name filter rather than rely on trailing arguments.
- Corrected command:
  `PVISOR_PLUGIN_LONG_LIVED=1 PVISOR_PLUGIN_LONG_LIVED_CASE='long-lived deadline' mise run test-agent-plugins-long-lived`
  under the same guard: **1 passed**, no extra case. Both long invocations set
  the supplied binary, owned TMPDIR/output under `.asv8/round10`,
  `CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1`.
- `mise run test-agent-plugins-long-lived` without opt-in: **1 skipped**, no
  native task. `node --check plugins/test/long-lived.native.mjs` and
  `git diff --check`: passed. Final exact exit/state/deadline assertions were
  tightened after the real runs and replayed against the retained JSON logs,
  not claimed as another long native execution.
- **Scope deviation:** the final `mise run test-agent-plugins` passed65/65 but
  included prohibited `DOC-PLUGINS-07` npm packed-install fixture. That existing
  test uses a disposable private npm prefix/cache/config and loopback registry,
  with install scripts/audit/fund disabled and fixture cleanup registered.
  It does not install into the host's actual global prefix. No external traffic
  audit was performed, so no independent “zero external traffic” proof is
  claimed. The broad portable task also used its normal file-level concurrency,
  unlike the serial long-lived task. This extra run was a mistake; it was not
  repeated. No toolchain installation, package publication or host-global
  cleanup was performed.

#### Ownership, resources and handoff

Early fixture hooks own every attempted run and operation with immediate
rejection handlers, combined fixture/test/caller/deadline signals, and joined
teardown. Approval readiness races early settlement; finally releases/cancels
and joins. Cleanup requires terminal nonlive status and successful drop (or
known applied-empty), then scans owned same-user command lines and mounts before
removing roots. The printed descendant PID3 is namespace-local: checking host
`/proc/3` would prove nothing. The corrected check scans host `/proc` for the
explicit inherited owned-root token; the existing fixture cleanup did that
already for the first completed cancellation case. Pipe closure was not used as
proof of descendant teardown.

Minute resource samples during real phases plus boundary checks stayed above
6GiB available/20GiB free and below load1=8: available RAM about
19.0–19.2 billion bytes, disk about342.9–343.0 billion bytes, load1 about0.58–1.66.
Final02:59:49 audit: available19,161,055,232 bytes, free342,930,722,816 bytes,
load1=0.740234375, zero owned live command lines and zero owned mounts.
All round10 logs/retained fixtures totaled119,667 bytes. Successful fixtures
`plugin-coding-task-yURbTs` and `plugin-coding-task-6diLC4` were removed after
reconcile/drop/process/mount checks. The interrupted but now **known terminal
and dropped** fixture remains for audit at
`/home/reiase/workspace/pvisor/.delta/worktrees/15x8wg2snasz/pvisor/.asv8/round10/evidence/plugin-coding-task-LWSEGh`;
it is not an unknown-live root. Durable runner logs are
`.asv8/round10/{cancel,deadline,default-skip,portable}.log`.

No commits, pushes, Git merges, publication or semspec changes. No further
workload started after the final audit. Next useful scenario: real host teardown
while approval is pending, followed by explicit recovery of the retained output
and summary from a fresh host; distinguish in-memory adapter output from
persisted native output (inherit does not also populate native capture).
Parent should statically review the opt-in fixture lifecycle before integrating
the four focused source/documentation files and this log; no production repair
is offered for PR121 in this round.

### Round 11 — coding-client stops reading MCP diagnostic replies

- Actual UTC: static preparation 03:00:32–03:02:20; sole TEST/BUILD slot granted
  by coordinator after round10's 03:00:59 release. Workloads
  03:02:33–03:07:45, final static/resource/ownership audit 03:06–03:08.
  This is a bounded round in the requested 01:04–09:04 campaign, not evidence
  of eight hours of testing.
- User story: a coding-agent UI stops reading replies while diagnostic calls
  finish and more requests arrive, then resumes, cancels or closes. No host
  pressure, network service, install, cloud call or credential was needed.
- Read root instructions/README, plugin README, campaign history and current
  MCP/backend/process/summary implementation. Previous summary and disconnect
  regression helpers were not changed. Source baseline: HEAD
  `14f226b47c31e459a803fe5b7611854f0659cb87`.

#### Confirmed queue rule and minimal repair

The existing 32-request admission limit covers execution only: `send()` writes
without observing backpressure and `finally` immediately removes the completed
ID. The 1 MiB incoming-frame limit and subprocess capture limit of 256 KiB per
stream do not bound completed replies accumulated in stdout's writable queue.
Parse errors, duplicate-ID and admission-error responses also use that queue.
This is a source-confirmed missing aggregate bound, not a claim that transient
buffering proves a universal memory leak.

Compared alternatives with the coordinator before implementation:
pausing stdin until drain delays cancellation/EOF, while async per-send waits
need coordinated admission/error queues and changed ID lifetime to avoid merely
moving the unbounded queue. Chosen rule: a conservative internal 8 MiB serialized
UTF-8 writable-backlog budget, counting `Buffer` bytes after JSON escaping.
Every response uses this rule. On overload, emit one bounded stderr diagnostic
(not another queued JSON-RPC error), stop input and abort all active operations.
Allow one second for queued replies to flush; then force output termination,
with explicit exit only after all active operations' `finally` blocks have
settled and their supervisors have returned. Request-ID/cancellation semantics,
incoming-frame limits and public tool authority are unchanged.

A forced closure may leave a partial final frame or undelivered completion.
That is an uncertain outcome requiring exact-selector reconciliation, never an
automatic rerun, acceptance or disposal. README documents both this limit and
that it does not bound total RSS, native work or client buffers.

#### Deterministic evidence and exact commands

New `plugins/test/transport.test.mjs` uses a fake CLI but the real MCP server,
real child processes and OS pipes. It never claims native pipeline coverage.
Finite diagnostics are four at a time, at most 36 × 256 KiB: 9 MiB raw plus
approximately 9 MiB serialized reply generation, not unlimited sends or memory
pressure. The escaped-NUL case uses six diagnostics with under 13 MiB combined
raw/serialized generation. Run the two overload cases separately to keep each
workload under 20 MiB fixture generation. The ordinary resume/cancel/EOF/EPIPE
cases each use only one diagnostic. Exact complete newline JSON-RPC frames,
unique IDs, cancellation, single task/no retry and explicit non-successful
overload exit are asserted. Forced-close testing permits an incomplete final
frame but still validates every complete frame.

All task invocations used an owned explicit PATH shim:

```sh
# node shim, preserving the task's file arguments:
/usr/bin/node --test --test-concurrency=1 --test-timeout=120000 \
  --test-name-pattern="$ROUND11_FILTER" "$@"

# Common task invocation (guarded before and after):
env PATH="$PWD/.round11-owned/bin:$PATH" TMPDIR="$PWD/.round11-owned/tmp" \
  MISE_TRUSTED_CONFIG_PATHS="$PWD" MISE_YES=1 MISE_AUTO_INSTALL=0 \
  CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1 \
  nice -n 10 timeout -k 30 120 mise run TASK
```

| UTC | Exact task / name filter | Actual outcome |
| --- | --- | --- |
| 03:02:33–03:02:35 | `test-agent-plugins`; `^transport: stopped reader` on original MCP | **Red**, one actual case failed the missing-overload assertion after all 36 finite diagnostics; 9 unmatched wrappers, cleanup passed |
| 03:02:58–03:03:00 | `test-agent-plugins`; `^transport:` before adding forced-close case | 4 actual cases passed, 9 wrappers; overload after 32 diagnostics, resumed framing/cancellation/EOF/EPIPE passed |
| 03:04:14–03:04:29 | `test-agent-plugins`; `^transport: escaped-byte` | **Failure retained:** new case timed out at 15s. `process.stdout.destroy()` alone leaves Node's pending native stdio writes able to hold process exit. Owned cleanup resumed output/joined processes and removed fixture |
| 03:04:55–03:04:57 | same escaped-byte task/filter, after explicit post-settlement exit repair | 1 actual case passed in 1.138s, 9 wrappers; stopped reader remained stopped until exit |
| 03:05:05–03:05:06 | `test-agent-plugins-native`; `^coding task: Python unittest failure retains candidates without retry$` | 1 actual native case / 1 tiny Session passed, 7 unmatched wrappers |
| 03:05:40–03:05:42 | `test-agent-plugins`; `^(transport: (?!escaped-byte)\|MCP \|lost MCP output\|shared model tool)` | 8 actual cases passed, 7 wrappers |
| 03:06:13–03:06:15 | same portable task/filter, after strengthening EOF/EPIPE to pause output after a diagnostic | **Final 8 actual cases passed**, 7 wrappers; 0 failed/cancelled |
| 03:07:43–03:07:45 | `test-agent-plugins`; `^transport: escaped-byte`, strengthened after coordinator review | **Final 1 actual case passed** in 2.000s, 9 wrappers; fake acknowledges cancellation but delays exit 1.8s, MCP remains alive beyond 1s flush deadline and exits 1 only after fake exit |

The table's `\|` denotes regex alternation as passed in the shell filter.
The full portable task's packed npm install case `DOC-PLUGINS-07` was excluded
by these precise names; no loopback registry, installs or external network.
No full portable aggregate was run. Final portable and separate escaped-byte
checks cover all five new cases, plus existing MCP protocol/authority and lost
output cancellation cases.

The final forced-close case keeps stdout paused until MCP exits. At 1.25s after
overload it explicitly checks that MCP is still alive and the cancelled fake task
has not exited; only after that task's exit record may MCP exit with code 1.
This covers both unconditional premature timer exit and a missing post-settlement
exit trigger, rather than merely proving that idle finite diagnostics shut down.

The native check is deliberately separate: existing offline Python unittest
failure retained candidates without retry or host acceptance, reported terminal
failed/nonlive state, reviewed and dropped the one owned Session. It used the
existing pre-setup ownership hook and abort/join/native-terminal/drop cleanup.
It is native backend lifecycle evidence, **not a native slow-consumer test**.
Read-only binary before/after SHA-256, with no rebuild or overwrite:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

#### Resources, cleanup and limits

- Every pre/post workload gate passed: MemAvailable
  19,084,685,312–19,186,778,112 bytes; free disk
  342,780,678,144–342,957,576,192 bytes; load1 0.882–1.460.
- New fixtures register teardown before setup, immediately observe server
  settlement/errors, cap retained stdout at 18 MiB, pending frames at 2 MiB and
  stderr at 8 KiB. Readiness waits have eight-second deadlines and stop on server
  exit. Fake subprocesses record start/exit, have a finite lifetime and are
  supervised by the actual adapter; unknown outstanding fake ownership preserves
  the fixture instead of deleting it. Teardown closes input/resumes output,
  bounds the join and checks all fake starts have exits before root removal.
- Final current-UID `/proc` and mount-namespace scans found no process or mount
  referring to the owned round11 root; durable fixture tmp directory was empty,
  including after the retained timeout failure. Native fixture cleanup passed.
  Temporary runner/shim/log files were removed after evidence transcription.
  `git diff --check` passed.
- Not measured: total RSS ceilings, arbitrary client kernel buffering, Windows,
  macOS, VM/OCI, real UI behavior, native slow-consumer cancellation or complete
  delivery after forced output close. Serialized backlog is bounded, but one
  frame's serialization and existing bounded execution captures are separate
  transient allocations. Joining supervised CLI operations does not certify
  descendant/native teardown after forced CLI termination.
- No Rust/Python aggregate, build, lint, rustdoc, semspec changes/approvals,
  commit, push, merge or publication. Original user stage/Pulsing and pending
  ancestor-policy decision remain untouched. Coordinator retains review and PR121
  publication authority.

#### Round 11 rereview — deterministic cancellation release, validation pending

After slot release, coordinator rereview identified scheduling headroom in the
1.8-second fake cancellation delay: the 1.25-second observation left only
550 milliseconds before expected task exit. The earlier passing result above
is retained, but it does not validate this follow-up.

Static-only follow-up changes only `transport.test.mjs`: the fake now waits for
an explicit owned release marker after acknowledging cancellation. The test
asserts MCP/task survival beyond the one-second flush deadline, then publishes
release and checks the fake's release/exit records before MCP exit. An independent
eight-second fake safety deadline and the existing per-test deadline still bound
failure. Teardown publishes release too, so assertions cannot strand the gated
fake. Removed the three-second shutdown performance assertion; the claim is
ordering and bounded completion, not scheduling performance. No production
change. **No workload run yet**: round12 owns the sole slot; focused validation
requires a new coordinator grant.

#### Round 11 final follow-up validation — marker-gated cancellation

Coordinator explicitly regranted the sole slot after round12 released at
03:11:15 UTC. Used the same exact guarded mise task, explicit serial Node shim,
durable `.round11-owned/tmp`, no-auto-install/nice/jobs/thread/deadline settings
and separate generation budgets documented above:

| UTC | Exact task / name filter | Actual final outcome |
| --- | --- | --- |
| 03:11:53–03:11:55 | `test-agent-plugins`; `^transport: escaped-byte` | **1 actual case passed**, 9 unmatched wrappers; marker-gated test 1.465s, no failed/cancelled |
| 03:11:55–03:11:58 | `test-agent-plugins`; `^(transport: (?!escaped-byte)\|MCP \|lost MCP output\|shared model tool)` | **8 actual cases passed**, 7 unmatched wrappers; no failed/cancelled, overload after 32 bounded diagnostics |

The marker case proved MCP remained alive after the flush deadline while the
fake still owned execution; the test then explicitly released the task, observed
its release/exit records and MCP exit 1 with stdout still paused. This replaces
the narrow 1.8s-versus-1.25s scheduling race with test-controlled ordering.
No production change or native repeat was needed. `DOC-PLUGINS-07` remained
excluded; no installation or network fixture ran.

All four new resource gates passed: MemAvailable
19,168,964,608–19,253,121,024 bytes, free disk 342,834,515,968 bytes,
load1 1.152–1.166. Final fixture tmp was empty; current-UID process and mount
scans found no owned round11 process/mount. Temporary runner/shim/log files
removed after transcription; `git diff --check` passed. Slot released again
after final cleanup for the coordinator's next round.

### Round 12 — inherited descriptor holders after nonzero exit and cancellation

- Measured UTC: 2026-11-08 03:07:19–03:11:15 (about four minutes, including
  static preparation; not an eight-hour elapsed result). Preparation remained
  static while round11finalchecks owned the slot. Parent granted the sole
  TEST/BUILD slot after its 03:08:13 release; this round released it at03:11:15.
- No production bug confirmed or production code changed. Added four focused
  cases in `plugins/test/descendants.native.mjs` and documented their scope in
  `plugins/CONTRIBUTING.md`. Read root README/AGENTS, campaign, plugin
  README/CONTRIBUTING and existing coding/approval/ownership helpers. No nested
  plugin README/AGENT instructions were present.
- Used the supplied native binary read-only at
  `/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor`;
  before/after SHA256 remained
  `6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`.
  No build, replacement, commit, push, Git merge or publication.

#### Story, source contract and actual outcomes

A coding Agent's test writes unfinished candidates, starts an inherited-stdio
background child or forks a grandchild that calls `setsid`, then exits7 or is
cancelled by the user. Every Python process explicitly arms its own45s alarm
(alarms are not inherited by fork); waits use kernel timers, not CPU spinning.
At most three workload Python processes coexist, with one fixture/Session at a
time. The native runtime has its own supervisor/helper processes; that workload
count is not a claim that the entire native process tree contains three PIDs.
Fixtures open no sockets or network clients and install no host service.

Source inspection distinguished the contracts: the plugin supervisor requests
native cancellation and bounds its own pipe wait, but does not promise generic
descendant containment. Linux safe native setup uses a private PID1 namespace;
the kernel tears down its remaining namespace members when PID1 exits.
Existing native coverage exercised a successful setsid double-fork under
record-only capture, not this adapter `--stdio inherit` nonzero/cancel matrix.
This round does not promote process-group signalling to a security guarantee.

| Final scoped scenario | Native/adapter result | Native terminal state |
| --- | --- | --- |
| Background child, parent exits7 | exit7, not interrupted, signal=null | failed, live=false |
| Background child, user cancels after readiness | exit143, interrupted, signal=null | cancelled, live=false |
| Detached setsid grandchild, parent exits7 | exit7, not interrupted, signal=null | failed, live=false |
| Detached setsid grandchild, user cancels after readiness | exit143, interrupted, signal=null | cancelled, live=false |

All four final cases passed; adapter completion was about0.18–0.30s, far below
the8s native deadline plus30s adapter allowance and5s escalation/5s output drain.
Parent and descendant stdout/stderr markers survived without truncation; both
observed stdin EOF. No task falsely reached its unfinished-parent completion
marker. Native finish timestamps were present and live_execution=null at
terminal observation. Each exact selector retained both candidates for status,
review and preview. A rejected real Pi-adapter/mock-host approval preserved
them, host project files remained absent, and exactly one task record proved
no automatic retry. Native live apply/drop refused in both cancellation cases.

Host `/proc` scans used the exact unique fixture-root argv token, same-user
ownership, host PID, process birth start ticks and wait channel. Namespace-local
PIDs were never interpreted as host PIDs. Both cancelling workload processes
were observed sleeping after their descriptor handshake and candidate writes;
readiness raced early run settlement under a6s deadline. All four terminal
owned-process scans were empty immediately: no process required manual
termination or waiting out its independent alarm. The observer sends no signals.

#### Attempts, commands and fixture correction

There were three bounded invocations,12 actual native task attempts total:
10 scenario passes and2 fixture-readiness failures, not12 passing cases.

1. Initial invocation: both nonzero cases passed. Both cancel cases hit the
   fixture's6s readiness timeout because native live review does not expose the
   stopped-only diff. Joined cleanup cancelled pending work, reconciled native
   terminal/nonlive status and dropped all four stages before removal. These
   two failures are harness defects, not evidence of a product lifecycle bug.
2. Corrected readiness uses host owned-token/birth observations and the two
   Python timed sleepers, then native live status. All four scenarios passed.
3. Final-source invocation after adding native finish-time/live_execution
   assertions and operation auditing: all four passed again. Final audited
   operations were26 adapter status/review/preview/refused-live-mutation calls,
   plus8 cleanup status/drop calls. Capability subprocesses are not included in
   that operation count; the initial polling attempt did not audit its exact
   number of live review calls.

All used the exact repository task `mise run test-agent-plugins-native` with a
private `.asv8/round12/bin/node` shim executing:

```sh
exec /usr/bin/node --test-concurrency=1 --test-name-pattern='^descendant pipes:' "$@"
```

Environment set `MISE_TRUSTED_CONFIG_PATHS="$PWD"`, the supplied
`PVISOR_PLUGIN_BINARY`, owned durable TMPDIR/output under `.asv8/round12`,
`CARGO_BUILD_JOBS=2`, `RUST_TEST_THREADS=1`, `OMP_NUM_THREADS=1` and
`OPENBLAS_NUM_THREADS=1`. Each task ran under
`nice -n 10 timeout -k 30s 120s`. Final runner elapsed2.376s. Node reported13
passes, but nine were no-matching-case file placeholders; the actual final
scenario count is four. The native task contains no portable packed-install
case. No portable aggregate or DOC-PLUGINS-07 fixture ran.

`/usr/bin/node --check plugins/test/descendants.native.mjs` and
`git diff --check` passed. Durable runner evidence is
`.asv8/round12/{native,native-corrected,native-final}.log`; private shim and
all logs totaled111,542 bytes at final audit.
After releasing the slot, removed the private shim from the source worktree;
the three durable local evidence logs remain. Only the test, contributor
documentation and this campaign entry are round12 integration deliverables.

#### Resources, cleanup, limits and next scene

Pre/post guards for every workload remained within campaign limits. Across
recorded boundaries available RAM was at least19,131,453,440 bytes, free disk
at least342,738,034,688 bytes, load1 at most1.2421875. Final03:11:15 audit:
available19,140,186,112 bytes, free342,870,376,448 bytes, load1=1.2421875;
zero owned task command lines, zero owned mounts, and an empty evidence fixture
directory. All12 attempted fixtures were reconciled/dropped and removed; no
unknown root remains. Durable logs live outside those removed fixtures.

The coding fixture installed its hook before setup, tracked every attempted run
and operation with immediate rejection handling and combined owned/test/caller/
deadline signals, cancelled/joined work before native terminal checks, and
required successful native drop (or known applied-empty) before process/mount
checks and removal. Unknown authority would preserve the fixture path.

This is Linux safe Host evidence, not macOS, Landlock-only, VM/OCI, arbitrary
daemon containment or actual Pi UI acceptance. Workload output is retained in
the adapter result/log, not a promised second native stdio capture. No MCP
transport/backpressure path ran, so this does not revalidate the parent's
concurrent round11 MCP8MiB overload repair. Existing parent changes to
`plugins/README.md`, `plugins/mcp.mjs` and transport tests were untouched.
No Rust/Python suites, full portable/native aggregate, build, lint/rustdoc,
cloud credentials, installs, host network/system changes, unrelated kills,
global cleanup or semspec approvals were performed.

Next scene: combine an owned inherited descriptor holder with real MCP
connection loss, recover its exact task record from a fresh adapter, and
separately reconcile returned versus undelivered output. Keep that transport
test distinct from these Backend/native lifecycle results and do not rerun an
uncertain task automatically.

#### Post-round static review correction (validation pending)

Parent review found two test-harness weaknesses, not production defects. The
03:11:15 results above exercised the **pre-review** observer: it collected
birth ticks without bracketing the other `/proc` reads, silently skipped
EACCES, and could let a terminal survivor exit voluntarily during its safety
wait before asserting absence. The actual logged terminal sets were empty,
but those runs do not validate the strengthened regression rule.

Prepared static-only changes while round13 owns the slot:

- Extracted a small read-only observer into
  `plugins/test/descendants-fixture.mjs`. It brackets UID/argv/wait-channel reads
  with process start-tick checks, rechecks UID, matches readiness hostPID/birth
  pairs even if argv changes, and records numeric-PID reuse separately. EACCES
  or EPERM is explicit incomplete visibility, not a zero-process result;
  unstable observations fail closed as well.
- Terminal ownership is sampled immediately on adapter settlement, before
  status/review delays. The fixture requires observation before15s, well before
  voluntary40s exit. Any observed survivor fails; the46s safety wait only
  precedes rethrowing the original failure, never turns eventual exit into PASS.
- Added an optional default-no-op ownership verification callback to the coding
  fixture's existing reconciliation guard. Incomplete ownership evidence
  preserves its root even if native status/drop otherwise succeed.
- Added four fake-I/O helper tests for stable identity/argv changes, numeric PID
  reuse, birth races/denied visibility and survivor self-exit during cleanup.

No validation workload has run against these post-review changes yet; waiting
for explicit parent slot grant. No new native/product bug is claimed.

### Round 13 — exact filename selection, including option-shaped names

- Actual UTC: static preparation 2026-11-08 03:09–03:12; sole TEST/BUILD
  grant received after round11's 03:12:18 release. Workloads 03:12:58–03:15:53;
  final audit 03:14–03:16. This is a bounded incremental round, not eight hours.
- Read root README/AGENTS, plugin README/CONTRIBUTING, campaign and existing
  coding/summary fixtures. No nested plugin instructions found. Parent was
  notified of backend/summary overlap before production edits and confirmed
  these translation points were free. No authority or ancestor-policy change.

#### Confirmed defect and coherent repair

The native immutable binary deterministically refused separate argv
`["--path", "--json"]` with exit2 and “a value is required”, while the same
read-only preview using `["--path=--json"]` selected exactly the staged file
named `--json`. The old Backend then failed that same preview with exit2.
Shell quoting alone does not stop the native option parser treating a leading
dash as an option. This is native red evidence, not inference from mocked argv.

Compared broad native `allow_hyphen_values` with fixing the owning JS translation:
chose one attached `--path=<literal>` argument for **every** valid selection,
in both preview/apply and the summary's shell-quoted review command. No parser
change, dash-specific branch or expanded filename API. Exact selection validation,
native planning, confirmation and writeback remain unchanged. Updated two
legacy exact expectations and documented the string-path/control-display scope.

#### Real Session and presentation evidence

`filename-selection.native.mjs` reuses `codingFixture`, including pre-setup
hooks, pre-invocation operation tracking, abort/join and terminal/drop cleanup.
One Session per attempt, **three total** (red, green, then parent-requested final
source revalidation); no external/user stage. The extra tiny final Session was
explicitly requested after the test-only diagnostic conditional changed.
The offline Python argv fixture creates eight tiny files plus one directory:
`--json`, `-notes.txt`, spaces, non-ASCII, apostrophe, newline and directional
control names, and scratch. No shell constructs these names.

The green Session verifies:

- Before acceptance, every host file is absent. Exact `--json` preview selects
  one entry, no required entry, inventory9, pending8; raw native entry identity
  agrees with summary, which shows selected1 and attached review command.
- Explicit apply publishes only `--json`. A missing-path preview is a no-op,
  not all-pending. Newline child selection retains its exact native identity
  and suppresses runnable commands. Newline/bidi labels are visibly escaped.
- Explicit `reviewer's 文件` directory selection includes that directory and
  its two newline/bidi children (selected3, required0, pending5). The review
  command preserves apostrophe quoting. Applying that explicit container,
  then dropping the rest, leaves only the accepted file and two accepted
  children; five unrelated files remain absent.
- Non-UTF-8 bytes were not added; adapter strings do not claim arbitrary byte
  selection. No live Pi/DSH UI or cloud model was involved.

#### Commands and actual case counts

Common harness: `nice -n 10 timeout -k 30 120 mise run TASK`,
process-local `MISE_TRUSTED_CONFIG_PATHS="$PWD" MISE_YES=1 MISE_AUTO_INSTALL=0`,
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1`,
`OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1`. Owned PATH Node shim preserves task
file arguments and adds `--test-concurrency=1 --test-timeout=120000
--test-name-pattern="$ROUND13_FILTER"`. TMPDIR/evidence root:
`$PWD/.asv8/round13/tmp`. Durable runner logs are `.asv8/round13/*.log`.

| UTC | Task and precise filter | Result |
| --- | --- | --- |
| 03:12:58–59 | `test-agent-plugins-native`; `^filename selection:` | Native **red**, 1 actual failure at Backend preview after separated/attached native comparison; 9 unmatched wrappers |
| 03:13:04–05 | `test-agent-plugins`; `^filename (argv\|summary):` | Portable **red**, 2 actual failures for old argv/display command; 8 unmatched wrappers |
| 03:13:31–32 | `test-agent-plugins-native`; `^filename selection:` | **1 actual case passed**, 9 unmatched wrappers; selected-only host state and disposal passed |
| 03:14:06–07 | `test-agent-plugins`; exact anchored filter below | **29 actual cases passed**, 8 unmatched wrappers, 37 reported passes; no install fixture selected |
| 03:15:52–53 | `test-agent-plugins-native`; `^filename selection:` | **Final source: 1 actual case passed**, 9 unmatched wrappers, separated diagnostic exit2; complete native acceptance/disposal repeated |

Final portable filter, with literal regex alternation:

```text
^(filename (argv|summary):|summary separates |binary and mode-only |empty and no-op |blocked/conflicting |malformed, truncated, |non-string action |untrusted/control/|every directional |long lists and labels |realistic ancestor |(pi|dsh): (malformed|malformed-kind|truncated|blocked) preview never |failure retains exact |reject config overrides, |run always selects |tasks cannot cross |DOC-PLUGINS-08:|Pi registers additive |DSH permits only |Pi and DSH never |shared model tool routing )
```

This includes exact argv for spaces/Unicode/apostrophe/dashes/newline/bidi and
shell-looking data; no sentinel is created. Empty/NUL/traversing input, cross-task
ownership, unsafe state, blocked approval, malformed/truncated preview, no-op,
byte-identity summary handling and all directional-control presentation
regressions remain covered. All workload invocations have zero cancelled cases.
`git diff --check` and `node --check` for new native test/backend/summary passed.

After native green, the separated-form comparison was made diagnostic rather
than requiring future native runtimes to reject it: a future successful result
must still select exactly `--json`; current exit2 branch retains the assertions
already run. Parent requested one final narrow native rerun on actual final
source; it passed at03:15:53, including the diagnostic exit2 branch. Production
code and all native success assertions are unchanged since initial native green.

Read-only native binary, SHA-256 unchanged before every workload and final audit:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

No build/overwrite; inherited artifact provenance is not a claim of building
from current uncommitted JS. Backend SHA-256 after repair:
`92f062ad8da944c7ec5246d2eb14ab5ef9fddb1975304415b13df2440f0072bd`;
summary: `3bc3e0b7545084e8557473d6e2217e57c6778ab3c1e9e3b5a76cb15ff68c6405`.

#### Resources, cleanup, gaps and next scene

- All ten pre/post workload gates passed: available RAM
  19,048,624,128–19,226,386,432 bytes; free disk
  342,721,675,264–342,774,968,320 bytes; load1 0.989–1.416.
- All three native roots reconciled terminal/nonlive, dropped and were removed by
  the shared fixture. Final scan found zero owned processes/mounts or native
  roots. Initial audit correctly reported one empty owned scratch HOME retained
  by the existing fake-runtime failed-run regression, not native uncertainty;
  inspected it and removed only that empty directory with `rmdir`. No unknown
  root was removed. Runner logs/scripts total under20KiB; fixture generation is
  tiny. No unrelated cleanup, process killing, installs or network.
- No full aggregate, Rust/Python suite, build, lint,
  rustdoc, VM/OCI/macOS, live UI, arbitrary-byte selector or semspec change.
  No commits, pushes, Git merges or publication. User stage and pending parent
  conflict-policy decision remain untouched.
- Next scene: exact selection of a rename/type change with a leading-dash
  destination, retaining native required dependencies and host conflict refusal;
  use another explicitly granted bounded slot rather than broad filename APIs.

### Round 12 post-review final-source validation

Parent granted the sole slot after round13's03:16:18 release. Strengthened
helper and native checks completed by03:17:24, when the slot was released
again. The preceding “validation pending” paragraph records the earlier
static-only phase, not the final state.

- `mise run test-agent-plugins`: **four actual fake-I/O helper tests passed**.
  Node reported14 passes, including10 no-match file placeholders. The filter
  excluded all other portable cases, including DOC-PLUGINS-07 packed install.
- `mise run test-agent-plugins-native`: **four actual native cases passed**.
  Node reported13 passes, including nine no-match file placeholders; elapsed
  2.683s. Final case durations including approval/reconciliation hooks were
  0.491s/0.693s/0.471s/0.683s in matrix order.
- Both exact mise tasks used the private Node shim with
  `--test-concurrency=1 --test-name-pattern='^descendant (pipes|observer):'`,
  the same owned TMPDIR/evidence/binary environment and nice10/jobs2/thread1/
  120s+30s outer limits as above. The shim was removed after release.
- All four immediate terminal snapshots explicitly recorded empty `processes`,
  `inaccessible`, `unstable` and `reused` arrays, before the15s early-observation
  bound. No delayed safety wait was needed. Cancellation readiness births were
  retained for matching by hostPID/start ticks. Nonzero cases finish without a
  live-readiness phase and use exact-token/bracketed identity observation.
- The focused helper tests passed simulated argv replacement, numeric PID reuse,
  birth-read race, EACCES and voluntary self-exit during safety cleanup. The
  last case still rejects with the original survivor assertion after the
  simulated survivor disappears; an empty observation at40s also fails.
- Final native operations again numbered26 explicit calls plus8 cleanup
  status/drop calls. All four stages were terminal/nonlive, rejected approval
  left host files absent and candidates reviewable, no automatic retry occurred,
  native drop succeeded, and all fixture roots were removed.
- Final read-only binary SHA256 was unchanged; `git diff --check` passed.
  Logs are `.asv8/round12/{observer-reviewed,native-reviewed}.log` in addition
  to the earlier three logs. Local evidence plus shim before its removal was
  212,435 bytes. No installs, publication or production edits.

Pre/post resources for these two workloads: minimum available RAM
18,963,443,712 bytes, minimum free disk342,657,921,024 bytes, maximum
load1=1.1171875. Final03:17:24 audit: available19,071,094,784 bytes,
free342,698,389,504 bytes, load1=1.1005859375; zero owned task command lines,
zero owned mounts, empty fixture directory. No unknown root remains.

Totals including pre-review attempts are now16 actual native attempts:
14 scenario passes and two initial readiness-harness failures. Definitive
post-review coverage is four native passes plus four helper passes; earlier
passes are not substituted for those stronger checks. The previously listed
Linux-only/MCP/UI/platform limitations remain unchanged. No full portable or
native aggregate, Rust/Python suite or build was run.
