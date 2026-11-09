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
| `r01-format-fixed` | same formatting command | 14:22:32.659–14:22:32.743 | 0.083 | exit 0 after applying formatting-only changes |
| `r01-post-format-targeted` | same exact two-test targeted command as `r01-regression-fixed` | 14:24:06.770–14:24:11.551 | 4.779 | **2 passed, 151 skipped** on final formatted source |

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
Final changes after package validation were formatting only; the exact two-test
targeted check was then rerun against the final formatted source at the parent's
request. Its nextest run ID is `ce2659e9-1177-4e72-bbf6-d731b7b41169`.
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

Independent static review arranged by the parent found no blocking issues.
It noted a nonblocking coverage gap: the empty-ID completed sequence proves
acceptance/reopen, but does not independently prove unresolved empty-ID ambiguity
tracking. That remains a follow-up, not a claimed tested property of this round.

### Round 02 — scoped approval narrowing, denial and policy reuse

Inspection found a divergence from the existing request-scoped API contract:
leaf `NetworkPolicy::one_time_grant` discarded unrelated grants, but Scoped
capabilities appended the approved rule to existing scope/fallback rules and
retained default-allow behavior. A new external contract test reproduced
authorization of `existing.example` through a temporary grant intended only
for `new.example:443/TcpTunnel`.

The minimal fix changes only scoped grant derivation in
`crates/pvisor-core/src/network.rs`: each scope uses default deny and the
singleton approved rule; the fallback is narrowed likewise. Explicit denies
and bandwidth limits remain, including per-scope and fallback constraints.
The source config is cloned, never mutated. Fallback Deny still refuses audit
extension. This is Core's embedding approval extension; it does not add an
interactive approval feature to the CLI.

Two external tests cover six default-action/fallback combinations, unrelated
preexisting and ambient permissions, changed port/transport, private resolution,
exact explicit-denial results, unchanged bandwidth constraints, failed-approval
retries and subsequent source-policy reuse. Separate denial cases locate
requested-destination denial in Session, Workspace, fallback policy and a
fully denied fallback. Scoped evaluation intersects all scopes and fallback;
approval never overrides an explicit deny.

All commands use the same supervised nice/offline/two-job/one-test-thread
environment as round 01:

| Evidence name | Command after supervisor | UTC interval on 2026-11-09 | Monotonic seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r02-regression` | `mise run test -- pvisor-core -- --offline --test-threads 1 -E 'test(scoped_approval_is_request_scoped_without_poisoning_the_source_policy)'` | 14:26:07.292–14:26:17.746 | 10.453 | **1 failed**, 88 skipped: unrelated existing host authorized |
| `r02-regression-fixed` | same targeted command | 14:26:50.997–14:26:55.434 | 4.436 | **1 failed**, 88 skipped: test fixture incorrectly assumed a fallback allowlist allowed `existing.example` |
| `r02-targeted-final` | `mise run test -- pvisor-core -- --offline --test-threads 1 -E 'test(scoped_approval_)'` | 14:27:20.581–14:27:21.262 | 0.680 | 2 passed, 88 skipped |
| `r02-package` | `mise run test -- pvisor-core -- --offline --test-threads 1 --no-fail-fast` | 14:27:21.309–14:27:22.105 | 0.794 | 90 passed, 0 skipped |
| `r02-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-core/src/network.rs crates/pvisor-core/tests/network_contracts.rs` | 14:27:22.151–14:27:22.250 | 0.097 | exit 1: one new multiline assertion formatting difference |
| `r02-final-package` | same full package command | 14:27:43.382–14:27:44.411 | 1.028 | **90 passed, 0 skipped**, including final strengthened deny assertions |
| `r02-format-final` | same formatting command | 14:27:44.456–14:27:44.555 | 0.097 | exit 0 |
| `r02-overlaynet-consumer` | `mise exec -- cargo check --offline --locked -p pvisor-overlaynet --all-targets` | 14:27:44.599–14:27:59.687 | 15.087 | exit 0 |

The test-fixture correction added `existing.example` to that fallback allowlist,
so the original source actually authorized it before the temporary grant.
Source-policy preservation compares the original exact decision instead of
incorrectly deriving it from only the top layer's default action. This corrects
the independent oracle for conjunctive scopes, not a weakened product check.
Original unexpected outputs remain in raw evidence. Formatting was applied
before the final full-package run. Nextest final package run:
`0907bfcb-4eb3-4084-bbb0-56624227229f`.

Unchanged HEAD/Cargo.lock as above. Final SHA-256:

- `crates/pvisor-core/src/network.rs`:
  `de84d155ca8dbdcea6016a7951c5a2485ee3c2f0cdd3e715a5f246a9f8c44117`
- `crates/pvisor-core/tests/network_contracts.rs`:
  `5e877fc06c196418a8bc37cc996e5927805e5cd7490d59a41ab446b7eb892df6`
- `crates/pvisor-core/README.md`:
  `812b276656f21d82b57b10f53b1b66a3496a7084b9ea5473a17a71d439914a6c`

Round safety samples: minimum MemAvailable 10,030,182,400 bytes; minimum disk
268,444,798,976 bytes; maximum load1 4.0777; maximum swap used 7,492,120,576
bytes. Growing ambient swap use is recorded without attributing it to tests.
No failed gate, termination, survivor or fixture mount. Local TMPDIR empty;
`git diff --check` passed. Consumer validation covers OverlayNet all-target
compilation, not runtime audit transport or all workspace consumers. No Python,
clippy/rustdoc/doctests or cross-platform execution.

### Round 03 — snapshot destination failures and independent retry recovery

Continuation investigation 14:32:03.923–14:34:23.640Z selected a new ordinary
public-API scenario after reading journal contracts and existing snapshot tests.
The new external `tests/snapshot_recovery.rs` exercises nine combinations:
destination WriteZero, Other and BrokenPipe at zero bytes, the complete header,
and one byte before EOF. Every destination also first returns two Interrupted
errors and thereafter accepts only seven-byte short writes. Each failure checks
the exact error kind, exact partial output, unchanged source bytes and records,
quiet original-event retry, and a fresh successful snapshot from byte zero.
The sequence finishes with a causal append through a clone, exact notification,
LocalSync/contiguous offset checks and closed-file validate/read/reopen retries.
No implementation defect was found; this is one new scenario, not nine new
tests or 32 new scenarios. The inherited header test is not newly authored work.

All commands retain the inspected supervisor's nice/offline/two-job/one-thread
limits. `python3 target/chaos8h/supervise.py NAME` precedes these exact commands:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r03-snapshot-targeted` | `mise run test -- pvisor-journal -- --offline --test-threads 1 -E 'test(snapshot_output_faults_preserve_source_receipts_and_reopen)'` | 14:33:32.985–14:33:43.821 | 10.835 | 1 passed, 31 skipped |
| `r03-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-journal/tests/snapshot_recovery.rs` | 14:33:56.215–14:33:56.301 | 0.084 | exit 1: array formatting only |
| `r03-package` | `mise run test -- pvisor-journal -- --offline --test-threads 1 --no-fail-fast` | 14:34:09.101–14:34:10.733 | 1.631 | **32 passed, 0 skipped** |
| `r03-format-final` | same formatting command | 14:34:10.780–14:34:10.864 | 0.082 | exit 0 |

Two test executions ran 33 test instances; two additional executions checked
formatting. Final package run ID `90e64e16-bf77-4a0c-8076-21419dcad3c4`.
New test SHA-256:
`9bf679300d03fe43924e3f018292f3f02be8f2e3fb62a9b7cd5a6a5e90ef4748`.
Inherited journal contract source hash remains exactly as recorded in round 01.
Final formatting preceded the full package run.

Minimum sampled available RAM 10,617,974,784 bytes; free disk 268,236,984,320
bytes; maximum load1 3.4932 and swap used 7,714,291,712 bytes. All admission
gates passed, no stop/termination, all survivor lists empty. At 14:34:23Z the
worktree TMPDIR was empty, fixture mount audit empty and `git diff --check`
passed. No real broken pipe/device-full condition, power-loss, FUSE mount,
Python, doctest, clippy, rustdoc or cross-platform run is claimed.

### Round 04 — local backend copy error, healthy retry and stage reopen

Investigation 14:34:23.640–14:37Z inspected existing direct-copy and prepared
copy contracts. Extended the existing conventional test backend, retaining its
original rejection test unchanged in meaning, with an explicit healthy toggle
and copy counter. New `direct_copy_failure_retries_cleanly_then_reopens_without_recopying`
uses eight combinations: ordinary/prepared copy, a partial EIO/short-success
length mismatch, and upper-root/separate-work scratch placement. Two rejected
attempts each leave no scratch entries or published upper file. A healthy retry
copies exactly once; subsequent copy-up and owner reopen preserve staged edits,
inode identity and lower bytes without invoking the backend again. This is one
new scenario and one new test. No product defect was found.

Commands use `python3 target/chaos8h/supervise.py NAME` and the unchanged
nice/offline/two-job/one-test-thread environment:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r04-direct-copy-targeted` | `mise run test -- pvisor-overlay-core -- --offline --test-threads 1 -E 'test(direct_copy_failure_retries_cleanly_then_reopens_without_recopying)'` | 14:36:11.475–14:36:19.905 | 8.428 | 1 passed, 248 skipped |
| `r04-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-overlay-core/tests/direct_copy.rs` | 14:36:28.026–14:36:28.111 | 0.084 | exit 0 |
| `r04-package` | `mise run test -- pvisor-overlay-core -- --offline --test-threads 1 --no-fail-fast` | 14:36:28.157–14:36:33.181 | 5.022 | **244 passed, 5 default ignored** |

Full-package run ID `0952ab77-9b4a-4e8a-a1fc-73cde8939868`.
Two test executions ran 245 test instances, plus one formatting execution.
Final `crates/pvisor-overlay-core/tests/direct_copy.rs` SHA-256:
`5ce85365b73682e75487371571838d7f631eeffc88427985a0ac9e6941fc36d7`.
Source/lockfile identity remains the continuation identity above.

**Containment protocol deviation:** the full-package validation was broader
than the inspected scenario. Existing apply tests create canonical
`/tmp/pvisor-apply-locks-1000` lock files independently of TMPDIR. A read-only
audit observed **150 zero-byte files** with mtimes inside this command's
interval, 14:36:28.598658–14:36:33.078517Z; directory and files were owned by
UID 1000. There was no prelaunch directory inventory, so these are observed
matching files, not 150 proven newly created files. No files outside the
worktree were removed or edited for cleanup. Raw bounded names/inodes/times
are in `target/chaos8h/r04-outside-artifact-audit.json`. The parent was notified
immediately; its instruction is to retain these normal helper artifacts and
not delete unknown shared state. This round does **not** satisfy universal
worktree-only fixture confinement. Subsequent selections will be inspected for
all fixture paths before running; broad apply suites are excluded.

Minimum sampled available RAM 10,495,451,136 bytes; disk 268,094,029,824 bytes;
maximum load1 3.1373 and swap used 7,670,259,712 bytes. All resource gates passed.
No owned survivors or fixture mounts; worktree TMPDIR empty and `git diff --check`
passed. At 14:37Z no matching nextest/build/test/supervisor workload remained.
The five preexisting ignored tests were not explicitly selected. No real service,
mount, actual device failure, power loss, Python, doctest, rustdoc, clippy or
cross-platform execution is claimed.

### Round 05 — reused empty native call IDs and terminal receipt ordering

The round 01 static-review follow-up identified a real coverage gap, not a
confirmed product defect: complete empty-ID sequences did not independently
prove unresolved tracking. A new conventional journal test covers both terminal
outcomes, prior completed work followed by reused empty IDs, a tool finish
without run completion, unresolved starts across terminal runs, and complete
reopens. Rejected admission is repeated, preserves exact bytes, reports the
empty ID, and releases its run lock. No implementation repair was needed.

After the round 04 confinement discovery, the entire journal-only execution
surface was inspected: its fixtures use `tempfile::tempdir()` and journal I/O
below that root, hence this supervisor's worktree TMPDIR. Only library journal
tests were selected, not process, bridge, model or full-package tests.

Commands use the same supervised environment:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r05-journal-targeted` | `mise run test -- pvisor-replay -- --offline --test-threads 1 --lib -E 'test(journal::tests::)'` | 14:39:49.486–14:40:10.819 | 21.331 | 10 passed, 130 skipped |
| `r05-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-replay/src/journal.rs` | 14:40:10.866–14:40:10.950 | 0.082 | exit 1: new append-call formatting |
| `r05-final` | same selected journal test command | 14:40:41.792–14:40:45.597 | 3.804 | **10 passed, 130 skipped** |
| `r05-format-final` | same formatting command | 14:40:45.643–14:40:45.731 | 0.086 | exit 0 |

Two test executions ran 20 instances; the new scenario itself ran twice.
Final nextest run ID `b3e9c8f3-bc73-48b6-b660-df7759f83ccd`.
Final source `crates/pvisor-replay/src/journal.rs` SHA-256:
`9ab7a871f6496a57cf703357349fa7ab04c40dc52bf875b5a104e2a361efc66d`.
Only a new test was added; the published round 01 production logic is preserved.
Minimum sampled RAM 10,237,308,928 bytes, disk 267,896,422,400 bytes;
maximum load1 2.9410 and swap used 7,660,560,384 bytes. All workload resource
gates passed; owned survivor lists empty.

**Inactive tool gap:** after the final format check, the same terminal command
included the read-only discovery `MISE_OFFLINE=true mise exec -- cargo nextest run --help`
without the supervisor's trusted-config environment. It blocked on an interactive
trust prompt; no trust option was accepted. Terminal's 600-second bound
terminated the whole invocation at 14:50:41.710Z. The interval from
14:40:45.731Z to that termination, **595.979 seconds**, is not active testing
or investigation. No descendant/supervisor/nextest/rustc process with this
worktree cwd remained in the 14:51:02Z read-only process audit. Local TMPDIR and
worktree mount audit were empty; `git diff --check` passed. No persistent trust
configuration was intentionally changed. All subsequent mise discovery must
also run through the supervisor, with a <=60-second outer discovery bound.

The direct alternative, supervised `mise exec -- cargo nextest run --help`,
completed at 14:51Z in 0.082 seconds (`r06-nextest-options`), with no prompt,
failed gate or survivor. It documents the supported finite stress options;
it is discovery, not a test execution.

## Checkpoints, gaps and limitations

This document is a live checkpoint, not a claim of completed eight-hour testing.
Each completed round will record exact commands, timings, source identity,
results, cleanup and coverage limitations. Testing gaps and interruptions will
be recorded separately; supervised workload time is not the same as elapsed
campaign time.

### Worker interruption and continuation

The original worker stopped after round 02 because of a provider content-safety
error. Its blocked scenario is not retried or reconstructed. The last recorded
workload ended 14:27:59.687Z; a new isolated continuation worker first observed
the worktree at 14:32:03.923080673Z (CLOCK_BOOTTIME 176375.96, same boot ID).
The intervening 244.236 seconds are not claimed as active testing. The parent
independently reported no matching survivors at 14:31:28Z.

Continuation HEAD is `90d4921f29355dc92720e62c56c44a30c74477f2`;
the round 01 replay source and round 02 Core source hashes match their recorded
final identities. Cargo.lock SHA-256 is
`7ba7442e1d003cb6b66be416504fb086324608de5e984ce84e1ccf94006c38a4`.
The inherited journal contract addition and prior reports are preserved.
The fixed deadline is unchanged. The existing local supervisor was inspected
before reuse, including its prompt adopted-zombie reaping.
