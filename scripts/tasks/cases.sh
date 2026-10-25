#!/usr/bin/env bash
set -euo pipefail
suite=doc
if [[ "${1:-}" == --suite ]]; then
  if [[ $# -lt 2 ]]; then echo "--suite requires doc, stage, use or vm" >&2; exit 2; fi
  suite="$2"
  shift 2
fi
case "$suite" in
  doc|vm)
    mise run build -- release
    mise run vm-case-driver --
    export PVISOR_CASE_VM_DRIVER="$CARGO_TARGET_DIR/release/examples/vm_control_case"
    spec=docs/src/zh/reference
    report="$CARGO_TARGET_DIR/pvisor-case-report.json"
    if [[ "$suite" == vm ]]; then
      spec=docs/src/zh/reference/cases-vm.md
      report="$CARGO_TARGET_DIR/pvisor-vm-case-report.json"
    fi
    mise run semaspec -- run "$spec" --domain DOC --subject-bin "$CARGO_TARGET_DIR/release/pvisor" --format json --output "$report" "$@"
    ;;
  stage)
    mise run build -- debug
    mise run semaspec -- run docs/src/zh/cases/06-stage-apply.md --domain STAGE --subject-bin "$CARGO_TARGET_DIR/debug/pvisor" "$@"
    ;;
  use)
    mise run build -- release
    report="$CARGO_TARGET_DIR/pvisor-learning-report.json"
    subject="$CARGO_TARGET_DIR/release/pvisor"
    run_args=()
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --output|--subject-bin|--case)
          if [[ $# -lt 2 ]]; then echo "$1 requires a value" >&2; exit 2; fi
          case "$1" in
            --output) report="$2" ;;
            --subject-bin) subject="$2" ;;
            --case) run_args+=(--case "$2") ;;
          esac
          shift 2
          ;;
        --output=*) report="${1#*=}"; shift ;;
        --subject-bin=*) subject="${1#*=}"; shift ;;
        --case=*) run_args+=(--case "${1#*=}"); shift ;;
        --keep|--require-reviewed) run_args+=("$1"); shift ;;
        *) echo "unknown USE case argument: $1" >&2; exit 2 ;;
      esac
    done
    mise run semaspec -- run docs/src/zh/cases --domain USE --require-pass --subject-bin "$subject" --format json --output "$report" ${run_args[@]+"${run_args[@]}"}
    ;;
  *) echo "expected doc, stage, use or vm, got: $suite" >&2; exit 2 ;;
esac
