"""Deterministic test scaffold: model -> real workspace/tool -> model.

The upstream fixture supplies model replies; tool execution and HTTP requests
are real. This validates integration, not model quality or agent performance.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.error
import urllib.request

assert "PVISOR_CLUSTER_TOKEN" not in os.environ
assert "PVISOR_CLUSTER_WORKER_TOKEN" not in os.environ
assert "PVISOR_TEST_MODEL_KEY" not in os.environ
assert os.environ["OPENAI_API_KEY"].startswith("pvisor-local-")

def request(messages, model="test-model"):
    body = json.dumps({"model": model, "messages": messages, "stream": False,
                       "tools": [{"type": "function", "function": {
                           "name": "write_and_test", "parameters": {
                               "type": "object", "properties": {
                                   "path": {"type": "string"},
                                   "contents": {"type": "string"}},
                               "required": ["path", "contents"]}}}]}).encode()
    req = urllib.request.Request(os.environ["OPENAI_BASE_URL"].rstrip("/") + "/chat/completions",
                                 data=body, headers={"Content-Type": "application/json",
                                 "Authorization": "Bearer " + os.environ["OPENAI_API_KEY"]})
    with urllib.request.urlopen(req, timeout=10) as response:
        return json.load(response)

messages = [{"role": "user", "content": "Implement multiply(a, b) in answer.py and test three cases."}]
first = request(messages)
assistant = first["choices"][0]["message"]
call = assistant["tool_calls"][0]
assert call["function"]["name"] == "write_and_test"
args = json.loads(call["function"]["arguments"])
assert args["path"] == "answer.py"
Path(args["path"]).write_text(args["contents"])
tool = subprocess.run([sys.executable, "-c",
    "import answer; assert answer.multiply(6, 7) == 42; "
    "assert answer.multiply(-2, 4) == -8; assert answer.multiply(0, 9) == 0; "
    "print('3 tests passed')"], check=True, capture_output=True, text=True)
assert tool.stdout == "3 tests passed\n"
messages.extend([assistant, {"role": "tool", "tool_call_id": call["id"], "content": tool.stdout}])
final = request(messages)
assert final["choices"][0]["message"]["content"] == "3 tests passed"
# A route's existence does not grant access beyond the Run's model capability.
try:
    request(messages, model="forbidden-model")
except urllib.error.HTTPError as error:
    assert error.code == 403, error.code
else:
    raise AssertionError("unauthorized model reached the upstream")
if os.environ.get("PVISOR_TEST_BINARY_ARTIFACT") == "1":
    # Cross several upload chunks with non-UTF-8 bytes in an actual guest write.
    Path("binary-result").write_bytes(bytes(range(256)) * 8193)
print("agent loop completed: 3 tests passed; unauthorized model denied")
