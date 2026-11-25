# Chaos campaign summaries

Historical results below apply to the recorded revisions and hosts, not the
current checkout. Iteration windows include investigation, builds, pauses and
review; they are not continuous-load durations. Raw logs remain in ignored
directories in the original executor worktrees and may not exist in a new clone.
Evidence paths below are relative to those worktree roots.

## 2026-11-07 — runtime, filesystem and network

- Scope: two Linux campaigns covering stage/apply/drop, replay, journals,
  OverlayFS/OverlayNet, VM teardown, control protocols and Host workflows.
- Results: [PR #111](https://github.com/DeepLink-org/pvisor/pull/111) repaired
  DNS name-length accounting, KVM exit notification after ready-callback abort,
  nested-stage backing grants and incomplete safe-process staging bindings.
  Its closing revision still had pending CI.
  [PR #118](https://github.com/DeepLink-org/pvisor/pull/118) repaired replay
  cancellation, SYN admission, vCPU teardown, apply backup cleanup, diff clipping
  and UTF-8 boundaries, and local/server inspect-root cleanup.
- Limits: the second campaign's final selected sweep passed, but its full
  runtime run remained incomplete after a socket-path failure and retry timeout.
  A Bats exact-output failure did not reproduce on one retry; its cause was
  unresolved. Neither campaign established full-workspace or cross-platform coverage.
- Follow-up questions: readiness-callback ordering, live stage-size enforcement,
  zero-MSS/Ethernet admission and saved-payload authentication.
- Cleanup: final second-campaign snapshots found no owned workload, fixture
  mount, socket or inspect root in the inspected scope.
- Evidence: worker-local `target/chaos013/`, `target/chaos018/`,
  `target/chaos028/`, `target/chaos044-evidence/` and
  `target/chaos054-evidence/`; changes and reviews are linked from the PRs.

## 2026-11-08 — runtime and network catalog

- Scope: Linux runtime/overlay contracts and nine low-load native catalog
  segments; catalog scenarios did not attach containers or deliver packets.
- Results: [PR #121](https://github.com/DeepLink-org/pvisor/pull/121) retained
  owner identity during status sampling, bounded cross-process lease handshakes
  and an apply-conflict matrix. Final selected package runs passed; the recorded
  CI checkpoint for `f2d5aa20` had 33 successful checks.
  Catalog segments completed 360 actions and 2,235 CLI commands without
  unexpected failure or timeout.
- Limits: controlled HOME/TMP topology; default skips and other platforms were
  outside local coverage. Process permission denials limited cleanup visibility.
- Cleanup: delayed audits found known catalogs unchanged and no readable
  exact-store process or store mount; private crash-temp evidence was retained.
- Evidence: `.data/round16*`, `.data/round19/followup`, `.data/round22`,
  `.data/runtime-final-verification/` and `.data/runtime-catalog/`.

## 2026-11-09 — generated recovery, stopped for host pressure

- Scope: replay receipts, scoped approvals, snapshot/copy failures, journal
  recovery, Gateway cancellation and generated filesystem traces.
- Results: [PR #129](https://github.com/DeepLink-org/pvisor/pull/129) included
  replay receipt admission (`340b4d85`) and scoped approval narrowing
  (`90d4921f`). Completed checks remain separate from interrupted traces.
- Stop: the supervisor terminated and reaped the owned namespace workload when
  free swap, memory PSI and sustained swap-out breached tightened limits.
  The requested eight-hour campaign did not complete; the interrupted trace
  was neither a pass nor a reproduced product defect.
- Cleanup: final owned-process, temporary-fixture and mount audits were empty.
- Evidence: `target/chaos8h/`, including `audit-r17-cleanup.json`.

## 2026-11-10 — OpenSandbox/containerd shim

- Scope: live Linux/KVM containerd restart, shim/runner death, concurrent create
  and exec, late/torn policy gates and abandoned sandbox cleanup.
- Results: 157 completed rounds recorded 212 PASS, one pre-fix FAIL and
  35 CHECK outcomes. CHECKs were unresolved/timing outcomes, not passes.
  Repairs addressed parent-death thread lifetime and holder cleanup
  (`f1d885f9`), late/torn gates and gate residue (`cd1e4504`), plus sandbox
  start and exec-kill handling (`9485bab1`).
- Limits: the driver mixed wall and monotonic clocks, overrunning its deadline
  by about nine minutes before manual stop. Some injected create failures
  surfaced as SDK health-check timeouts. Surveyed exit-watcher PID collision,
  bridge failure, concurrent payload delivery, FIFO fallback and gate-removal
  hypotheses were not reproduced or resolved.
- Cleanup: campaign sandboxes and containers were removed; the recorded
  business baseline was preserved.
- Evidence: `target/chaos-osb-20261110/` and the original node archive
  `/root/chaos8h-osb-20261110.tar.gz`.

## 2026-11-12 — completion publication and recovery

- Scope: Journal/Core/Overlay Core/Replay contracts and safe NativeHost
  result-publication failures, including a six-minute live-owner experiment.
- Results: result export could overwrite its own Session records; opaque-storage
  admission was repaired in `a5e3e093` with follow-up `79835afa`.
  Final local package selections passed. The recorded CI checkpoint for
  `79835afa` had 33 successful checks.
- Limits: initial resource stops/OOM preceded test execution; pre-fix failures
  and earlier CLI fixture failures were not passing coverage. The six-hour
  iteration window contained about two hours of recorded nextest/native
  execution. A historical Replay CI timing failure remained unexplained.
- Cleanup: delayed audits found all 47 captured owned cgroups and successful
  Native fixture paths absent; fourteen retained failed-evidence roots were
  unchanged. Checks did not authenticate every host namespace.
- Evidence: `.data/chaos6/`, including `validation-totals.json`,
  `failed-evidence-integrity-baseline.json` and `delayed-audits.jsonl`.

## 2026-11-21 — staged workflows, stopped for host swap-out

- Scope: Linux/KVM stage/review/selective apply/drop, literal and raw-byte
  filenames, frontend/owner/runner death, one-service Compose and persistent
  staged containers; generated filesystem, Core, Journal, OverlayNet, Gateway
  and Replay contracts ran under serial, resource-capped supervision.
- Results: [PR #177](https://github.com/DeepLink-org/pvisor/pull/177) repaired
  impossible opaque-directory selection advice for rootfs/project-scoped apply
  in `51c8857d`. Publication restrictions and no-write refusals were preserved.
  Final completed domain and CLI selections passed; remote CI was successful
  for that exact code commit, not a later documentation head.
- Stop: the six-hour request was **not completed**. Tests stopped at
  05:50:34 UTC after sustained host swap-out crossed the safety gate, before
  the 06:20:28 UTC deadline. The interrupted corpus and unstarted tail jobs
  were not passes. Builds, diagnostics, pacing and the quiescent remainder
  are not continuous testing.
- Cleanup: read-only audits found no matching owned process, populated campaign
  cgroup or fixture mount in the inspected namespace. Two unknown owner-death
  stages and all pending/aborted evidence were retained unchanged. Four inert
  global control sockets remain; deleting them requires explicit user confirmation.
  No outside-worktree paths were manually unlinked or removed.
- Limits: local coverage is Linux/KVM only; no physical storage failure, power
  loss, native pending-apply recovery, four-VM daemon/SDK run or live-model
  acceptance is claimed. The campaign did not approve specifications.
- Evidence: `target/chaos6h-20261121-002028-3a6cc2/`, with owned temporary
  fixtures under `target/t3a6cc2/`.

## 2026-11-25 — interrupted Docker, Compose, and Host workflows

- Scope: a three-hour Linux x86-64/KVM and Host/FUSE campaign against frozen
  `705846c8`, seed `98820261125`. Eight scenarios used private stores and
  exercised same-name reuse, execution receipts, interrupted Compose teardown,
  image staging leases, and workspace selection. The timer excluded preparation.
- Baseline: 8,341 iterations in 10,801 seconds; 6,256 passed. An immediate
  interactive exit lost its result 1,043 times, and a successful Compose retry
  left a retired directory 1,042 times. The other six scenarios had no failures.
- Follow-up fault tests also found unretryable Compose volume publication,
  orphaned cold-image workers that blocked an identical retry, and `last`
  selecting a foreign Session after the original workspace path was rebound.
  All five fixes were integrated locally. The final frozen candidate passed
  230 iterations in a separate five-minute comparison; native regressions also
  checked retained named-volume data, exact streams and exits, and detached
  cold-image publication. This was not a three-hour run of the candidate.
- Cleanup: the baseline audit found no owned live process, populated campaign
  cgroup, or fixture mount in the inspected namespace. Failed receipts remain
  available for inspection.
- Limits: no macOS/Windows, physical storage failure, power loss, or explicit
  writeback through a rebound workspace path was tested. Specification fixture
  tests passed without changing approvals.
- Evidence: `.data/t988/`, especially `findings.md`, `three-hour-audit.json`,
  `integration-cli.log`, `final-volumes/`, and `final-chaos/`.

## 2026-11-25 — candidate endurance and workspace writeback

- The frozen five-fix candidate completed a separate Linux x86-64/KVM and
  Host/FUSE three-hour run with seed `98820261126`: 8,217 iterations and 89,372
  CLI commands, with no oracle or cleanup failures. The audit found no owned
  live processes, populated cgroup, or fixture mounts in the inspected namespace.
- A separate native Host/FUSE probe found that explicit `apply` could still write
  an ordinary stage's changes into a replacement project at the old path, with
  or without a symlink. Fresh stages now bind writeback to the original
  directory. Preview and real apply rejected both replacements without writing;
  restoring the original directory allowed the same stage to apply successfully.
- The new workspace fix was not in the endurance binary. Older unbound stages
  still use path-based apply; no concurrent external-writer, macOS, or Windows
  guarantee is inferred.
- Evidence: `.data/t988/fixed-three-hour-audit.json` and
  `.data/t988/apply-target-{before,after}/observations.json`.
