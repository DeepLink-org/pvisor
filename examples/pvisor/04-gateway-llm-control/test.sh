#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
work_root="${WORK_ROOT:-$example_dir/.work}"
work_dir="$work_root/gateway-llm-control"
export WORK_ROOT="$work_root"

bash "$example_dir/run.sh"

run_dir="$(find "$work_dir/runs" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
test -n "$run_dir"
upstream_posts="$(grep -c 'POST /v1/chat/completions' "$work_dir/mock.log")"
test "$upstream_posts" = 2
PYTHONPATH="$example_dir" python3 - "$run_dir/.capture/events.jsonl" <<'PYTHON'
import json
import sys
from pathlib import Path

from dialogue_fixture import REPLIES, TURNS

events = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines()]
llm = [event for event in events if event["kind"] in {"llm.request", "llm.response"}]
assert [event["kind"] for event in llm] == ["llm.request", "llm.response"] * 2
assert len({event["call_id"] for event in llm}) == 2
messages = []
for index, (user, reply) in enumerate(zip(TURNS, REPLIES, strict=True)):
    request, response = llm[index * 2:index * 2 + 2]
    assert request["call_id"] == response["call_id"]
    messages.append({"role": "user", "content": user})
    assert request["payload"]["http"]["request_body"]["messages"] == messages
    assert response["payload"]["status"] == 200
    assert response["payload"]["assistant_content"] == reply
    messages.append({"role": "assistant", "content": reply})
PYTHON
jq -e '
  .run.state == "completed" and
  .network.intercepted.requests_seen == 2 and
  .network.intercepted.sink_requests == 2 and
  .network.intercepted.failures == 0
' "$run_dir/run-bundle.json" >/dev/null

echo 'RESULT example=gateway-llm-control upstream_posts=2 sink_requests=2 llm_events=4 failures=0'
