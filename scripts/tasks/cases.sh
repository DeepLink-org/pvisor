#!/usr/bin/env bash
set -euo pipefail
suite=doc
feature= report= subject= list=0
ids=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --suite|--feature|--output|--subject-bin|--case)
      if [[ $# -lt 2 || -z "$2" ]]; then echo "$1 requires a value" >&2; exit 2; fi
      key="$1"; value="$2"; shift 2
      case "$key" in
        --suite) suite="$value" ;;
        --feature) feature="$value" ;;
        --output) report="$value" ;;
        --subject-bin) subject="$value" ;;
        --case) IFS=, read -r -a batch <<< "$value"; ids+=("${batch[@]}") ;;
      esac
      ;;
    --case=*) IFS=, read -r -a batch <<< "${1#*=}"; ids+=("${batch[@]}"); shift ;;
    --output=*) report="${1#*=}"; shift ;;
    --subject-bin=*) subject="${1#*=}"; shift ;;
    --keep) export PVISOR_CASE_KEEP=1; shift ;;
    --strict) export PVISOR_CASE_REQUIRE_PASS=1; shift ;;
    --require-reviewed)
      echo "Scrut digest review has not been authorized/migrated; refusing --require-reviewed" >&2
      exit 2 ;;
    --list) list=1; shift ;;
    *) echo "unknown case task argument: $1" >&2; exit 2 ;;
  esac
done
case "$suite" in
  doc) pattern='S-DOC-*.md'; default_report=pvisor-case-report.json ;;
  vm) pattern='S-DOC-*.md'; default_report=pvisor-vm-case-report.json ;;
  stage) pattern='S-STAGE-*.md'; default_report=pvisor-stage-report.json ;;
  use) pattern='S-USE-*.md'; default_report=pvisor-learning-report.json; export PVISOR_CASE_REQUIRE_PASS=1 ;;
  *) echo "expected doc, stage, use or vm, got: $suite" >&2; exit 2 ;;
esac
base="$MISE_PROJECT_ROOT/tests/spec"
if [[ -n "$feature" ]]; then
  [[ "$feature" =~ ^[a-z][a-z0-9-]*$ && -d "$base/$feature" ]] || { echo "unknown feature: $feature" >&2; exit 2; }
  base="$base/$feature"
  pattern='S-*.md'
fi
files=()
discovered="$(find "$base" -type f -name "$pattern" | LC_ALL=C sort)"
if [[ -z "$discovered" ]]; then echo "empty case selection" >&2; exit 2; fi
while IFS= read -r file; do
  # VM RAM/control suite is the six preserved DOC IDs, not all VM features.
  if [[ "$suite" == vm && -z "$feature" && ! "$file" =~ S-DOC-0(57|58|59|60|61|62)\.md$ ]]; then continue; fi
  files+=("$file")
done <<< "$discovered"
if [[ ${#files[@]} -eq 0 ]]; then echo "empty case selection" >&2; exit 2; fi
catalog=' '
for file in "${files[@]}"; do
  identifier="${file##*/}"; identifier="${identifier%.md}"
  [[ "$identifier" =~ ^S-[A-Z]+-[0-9]+$ && "$catalog" != *" $identifier "* ]] || { echo "invalid or duplicate catalog ID: $identifier" >&2; exit 2; }
  catalog+="$identifier "
done
if [[ ${#ids[@]} -gt 0 ]]; then
  selected=()
  seen=' '
  for id in "${ids[@]}"; do
    [[ "$id" =~ ^S-[A-Z]+-[0-9]+$ && "$seen" != *" $id "* ]] || { echo "invalid or duplicate case ID: $id" >&2; exit 2; }
    seen+="$id "
    found=0
    for file in "${files[@]}"; do
      if [[ "${file##*/}" == "$id.md" ]]; then selected+=("$file"); found=$((found+1)); fi
    done
    if [[ "$found" != 1 ]]; then echo "case is absent or duplicated: $id" >&2; exit 2; fi
  done
  files=("${selected[@]}")
fi
if [[ "$list" == 1 ]]; then printf '%s\n' "${files[@]}"; exit 0; fi
if [[ -n "$feature" ]]; then default_report="cases-$feature.json"; fi
report="${report:-$CARGO_TARGET_DIR/$default_report}"
[[ "$report" == *.json ]] || { echo "report must be a separate .json artifact" >&2; exit 2; }
mkdir -p "$(dirname "$report")"
report="$(cd "$(dirname "$report")" && pwd -P)/$(basename "$report")"
[[ "$report" != *'/tests/spec/'* ]] || { echo "report must not replace specification inputs" >&2; exit 2; }
rm -f -- "$report"
profile=release
if [[ "$suite" == stage ]]; then profile=debug; fi
if [[ -z "$subject" ]]; then
  mise run build -- "$profile"
  subject="$CARGO_TARGET_DIR/$profile/pvisor"
fi
[[ -x "$subject" ]] || { echo "subject is not executable: $subject" >&2; exit 2; }
export SUBJECT_BIN
SUBJECT_BIN="$(cd "$(dirname "$subject")" && pwd)/$(basename "$subject")"
needs_driver=0
for file in "${files[@]}"; do
  if [[ "$file" =~ S-DOC-0(59|60|61|62)\.md$ ]]; then needs_driver=1; fi
done
if [[ "$needs_driver" == 1 ]]; then
  if [[ -z "${PVISOR_CASE_VM_DRIVER:-}" ]]; then
    mise run vm-case-driver --
    export PVISOR_CASE_VM_DRIVER="$CARGO_TARGET_DIR/release/examples/vm_control_case"
  fi
fi
export PVISOR_CASE_ARTIFACTS="${PVISOR_CASE_ARTIFACTS:-/tmp/pvisor-cases-$UID}"
temporary="$(mktemp "$report.XXXXXX")"
trap 'rm -f -- "$temporary"' EXIT
status=0
mise run scrut -- test --renderer json "${files[@]}" > "$temporary" || status=$?
mv -- "$temporary" "$report"
python3 "$MISE_PROJECT_ROOT/tests/spec/report.py" "$report" "${files[@]}" || exit 1
exit "$status"
