# Public CLI only; all personal settings and Job records stay in CASE_ROOT.
pvisor() { XDG_DATA_HOME="$CASE_ROOT/data" XDG_CONFIG_HOME="$CASE_ROOT/config" "$SUBJECT_BIN" "$@"; }
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
