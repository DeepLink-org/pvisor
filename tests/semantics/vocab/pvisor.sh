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
