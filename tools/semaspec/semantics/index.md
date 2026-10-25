# Runner review preparation

These functions create only temporary fixture projects and simulated review records.
S-REVIEW-004 remains human-only; AI must not execute it.

<!-- semspec: setup -->
````bash
# Sealed assertion vocabulary. Exact bytes matter, including final newlines.
fail() { printf 'SEMANTIC VIOLATION: %s\n' "$*" >&2; exit 1; }
expect_exit() {
  local want=$1 got=0; shift
  "$@" || got=$?
  [ "$got" -eq "$want" ] || fail "expected exit $want, got $got: $*"
}
expect_refused() { if "$@"; then fail "expected refusal: $*"; fi; }
snapshot() {
  [[ $1 =~ ^[a-zA-Z0-9_-]+$ ]] || fail 'invalid snapshot name'
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/snapshot.$1"
}
assert_unchanged() {
  [[ $1 =~ ^[a-zA-Z0-9_-]+$ ]] || fail 'invalid snapshot name'
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/current.tree"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/snapshot.$1" "$CASE_ROOT/current.tree" || fail "tree changed: $2"
}
assert_same_tree() {
  "$SEMSPEC_BIN" helper tree-state "$1" > "$CASE_ROOT/a.tree"
  "$SEMSPEC_BIN" helper tree-state "$2" > "$CASE_ROOT/b.tree"
  "$SEMSPEC_BIN" helper diff "$CASE_ROOT/a.tree" "$CASE_ROOT/b.tree" || fail 'trees differ'
}
assert_content() {
  [ -f "$1" ] && [ ! -L "$1" ] || fail "not a regular file: $1"
  cmp -s -- "$1" <(printf '%s' "$2") || fail "content differs: $1"
}
assert_absent() { if [ -e "$1" ] || [ -L "$1" ]; then fail "path exists: $1"; fi; }

# Ephemeral test data only. This never approves a real specification.
fixture() {
  mkdir -p "$CASE_ROOT/project/spec"
  printf '<!-- semspec: setup -->\n```bash\nnoop() { :; }\n```\n' > "$CASE_ROOT/project/spec/index.md"
  cat > "$CASE_ROOT/project/spec/case.md" <<'SPEC'
### S-FIXTURE-001: preserve

**语义**: preserve the result.

**违反示例**: alter the result.

<!-- semspec: case id=S-FIXTURE-001 -->
```bash
true
```
SPEC
  # Independent digest calculation for a simulated human approval in test data.
  python3 - "$CASE_ROOT/project" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
normalize = lambda s: ('\n'.join(line.rstrip() for line in s.splitlines()).rstrip('\n') + '\n').encode()
sha = lambda b: 'sha256:' + hashlib.sha256(b).hexdigest()
vocab = sha(b'semspec/vocab/v1\0index.md\0' + normalize((root/'spec/index.md').read_text()))
case = sha(b'semspec/case/v1\0' + normalize((root/'spec/case.md').read_text()) + b'\0' + vocab.encode() + b'\0' + b'6')
(root/'spec/REVIEWED.toml').write_text(f'format = 1\n[[approval]]\nitem = "S-FIXTURE-001"\ndigest = "{case}"\nreviewer = "simulated-fixture"\ndate = "2026-10-01"\n')
PY
}
fixture_state() { "$SUBJECT_BIN" --spec-dir "$CASE_ROOT/project/spec" list; }

require_python() { python3 --version >/dev/null 2>&1 || { echo 'SKIP: python3 unavailable' >&2; exit 77; }; }

````
