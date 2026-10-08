#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" changeset-management
command -v jq >/dev/null

pvisor_example_reset
mkdir -p "$work_dir/base"
printf 'original\n' >"$work_dir/base/existing.txt"
base="$work_dir/base"
apply_stage="$PVISOR_RUN_HOME/run-apply"
drop_stage="$PVISOR_RUN_HOME/run-drop"

(
  cd "$base"
  "$pvisor_bin" run --stage "$apply_stage" --stdio capture -- \
    /bin/sh -c 'printf "accepted\n" > existing.txt; printf "accepted\n" > accepted.txt'
)
"$pvisor_bin" status --review --json "$apply_stage" >"$work_dir/apply-review.json"
jq '{run, filesystem}' "$work_dir/apply-review.json"
"$pvisor_bin" apply "$apply_stage" --all >/dev/null

echo 'Base directory after apply:'
cat "$base/existing.txt"
cat "$base/accepted.txt"

(
  cd "$base"
  "$pvisor_bin" run --stage "$drop_stage" --stdio capture -- \
    /bin/sh -c 'printf "rejected\n" > rejected.txt'
)
"$pvisor_bin" status --review --json "$drop_stage" >"$work_dir/drop-review.json"
jq '{run, filesystem}' "$work_dir/drop-review.json"
"$pvisor_bin" drop "$drop_stage" >/dev/null

echo 'Base directory after drop:'
cat "$base/existing.txt"
cat "$base/accepted.txt"
