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
