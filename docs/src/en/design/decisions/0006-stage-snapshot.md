# 0006: Snapshot stages and reuse immutable bases

> CLI update: the standalone `pvisor snapshot` entry is removed. Old interfaces/measurements below belong to their historical artifacts, not current executable instructions. See [CLI reference](../../reference/cli.md) for current entries and capability boundaries.


**Status: accepted, initial CLI profile implemented.** 2026-10-04. Manifest v4/v5 and `pvisor snapshot` now persist complete stages with owned base references. Existing v1–v3 full-environment restore stays supported. Ordinary Job checkpoint transport, workspace integration and incremental stage/RAM persistence remain future work.

## Context

Full-tree snapshots traverse and verify the entire rootfs during save and restore. Per-file clone/reflink reduces data copying, but directory traversal and full content verification still scale with the base. Agent branches should primarily pay for the stage produced by execution, while reusing image or input revisions.

## Choice

A file checkpoint preserves the writable stage and its restore/review semantics: upper content, whiteouts, opaque directories, hardlink/copy-up state, required transaction records, preimages, and absence observations. Read-only lowers are ordered immutable generation references; each snapshot does not copy the entire base. An execution checkpoint additionally combines CPU, RAM, and devices from the same epoch. File checkpoint cost must be distinguished from full execution restore.

Measure initial base import, identity verification, and cache preparation separately. A mutable host directory must first become a fixed revision or be rejected by this fast profile. Paths, image tags, and chmod do not prove immutability. Saved objects, restored instances, and branches pin their dependencies independently. Resolve a missing base by identity or fail explicitly.

## Consequences and migration

A stage is logical state, not merely an upper-directory copy. Writable workspaces also require stage capture. External apply targets are rebound and remain subject to conflict checks. The first version preserves the complete stage; stage mutation increments come later. The existing preimage journal is not a complete dirty journal.

Keep existing full-environment formats readable. New manifests/profiles distinguish base references from independent full copies; old readers reject unknown formats. Only after both stage and RAM have versions unaffected by further source writes may hashing, compression, and durable publication move outside the freeze window.

## Validation

Hold stage/RAM fixed while increasing the base: warm save/fork must not traverse or copy an unchanged base. Then vary stage bytes, entry count, large-file copy-up, and 1/8/64 branches. Preserve branch isolation, deletion/metadata/hardlinks, open handles, dependency GC, corruption rejection, and failed-publication contracts. Budgets such as 100 ms must state size, preparation, durability, and first-useful-work boundaries; this record makes no latency commitment.

The runtime retains one public `api`, uniform cross-platform traits/structs, and private backend implementations. Image resolution, base pins, and store publication stay outside the VMM. See the [current full-environment implementation](../environment-snapshot.md), [OverlayCore](../overlayfs.md), and [ADR 0005](0005-rust-vm-api.md#adr-0005).
