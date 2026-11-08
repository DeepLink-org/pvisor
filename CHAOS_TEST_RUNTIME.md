# Bounded runtime and overlay validation

## Scope

Requested campaign window: 2026-11-08 01:04–09:04 UTC. This is an iterative
testing, investigation and repair window, not eight hours of uninterrupted load.
The final resource/clock observation was recorded at 09:04:00.000362 UTC,
completing the requested window. No further workload was started afterward.

The user subsequently requested that plugin changes be removed from PR #121
because that component is being iterated independently. The PR now retains
only native runtime/overlay changes and their evidence. Plugin sources, plugin
tests, associated task declarations, task-routing changes and plugin-specific
reports are restored to the PR merge base, `84b0898a`.

No main merge, history rewrite, force-push, package publication, real-user stage
mutation or semantic-specification approval is part of this work.

## Retained changes

### Status remains readable across owner discovery teardown

`crates/pvisor/src/runtime/session_service/status.rs` retains the selected owner
while sampling filesystem status and optional activity. It no longer pings and
then independently rediscovers an owner that can disappear between the calls.
Exact Session/Execution/stage validation remains enforced; identity changes and
malformed overlay data still fail rather than being treated as offline success.

The synchronized regression in
`crates/pvisor/src/runtime/instance_control.rs` reproduced the original
`owning Session control endpoint unavailable` error before the repair.
It models discovery teardown, not arbitrary concurrent physical teardown.
Independent static review found no blocking issue.

### Cross-process lease tests use a bounded readiness protocol

The rootfs and workspace lease integration tests now share
`crates/pvisor/tests/support/lease_child.rs`. Child libtest output uses terse
format so the readiness marker is not attached to a pretty-output prefix.
The helper owns the child and stdout reader, has five-second readiness/exit
waits, drains fixed-size chunks, caps diagnostic retention, and sends at most
one readiness/error notification. Cleanup kills/reaps the owned child before
joining its reader.

Both original failures were reproduced with bounded diagnostics:

```text
test prepared_rootfs_lease_is_independent_in_another_process ... LEASE_READY
test prepared_workspace_independent_process_lease_survives_lost_catalog_pin ... WORKSPACE_LEASE_READY
```

All lease, catalog-pin and garbage-collection assertions remain intact, including
normal and abrupt workspace-reader exit. The fix changes test orchestration,
not production lease behavior. Focused validation passed both protocols and
four helper-test instantiations: six passes, eleven filtered skips.
Focused Clippy with warnings denied, rustfmt checks and independent static
reviews passed. OS kill/reap and arbitrary descendants are not given a universal
hard-deadline guarantee.

### Existing-parent apply conflicts are characterized, not relaxed

`crates/pvisor-overlay-core/src/apply.rs` adds a 36-combination matrix:
two journal formats, two selection forms and nine mutations.

Unrelated sibling creation/deletion or subdirectory creation can change a
recorded parent timestamp and block apply. Existing-sibling content edits do not
have that namespace effect. Selected-file, parent-metadata, replacement and
symlink conflicts remain protected. Tests check preserved host/stage state.
No production conflict policy was loosened, and the specific cause of the
user's real reported conflict is not established by these disposable fixtures.

## Validation evidence

All commands below were executed by subagents serially with two Cargo build
jobs, one test thread, `nice -n 10`, finite watchdogs and resource admission
checks. Package arguments select Rust coverage; they do not run the repository
Python suite.

| Command | Result | Qualification |
| --- | --- | --- |
| `mise run test -- pvisor` | 879 passed, 10 default skips, zero filtered/unrun | Final unfiltered run, 05:55:32–05:56:56 UTC, 24 binaries |
| `mise run test -- pvisor-cli` | 320 passed, 40 default skips | Unfiltered controlled private-HOME run, 31 binaries |
| `mise run test -- pvisor-core pvisor-gateway pvisor-overlaynet pvisor-replay` | 762 passed, zero skips | Core 88, Gateway 408, OverlayNet 115, replay 151; 25 binaries |
| `mise run test -- pvisor-overlay-core` | 243 passed, 5 default skips | Includes the parent-conflict matrix |
| `mise run test -- pvisor-overlayfs pvisor-journal` | 103 passed, 13 default skips | OverlayFS 73, journal 30; 8 binaries; final source identities checked before and after |

The final runtime pass includes the status regression and both lease protocols.
Its nextest ID is `8123a06a-833b-4686-813c-1d0076fa0978`. The shared helper SHA256
is `c9cf830fd8d97db4af982b90f9c69fa695dddbfd85c835e539c87a56737298bc`.

Earlier full runtime attempts remain failed historical evidence:
825 passes / one SIGTERM / ten skips / 49 unrun, then
831 passes / one SIGTERM / ten skips / 43 unrun. Each hit a 900-second watchdog.
The successful final run followed deterministic, reviewed protocol repairs;
it does not retroactively turn those attempts into passes.

Durable streamed logs and metadata remain in the corresponding worker worktrees
under `.data/round16*`, `.data/round19/followup` and `.data/round22`. Machine-local
runner scripts and caches are not publication artifacts.

### Subsequent NativeHost public CLI workflow check

At 07:12:57–07:13:17 UTC, an existing four-test selection passed once:
four actual tests, seventeen filtered/default skips, two binaries. It used
`mise run test -- pvisor-cli` with only the `rootless_local` and
`documentation_json` binaries and exact filters for:

- `safe_abnormal_exits_retain_reviewable_stages_and_release_owners_before_retry`
- `safe_run_selectively_applies_then_drops_remaining_changes`
- `safe_apply_refuses_to_overwrite_a_concurrently_changed_target`
- `documented_ci_failure_and_timeout_keep_reviewable_candidates`

These cover seven source-defined branches: exit23, workload self-SIGTERM/143,
healthy successor, selective acceptance/drop, concurrent host-edit conflict,
exit7 and a two-second execution deadline. Self-SIGTERM is not external
cooperative cancellation. The conflict test uses `--filesystem sandbox`, not
the full safe preset. No VM, namespace relaxation, ignored-test opt-in or
external service was selected.

Nextest ID: `b42fb88d-9c5e-4ff9-ae10-28ba96b35e0e`. The command took 20.154s,
including compilation; test time was 3.157s. All three resource samples passed:
minimum available RAM 18,818,301,952 bytes, disk 339,347,656,704 bytes,
maximum load1 1.1836. Owned process/mount audits found no workload survivors;
three inaccessible same-UID entries remain an explicit visibility limit.
Some existing tests rely on TempDir cleanup rather than preserving unknown
failure trees, so this is not universal cleanup certification.

### Final source-matched checks

At 08:42:42 UTC, a final exact-filtered runtime command passed seven identities
across three binaries: the status regression, both lease protocols, and two
helper tests instantiated in each integration binary. The command selected
`--lib --test prepared_rootfs --test prepared_workspace`, used exact-name
filters and `--no-tests=fail`, and additionally checked that the unique passing
`(binary, test)` set exactly matched the expected seven. Nextest reported
seven passes and 778 selection/default skips; these skips are not environmental
failures or evidence of full-package coverage. Nextest ID:
`0d11babc-93fe-4f0b-a30b-58f3a9627ba2`.

The command took 0.514s, with 0.078s test time. Pre/post source manifests matched
the earlier successful 879-test run, including the untracked-at-test-time shared
helper. Durable logs remain in `.data/runtime-final-verification/`; combined
log SHA256 is
`200242569efef7cf8a2a991fffe4af4f85174e4886ee976eb3e847dd9c94c61c`.

The subsequent exact OverlayFS/journal package command passed 103 tests with
13 default skips, completing at 08:45:17 UTC. Package, core dependency,
manifest/lockfile and working overlay apply source identities matched before
and after. Nextest ID: `ab3f4471-0bbe-4d27-b95a-51bae2821201`.
Compilation took 11.16s and tests 5.878s. This checks default API contracts and
private implementation regressions, not ignored real FUSE mounts, rustdoc,
doctests or other platforms. Evidence remains in the worker-local
`.api-validation-1130761`.

Both final checks passed resource gates and found their owned process groups
quiescent. No plugin execution, shared-binary overwrite or production edit was
part of these checks.

## Safety and remaining limits

- One active build/test workload; at most two Cargo jobs and one test thread.
- Refuse launch below 6 GiB available RAM or 20 GiB free disk, or above load1 8.
  Observed admission checks passed throughout the listed runs.
- Use owned disposable fixtures, bounded commands and offline existing tools.
  No host-network reconfiguration, global cache cleanup or unrelated PID kills.
- Controlled Linux HOME/TMP layouts avoid inherited mounts and overlong sockets.
  These passes do not establish the original real-HOME topology as supported.
- Process/mount audits found no identifiable owned survivors. Inaccessible
  same-UID `/proc` entries were preserved and reported; no all-namespace absence
  or arbitrary descendant-containment claim is made.
- Default skips, other platforms, optional features and the full workspace are
  not covered by the package counts above.

The scope-only cleanup does not change the retained Rust implementation/tests.
Those prior results apply to the same retained source bytes; they are not
described as newly rerun tests after cleanup. CI for the narrowed PR is separate
evidence and must be checked on its new revision.

At 07:40:28 UTC, `gh pr checks 121` reported 33 successful checks, zero failures,
zero pending and zero skips for the published runtime-only cleanup revision
`f2d5aa20`. CI run:
https://github.com/DeepLink-org/pvisor/actions/runs/42456000702.
The PR file list contained no plugin, mise-task or task-routing modifications,
and the committed-path comparison against the merge base confirmed that scope.
This checkpoint covers that revision, not later documentation updates, and
does not authorize a main merge or package publication.

## Campaign close

The existing native catalog driver completed nine low-load generated segments:
360 actions and 2235 CLI commands, with no unexpected failure or timeout.
These are metadata-catalog checks, not attached-container or packet-delivery
coverage. See `RUNTIME_CATALOG_VALIDATION.md` for per-segment results.

Delayed read-only checks at 08:50, 08:55 and 09:03 found the nine known catalogs
unchanged, their recorded groups absent, no readable exact-store processes or
store mounts, and unchanged artifact/driver hashes. The final CLI queries
finished at 09:03:00.160 UTC; none started after the 09:03:30 cutoff.
Private crash-temp evidence was retained, not globally cleaned.

At 09:04:00.000362 UTC, available RAM was 18,309,582,848 bytes, free disk
339,344,244,736 bytes and load1 0.5034. Two inaccessible same-UID environment
reads remain an explicit final-audit visibility limit. No universal descendant
or all-namespace cleanup claim is made. Plugin work remains excluded from this
PR; no main merge, package publication or additional campaign segment followed.
