#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" tui-interception
command -v curl >/dev/null

mode="${1:-}"
if [[ -n "$mode" && "$mode" != '--once' ]]; then
  echo "usage: $0 [--once]" >&2
  exit 2
fi

pvisor_example_reset
workspace="$work_dir/workspace"
stage="$work_dir/stage"
mkdir -p "$workspace/private"
printf 'original secret\n' >"$workspace/private/token"
cp "$example_dir/agent.sh" "$workspace/agent.sh"

args=(
  --stage "$stage"
  --access 'private/**:deny'
  --overlaynet-deny blocked.example
)
if [[ "$mode" != '--once' ]]; then
  args=(--tui "${args[@]}")
fi

echo 'The Job will touch a staged file, deny a protected touch, and deny a proxied curl.'
set +e
(
  cd "$workspace"
  "$pvisor_bin" "${args[@]}" -- bash ./agent.sh "$mode"
)
job_status=$?
set -e

echo
echo "Lower protected file: $(cat "$workspace/private/token")"
if [[ -e "$workspace/created-by-agent.txt" ]]; then
  echo 'ERROR: staged file reached the lower workspace' >&2
  exit 1
fi
echo 'Lower workspace has no created-by-agent.txt'
echo
if [[ -f "$stage/run-bundle.json" ]]; then
  "$pvisor_bin" status --review "$stage"
fi
exit "$job_status"
