#!/usr/bin/env bash
set -euo pipefail
if [[ $# -gt 2 ]]; then echo "too many arguments for build" >&2; exit 2; fi
if [[ $# -lt 1 ]]; then set -- "$@" debug; fi
if [[ $# -lt 2 ]]; then set -- "$@" all; fi
case "$1" in
  debug) cargo_profile=dev ;;
  release) cargo_profile=release ;;
  performance) cargo_profile=performance ;;
  *) echo "expected debug, release or performance, got: $1" >&2; exit 2 ;;
esac
component_args=()
case "$2" in
  all|product)
    names=(pvisor pvisor-tui pvisor-replay pvisor-daemon)
    if [[ "$2" == all ]]; then
      component_args=(--all)
      if [[ "$(uname -s)-$(uname -m)" == Linux-x86_64 ]]; then names+=(containerd-shim-pvisor-v2 pvisor-guest); fi
    fi
    ;;
  daemon) component_args=(--daemon); names=(pvisor-daemon) ;;
  shim|shim-vm) component_args=("--$2"); names=(containerd-shim-pvisor-v2) ;;
  *) echo "expected all, product, daemon, shim or shim-vm, got: $2" >&2; exit 2 ;;
esac
python3 scripts/packaging/stage_wheel_binaries.py --profile "$cargo_profile" --target-dir "$CARGO_TARGET_DIR" ${component_args[@]+"${component_args[@]}"}
for name in "${names[@]}"; do
  binary="$CARGO_TARGET_DIR/$1/$name"
  test -x "$binary"
  if [[ ( "$2" == all || "$2" == product || "$2" == daemon ) && "$(uname -s)" == Darwin ]]; then
    codesign --force --sign - --entitlements "$MISE_PROJECT_ROOT/crates/pvisor/macos-hypervisor.entitlements" "$binary"
    codesign --verify --strict "$binary"
  fi
done
