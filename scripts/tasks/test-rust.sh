#!/usr/bin/env bash
set -euo pipefail
args=()
needs_vm_signature=0
while [[ $# -gt 0 && "$1" != -- ]]; do
  package="$1"
  shift
  case "$package" in
    pvisor) package=pvisor ;;
            cli) package=pvisor-cli ;;
    core|control|agentctl) package=pvisor-core ;;
    capture) package=pvisor-gateway ;;
    shim) package=pvisor-shim ;;
  esac
  if [[ "$package" == pvisor-vm || "$package" == pvisor || "$package" == pvisor-cli ]]; then needs_vm_signature=1; fi
  args+=(-p "$package")
done
if [[ ${#args[@]} -eq 0 ]]; then args+=(--workspace); needs_vm_signature=1; fi
if [[ "${1:-}" == -- ]]; then shift; fi
if [[ "$needs_vm_signature" == 1 && "$(uname -s)" == Darwin ]]; then
  # Sign the same build selection as run; runner-only flags do not belong to list.
  signature_args=("${args[@]}")
  remaining=("$@")
  index=0
  while [[ "$index" -lt "$#" ]]; do
    option="${remaining[$index]}"
    case "$option" in
      --) break ;;
      -p|--package|--exclude|--bin|--example|--test|--bench|-F|--features|--build-jobs|--cargo-profile|--target|--target-dir|--manifest-path|--config|-Z)
        signature_args+=("$option")
        index=$((index + 1))
        if [[ "$index" -ge "$#" ]]; then echo "missing value for $option" >&2; exit 2; fi
        signature_args+=("${remaining[$index]}")
        ;;
      --package=*|--exclude=*|--bin=*|--example=*|--test=*|--bench=*|--features=*|--build-jobs=*|--cargo-profile=*|--target=*|--target-dir=*|--manifest-path=*|--config=*|-F?*|-p?*|-Z?*|--workspace|--all|--lib|--bins|--examples|--tests|--benches|--all-targets|--all-features|--no-default-features|-r|--release|--frozen|--locked|--offline|--ignore-rust-version)
        signature_args+=("$option")
        ;;
    esac
    index=$((index + 1))
  done
  mise run sign-vm-tests -- "${signature_args[@]}"
fi
cargo nextest run --locked "${args[@]}" "$@"
