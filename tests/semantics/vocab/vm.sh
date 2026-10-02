# VM case prerequisites and SDK driver selection; no product failure becomes SKIP.
require_vm_case() {
  require_python3
  case "$(uname -s)" in
    Linux)
      [ -r /dev/kvm ] && [ -w /dev/kvm ] || skip '/dev/kvm unavailable'
      export PVISOR_CASE_ROOTFS="${PVISOR_CASE_ROOTFS:-/}"
      ;;
    Darwin)
      [ "$(uname -m)" = arm64 ] || skip 'Apple Silicon required'
      [ -n "${PVISOR_CASE_ROOTFS:-}" ] || skip 'explicit Linux guest rootfs required'
      ;;
    *) skip 'VM unsupported on this OS' ;;
  esac
  [ -d "$PVISOR_CASE_ROOTFS" ] || skip 'guest rootfs unavailable'
}

vm_case_setup() {
  case_setup
  export XDG_CACHE_HOME="$CASE_ROOT/cache"
  mkdir -p "$XDG_CACHE_HOME"
}

require_vm_sdk() {
  require_vm_case
  export VM_CASE_DRIVER="${PVISOR_CASE_VM_DRIVER:-$(dirname "$SUBJECT_BIN")/examples/vm_control_case}"
  [ -x "$VM_CASE_DRIVER" ] || skip 'SDK driver unavailable; prepare with just vm-cases'
  export PVISOR_CASE_VM_PYTHON="${PVISOR_CASE_VM_PYTHON:-/usr/bin/python3}"
  [ -x "$PVISOR_CASE_ROOTFS/$PVISOR_CASE_VM_PYTHON" ] || skip 'guest Python unavailable'
}

require_vm_compression() {
  require_vm_sdk
  case "$(uname -s)" in
    Linux) [ -r /dev/fuse ] && [ -w /dev/fuse ] || skip '/dev/fuse unavailable' ;;
    Darwin) [ -d /Library/Filesystems/macfuse.fs ] || skip 'macFUSE kernel backend unavailable' ;;
  esac
}
