# Stage / Apply / Drop checks (unreviewed draft)

These specifications describe design promises. Passing checks remain separate from human approval.

### S-STAGE-001: Staged Sessions preserve the workspace

**Semantics**: After successful execution, a nonzero exit, or signal termination, paths, types, complete contents, permission bits and symlink targets before apply are identical to the initial state.

**Violation example**: Deletion reaches the lower layer, or exceptional cleanup merges the upper layer into the workspace.

Execution and assertions: [Scrut S-STAGE-001](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-001.md).

### S-STAGE-002: Sessions read their own changes

**Semantics**: Reads inside the Session observe its writes, deletions and renames; the host workspace remains unchanged before apply.

**Violation example**: Reading returns old content after a write, a deleted path remains visible, or a renamed file cannot be found.

Execution and assertions: [Scrut S-STAGE-002](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-002.md).

### S-STAGE-003: Review describes the exact net effect

**Semantics**: The review inventory exactly matches net file additions, deletions and modifications; a temporary file created and deleted during the Session is absent.

**Violation example**: Temporary files leak into review, deletions are missing, or touched paths with no net effect are reported.

Execution and assertions: [Scrut S-STAGE-003](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-003.md).

### S-STAGE-004: Apply all equals direct execution

**Semantics**: Running the same successful script from the same initial workspace produces an identical complete tree, whether applied from staging with --all or run directly.

**Violation example**: Contents match but permissions or symlinks are lost, or rename and directory deletion have different results.

Execution and assertions: [Scrut S-STAGE-004](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-004.md).

### S-STAGE-005: Drop preserves the workspace and refuses later apply

**Semantics**: The workspace is unchanged after drop; later apply on that stage must be refused without effects.

**Violation example**: Drop publishes changes, or discarded changes can still be applied.

Execution and assertions: [Scrut S-STAGE-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-005.md).

### S-STAGE-006: Selective apply changes only the selected subtree

**Semantics**: apply --path P changes only P and its descendants; two selective applications covering every change equal one --all application.

**Violation example**: Selecting a directory changes sibling files, or the first apply consumes unselected changes.

Execution and assertions: [Scrut S-STAGE-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-006.md).

### S-STAGE-007: Repeated apply has no additional effects

**Semantics**: Applying an already applied path again leaves the workspace unchanged, regardless of whether the command reports already applied.

**Violation example**: Repeated deletion or copying introduces another modification.

Execution and assertions: [Scrut S-STAGE-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-007.md).

### S-STAGE-008: External changes cause conflicts

**Semantics**: When a target is externally modified, created or deleted after staging, apply is refused and preserves the complete external state.

**Violation example**: Apply overwrites external modifications or new files, or restores externally deleted files.

Execution and assertions: [Scrut S-STAGE-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-008.md).

### S-STAGE-009: A conflict leaves the entire selection unchanged

**Semantics**: If any selected path conflicts, that apply changes none of the selected paths.

**Violation example**: Apply publishes conflict-free paths before detecting a conflict and exits with partial changes.

Execution and assertions: [Scrut S-STAGE-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-009.md).

### S-STAGE-010: External changes on untouched paths do not block apply

**Semantics**: External modifications, creations and deletions of paths the Agent did not touch do not block apply and are all preserved.

**Violation example**: A whole-tree conflict check refuses unrelated changes, or apply restores an unrelated external deletion.

Execution and assertions: [Scrut S-STAGE-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-010.md).

### S-STAGE-011: Deleting a directory preserves external additions

**Semantics**: If the Agent deletes a directory and an external writer subsequently adds files to it, apply must refuse and preserve the entire external tree.

**Violation example**: Only old directory files are checked and newly added files are recursively deleted.

Execution and assertions: [Scrut S-STAGE-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-011.md).

### S-STAGE-012: Apply stays within the workspace

**Semantics**: If a directory is replaced by an external symlink after staging, apply does not write to the external directory; this case requires conflict refusal and both trees unchanged.

**Violation example**: Apply follows the replacement link and writes staged content outside the workspace.

Execution and assertions: [Scrut S-STAGE-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-012.md).

### S-STAGE-013: Symlink results agree with observable effects


**Semantics**: A successful symlink operation creates the corresponding link; a failed operation creates no link. Return values agree with observable effects.

**Violation example**: ln returns EPERM but creates a link that can be applied.

Execution and assertions: [Scrut S-STAGE-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-013.md).

### S-STAGE-014: Apply preserves executable bits and symlink targets

**Semantics**: Executable bits and symlink targets are published unchanged; links are not dereferenced into target files.

**Violation example**: An executable loses its execute bit, or a link becomes a copy of /etc/hosts.

Execution and assertions: [Scrut S-STAGE-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-014.md).

## Preparation and execution

Use chapter 2 to learn stage, review, apply and drop, then run these independent checks for net effects, conflicts and links. macOS requires macFUSE; Linux requires /dev/fuse and user/mount namespaces. A missing prerequisite makes the original check exit 77; the product fixture reports SKIP and fails the gate. S-STAGE-013 retains its known macOS XFAIL, with an actual success failing as XPASS. Checks and preparation still require human review.

```bash
mise run cases -- --suite stage
mise run cases -- --suite stage --case S-STAGE-008 --keep
```

Preparation and assertion source: [fixtures/stage.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/stage.bash).
