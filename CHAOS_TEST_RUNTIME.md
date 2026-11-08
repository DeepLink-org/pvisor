# Bounded runtime and overlay validation

## Scope

Requested campaign window: 2026-11-08 01:04–09:04 UTC. This is an iterative
testing, investigation and repair window, not eight hours of uninterrupted load.
Completion of that window is not yet claimed.

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
