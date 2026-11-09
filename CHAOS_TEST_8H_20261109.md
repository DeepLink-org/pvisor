# pVisor eight-hour chaos campaign — 2026-11-09

## Window and provenance

- Requested fixed window: 2026-11-09T14:14:45.971695Z through
  2026-11-09T22:14:45.971695Z; never extended.
- Expected Linux boot ID: `504c40ca-6c67-41ab-8079-892a76aad3df`.
- Start CLOCK_BOOTTIME: 175338.010742897; deadline: 204138.010742897.
- Start monotonic: 175338.010741735.
- First worker observation: 2026-11-09T14:15:54.244539Z, boottime
  175406.283647722. Boot matched; UTC/elapsed offset agreed. The preceding
  68.273 seconds were not observed by this worker and are not active testing.
- Initial HEAD: `2eb6eebd1ff63963099b69d28398721fc3696db5`.
- Inherited modifications preserved: `CHAOS_TEST_NEW.md`,
  `CHAOS_TEST_NEW_02.patch`, `CHAOS_TEST_NEW_03.md`, and the journal
  `tests/api_contract.rs` addition. No inherited result counts as new evidence.
- Read root AGENTS.md and README.md. Plugins, global half-close policy and
  semspec approval artifacts are out of scope.

## Safety protocol and evidence

Only this worker launches test/build workloads, serially, with
`CARGO_BUILD_JOBS=2`, explicit nextest `--test-threads 1`, `nice -n 10`,
offline Cargo/mise and worktree-local temporary fixtures. Each launch is
admitted against MemAvailable >= 6 GiB, worktree disk free >= 20 GiB and
load1 <= min(8, online CPU count). A new process-session watchdog bounds each
command to at most 580 seconds, samples every five seconds, terminates owned
work on deterioration, escalates TERM to KILL after five seconds, and audits
survivors. No global cleanup or unrelated PID signals.

Initial sample: MemAvailable 13,374,337,024 bytes, disk free 269,170,847,744
bytes, load1 2.2876 (16 CPUs), swap used 6,758,268,928 bytes. Swap is observed,
not changed. There was no existing worktree target cache at first observation.
Bounded raw logs and per-command JSON are retained under ignored
`target/chaos8h/`. Output cap is 16 MiB per command. No commits/publication.

## Round ledger

### Round 01 — malformed persisted replay receipts and owner restoration

New scenario selected after inspecting replay admission, journal decoding,
native adapter ID construction and existing replay contract tests. Existing
trajectory-corruption and torn-tail tests did not cover a complete, Core-valid
observation whose replay-specific receipt payload is malformed.

**Defect reproduced:** persisted boolean `tool_started` payload panicked in
`serde_json::Value` string indexing instead of returning an admission error.
Missing/non-string call IDs could also be silently ignored by ambiguity tracking.
This is malformed local state, not a remote exploit or proof of physical
filesystem corruption. Input was written through the public journal API.

**Minimal repair:** `crates/pvisor-replay/src/journal.rs` decodes replay
observations as objects before adding their event discriminator and requires a
string call ID specifically for `tool_started`/`tool_finished`. Other domains
are ignored as before, unknown replay object fields/names remain accepted.
Inspection found Codex/OpenCode accept empty string IDs, so **empty IDs remain
valid** and are explicitly exercised through start/finish/terminal/reopen. No
native adapter contract was tightened.

New tests cover 16 malformed receipt combinations (eight payload forms, two
receipt names), two byte-preserving rejection attempts apiece, diagnostic-read
rejection, explicit owner restoration, empty/nonempty IDs, healthy completion
and reopen. A separate scope test covers unrelated domains, arbitrary payloads,
and future replay object observations. README describes the rule.

Commands below all run through `python3 target/chaos8h/supervise.py NAME ...`,
which prepends `nice -n 10` and the documented environment. Test commands use
the exact project runner; positional package selection runs Rust only.

| Evidence name | Command after supervisor | UTC interval on 2026-11-09 | Monotonic seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r01-regression` | `mise run test -- pvisor-replay -- --offline --test-threads 1 -E 'test(malformed_receipts_reject_without_mutation_then_allow_explicit_restore)'` | 14:18:17.783–14:18:44.292 | 26.507 | **1 failed**, 151 skipped; original panic retained |
| `r01-regression-fixed` | `mise run test -- pvisor-replay -- --offline --test-threads 1 -E 'test(malformed_receipts_reject_without_mutation_then_allow_explicit_restore) \| test(receipt_validation_preserves_other_domains_and_open_observation_fields)'` | 14:20:49.316–14:20:54.079 | 4.761 | 2 passed, 151 skipped |
| `r01-package` | `mise run test -- pvisor-replay -- --offline --test-threads 1` | 14:20:54.124–14:21:03.640 | 9.514 | 123 passed, 1 failed; 29 not run |
| `r01-process-harness-check` | `mise run test -- pvisor-replay -- --offline --test-threads 1 -E 'test(cleans_background_descendants_after_the_leader_exits)'` | 14:21:34.898–14:21:35.428 | 0.528 | 1 passed, 152 skipped |
| `r01-package-fixed-harness` | `mise run test -- pvisor-replay -- --offline --test-threads 1 --no-fail-fast` | 14:21:35.473–14:21:56.641 | 21.166 | **153 passed, 0 skipped** |
| `r01-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-replay/src/journal.rs` | 14:22:07.471–14:22:07.572 | 0.099 | exit 1: two new formatting differences |
| `r01-format-fixed` | same formatting command | 14:22:39 approximately | 0.083 | exit 0 after applying formatting-only changes |

The first package failure was a **supervisor instrumentation defect**, not a
product repair: enabling child-subreaper ownership without promptly reaping
adopted zombies made an existing `kill(pid, 0)` disappearance assertion fail.
The supervisor now reaps its adopted zombies during the workload rather than
only at final cleanup. The exact failing test then passed unchanged, followed
by the complete package. Original evidence remains intact. No product assertion
or timeout was relaxed. Supervisor code is retained locally in ignored target,
not proposed as production code.

Source identity: unchanged HEAD and Cargo.lock above. Final journal source
SHA-256 `4bce1859478dfc4f991414150c3dca8dfed1d2a74b223c40f1a63a7b5867b933`;
README `b05420cf0eb7100f3fb071c9e249a68482c4c520c0e75c1d43e355ef5d0f5c64`.
Final changes after package validation were formatting only.
Inherited journal test SHA-256 remains
`ae821548276faf1602832271de71cfc343a2b982ae0496bfefda2b1733e1c34a`.

Safety across first six command samples: minimum MemAvailable 10,376,253,440
bytes, minimum free disk 268,573,650,944 bytes, maximum load1 5.1729 and maximum
observed swap used 6,898,970,624 bytes. No gate failed or termination was needed.
All owned command survivors lists were empty. Worktree fixture TMPDIR was empty
and `/proc/self/mountinfo` contained no worktree fixture mounts at the round
audit. `git diff --check` passed. No Python suite, clippy, rustdoc, doctests,
live model/network service, other-platform or workspace-wide validation was run.
This is one newly designed scenario with its regression/validation executions,
not 153 new chaos scenarios.

## Checkpoints, gaps and limitations

This document is a live checkpoint, not a claim of completed eight-hour testing.
Each completed round will record exact commands, timings, source identity,
results, cleanup and coverage limitations. Testing gaps and interruptions will
be recorded separately; supervised workload time is not the same as elapsed
campaign time.
