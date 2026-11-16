"""Independent OCI byte oracle and bounded test registry.

These fixtures do not organize or time experiments. Rust owns storage/runtime/
remote experiments; this module only supplies synthetic correctness tests.
"""

import hashlib
import http.server
import json
import tarfile
from pathlib import Path

PYTHON_TREE = "/usr/local/lib/python3.12"
MMAP_PROBE = """import os,sys,stat,mmap,hashlib
assert sys.version_info[:2] == (3,12)
root='/usr/local/lib/python3.12'; files=[]; total=0; aggregate=hashlib.sha256()
for parent,dirs,names in os.walk(root,followlinks=False):
 for name in names:
  path=os.path.join(parent,name)
  if stat.S_ISREG(os.lstat(path).st_mode): files.append(path)
for path in sorted(files):
 size=os.stat(path).st_size; total+=size
 with open(path,'rb') as stream:
  if size:
   with mmap.mmap(stream.fileno(),0,access=mmap.ACCESS_READ) as data: digest=hashlib.sha256(data).hexdigest()
  else: digest=hashlib.sha256(b'').hexdigest()
 aggregate.update(path.encode()+b'\\0'+digest.encode()+b'\\n')
assert (len(files),total,aggregate.hexdigest()) == EXPECTED_ORACLE, (len(files),total,aggregate.hexdigest())
print('REFERENCE_READY',flush=True)
"""


def tree_oracle(image):
    """Derive the exact regular-file mmap oracle from verified OCI layer bytes."""
    files = {}
    prefix = PYTHON_TREE.lstrip("/") + "/"
    for layer in image["manifest"]["layers"]:
        with tarfile.open(Path(image["source"]) / layer["digest"][7:], "r:*") as archive:
            members = archive.getmembers()
            for member in members:
                path = member.name.removeprefix("./").lstrip("/")
                parent, _, name = path.rpartition("/")
                if name.startswith(".wh."):
                    victim = parent + "/" + name[4:]
                    if name == ".wh..wh..opq":
                        victim = parent
                    files = {
                        k: v
                        for k, v in files.items()
                        if k != victim and not k.startswith(victim + "/")
                    }
            for member in members:
                path = member.name.removeprefix("./").lstrip("/")
                if not path.startswith(prefix) or Path(path).name.startswith(".wh."):
                    continue
                if member.isfile():
                    with archive.extractfile(member) as stream:
                        digest = hashlib.file_digest(stream, "sha256").hexdigest()
                    files[path] = (member.size, digest)
                elif member.islnk():
                    target = member.linkname.removeprefix("./").lstrip("/")
                    files[path] = files[target]
                else:
                    files.pop(path, None)
    aggregate = hashlib.sha256()
    for path, (_, digest) in sorted(files.items()):
        aggregate.update(b"/" + path.encode() + b"\0" + digest.encode() + b"\n")
    assert files, "empty OCI file oracle"
    return len(files), sum(size for size, _ in files.values()), aggregate.hexdigest()


class Registry(http.server.BaseHTTPRequestHandler):
    """Synthetic read-only OCI endpoint, with exact body accounting."""

    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.reply(False)

    def do_GET(self):
        self.reply(True)

    def reply(self, body):
        status = 200
        digest = self.path.rsplit("/", 1)[-1]
        descriptor = self.server.objects.get(digest)
        if self.path in ("/v2", "/v2/"):
            payload, media = b"{}", "application/json"
        elif descriptor and ("/manifests/" in self.path or "/blobs/" in self.path):
            path, media = descriptor
            payload = path.read_bytes()
        else:
            status = 404
            payload = b'{"errors":[{"code":"BLOB_UNKNOWN"}]}'
            media = "application/json"
        self.send_response(status)
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Content-Type", media)
        self.send_header("Docker-Distribution-Api-Version", "registry/2.0")
        if descriptor:
            self.send_header("Docker-Content-Digest", digest)
        self.end_headers()
        if body:
            self.wfile.write(payload)
        with self.server.guard:
            row = dict(
                path=self.path,
                method=self.command,
                status=status,
                response_bytes=len(payload) if body else 0,
                body_sha256=hashlib.sha256(payload).hexdigest() if body else None,
            )
            with self.server.rows_path.open("a") as stream:
                stream.write(json.dumps(row) + "\n")
