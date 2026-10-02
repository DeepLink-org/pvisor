---
status: todo
search:
  exclude: true
---

# OverlayCore design

!!! warning "Planned"
    Implementation-owner review is pending. See [Staging and apply semantics](../concepts/staging.md) for public promises and [Run project discovery](../reference/cli.md#run-项目发现) for CLI behavior.

## Question

How are S-STAGE-001 through 014 implemented and recovered after crashes?

## Requirements

- Copy-on-write, first-touch preimages, durable fingerprints.
- Prepared → TargetApplied → Committed persistence and recovery.
- Recursive deletion, directory/link replacement conflicts (S-STAGE-008, S-STAGE-011, S-STAGE-012).
- macFUSE/FSKit/Linux FUSE/virtio-fs differences; hard-link groups and opaque directories.
- Known issue: copied_hard_links is currently in-memory only.

## Acceptance criteria

- Code/test references for each transition.
- Cross-reference the crash-injection results in [apply/drop cost and crash consistency (planned)](../benchmarks/apply.md).

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: `crates/pvisor-overlay-core`, `crates/pvisor-overlayfs`

## Current implementation: writes to application

`OverlayCore` in `core.rs` manages lower/upper views; host FUSE and VM file services use these operations. Before first modification, `record_preimage` fingerprints the actual apply target rather than an overlaid visible lower. Later writes do not reset existing entries.

Fingerprints distinguish absence, regular files, directories, symlinks and other nodes. Regular files store SHA-256 plus permissions/ownership; links store target bytes without dereferencing; directories also store modification time. Entries live in `preimages/entries/` and file/directory sync precedes continuing the operation. A complete journal missing a selected path causes refusal. Apply-time sampling for older stages cannot claim equivalent run-time conflict detection.

## Durable apply states

`apply.rs::apply_overlay_selected` acquires the target lock, recovers pending batches, calculates net changes/selections and checks all selected preimages before persisting intent. The lock coordinates pVisor apply; it cannot stop external editors.

| State | Persisted information / recovery |
| --- | --- |
| `Prepared` | apply ID, Overlay ID/generation, target, selector, changes, recovery paths and preimages are in `apply-ledger.json`; recovery accepts only original or batch-desired target state and completes writes forward |
| `TargetApplied` | Target updates are complete; state persists before pruning upper; recovery finishes pruning/consuming preimages without treating partially pruned opaque directories as complete new mutations |
| `Committed` | Remaining changes have been calculated/persisted; Overlay stays `Staged` if changes remain, otherwise `Applied` |

Recovery rejects batches with different Overlay ID, generation or target. Directory deletion/replacement also validates collapsed descendant preimages. Selective apply expands related hard-link groups; files created then deleted are absent from the net diff.

## Validation and remaining work

```bash
just test pvisor-overlay-core pvisor-overlayfs
just semantics
```

`first_touch_preimage_is_durable_and_never_rebased` in `core.rs` checks the first baseline. In `apply.rs`, `apply_rejects_a_target_changed_after_first_touch`, `directory_replacement_checks_descendants_and_recovers_after_mutation`, `prepared_apply_recovers_before_or_after_target_mutation` and `target_applied_recovery_only_finishes_partially_pruned_opaque_upper` cover conflicts/recovery.

External readers can still see intermediate multi-file states. Recovery completes forward rather than rolling back automatically. In-memory `copied_hard_links`, mount-backend differences and system crash-injection matrices remain part of the pending design review above.
