"""Owned loopback static repository with exact Range GET and request evidence."""

import argparse
import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit


def serve(root, ready, requests, deny):
    root = root.resolve(strict=True)

    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            self.request.settimeout(2)
            super().setup()

        def log_message(self, *_args):
            pass

        def reply(self, status, body=b"", content_range=None):
            self.send_response(status)
            self.send_header("Content-Length", str(len(body)))
            if content_range is not None:
                self.send_header("Content-Range", content_range)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = unquote(urlsplit(self.path).path)
            requested_range = self.headers.get("Range")
            with requests.open("a") as log:
                log.write(json.dumps({"path": path, "range": requested_range}) + "\n")
            if deny.exists():
                self.reply(503)
                return
            target = (root / path.lstrip("/")).resolve()
            if not target.is_relative_to(root):
                self.reply(403)
                return
            if not target.is_file():
                self.reply(404)
                return
            body = target.read_bytes()
            if requested_range is None:
                self.reply(200, body)
                return
            match = re.fullmatch(r"bytes=(\d+)-(\d*)", requested_range)
            if match is None:
                self.reply(416, content_range=f"bytes */{len(body)}")
                return
            start = int(match[1])
            end = min(int(match[2]) if match[2] else len(body) - 1, len(body) - 1)
            if start > end:
                self.reply(416, content_range=f"bytes */{len(body)}")
                return
            self.reply(206, body[start : end + 1], f"bytes {start}-{end}/{len(body)}")

    with HTTPServer(("127.0.0.1", 0), Handler) as server:
        ready.write_text(json.dumps({"url": f"http://127.0.0.1:{server.server_port}"}))
        server.serve_forever(poll_interval=0.05)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--ready", type=Path, required=True)
    parser.add_argument("--requests", type=Path, required=True)
    parser.add_argument("--deny", type=Path, required=True)
    args = parser.parse_args()
    serve(args.root, args.ready, args.requests, args.deny)
