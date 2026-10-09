# Staging and apply semantics

Staging keeps a Job's workspace changes in a copy-on-write upper layer. Only an explicit `apply` changes the target workspace. This makes file changes reversible before apply: select what to merge, discard the stage, or fork it.

This page defines the staging contract. Each promise corresponds to a case in `docs/src/zh/cases/06-stage-apply.md`, reproducible with `just cases --suite stage`. For the workflow, see [Review and apply changes](../guides/review-apply.md).

## Promises

| Promise | Meaning | Semantic case |
| --- | --- | --- |
| Workspace unchanged before apply | After success, a nonzero exit, or signal termination, workspace paths, types, contents, permission bits and link targets match their initial state until apply | S-STAGE-001 |
| The Job reads its own changes | The Job sees its writes, deletions and renames while the host workspace remains unchanged | S-STAGE-002 |
| Review shows the net effect | The list equals the net additions, deletions and modifications; files created then deleted are absent | S-STAGE-003 |
| Applying everything equals direct execution | Staging a script then running `apply --all` produces the same complete tree as running the script directly in the workspace | S-STAGE-004 |
| Apply is refused after drop | Drop leaves the workspace unchanged; subsequent apply is rejected without effect | S-STAGE-005 |
| Selective apply affects only the selected subtree | `apply --path P` changes only P and descendants; two selective batches covering all changes equal one `--all` | S-STAGE-006 |
| Repeated apply has no additional effect | Reapplying paths already applied does not change the workspace | S-STAGE-007 |
| Conflicts preserve external changes | If a target is externally modified, created or deleted after staging, apply is refused and the external state is preserved completely | S-STAGE-008 |
| A conflicting batch has no effect | If any selected path conflicts, this apply changes none of the selected paths | S-STAGE-009 |
| Unrelated external changes do not block apply | External changes to paths untouched by the agent are preserved and do not block apply | S-STAGE-010 |
| Directory deletion preserves external additions | Apply is refused if the agent deleted a directory and an external writer subsequently added files inside it | S-STAGE-011 |
| Apply stays inside the workspace | If a directory is replaced by a link pointing outside after staging, apply is refused and both trees remain unchanged | S-STAGE-012 |
| Return values agree with effects | Successful link creation leaves a link; failed creation leaves none | S-STAGE-013 (known macOS gap below) |
| Executable bits and link targets survive | Executable bits and symbolic link targets are preserved without dereferencing links | S-STAGE-014 |

These cases are drafts without human approval. A passing test demonstrates that the implementation satisfies the script; it does not mean the semantics have been reviewed. See [Testing and semspec](../community/testing.md).

## Known gaps

- **S-STAGE-013 is XFAIL on macOS**: macFUSE can return EPERM when creating a symbolic link even though the link was created. See [Known limitations](../security/known-limitations.md).
- **Renames appear as deletion plus addition**: the review list has no separate rename representation.
- **Multi-file apply is not atomic to external editors**: conflict checks precede writes; stop other writers during apply. After interruption, `apply-ledger.json` allows a prepared batch to recover forward.

## Irreversible effects {#不可逆的部分}

Staging covers workspace files. These effects bypass it and cannot be undone by either `apply` or `drop`:

- Remote API calls, database writes, sent messages and other external effects;
- Host paths explicitly shared with `--mount SOURCE:write`;
- Batches already applied: drop does not undo them.

For writes to HOME, the VM root and other locations, see [Staging and storage](../reference/cli.md#暂存与存储).
