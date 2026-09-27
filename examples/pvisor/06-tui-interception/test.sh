#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
pvisor_example_init "$example_dir" tui-interception

bash "$example_dir/run.sh" --once

test "$(cat "$work_dir/workspace/private/token")" = 'original secret'
test ! -e "$work_dir/workspace/created-by-agent.txt"
test -e "$work_dir/stage/upper/created-by-agent.txt"
jq -e '
  .run.state == "completed" and
  ([.run_observation.filesystem.paths["private/token"][].denied] | any(. > 0))
' "$work_dir/stage/run-bundle.json" >/dev/null
jq -e '
  .network.intercepted.targets["HTTP blocked.example:80"].denied > 0
' "$work_dir/stage/run-bundle.json" >/dev/null

echo 'RESULT example=tui-interception file_denied=true network_denied=true lower_unchanged=true'
