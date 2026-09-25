#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$example_dir/../common.sh"
case "${MOCK_SCENARIO:-write}" in
  write) scenario=zcode-cli ;;
  timeout) scenario=zcode-cli-timeout ;;
  *) echo 'MOCK_SCENARIO must be write or timeout' >&2; exit 2 ;;
esac
pvisor_example_init "$example_dir" "$scenario"
: "${ZCODE_NODE:?set an absolute Node.js path}"
: "${ZCODE_ENTRY:?set an absolute zcode.cjs path}"
: "${ZCODE_BUILTIN_PROVIDER_CONFIG_FILE:?set the bundled zcode-builtin.json path}"
: "${ZCODE_RUNTIME_ROOT:?set the installed CLI runtime directory}"
command -v zcode >/dev/null
command -v python3 >/dev/null
command -v jq >/dev/null
# Resolve before entering the fixture workspace; paths with spaces are supported.
work_dir="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$work_dir")"
test -x "$ZCODE_NODE"
test -f "$ZCODE_ENTRY"
test -f "$ZCODE_BUILTIN_PROVIDER_CONFIG_FILE"
case "$ZCODE_NODE" in /*) ;; *) echo 'ZCODE_NODE must be absolute' >&2; exit 2 ;; esac
case "$ZCODE_ENTRY" in /*) ;; *) echo 'ZCODE_ENTRY must be absolute' >&2; exit 2 ;; esac
case "$ZCODE_BUILTIN_PROVIDER_CONFIG_FILE" in /*) ;; *) echo 'Provider config path must be absolute' >&2; exit 2 ;; esac
pvisor_example_reset
read -r mock_port proxy_port admin_port <<<"$(pvisor_free_ports 3)"
python3 "$example_dir/prepare.py" "$work_dir" "http://127.0.0.1:$proxy_port/v1" "$pvisor_bin"
python3 "$example_dir/mock_llm.py" "$mock_port" "${MOCK_SCENARIO:-write}" >"$work_dir/mock.jsonl" 2>"$work_dir/mock.stderr" &
mock_pid=$!
trap 'kill "$mock_pid" 2>/dev/null || true; wait "$mock_pid" 2>/dev/null || true' EXIT
pvisor_wait_tcp "$mock_port"
zcode --version >"$work_dir/zcode-version.txt"
"$pvisor_bin" --version >"$work_dir/pvisor-version.txt"
prompt='Create hello.txt containing exactly hello from zcode followed by a newline. Then reply PVISOR_ZCODE_OK.'
zcode_mode=edit
if [[ "${MOCK_SCENARIO:-write}" == timeout ]]; then
  prompt='Run sleep 120 in Bash and wait for it. This tests supervisor cancellation.'
  # A deterministic mock requests only sleep; headless edit mode would reject Bash.
  zcode_mode=yolo
fi
set +e
(
  cd "$work_dir/base"
  export XDG_CONFIG_HOME="$work_dir/config"
  export ZCODE_DATA_BASE_DIR="$work_dir/state"
  export ZCODE_STORAGE_DIR="$work_dir/state/storage"
  export ZCODE_SESSION_DB_PATH="$work_dir/state/storage/sessions.sqlite"
  export ZCODE_LOG_DIR="$work_dir/state/logs"
  export ZCODE_PERSONAL_PROVIDER_CONFIG_FILE="$work_dir/state/provider.json"
  export ZCODE_MODEL_TELEMETRY_ENABLED=false
  "$pvisor_bin" run --name zcode-cli --stage "$work_dir/stage" --stdio capture --timeout "${PVISOR_TIMEOUT:-60s}" \
    --pass-env ZCODE_DATA_BASE_DIR --pass-env ZCODE_STORAGE_DIR --pass-env ZCODE_SESSION_DB_PATH \
    --pass-env ZCODE_LOG_DIR --pass-env ZCODE_PERSONAL_PROVIDER_CONFIG_FILE \
    --pass-env ZCODE_BUILTIN_PROVIDER_CONFIG_FILE --pass-env ZCODE_MODEL_TELEMETRY_ENABLED \
    --gateway-mode capture --gateway-level full \
    --overlaynet-listen "127.0.0.1:$proxy_port" \
    --gateway-admin-listen "127.0.0.1:$admin_port" \
    --gateway-route "name=\"*\", upstream=\"http://127.0.0.1:$mock_port/v1\"" \
    -- zcode --prompt "$prompt" --mode "$zcode_mode" --no-color
)
status=$?
set -e
if [[ -f "$work_dir/stage/run-bundle.json" ]]; then
  jq '.run | {state, exit_code, failure, output}' "$work_dir/stage/run-bundle.json"
fi
if [[ "$status" -eq 0 ]]; then
  "$pvisor_bin" review --json "$work_dir/stage" >"$work_dir/review.json"
  echo "Review: $pvisor_bin review $work_dir/stage"
  echo "Apply only the task output: $pvisor_bin apply $work_dir/stage --path hello.txt"
fi
exit "$status"
