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

### Round 06 — cancelled queued durable appends retain the last writer owner

Existing cancellation coverage used volatile memory or an unpolled future.
A new external `tests/async_cancellation.rs` uses the documented public API and
a deliberately occupied one-thread Tokio blocking pool. Zero, one and two
manual polls distinguish never-submitted work from accepted queued work. After
dropping every caller journal/producer/future, only accepted work retains the
writer and notification sender. Releasing the bounded gate must commit once,
notify once, close the last sender, release its lock, validate/read, and resolve
an exact close/reopen retry without rewriting or duplicate notifications.
The gate has its own five-second fail-safe; readiness/commit checks are bounded.
This is ordinary cancellation/ownership testing, not a storage or process fault.
No product defect was found.

All journal fixture paths and subprocess cwd handling were inspected before
the package run. Fixtures follow the supervisor's local TMPDIR; no canonical
external lock path is used by this package. Commands have the usual supervisor
prefix and environment:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r06-queued-cancellation` | `mise run test -- pvisor-journal -- --offline --test-threads 1 --test async_cancellation` | 14:53:07.645–14:53:08.320 | 0.674 | 1 passed |
| `r06-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-journal/tests/async_cancellation.rs` | 14:53:08.366–14:53:08.449 | 0.081 | exit 0 |
| `r06-package` | `mise run test -- pvisor-journal -- --offline --test-threads 1 --no-fail-fast` | 14:53:47.595–14:53:49.067 | 1.470 | **33 passed, 0 skipped** |
| `r06-cancellation-repeat` | `mise run test -- pvisor-journal -- --offline --test-threads 1 --test async_cancellation --stress-count 1000 --status-level fail --final-status-level fail --show-progress none` | 14:53:49.114–14:53:59.115 | 10.000 | **1000 iterations passed** (9.540s nextest time) |

This is one newly designed scenario, three test invocations, 1,034 test
instances including 1,000 repetitions, not 1,000 new scenarios.
Package run ID `e6335582-306e-433f-962c-9469c5b95a6e`; finite repetition run ID
`1aa6afa1-ad2f-4335-a80f-6d97f6eab4f4`. New test SHA-256:
`d77568d41b53c4ab2ced4a4e624e4e0bbdecb6dfe27f98135274fbde855f1afd`.
Minimum sampled RAM (including supervised help discovery) 10,399,309,824 bytes;
disk 267,898,032,128 bytes; maximum load1 2.2847, swap used 7,656,476,672 bytes.
No failed gates, stop reason or survivors. TMPDIR/mount audit empty and
`git diff --check` passed at 14:54Z. No consumer, Python, doctest, clippy,
rustdoc or cross-platform validation was performed.

### Round 07 — Linux CI same-Session review failure triage (not reproduced)

The parent supplied Linux CI run `42636228764`, job `129225272460`: 323 passed,
one failed and 40 skipped. The failed same-Session exec fixture reached embedded
Python `KeyError: 'filesystem'` during final review. This is external CI evidence,
not a local campaign test result. No full-green CI result is claimed.

Static inspection confirms normal successful `review --json` still includes
the Bundle's filesystem. Its separate successful diagnostic response for unknown
completion deliberately omits filesystem and reports
`state: unknown`, `code: COMPLETION_UNCOMMITTED`. A missing completion receipt
is therefore a plausible path to the KeyError, **not a reproduced root cause**.
The fixture previously discarded the diagnostic JSON and owner/kill results in
this failure. A minimal diagnostics-only change keeps the original required
filesystem/child.txt assertion, adds an explicit filesystem-key assertion with
the complete review response and kill/owner return codes/stdout/stderr, and
retains all lifecycle, readiness, isolation and process-cleanup assertions.
No product behavior or expected success was changed.

The real CLI fixture was **not executed locally**: it requires safe Host
namespace/FUSE lifecycle and canonical `/tmp/pvisor-host-UID` authority without
a discovered contained override. Broadening that shared state/configuration is
outside campaign permission. The parent explicitly agreed to static diagnostics
and independent contained follow-up rather than running this fixture.

Supervised commands and outcomes:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r07-cli-compile` | `mise run test -- pvisor-cli -- --offline --test-threads 1 --test session_exec_cli --no-run` | 14:56:26.897–14:57:30.410 | 63.512 | compiled; **zero tests executed** |
| `r07-python-parse` | `python3 -c` reading CASE from `session_exec_cli.rs` and calling `ast.parse` | 14:57:43.543–14:57:43.592 | 0.047 | syntax valid; CASE not evaluated |
| `r07-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-cli/tests/session_exec_cli.rs` | 14:57:43.639–14:57:43.746 | 0.106 | exit 0 |

Exact parse command is retained in `r07-python-parse.json`. New source SHA-256:
`d47766e9aca4ff6b539606cc14c4d5720ebef76a62d108b45575dfcc58c8a0b7`.
This round contains **no newly executed fault scenario** and no validated repair
of the CI failure. Compilation checks Rust embedding, not Python runtime
semantics or real Session cleanup. Nextest correctly warns test-threads is
ignored for no-run. Minimum sampled RAM 9,469,300,736 bytes; disk
267,576,438,784 bytes; maximum load1 4.2505, swap used 8,002,011,136 bytes.
No failed gate/termination/survivor. At 14:57:53Z local TMPDIR and fixture
mount audit were empty and `git diff --check` passed.

### Round 08 — real slow progress, idle cancellation and fresh-process recovery

Added an explicitly ignored conventional native-process lifecycle test after
inspecting the existing process runner and cancellation fixtures. It runs two
sequential pipe/redirected-stdout cases. Each child emits progress at 20, 40 and
60 seconds, including stderr-only progress at 40 seconds, then stalls with
512 KiB configured stdin pending. The real 30-second idle timer must refresh
from both output sources and terminate around 90 seconds rather than the
120-second wall deadline. Cooperative termination must receive stdin EOF,
retain final stdout/stderr, reap the child and remove its process group. A
fresh small native child then succeeds without cleanup/timeout flags.

These approximately three minutes are **observed low-load lifecycle/timer
testing**, not inactivity counted as work. Per-child 150-second independent
alarms bound a broken owner. No shell interpreter, external agent, network,
mount, shared host authority or real-service mutation is used by the new case.
Only an owned native re-execution and small local disposable files are involved.
The default suite stays fast by leaving the long test ignored; README supplies
its explicit selection. No production behavior was changed or defect found.

Supervised exact commands:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r08-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-replay/src/process.rs` | 15:00:18.259–15:00:18.361 | 0.101 | exit 1: two new formatting differences |
| `r08-slow-idle` | `mise run test -- pvisor-replay -- --offline --test-threads 1 --lib --run-ignored only -E 'test(slow_progress_then_idle_cancellation_drains_and_allows_a_fresh_process)'` | 15:00:38.984–15:03:42.688 | 183.702 | **1 passed (180.058s test), 140 skipped** |
| `r08-related-process` | `mise run test -- pvisor-replay -- --offline --test-threads 1 --lib -E 'test(idle_timeout_still_fires_when_redirect_stalls) \| test(redirected_stdout_growth_refreshes_idle_watchdog) \| test(cancellation_closes_pending_stdin_before_graceful_termination) \| test(times_out_and_reaps_the_foreground_process_group) \| test(stdin_backpressure_does_not_suspend_wall_or_idle_supervision)'` | 15:04:20.177–15:04:28.568 | 8.390 | **5 passed, 136 skipped** |
| `r08-format-final` | same formatting command | 15:04:28.614–15:04:28.709 | 0.094 | exit 0 |

One newly designed scenario, two test invocations, six test instances. The four
native subprocesses in the new scenario are two stalled+recovered pairs, not
four independent scenarios. New test run ID `77357c8f-ed45-4073-ab2f-007a08797ae1`;
related run `e20d8888-a187-45bc-8291-ad18077ab1fb`.
Source SHA-256 `crates/pvisor-replay/src/process.rs`:
`7522237d717524bb46fb8dabad89bf685bc48aee1d9c6089c83508fb5e52f8ec`;
README:
`52f686510ffc69bc7505dbfacd08eda6ccb67e83bfd2ef18cf985fa8d0bb1ee3`.
Formatting was applied before execution; README-only documentation followed.

Minimum sampled available RAM 10,776,887,296 bytes, disk 267,498,651,648 bytes;
maximum load1 3.671875, swap used 7,976,529,920 bytes. Samples every five
seconds passed all gates. No termination/survivors; no matching owned workload
at 15:04:45Z. TMPDIR/mount audit empty; the two transient crate-local `.tmp*`
directories reported by Delta during existing fixtures were both already gone.
`git diff --check` passed. Broader package, live model, Python, doctest, rustdoc,
clippy and non-Linux platforms were not run.

### Round 09 — delayed response error, sibling cancellation and slot recovery

Inspected OverlayNet's public router/sink, body-owned admission and the existing
in-memory bandwidth lifecycle tests. New `tests/stream_recovery.rs` uses an
always-accepting local sink and literal-IP requests; it opens no socket, does no
DNS or upstream connect and creates no filesystem fixture. One body produces an
exact prefix then a delayed BrokenPipe; another remains pending. Independent
healthy requests at 45 and 90 seconds must leave both retained bodies active.
Dropping the sibling and observing the error must release only their owners.
Finally, admitting all 128 retained bodies, rejecting the 129th with 429, dropping
them and serving another healthy request proves actual semaphore recovery, not
just a counter decrement.

The ordinary test uses paused Tokio time; an explicitly ignored twin executes
the same lifecycle against **90 seconds of real Tokio timers** with a 120-second
outer deadline. This is one designed scenario exercised with two clock modes,
not transport reconnect or a real upstream read-timeout test. No product defect
was found. README documents the contained scope and explicit real-timer command.

Supervised commands:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r09-stream-virtual` | `mise run test -- pvisor-overlaynet -- --offline --test-threads 1 --test stream_recovery -E 'test(delayed_body_error_and_cancelled_sibling_release_only_their_own_requests)'` | 15:07:52.988–15:08:03.500 | 10.510 | 1 passed, 1 skipped |
| `r09-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-overlaynet/tests/stream_recovery.rs` | 15:08:03.545–15:08:03.628 | 0.082 | exit 1: formatting only |
| `r09-stream-virtual-final` | same virtual command, after adding full-capacity assertions | 15:08:27.029–15:08:27.924 | 0.892 | 1 passed, 1 skipped |
| `r09-stream-real` | `mise run test -- pvisor-overlaynet -- --offline --test-threads 1 --test stream_recovery --run-ignored only -E 'test(real_delayed_body_error_retains_admission_then_recovers)'` | 15:08:27.972–15:09:58.470 | 90.497 | **1 passed (90.018s test), 1 skipped** |
| `r09-related-contracts` | `mise run test -- pvisor-overlaynet -- --offline --test-threads 1 --test bandwidth_lifecycle --test stream_recovery` | 15:10:33.385–15:10:34.282 | 0.896 | **3 passed, 1 ignored** |
| `r09-format-final` | same formatting command | 15:10:34.329–15:10:34.411 | 0.081 | exit 0 |

Four test invocations ran six test instances. Real run ID
`f668336c-dd29-4279-ad18-1289208f4f6b`; final related run
`1cc50034-fce1-4f05-bfeb-0ad7579175ba`. Final test SHA-256:
`3f43b2d102eb00ccb52cf8fc040da64772d03ce67e7fde58a8ed38a4264a69ca`;
README:
`660f2ee4558baea833b3d962bada5cab340c0695195876a9d6eacafff9339c76`.

Minimum sampled RAM 10,529,202,176 bytes; disk 267,444,760,576 bytes;
maximum load1 3.2740, swap used 7,963,992,064 bytes. All gates passed; no
stop/termination or owned survivors. TMPDIR and mount audit empty, diffcheck
passed at 15:10:57Z. Broader OverlayNet tests were intentionally not run:
peer fixtures use canonical `/tmp`, and the scenario requires neither peer
networking nor live transport. No full package/platform/Python/doctest/rustdoc/
clippy or external service coverage is claimed.

### Round 10 — namespace failure/retry trace against a native filesystem oracle

New conventional external `tests/namespace_recovery.rs` exercises a fixed
28-operation trace through the existing public in-process FilesystemService.
An independent disposable native tree supplies expected errno and visible
names/types/bytes. Fourteen ordinary type, existence and nonempty-directory
errors are interleaved with successful rename replacement, deletion, whiteouts
and file/directory recreation. Every operation checks lower contents unchanged,
compares the complete visible tree, drops/reopens the owner and compares again,
including after each rejected operation. Work scratch must be empty.

The final trace runs three variants: no journal, legacy preimages and compact
preimages, for 84 operations and 84 reopens per test execution. Both journaled
variants check the original first-touch baseline remains after all replacements.
This is one designed failure/recovery scenario with three storage variants,
not 84 new scenarios. No implementation defect was found.

Both selected test binaries were inspected for fixture confinement; they use
small owned paths under local TMPDIR and no mounts, apply/global-lock helper,
host-control authority or network. Full overlay-core package validation was
deliberately **not** repeated after round 04's containment deviation.

Supervised commands:

| Evidence name | Command | UTC interval | Elapsed seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r10-namespace-trace` | `mise run test -- pvisor-overlay-core -- --offline --test-threads 1 --test namespace_recovery` | 15:13:08.238–15:13:08.857 | 0.617 | 1 passed, initial no-journal trace |
| `r10-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-overlay-core/tests/namespace_recovery.rs` | 15:13:49.679–15:13:49.763 | 0.082 | exit 1: multiline assertions |
| `r10-journal-variants` | same selected test command | 15:14:04.662–15:14:05.316 | 0.652 | 1 passed, all three variants |
| `r10-format-final` | same formatting command | 15:14:05.361–15:14:05.442 | 0.080 | exit 0 |
| `r10-related-contracts` | `mise run test -- pvisor-overlay-core -- --offline --test-threads 1 --test namespace_recovery --test direct_copy` | 15:14:43.270–15:14:43.770 | 0.499 | **3 passed, 0 skipped** |

Three test invocations ran five test instances; the new trace itself ran three
times (one initial + two final three-variant executions), not five new traces.
Final nextest run ID `c9604e1d-f8cd-4feb-9d91-0e7ff3b69113`.
Final test SHA-256
`1a8f2938878645cc33ccfe776fecf030a1280a866e809035fe848d361057ed65`;
README
`002a19bf6c53f0ebf6a56325ca0a8eba0ddd91447a92b327c500353f8aa1f406`.
README documents the finite trace scope, not arbitrary POSIX equivalence.

Minimum sampled RAM 10,918,133,760 bytes; disk 267,418,169,344 bytes;
maximum load1 3.0245, swap used 7,955,509,248 bytes. All admission gates passed,
no stop/termination/survivors. TMPDIR/mount audit empty; diffcheck passed at
15:14:55Z. This is in-process namespace/reopen coverage, not mounted protocol,
apply, power-loss, Python, doctest, rustdoc, clippy or other-platform coverage.

### Round 11 — finite real-timer lifecycle repetition under normal host load

Repeated the already inspected round 08 native idle/progress/cancellation
scenario three times to check scheduling/cleanup stability over a longer real
lifecycle run. This adds **zero new scenarios or permanent tests**. Each
iteration still executes the two independent 90-second pipe/redirected cases
and their fresh-process recovery checks, rather than sleeping outside a test.

Exact supervised command:

```sh
python3 target/chaos8h/supervise.py r11-slow-idle-repeat \
  mise run test -- pvisor-replay -- --offline --test-threads 1 --lib \
  --run-ignored only \
  -E 'test(slow_progress_then_idle_cancellation_drains_and_allows_a_fresh_process)' \
  --stress-count 3 --status-level slow --final-status-level fail --show-progress none
```

UTC 15:16:15.355–15:25:16.004Z; supervised 540.648 seconds, nextest 540.175
seconds; **3/3 iterations passed**, each with 140 selected/default skips.
Individual test durations 180.059, 180.057 and 180.059 seconds. Run ID
`b15b8d58-bd90-4998-981c-bc9fcd4d9452`. No source change since round 08.
Minimum of 109 safety samples: RAM 10,731,810,816 bytes, disk 267,387,736,064
bytes; maximum load1 3.6953125, swap used 7,952,318,464 bytes. No failed gate,
stop or survivors. Post-round TMPDIR/mount audit empty at 15:27:06Z.

Read-only CI checkpoint supplied by the parent: on published commit `95799565`,
CI run `42640053333` Linux CLI job `129238750428` subsequently completed
SUCCESS; the overall CI run was still in progress when observed. This later
pass is **not** proof that the earlier missing-filesystem/completion failure
was repaired; its root cause remains unknown. No risky local CLI rerun or
universal/full-green CI claim follows from that checkpoint.

### Round 12 — deeper generated namespace exploration

A disposable standalone Rust driver under ignored `target/chaos8h/driver`
uses public OverlayCore/FilesystemService and a native oracle. No permanent
test/product API is added. Xorshift64 `(<<13, >>7, <<17)` selects bounded
create/mkdir/remove/rename operations over 18 fixed relative names (depth <=3).
Rename sources are existing names and moves into their own descendants are
excluded. Every operation compares result, complete visible names/types/bytes
and unchanged lower contents. Every 32 steps, the owner captures/restores its
documented hardlink groups across reopen and checks visible state and scratch
cleanup. Three formats are explored: none, legacy and compact preimages.
Optional link/write exploration is separately controlled, initially disabled.
Fixture sizes remain bounded by the small name set, with no mount/apply/
external authority or service.

**Unexpected result, not a product defect:** the first strict native-errno
oracle stopped at format 0, seed 1, step 65, `Rename("f/b", "f")`. Both backends
rejected: native ENOTEMPTY (39), OverlayCore EISDIR (21). The destination was a
nonempty directory and the source a regular file. The exact-oracle failure and
full bounded trace remain retained. It was reproduced unchanged using root
dependency pins; no product repair or successful full trace is claimed.

Initially, standalone offline lock generation selected newer already-cached
dependencies (`r12-basic-generated`), so that execution is diagnostic only.
Its lock is retained as `r12-driver-initial.lock`, SHA-256
`159d088f0d3785e6e515dea4e15ebdf2a7dab046ab109fcd0aea1f5f8b7e02a8`.
The driver lock was then seeded from the unchanged root lock and resolved
offline. A package identity comparison confirms every dependency's
name/version/source matches a root-lock package (only the disposable driver
itself is additional). New driver lock SHA-256
`d943278c8e08b0c19cd1a9de671ccbf8b7958b454275b800fd05d6a97c1f58c1`.
`r12-root-pinned-exact` reproduces the same errno-precedence mismatch.

Before further exploration, a **separately named semantic oracle** is defined:
only when the pre-operation source is a regular file and destination is a
known nonempty directory, accept EISDIR, ENOTEMPTY or EEXIST, still requiring
failure and unchanged native/visible/lower trees. All other cases retain exact
native errno checks. The original strict-oracle test is retained, not rewritten
to pass. POSIX.1-2024 [2.3 Error Numbers](https://pubs.opengroup.org/onlinepubs/9799919799/functions/V2_chap02.html)
states that when multiple errors apply any may be returned because detection
order is undefined; [rename](https://pubs.opengroup.org/onlinepubs/9799919799/functions/rename.html)
lists precisely these conditions/codes. This is explicit diagnostic
classification, not assertion relaxation of a product regression or semspec.
The parent reviewed and required this distinction.

The driver uses the project's exact `mise run test` wrapper with its own
explicit disposable package and manifest, **not** an affected production-package
gate. All commands run through the inspected supervisor. Define:

```sh
DRIVER='target/chaos8h/driver/Cargo.toml'
# Each run's four environment values and filter are recorded below.
python3 target/chaos8h/supervise.py NAME env \
  PVISOR_CHAOS_SEED_BASE=BASE PVISOR_CHAOS_SEEDS=SEEDS \
  PVISOR_CHAOS_STEPS=STEPS PVISOR_CHAOS_LINKS=LINKS \
  mise run test -- pvisor-chaos-driver -- --manifest-path "$DRIVER" \
  --offline --test-threads 1 -E 'test(=generated_namespace_trace_posix_errors)' \
  --success-output final
```

| Evidence | Selection/configuration | UTC interval | Supervised seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r12-driver-lock` | `mise exec -- cargo generate-lockfile --manifest-path target/chaos8h/driver/Cargo.toml --offline` | 15:28:49.163–49.344 | 0.180 | setup only, independent newer-cache lock |
| `r12-basic-generated` | BASE=1 SEEDS=16 STEPS=512 LINKS=0; original exact test, `--no-capture` | 15:28:55.444–15:29:07.469 | 12.024 | **1 failed** at step 65; later seeds not reached |
| `r12-pin-driver-lock` | supervised Python copies initial driver lock to evidence then seeds driver lock from root | 15:30:44.118–44.167 | 0.047 | setup only, root lock unchanged |
| `r12-resolve-root-pins` | `mise exec -- cargo check --manifest-path target/chaos8h/driver/Cargo.toml --offline --tests` | 15:30:44.214–50.753 | 6.538 | exit 0, no tests; root-pinned dependencies |
| `r12-root-pinned-exact` | BASE=1 SEEDS=1 STEPS=128 LINKS=0; `-E 'test(=generated_namespace_trace)'` | 15:31:35.504–45.435 | 9.930 | **1 failed**, same step 65 / errno difference |
| `r12-posix-generated` | BASE=1 SEEDS=16 STEPS=512 LINKS=0; separate POSIX oracle | 15:32:40.904–51.480 | 10.575 | 1 passed, 1 exact-oracle test not selected |
| `r12-links-generated` | BASE=1 SEEDS=16 STEPS=512 LINKS=1 | 15:33:08.240–18.347 | 10.106 | 1 passed, 1 not selected |
| `r12-deeper-links` | BASE=1001 SEEDS=64 STEPS=4096 LINKS=1 | 15:33:43.417–15:39:16.613 | 333.194 | 1 passed (332.886s nextest), 1 not selected |

The first exact run used the same base command with `--no-capture` instead of
the filter/success-output options; nextest enforces serial execution in that
mode and warned the explicit thread count was redundant. Complete exact
commands for all setup/executions are retained in their JSON evidence.

Three successful generated executions cover **288 seed/format traces, 835,584
operations and 26,112 close/reopen checkpoints**: 252,237 successful mutations
and 583,347 ordinary rejected operations. Basic counts by create/mkdir/remove/
rename: 5,997 / 6,135 / 6,423 / 6,021. First link/write sweep counts:
4,161 / 4,101 / 4,290 / 3,885 / 4,104 / 4,035. Deep sweep counts:
132,363 / 129,828 / 138,072 / 124,422 / 130,788 / 130,959.
These are generated cases of one parameterized namespace harness, not 835,584
new scenarios. Five test invocations ran five test instances: two retained
exact-oracle failures and three separately defined semantic-oracle passes.
No implementation defect or repair is claimed.

Final driver source SHA-256
`65b4cf15774dea5138f1905174cd804430d53d3f8e0e815db380b3c4d4aaf2aa`;
manifest `884076dbe90ab09c3ec98ac49467d7eba58fae8659f9cee02a77ff62d1720a4d`.
Root lock hash remains the continuation identity. Driver profile is standalone
debug-with-debuginfo, distinct from root package profile; package identity
agreement is not a claim of full workspace feature/profile equivalence.
Deep run ID `b0e64ccb-38e0-42c6-a369-b4f57d19b37b`.

Minimum sampled available RAM 10,504,192,000 bytes; disk 266,903,248,896 bytes;
maximum load1 5.4453125, swap used 8,190,373,888 bytes. Offline cache builds
were serial; all resource gates passed. All survivor lists empty, no stop.
At 15:39:45Z TMPDIR and fixture-mount audit were empty and diffcheck passed.
No external lock/socket artifact was created by the driver. The strict
failures remain historical failures; the limited POSIX classification does not
assert arbitrary errno equivalence, mounted protocol correctness, native power
loss, or non-Linux behavior.

### Round 13 — correct the introduced native-oracle portability defect

Parent-supplied macOS CI evidence on run `42643028679`, job `129249032013`
found the new round 10 trace fails at step 6, `Remove("dir", false)`:
OverlayCore EISDIR (21), native macOS EPERM (1). That job had 718 passes,
one failure and 463 not run; Linux passed. These are external CI observations,
not local macOS execution. The test's assumed universal native errno was wrong;
this is an **introduced test-oracle defect**, not an OverlayCore product defect.

The authoritative [POSIX.1-2024 unlink contract](https://pubs.opengroup.org/onlinepubs/9799919799/functions/unlink.html)
normatively specifies EPERM for directory unlink; its rationale explicitly
documents Linux/LSB EISDIR and advises portable applications to handle either.
It is imprecise to say the normative POSIX text simply permits both.

The narrow test-only repair captures pre-operation native type/state. **Only**
`Remove(path, false)` of a pre-proven directory accepts native EPERM or EISDIR.
Core must still return exactly EISDIR. Both operations must fail, and explicit
before/after native and overlay trees must remain identical. Every other errno
assertion, all 28 operations/14 failures, all three journal variants, lower
isolation, first-touch baseline and every reopen check remain. No whole-test or
platform skip was introduced. README records the exact exception.

Supervised commands:

| Evidence | Command | UTC interval | Seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r13-oracle-format` | `mise exec -- rustfmt --check --edition 2024 crates/pvisor-overlay-core/tests/namespace_recovery.rs` | 15:43:24.573–24.672 | 0.097 | exit 1: new assertion formatting |
| `r13-portable-oracle` | `mise run test -- pvisor-overlay-core -- --offline --test-threads 1 --test namespace_recovery --test direct_copy` | 15:43:44.471–45.171 | 0.699 | **3 passed, 0 skipped** on Linux |
| `r13-oracle-format-final` | same format command | 15:43:45.217–45.302 | 0.083 | exit 0 |

One root-package test invocation ran three instances. This repairs the oracle
based on observed macOS CI and the documented native-platform behavior; local
macOS validation was **not** run. Final test SHA-256:
`4e42e3e126755f7679dd6d5f1a9f2685562eb051ba07d39db97b0c5cd0bd2812`;
README:
`adc72d647cb00ba4f721fff89c3241238d85d5f0fa14c35e9f41cb6dc167e019`.
Nextest run `c0a9d49f-853e-4f48-8a0e-621f7f26f336`.

Minimum sampled RAM 11,025,760,256 bytes; disk 266,898,755,584 bytes;
maximum load1 2.8174, swap used 8,231,944,192 bytes. All gates passed, no
stop/termination/survivors. TMPDIR/mount audit empty; diffcheck passed at
15:43:56Z. The unfinished disposable journal driver had not been run and was
paused for this CI correction. The parent's benchmark comparison on this head
subsequently passed, but the earlier benchmark threshold failure remains part
of the historical record, not erased by a later pass.

### Round 14 — generated journal graph, retries and torn-tail recovery

Extended the disposable standalone driver with a second public-API harness,
`journal_generated.rs`. Xorshift64 generates 48 event templates per seed with
0..3 causal edges, fixed identities, finite fractional JSON values and small
payloads. A separate graph oracle repeatedly removes nodes whose causes are
outside the remaining graph (independent of the implementation's DFS). Selected
requests are original templates, changed-content identity conflicts, explicitly
invalid empty scopes or self-causes. Memory and durable journals must agree with
the independent commit/retry/reject prediction, exact receipts, durability,
contiguous offsets and quiet retries/rejections. Durable bytes must remain
unchanged for every retry/rejection.

Every 16 operations the durable owner closes/reopens and compares all committed
events/receipts. Every 32 operations the closed fixture gets one small
unterminated tail (`{`, `{"position":`, or `{"event":{"id":"partial`).
Read/validate must fail without modifying bytes; writer reopen must truncate
exactly that tail and retain the complete original prefix. This is explicit
closed-file fixture injection, **not** process interruption, device failure or
power loss. Fixtures stay small and worktree-local; no mounts/host-control/
external services or shared locks are used.

Setup: supervised `mise exec -- cargo check --manifest-path target/chaos8h/driver/Cargo.toml --offline --tests`
at 15:45:03.112–15:45:04.475Z, 1.361 seconds, exit 0. Exact test template:

```sh
python3 target/chaos8h/supervise.py NAME env \
  PVISOR_CHAOS_SEED_BASE=BASE PVISOR_CHAOS_SEEDS=SEEDS PVISOR_CHAOS_STEPS=STEPS \
  mise run test -- pvisor-chaos-driver -- \
  --manifest-path target/chaos8h/driver/Cargo.toml --offline --test-threads 1 \
  --test journal_generated --success-output final
```

| Evidence | BASE / SEEDS / STEPS | UTC interval | Supervised seconds | Outcome |
| --- | --- | --- | ---: | --- |
| `r14-graph-initial` | 1 / 8 / 1024 | 15:45:04.521–15:45:12.207 | 7.685 | 1 passed |
| `r14-graph-deeper` | 1001 / 64 / 4096 | 15:45:26.976–15:47:38.209 | 131.232 | **1 passed** (130.932s nextest) |

Two test invocations/instances, **72 generated seed traces and 270,336 logical
transitions**, executed on both memory/durable backends. Model counts: 3,243
new commits, 124,861 retries, 74,641 identity conflicts, 13,060 causal-cycle
rejections and 54,531 invalid events. There were 16,896 close/reopen checkpoints
and 8,448 torn-tail injections. Recovery also rechecks each committed original
receipt; those extra retry calls are not mislabeled as new logical transitions.
One new parameterized scenario family, no permanent product/test additions and
no implementation defect found.

Driver source SHA-256
`1c62d1e09fc429b1b7584d856409dcbdd7e610d741ad442ba8107e4dca763a49`;
updated manifest
`b4384dd5dc660626a604e6d4203c6e72cde89d1bb28ab0b5252c3deee6e4f97d`;
updated standalone lock
`5a6c59ead28f80bcbfb0440fa658c214de76c6dd2bc3e84441571d30cf947615`.
Every dependency package identity still matches the unchanged root lock; only
the disposable driver package is additional. The same standalone-profile/feature
and not-an-affected-package-gate limitations as round 12 apply.
Deep run ID `136526a8-1d3e-477f-b930-6a6ef218e437`.

Minimum sampled RAM 10,992,730,112 bytes; disk 266,784,657,408 bytes;
maximum load1 3.6915, swap used 8,300,175,360 bytes. Swap remains closely
observed without changing host configuration. All resource gates passed, no
stop/termination/survivors. TMPDIR/mount audit empty and diffcheck passed at
15:48:09Z. No broader package, real crash/power loss, other platform, Python,
doctest, rustdoc or clippy execution was performed.

### Round 15 — expanded namespace seeds and finite-float journal recovery

No permanent tests or product code were added. Reused the bounded namespace
driver with a disjoint seed range, then varied the disposable journal payload
profile. The float profile includes templates for positive/negative zero,
minimum positive normal/subnormal, positive/negative maximum finite values,
`1e-307`, `1e307`, and additional random finite `f64` bit patterns. Nonfinite
bit patterns are replaced by zero because JSON's contract here is finite values.
The graph still determines which templates can commit; this is not a claim
that every individual generated boundary template committed in every seed.
Receipt identity, unchanged rejected/retried bytes, independent graph outcomes
and torn-tail/reopen checks remain unchanged.

Exact supervised commands:

```sh
python3 target/chaos8h/supervise.py r15-namespace-expanded env \
  PVISOR_CHAOS_SEED_BASE=100001 PVISOR_CHAOS_SEEDS=64 \
  PVISOR_CHAOS_STEPS=4096 PVISOR_CHAOS_LINKS=1 \
  mise run test -- pvisor-chaos-driver -- \
  --manifest-path target/chaos8h/driver/Cargo.toml --offline --test-threads 1 \
  --test namespace_generated -E 'test(=generated_namespace_trace_posix_errors)' \
  --success-output final

python3 target/chaos8h/supervise.py r15-graph-float-boundaries env \
  PVISOR_CHAOS_SEED_BASE=200001 PVISOR_CHAOS_SEEDS=64 \
  PVISOR_CHAOS_STEPS=4096 PVISOR_CHAOS_FLOAT_BITS=1 \
  mise run test -- pvisor-chaos-driver -- \
  --manifest-path target/chaos8h/driver/Cargo.toml --offline --test-threads 1 \
  --test journal_generated --success-output final
```

| Evidence | UTC interval | Supervised seconds | Outcome |
| --- | --- | ---: | --- |
| `r15-namespace-expanded` | 15:49:43.902–15:55:16.834 | 332.930 | **1 passed**, 1 exact-oracle test not selected |
| `r15-graph-float-boundaries` | 15:56:05.181–15:58:15.843 | 130.661 | **1 passed**, 0 skipped |

Namespace: 192 seed/format traces, 786,432 operations, 24,576 reopen checkpoints;
239,892 successful mutations and 546,540 rejected operations. Operation counts
create/mkdir/remove/rename/link/write:
130,416 / 131,184 / 138,258 / 124,230 / 130,848 / 131,496.
Journal: 64 graph traces, 262,144 logical transitions on both memory/durable
backends, 16,384 reopens and 8,192 injected tails. Model counts commit/retry/
conflict/cycle/invalid: 2,890 / 122,889 / 71,781 / 12,202 / 52,382.
Two test invocations ran two instances. These are deeper parameter profiles
of the existing two harness families, **not 1,048,576 new scenarios**.
No implementation defect was found. Original exact-oracle failures remain
historical failures.

Namespace source unchanged from round 12; journal source SHA-256 now
`8f4c54c165d0d8390ded0c88ce20488df0d45c604886bc334ed0e7edb4185986`.
Manifest/driver lock retain round 14 identities. The default fractional profile
retains its prior generation rule; this execution explicitly selected float bits.
Standalone package/profile limitations still apply. Run IDs:
namespace `a4c86dc2-e7f4-4587-98e5-8862d40f70e2`;
journal `9f1f36a1-e957-4098-8b72-b19f8e31654e`.

Minimum sampled RAM 10,745,585,664 bytes; disk 266,763,980,800 bytes;
maximum load1 4.7037. No formal RAM/disk/load admission gate failed, no watchdog
stop, and all owned survivors were empty. TMPDIR/mount audit empty; diffcheck
passed at 15:59:14Z.

**New safety concern:** swap became entirely used (8,589,930,496 bytes) in four
samples at 15:56:50.462667, 15:56:55.491000, 15:57:00.508186 and
15:57:05.547501Z. At the single read-only follow-up sample, 15:59:41.003568Z,
only 233,376 swap bytes were free, although MemAvailable was 11,212,275,712
bytes, load1 3.0127 and disk 266,768,576,512 bytes. Boot/clock still matched;
the formal gate error list was empty. This is not attributed to this workload.
The worker paused further launches for parent safety review instead of inventing
a failed formal gate or running cold builds into nearly exhausted swap.

Through this checkpoint the continuation has 51 supervised command executions
totaling **1,952.606 elapsed seconds** (tests/builds/format/discovery combined),
not CPU time or elapsed campaign time. Investigation/read-only work is separate;
the recorded interruption/trust-prompt gaps remain excluded. The fixed deadline
is unchanged and eight hours of active execution is **not** claimed.

Safety-pause audit at 16:01:17.244557Z found no matching worktree-owned
nextest/rustc/test/supervisor workload, no TMPDIR entries and no worktree fixture
mounts (`target/chaos8h/r15-pause-audit.json`). Diffcheck passed. The inherited
journal contract source still hashes to
`ae821548276faf1602832271de71cfc343a2b982ae0496bfefda2b1733e1c34a`.
External canonical lock artifacts observed in round 04 remain untouched.
This pause is reported for conservative safety review, not a claim that the
formal RAM/disk/load gate failed or that the deadline has been reached.

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
