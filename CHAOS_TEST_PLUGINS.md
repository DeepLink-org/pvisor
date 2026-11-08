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
