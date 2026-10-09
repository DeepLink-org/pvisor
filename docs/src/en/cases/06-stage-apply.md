# Stage / Apply / Drop checks (unreviewed draft)

These specifications describe design promises. Passing checks remain separate from human approval.

### S-STAGE-001: Staged Jobs preserve the workspace

**Semantics**: After successful execution, a nonzero exit, or signal termination, paths, types, complete contents, permission bits and symlink targets before apply are identical to the initial state.

**Violation example**: Deletion reaches the lower layer, or exceptional cleanup merges the upper layer into the workspace.

<!-- semspec: case id=S-STAGE-001 -->
```bash
require_stage
printf original > edit; mkdir d; printf keep > d/keep
snapshot before "$WS"
expect_exit 0 stage "$CASE_ROOT/success" 'printf changed > edit; rm -rf d; printf new > added'
assert_unchanged before "$WS"
expect_exit 3 stage "$CASE_ROOT/nonzero" 'printf changed > edit; rm -rf d; exit 3'
assert_unchanged before "$WS"
expect_refused stage "$CASE_ROOT/signal" 'printf changed > edit; rm -rf d; kill -TERM $$'
assert_unchanged before "$WS"
```

### S-STAGE-002: Jobs read their own changes

**Semantics**: Reads inside the Job observe its writes, deletions and renames; the host workspace remains unchanged before apply.

**Violation example**: Reading returns old content after a write, a deleted path remains visible, or a renamed file cannot be found.

<!-- semspec: case id=S-STAGE-002 -->
```bash
require_stage
printf original > edit; printf gone > deleted; printf moved > old
snapshot before "$WS"
stage "$CASE_ROOT/stage" 'printf changed > edit; test "$(cat edit)" = changed; rm deleted; test ! -e deleted; mv old new; test ! -e old; test "$(cat new)" = moved'
assert_unchanged before "$WS"
```

### S-STAGE-003: Review describes the exact net effect

**Semantics**: The review inventory exactly matches net file additions, deletions and modifications; a temporary file created and deleted during the Job is absent.

**Violation example**: Temporary files leak into review, deletions are missing, or touched paths with no net effect are reported.

<!-- semspec: case id=S-STAGE-003 -->
```bash
require_stage
printf original > edit; printf gone > deleted
stage "$CASE_ROOT/stage" 'printf changed > edit; rm deleted; printf added > added; printf temporary > temporary; rm temporary'
assert_changes "$CASE_ROOT/stage" <<'EXPECTED'
added added
deleted deleted
modified edit
EXPECTED
```

### S-STAGE-004: Apply all equals direct execution

**Semantics**: Running the same successful script from the same initial workspace produces an identical complete tree, whether applied from staging with --all or run directly.

**Violation example**: Contents match but permissions or symlinks are lost, or rename and directory deletion have different results.

<!-- semspec: case id=S-STAGE-004 -->
```bash
require_stage
printf original > edit; mkdir d; printf gone > d/file; printf moved > old
mkdir "$CASE_ROOT/direct"; cp -pR "$WS/." "$CASE_ROOT/direct/"
script='printf changed > edit; rm -rf d; mv old renamed; mkdir newdir; printf new > newdir/file; chmod 700 renamed'
(cd "$CASE_ROOT/direct"; /bin/sh -eu -c "$script")
stage "$CASE_ROOT/stage" "$script"
pvisor apply --all "$CASE_ROOT/stage"
assert_same_tree "$WS" "$CASE_ROOT/direct"
```

### S-STAGE-005: Drop preserves the workspace and refuses later apply

**Semantics**: The workspace is unchanged after drop; later apply on that stage must be refused without effects.

**Violation example**: Drop publishes changes, or discarded changes can still be applied.

<!-- semspec: case id=S-STAGE-005 -->
```bash
require_stage
printf original > edit
snapshot before "$WS"
stage "$CASE_ROOT/stage" 'printf changed > edit; printf new > added'
pvisor drop "$CASE_ROOT/stage"
assert_unchanged before "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged before "$WS"
```

### S-STAGE-006: Selective apply changes only the selected subtree

**Semantics**: apply --path P changes only P and its descendants; two selective applications covering every change equal one --all application.

**Violation example**: Selecting a directory changes sibling files, or the first apply consumes unselected changes.

<!-- semspec: case id=S-STAGE-006 -->
```bash
require_stage
mkdir chosen other; printf a > chosen/a; printf b > other/b
mkdir "$CASE_ROOT/all"; cp -pR "$WS/." "$CASE_ROOT/all/"
script='printf changed > chosen/a; printf new > chosen/new; printf changed > other/b'
stage "$CASE_ROOT/stage" "$script"
(cd "$CASE_ROOT/all"; stage "$CASE_ROOT/all-stage" "$script")
snapshot other "$WS/other"
pvisor apply --path chosen "$CASE_ROOT/stage"
assert_content chosen/a changed; assert_content chosen/new new
assert_unchanged other "$WS/other"
pvisor apply --path other "$CASE_ROOT/stage"
pvisor apply --all "$CASE_ROOT/all-stage"
assert_same_tree "$WS" "$CASE_ROOT/all"
```

### S-STAGE-007: Repeated apply has no additional effects

**Semantics**: Applying an already applied path again leaves the workspace unchanged, regardless of whether the command reports already applied.

**Violation example**: Repeated deletion or copying introduces another modification.

<!-- semspec: case id=S-STAGE-007 -->
```bash
require_stage
printf original > edit
stage "$CASE_ROOT/stage" 'printf changed > edit'
pvisor apply --path edit "$CASE_ROOT/stage"
snapshot applied "$WS"
# 重复请求可以成功或明确拒绝，但必须无效果。
pvisor apply --path edit "$CASE_ROOT/stage" || :
assert_unchanged applied "$WS"
```

### S-STAGE-008: External changes cause conflicts

**Semantics**: When a target is externally modified, created or deleted after staging, apply is refused and preserves the complete external state.

**Violation example**: Apply overwrites external modifications or new files, or restores externally deleted files.

<!-- semspec: case id=S-STAGE-008 -->
```bash
require_stage
printf original > modified; printf original > removed
stage "$CASE_ROOT/stage" 'printf agent > modified; printf agent > created; printf agent > removed'
printf external > modified; printf external > created; rm removed
snapshot external "$WS"
for path in modified created removed; do
  expect_refused pvisor apply --path "$path" "$CASE_ROOT/stage"
  assert_unchanged external "$WS"
done
```

### S-STAGE-009: A conflict leaves the entire selection unchanged

**Semantics**: If any selected path conflicts, that apply changes none of the selected paths.

**Violation example**: Apply publishes conflict-free paths before detecting a conflict and exits with partial changes.

<!-- semspec: case id=S-STAGE-009 -->
```bash
require_stage
printf original > a; printf original > z
stage "$CASE_ROOT/stage" 'printf agent > a; printf agent > z'
printf external > z
snapshot external "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged external "$WS"
```

### S-STAGE-010: External changes on untouched paths do not block apply

**Semantics**: External modifications, creations and deletions of paths the Agent did not touch do not block apply and are all preserved.

**Violation example**: A whole-tree conflict check refuses unrelated changes, or apply restores an unrelated external deletion.

<!-- semspec: case id=S-STAGE-010 -->
```bash
require_stage
printf original > touched; mkdir other; printf base > other/edit; printf base > other/deleted
stage "$CASE_ROOT/stage" 'printf agent > touched'
printf external > other/edit; printf external > other/new; rm other/deleted
snapshot external "$WS/other"
pvisor apply --all "$CASE_ROOT/stage"
assert_content touched agent
assert_unchanged external "$WS/other"
```

### S-STAGE-011: Deleting a directory preserves external additions

**Semantics**: If the Agent deletes a directory and an external writer subsequently adds files to it, apply must refuse and preserve the entire external tree.

**Violation example**: Only old directory files are checked and newly added files are recursively deleted.

<!-- semspec: case id=S-STAGE-011 -->
```bash
require_stage
mkdir d; printf base > d/base
stage "$CASE_ROOT/stage" 'rm -rf d'
printf external > d/new
snapshot external "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged external "$WS"
```

### S-STAGE-012: Apply stays within the workspace

**Semantics**: If a directory is replaced by an external symlink after staging, apply does not write to the external directory; this case requires conflict refusal and both trees unchanged.

**Violation example**: Apply follows the replacement link and writes staged content outside the workspace.

<!-- semspec: case id=S-STAGE-012 -->
```bash
require_stage
mkdir d; printf base > d/file
stage "$CASE_ROOT/stage" 'printf agent > d/file'
mkdir "$CASE_ROOT/outside"; printf external > "$CASE_ROOT/outside/file"
rm -rf d; ln -s "$CASE_ROOT/outside" d
snapshot workspace "$WS"; snapshot outside "$CASE_ROOT/outside"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged workspace "$WS"; assert_unchanged outside "$CASE_ROOT/outside"
```

### S-STAGE-013: Symlink results agree with observable effects


**Semantics**: A successful symlink operation creates the corresponding link; a failed operation creates no link. Return values agree with observable effects.

**Violation example**: ln returns EPERM but creates a link that can be applied.

<!-- semspec: case id=S-STAGE-013 xfail-on=macos 'xfail-reason=macFUSE 创建链接返回 EPERM 但已有实际效果；tools/semspec/DESIGN.md §12' -->
```bash
require_stage
stage "$CASE_ROOT/stage" 'code=0; ln -s /etc/hosts link || code=$?; if test "$code" = 0; then test -L link; test "$(readlink link)" = /etc/hosts; else test ! -e link; test ! -L link; fi'
assert_absent link
```

### S-STAGE-014: Apply preserves executable bits and symlink targets

**Semantics**: Executable bits and symlink targets are published unchanged; links are not dereferenced into target files.

**Violation example**: An executable loses its execute bit, or a link becomes a copy of /etc/hosts.

<!-- semspec: case id=S-STAGE-014 -->
```bash
require_stage
stage "$CASE_ROOT/stage" 'printf executable > executable; chmod 755 executable; ln -s executable link'
pvisor apply --all "$CASE_ROOT/stage"
assert_content executable executable
[ -x executable ] || fail 'executable bit lost'
[ -L link ] && [ "$(readlink link)" = executable ] || fail 'link was dereferenced or changed'
```

## Preparation and execution

Use chapter 2 to learn stage, review, apply and drop, then run these independent checks for net effects, conflicts and links. macOS requires macFUSE; Linux requires /dev/fuse and user/mount namespaces. Missing prerequisites exit 77; S-STAGE-013 retains its known macOS xfail. Checks and preparation still require human review.

```bash
just cases --suite stage
just cases --suite stage --case S-STAGE-008 --keep
```

<!-- semspec: setup -->
````bash
export RUST_LOG=warn
# Public CLI only; all personal settings and Job records stay in CASE_ROOT.
pvisor() { XDG_DATA_HOME="$CASE_ROOT/data" XDG_CONFIG_HOME="$CASE_ROOT/config" command "${SUBJECT_BIN:-pvisor}" "$@"; }
stage() { pvisor run --no-agent-defaults --executor host --gateway-mode off --stage "$1" -- /bin/sh -eu -c "$2"; }
review_changes() {
  pvisor status --review --json "$1" > "$CASE_ROOT/review.json"
  python3 - "$CASE_ROOT/review.json" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    changes = json.load(f)["filesystem"]["changes"]
for row in sorted(f'{item["kind"]} {item["path"]}' for item in changes):
    print(row)
PY
}
assert_changes() {
  cat > "$CASE_ROOT/expected.changes"
  review_changes "$1" > "$CASE_ROOT/actual.changes"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/expected.changes" "$CASE_ROOT/actual.changes" || fail 'review differs from net changes'
}

# Environment prerequisites: exit 77 skips this check, never changes its assertions.
skip() { printf 'SKIP: %s\n' "$*" >&2; exit 77; }
require_python3() { python3 --version >/dev/null 2>&1 || skip 'python3 unavailable'; }
require_linux() { [ "$(uname -s)" = Linux ] || skip 'Linux required'; }
require_stage() {
  require_python3
  case "$(uname -s)" in
    Darwin) [ -e /Library/Filesystems/macfuse.fs ] || skip 'macFUSE unavailable' ;;
    Linux) [ -e /dev/fuse ] && unshare -Ur -m true || skip 'FUSE/user namespaces unavailable' ;;
    *) skip 'stage unsupported on this OS' ;;
  esac
}
require_rootless() { require_linux; unshare --user --mount --pid --fork true || skip 'user namespaces unavailable'; }
require_kvm() { require_linux; [ -e /dev/kvm ] || skip '/dev/kvm unavailable'; }
require_rootfs() { require_linux; [ -d "${PVISOR_CASE_ROOTFS:-/}" ] || skip 'rootfs unavailable'; }
require_image() { require_linux; [ -n "${PVISOR_CASE_IMAGE:-ubuntu:latest}" ] || skip 'image unavailable'; }
require_agent() { require_linux; [ -n "${PVISOR_CASE_AGENT:-}" ] || skip 'agent unavailable'; }
require_container() {
  require_linux
  if [ -n "${PVISOR_CASE_CONTAINER_RUNTIME:-}" ]; then
    command -v "$PVISOR_CASE_CONTAINER_RUNTIME" || skip 'container runtime unavailable'
  else
    command -v crun || command -v runc || skip 'container runtime unavailable'
  fi
}
require_container_runtime() { require_container; }
require_runc() { require_linux; command -v runc || skip 'runc unavailable'; }
require_curl() { curl --version >/dev/null 2>&1 || skip 'curl unavailable'; }
````
