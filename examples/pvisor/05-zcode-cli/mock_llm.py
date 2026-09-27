#!/usr/bin/env python3
"""Local OpenAI SSE fixture driving one real ZCode file-tool invocation."""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        print(json.dumps({"path": self.path, "body": body}), flush=True)
        names = [t["function"]["name"] for t in body.get("tools", [])]
        completed = any(m.get("role") == "tool" for m in body["messages"])
        scenario = sys.argv[2] if len(sys.argv) > 2 else "write"
        tool = "Bash" if scenario == "timeout" else "Write"
        arguments = (
            {
                "command": "sleep 120 & wait",
                "description": "pVisor timeout fixture",
                "timeout": 120000,
            }
            if scenario == "timeout"
            else {"file_path": "hello.txt", "content": "hello from zcode\n"}
        )
        if not completed and tool in names:
            delta = {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_pvisor_write",
                        "type": "function",
                        "function": {"name": tool, "arguments": json.dumps(arguments)},
                    }
                ],
            }
            finish = "tool_calls"
        else:
            delta = {"role": "assistant", "content": "PVISOR_ZCODE_OK"}
            finish = "stop"
        if not body.get("stream"):
            self.send_error(400, "This fixture expects ZCode SSE streaming")
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for chunk, reason in [(delta, None), ({}, finish)]:
            event = {
                "id": "mock-zcode",
                "object": "chat.completion.chunk",
                "created": 1,
                "model": body.get("model"),
                "choices": [{"index": 0, "delta": chunk, "finish_reason": reason}],
            }
            self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *_):
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
