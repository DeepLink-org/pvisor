"""Owned TLS OCI registry fixture. Files and keys exist only in a test TempDir."""

import gzip
import hashlib
import io
import json
import ssl
import subprocess
import sys
import tarfile
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

root, architecture = Path(sys.argv[1]), sys.argv[2]
root.mkdir(parents=True)


def openssl(*args):
    subprocess.run(["openssl", *args], cwd=root, check=True, capture_output=True)


openssl(
    "req",
    "-x509",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-days",
    "1",
    "-subj",
    "/CN=pvisor-test-root",
    "-keyout",
    "ca.key",
    "-out",
    "ca.pem",
)
openssl(
    "req",
    "-new",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-subj",
    "/CN=127.0.0.1",
    "-keyout",
    "server.key",
    "-out",
    "server.csr",
)
(root / "extensions").write_text("subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\n")
openssl(
    "x509",
    "-req",
    "-in",
    "server.csr",
    "-CA",
    "ca.pem",
    "-CAkey",
    "ca.key",
    "-CAcreateserial",
    "-days",
    "1",
    "-extfile",
    "extensions",
    "-out",
    "server.pem",
)

buffer = io.BytesIO()
with tarfile.open(fileobj=buffer, mode="w") as archive:
    files = {"large": bytes([42]) * (2 * 1024 * 1024 + 13)}
    files.update({f"small-{n:03}": f"value-{n}".encode() for n in range(100)})
    if len(sys.argv) > 3:
        files["guest"] = Path(sys.argv[3]).read_bytes()
    for name, data in files.items():
        entry = tarfile.TarInfo(name)
        entry.size, entry.mode = len(data), 0o755 if name == "guest" else 0o640
        entry.uid, entry.gid = 123, 456
        archive.addfile(entry, io.BytesIO(data))
    entry = tarfile.TarInfo("alias")
    entry.type, entry.linkname = tarfile.SYMTYPE, "large"
    archive.addfile(entry)
    root_entry = tarfile.TarInfo(".")
    root_entry.type, root_entry.mode, root_entry.mtime = tarfile.DIRTYPE, 0o555, 0
    archive.addfile(root_entry)


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


raw = buffer.getvalue()
layer = gzip.compress(raw, mtime=0)
config = json.dumps(
    {
        "architecture": architecture,
        "os": "linux",
        "config": {"Env": ["EXAMPLE=value"], "Entrypoint": ["/bin/sh"], "Cmd": ["-c"]},
        "rootfs": {"type": "layers", "diff_ids": [digest(raw)]},
    }
).encode()
manifest = json.dumps(
    {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": digest(config),
            "size": len(config),
        },
        "layers": [
            {
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": digest(layer),
                "size": len(layer),
            }
        ],
    }
).encode()
objects = {f"/v2/example/manifests/{tag}": manifest for tag in ("test", digest(manifest))}
objects.update({f"/v2/example/blobs/{digest(data)}": data for data in (layer, config)})


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body = objects.get(self.path)
        if body is None or ((root / "fail").exists() and self.path.endswith(digest(layer))):
            self.send_error(503)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
        self.end_headers()
        self.wfile.write(body)


server = HTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / "server.pem", root / "server.key")
server.socket = context.wrap_socket(server.socket, server_side=True)
print(
    json.dumps(
        {"reference": f"127.0.0.1:{server.server_port}/example:test", "digest": digest(manifest)}
    ),
    flush=True,
)
server.serve_forever()
