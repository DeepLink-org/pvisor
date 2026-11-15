# OpenSandbox-driven shim chaos campaign (2026-11-10, eight-hour window)

## Requested window

- Window start: **2026-11-10T12:35:41Z** (driver campaign-start event,
  node pid 4193263, single instance after correcting an accidental
  double-launch during setup).
- Deadline: **2026-11-10T20:35:40Z** (epoch 1794342940, recorded in
  `/root/chaos8h-osb/deadline-epoch.txt`). The deadline is a stopping
  bound, not evidence of eight hours of active testing. Interruptions and
  gaps are recorded separately.
- Baseline revision: `515521b2` (main, includes #131 runner reaping and
  containerd restore-probe fixes). Baseline binary rebuilt from HEAD with
  `PVISOR_KRUNFW_KERNEL_BUNDLE` from `/root/fwbuild/bundle` reproduces the
  deployed `/usr/local/bin/containerd-shim-pvisor-v2` sha256
  `699023e5a7b86bdcba3eb437f2babc5fd7e5bfd50ca7589b2efb54693087f5b5`
  byte-for-byte, so the node binary is the HEAD build.
- Branch for repairs: `chaos/20261110-osb-eight-hour` (created on first
  confirmed repair). No merge into `main`.

## Confirmed defects (phase 1, baseline binary `699023e5`)

### Defect 1 — VM runner SIGTERMed ~11 s after creation (regression from #131)

- Evidence: containerd journal for an untouched create —
  `12:35:43.205 holder ready (pid 1898)` →
  `12:35:54.339 container ... exit_status:143` (the runner's
  parent-death handler exit code) — **exactly +11.1 s**, matching the
  tokio `spawn_blocking` pool keep-alive. Pod `3c7359c5-...-0` `0/1
  Error`, execd ping 502, `internal_procs=0` while the shim lived.
- Campaign rounds 1–2 failed identically (`SandboxReadyTimeoutException`,
  policy create timeout).
- Root cause: `PR_SET_PDEATHSIG` binds to the *thread* that forked the
  child; the runner was forked from a pooled blocking thread whose idle
  exit SIGTERMed the healthy runner.

### Defect 2 — sandbox holder had no parent-death cleanup (pre-#131 gap)

- Evidence: two stale holders (pids 251998/804565, one since Nov 5,
  ppid 1, pinning dead pod netns `4026537756`) reaped pre-campaign;
  campaign round 3 (`shim-kill-idle`) reproduced it live — after the
  driver SIGKILLed the shim, holder pid 12103 survived ≥20 s as ppid 1.

### Fix

Branch `chaos/20261110-osb-eight-hour`, commit `f1d885f9`
"fix(shim): fork internal children from a dedicated thread and reap
holders with their shim":

- All internal children now fork on the never-exiting
  `pvisor-internal-spawner` thread, so the death signal tracks the shim
  process lifetime.
- The holder arms `PR_SET_PDEATHSIG(SIGTERM)` (default disposition) with
  the same spawning-pid race re-check.
- Regression `tests/internal_spawn_lifetime.rs` (custom harness):
  a pdeath child forked directly on a 100 ms keep-alive pool thread dies
  with SIGTERM when the thread idles out (control reproduces the
  mechanism); the same child via the dedicated thread survives it.
- Validation: `cargo test -p pvisor-shim` (86+9 pass),
  `cargo test -p pvisor-shim --features vm` (96+9 pass),
  `cargo clippy -p pvisor-shim [--features vm] --all-targets` clean,
  `cargo fmt --check` clean. mise unavailable on this build host; direct
  cargo used per runbook §11.
- Deployed to the node at 12:47 UTC as
  `/usr/local/bin/containerd-shim-pvisor-v2` sha256
  `4b54824fcba0babb7b70848e857dc551bafd0e2621a30fbb20548de03e6b465e`.
  Existing pods keep their old shims; new pods pick up the fix.

## Confirmed defects (phase 2, binary `4b54824f`)

### Defect 3 — late egress policy never applies (gate-late FAIL)

- Probe `gate-late` (13:14–13:18 UTC): gate file removed before the
  runner's first read, valid policy republished ~10 s after boot —
  allowed target stayed unreachable ("Temporary failure in name
  resolution": the VM booted with no NIC at all) and never recovered.
- Root cause: on gate timeout/parse/compile failure the runner booted
  without network AND never spawned `watch_gate`, so every later
  republish was ignored — violating the documented dynamic-policy
  contract ("the gate stays a live channel for the task's its lifetime").

### Defect 4 — torn gate read failed the boot instead of retrying

- `wait_for_egress_rules` returned `Err` on the first unparseable read;
  the sidecar publishes the file non-atomically (`fs::write`), so a
  concurrent read could strand the task. The `gate-torn` probe (driver
  used atomic renames) could not reproduce it live — fixed as a
  code-level hardening with unit regressions.

### Defect 5 — egress gate file leaked in /run on shim SIGKILL

- Probe `shim-kill-policy` (13:22–13:28 UTC): after SIGKILLing the shim
  of a policy sandbox, `/run/pvisor-egress-3dc4cb73c7cc244e.json`
  (tuna-policy content) survived the full teardown; `ShutdownSandbox`'s
  unlink is the only cleanup path and never runs on a crash. File reaped
  manually after evidence capture.
- (A driver dispatcher bug initially routed this fault to the generic
  shim-kill handler; fixed in the driver, evidence captured manually.)

### Fix (commit `cd1e4504`)

Torn reads retry within the boot window; absent/uncompilable gates boot
deny-all (empty allowlist on a real NIC) and keep the gate watch so a
late valid policy hot-swaps in; the holder's parent-death handler unlinks
the pod's egress gate before exiting. README documents the contracts.
Validation: `cargo test -p pvisor-shim [--features vm]` 86/98+9 pass
including two new torn-read regressions; clippy clean both configs.
Deployed (post-cosmetic rebuild) as phase-3 binary.

## Final report (window closed 2026-11-10T20:59Z)

### Time accounting (honest window math)

- Declared window: 12:35:41Z start; fixed-binary phase deadline
  20:49:34Z (= phase-2 start 12:49:34 + 8 h).
- Actual supervised workload end: **20:58:50Z** (last journal event).
  Continuous fixed-binary testing 12:49:34→20:58:50 = **8 h 9 m 16 s**
  (met the requested eight hours); total window including the
  broken-binary phase 1: **8 h 23 m 9 s**. The driver overshot the
  deadline by ~9 m because of a driver bug (below), not by intent; the
  overshoot was still active testing and the driver was stopped manually
  at 20:59Z.

### Driver defect (test harness, not product)

`--deadline` was a wall-clock epoch but the round loop compared it with
`time.monotonic()` (uptime seconds), so the self-stop never fired; rounds
continued past 20:49:34 until manual stop. Fix for future runs: compare
`time.time()` (or convert once). Also: pkill -f patterns matched their own
wrapper shell twice during operation (the runbook §7 warning); pid-listed
kills avoid it.

### Outcome tally (journal 13:10:27→20:58:50; phase-1 evidence quoted above)

- **157 rounds completed**; verdicts: **212 PASS, 1 FAIL, 35 CHECK**;
  466 invariant samples; **0 safety stops, 0 orphan-helper anomalies**
  after the fixes deployed.
- The single FAIL is the pre-fix `gate-late` reproduction (13:18); after
  the fix deployed, gate-late ran 9/9 PASS in rotation.
- CHECKs: 34× injection-churn "timeout-no-clean-error" — the shim's
  Create rejects broken catalogues cleanly at the CRI seam (journal-proven
  `InvalidArgument … payload … source path … missing`), but the
  OpenSandbox server's create health check polls until ready-timeout
  instead of observing the pod failure (upstream behavior, recorded); 1×
  loosen-phase probe timing.
- Per-fault (PASS/FAIL/CHECK): containerd-restart 22/0/0, create-burst
  11/0/0, hotswap-loosen 20/0/1, hotswap-tighten 26/0/0, exec-storm
  14/0/0, gate-late 9/1/0, gate-torn 14/0/0, injection-churn 11/0/34,
  shim-kill-idle 25/0/0, shim-kill-midcreate 34/0/0, shim-kill-policy
  20/0/0, ttl-abandon 6/0/0, runner-kill included in earlier tallies.
- containerd restarts (5 in rotation): every time — zero restore-probe
  errors, business pods unaffected, fleet sandboxes kept executing
  ('ok','ok','ok') and tore down cleanly.

### Repairs shipped (branch `chaos/20261110-osb-eight-hour`)

| Commit | Defects | Live re-verification |
| --- | --- | --- |
| `f1d885f9` | #1 runner PDEATHSIG bound to pool thread; #2 holder had no parent-death cleanup | shim-kill-idle 25× PASS post-fix (reap ≤1.2 s), zero orphans |
| `cd1e4504` | #3 late gate never applies; #4 torn gate read fatal; #5 gate file leak on shim crash | gate-late 9/9 PASS, shim-kill-policy 20× PASS zero residue |
| `9485bab1` | #6 StartSandbox placeholder hang; #7 exec Kill idempotency after runner death | defensive (narrow windows); containerd journal clean of exec-kill errors |

Deployed binaries: `699023e5` (baseline=HEAD) → `4b54824f` (f1d885f9)
→ `d5b04d2a` (cd1e4504) → `58822785` (9485bab1, final). Binary swaps
were live-safe (runtime v2 execs per pod); no containerd restarts needed
for deployment.

### Residual hypotheses (not fixed, documented for follow-up)

From the code survey: host-vs-guest pid collision in the exit watcher
(needs small pid_max, not injectable here); bridge proxy dying silently
after a 30 s EADDRINUSE window; concurrent same-pod payload delivery
torn copy (needs two real injectors per pod — the execd-installer is
synthesized, so unreachable in this topology); FIFO >/dev/null fallback
after 1 s; ShutdownSandbox removing the gate while gated tasks still
live (policy freeze). None reproduced in 157 rounds.

### Cleanup record (2026-11-10 20:59–21:05Z)

- Driver stopped (no driver/python/start.sh processes remain).
- All campaign BatchSandbox CRs deleted via the server API; opensandbox
  namespace back to controller+server only; CRI back to the 20-pod
  business baseline; 7 exited campaign containers removed (`crictl rm`).
- `/run`: zero `pvisor-*` files (campaign bridge/egress sockets cleaned
  by the fixed shim paths; one Oct-22 stale `pvisor-agent-*.sock` from a
  prior session removed after no-holder verification).
- Memory 456 GiB available (baseline), load < 1, D-state 0. Business
  pods all Running (`default/dind-ct` remains in its pre-campaign Error
  state, untouched).
- Evidence archived: node `/root/chaos8h-osb-20261110.tar.gz` and local
  `target/chaos-osb-20261110/` (journal.jsonl 1586 events, driver.py,
  driver.out, state snapshots).

## Campaign phases

- Phase 1 (12:35:41–12:49:34 UTC): baseline binary `699023e5`; rounds
  1–3 produced defect 1/2 evidence; cleanup before phase 2.
- Phase 2 (12:49:34–13:29:40 UTC): binary `4b54824f` (defects 1/2 fixed);
  rounds 1–6 of the rotation (exec-storm, hotswap, shim-kill, ttl,
  injection-churn, runner-kill in progress) plus targeted gate probes
  13:14–13:28 producing defect 3/4/5 evidence.
- Phase 3 (13:29:40–20:49:34 UTC): binary with all five defect fixes;
  full 13-fault rotation until the fixed deadline 20:49:34 UTC
  (epoch 1794343774, unchanged since phase 2 start).

## Pre-campaign baseline observations

- Two stale sandbox holders (`--pvisor-shim-internal-sandbox`, ppid 1,
  pids 251998/804565, one alive since Nov 5 pinning a dead pod netns
  `4026537756`) and one stale density-test monitor loop (pid 2915387,
  since Oct 16) were present before this campaign and reaped at 12:31–12:35
  UTC after ownership/marker checks. They are prior-session residue, not
  this campaign's results — but the holder residue matches defect 2
  and motivated its code-level confirmation.
- `default/dind-ct` pod is `Error` since before the campaign; excluded
  from the business-health gate.
- Node at start: MemAvailable 456.7 GiB, load1 0.29, 20 CRI pods (all
  business), zero pvisor shims/gates/bridges.

## Test machine and topology

From `build/pvisor-density-test-runbook.md`:

- Node `ecs-10-140-84-58` (`10.140.84.58`), 255 vCPU / 503 GiB DRAM,
  Ubuntu 24.04, kernel 6.8.0, AMD KVM. K8s v1.36.2 single node,
  containerd v2.2.5, Cilium 1.20, OpenSandbox server v0.2.2 /
  controller v0.2.0, postgres, ollama×2, dataagent are co-resident business
  pods that must stay healthy.
- Shim endpoint: `/usr/local/bin/containerd-shim-pvisor-v2`, runtime `pvisor`,
  CRI sandboxer=shim, VM tier via `io.pvisor.executor: vm` annotations from
  OpenSandbox server template `tpl-vm.yaml`.
- SDK: node-local `/root/osb-venv` (`opensandbox` 1.1.0),
  domain `10.140.84.58:30847`, `use_server_proxy=True`, key via
  `OPEN_SANDBOX_API_KEY`.

## Scenario (standing)

"Agent fleet under operational chaos": the driver continuously materializes a
rolling fleet of VM sandboxes through OpenSandbox SDK parameters — varying
`resource` shapes, `env`, `metadata`, `network_policy` (deny-default +
allowlists, dynamic `patch_egress_rules`), `timeout`/renew, background vs
foreground commands, files API traffic — and exercises the lifecycle
(create→ready→work→destroy / TTL-expire / abandon).

Each round injects exactly one fault class, then verifies recovery and the
invariant set. Fault classes (rotating):

1. `shim-kill`: SIGKILL one owned shim mid-lifecycle. Expect: VM runner
   reaped via PDEATHSIG (#131), task sockets unlinked, next create works.
2. `containerd-restart`: `systemctl restart containerd`. Expect: no
   'unable to load shim ... not found' restore errors for pvisor sandboxes
   (#131), running VM pods survive, business pods unaffected.
3. `egress-hotswap`: rewrite `/run/pvisor-egress-*.json` gate files
   mid-flight (tighten then loosen). Expect hot recompile; allowed flows
   stay reachable, denied flows reset.
4. `injection-churn`: rewrite `/etc/pvisor-shim/injection.json` between
   valid/invalid variants while creates are in flight. Expect clean
   per-Create failures on invalid input, recovery on restore, no shim crash.
5. `runner-kill`: SIGKILL one VM runner process. Expect task failure is
   observable through the SDK, sandbox teardown is clean, no leaked netns.
6. `sdk-abandon`: create sandboxes and abandon them with short TTL.
   Expect server TTL reaper reclaims pod + node resources within budget.
7. `exec-storm`: burst of concurrent commands/files operations on a fleet.
   Expect no shim/task errors, bounded latency.
8. `create-burst`: bounded batch create (≤100) with convergence wait,
   then batch destroy via SDK (never `kubectl delete pods --all`).

## Invariants (checked after every round)

- No orphan `--pvisor-shim-internal-vm` runners (parent shim gone).
- No stale `/run/pvisor-bridge-*.sock` / `/run/pvisor-egress-*.json` for
  dead tasks.
- pvisor pod count in `pvisor-load`/opensandbox namespaces matches SDK-owned
  fleet; no Terminating pileup.
- Business pods all Ready; containerd journal free of pvisor shim errors
  (new occurrences after round start).
- Node safety: `MemAvailable` ≥ 80 GiB stop line, load sane, D-state count
  bounded.

## Safety limits

- Batch ≤ 100 sandboxes with convergence wait; fleet ≤ 300 concurrent VMs.
- Stop new launches if MemAvailable < 80 GiB or business pods unhealthy.
- Kills target only proven-owned pvisor test processes (cmdline audited).
- containerd restarts allowed (documented, tested recovery path).
- No `kubectl delete pods --all`, no disk fill, no business-data mutation,
  no host network changes, no semspec approval changes.
- Batched deletion only (runbook §6/§9.1 discipline).

## Execution model

- Driver: `/root/chaos8h-osb/driver.py` on the node (osb-venv), serial
  rounds, JSONL journal `/root/chaos8h-osb/journal.jsonl` + round reports.
- The agent session periodically reviews the journal, investigates anomalies
  (containerd journal, `/var/log/pvisor-shim/`, shim source), repairs code,
  rebuilds locally, redeploys the binary, and restarts/continues the driver.
- Every confirmed defect requires: reproducer evidence, focused fix,
  regression validation (`mise run test -- pvisor-shim` or targeted suite),
  then redeploy.
