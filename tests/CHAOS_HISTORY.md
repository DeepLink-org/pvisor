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

## 2026-11-18 — bounded packet admission and stage lifecycle

- Scope: the 00:24:05–06:24:05 UTC observation window covered Core, Journal,
  Replay, Gateway, Overlay Core/FS/Net, private network catalogs and real KVM
  stage/apply/drop at 128/256/512 MiB. CPU was capped at two cores, memory at
  4 GiB with swap disabled, and compilation/test batches were serial.
- Results: [PR #160](https://github.com/DeepLink-org/pvisor/pull/160) repaired
  foreign Ethernet destination admission (`a17d4e5f`), staged VM scratch
  mountpoints (`fb231ff5`) and unspecified TCP destination admission
  (`135de3b5`). Independent hardlink, stream-fragmentation and body-ownership
  oracles were added in `14d0bd9a`. Final selected regressions and native Scrut
  stage specifications passed; the PR was not merged and had no reported CI checks.
- Timing: this was not six hours of continuous testing. Recorded bounded
  build/test batches occupied about 4h23m, including 3h07m of native stage
  activity. A 43m19s model/scheduling interval had no owned test load.
  Final tests ended at 06:18:31 UTC; read-only resource observation continued
  to the original deadline. Report finalization resumed after 07:13 UTC.
- Non-passes: preserve the pre-fix failures, one budget timeout, one evidence-cap
  stop and a conservative swap-out stop. An invalid positive fixture correctly
  hit the existing non-root directory-ownership refusal. Deferred subreaper
  recovery also caused a Replay zombie-PID assertion failure; recorded zombie
  state/exit status, positive/negative controls and the unchanged full test
  selection distinguished this harness interference from live-process leakage.
- Limits: no power-loss, automatic recovery of retained pending applies,
  old-checkpoint native restore, HVF/WHP or full-workspace acceptance claim.
  Passing specifications did not approve or alter review ledgers.
- Cleanup: the final delayed audit found all 53 recorded cgroups absent, no
  campaign-marker process and no campaign mount in the observer namespace.
  Failed/pending fixtures and raw evidence were retained; unrelated host
  namespaces and shared runtime lock caches were not swept.
- Evidence: worker-local `target/chaos6h-20261118/`, including `clock.json`,
  `totals.json`, `activity-intervals.json`, `final-observer.jsonl`,
  `audit-071359.json` and per-batch monitor/reaper logs.
