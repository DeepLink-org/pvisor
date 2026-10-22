"""Persistent guest command worker; Python >=3.9, no model credentials.

Reuses the trajectory smoke's atomic file-handshake and process-group timeout
pattern. This is an experiment protocol for a trusted, single controller.
"""
import http.client
import json
import os
import pwd
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import urlsplit


def publish(path, data):
    temp = path.with_suffix('.tmp')
    with temp.open('w') as stream:
        json.dump(data, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temp, path)


def execute(request, root):
    ident = request['id']
    out, err = root / (ident + '.stdout'), root / (ident + '.stderr')
    user = request.get('user')
    options = {}
    if user is not None:
        entry = pwd.getpwuid(int(user)) if str(user).isdigit() else pwd.getpwnam(str(user))
        options.update(user=entry.pw_uid, group=entry.pw_gid, extra_groups=[])
    started = time.monotonic()
    with out.open('wb') as stdout, err.open('wb') as stderr:
        child = subprocess.Popen(
            ['/bin/bash', '-c', request['command']], cwd=request.get('cwd') or '/',
            env={**os.environ, **request.get('env', {})}, stdout=stdout, stderr=stderr,
            start_new_session=True, **options)
        timed_out = cancelled = False
        while child.poll() is None:
            cancelled = (root / (ident + '.cancel')).exists()
            timed_out = time.monotonic() - started >= request['timeout']
            if cancelled or timed_out:
                break
            time.sleep(.02)
        if timed_out or cancelled:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        code = child.wait()
    return dict(return_code=124 if timed_out else code, timed_out=timed_out,
                cancelled=cancelled, duration_s=time.monotonic() - started,
                raw_return_code=code)


def connect_notifications(token):
    host, port = os.environ['SWE_NOTIFY_ADDRESS'].rsplit(':', 1)
    proxy = os.environ.get('HTTP_PROXY')
    if proxy:
        endpoint = urlsplit(proxy)
        tunnel = http.client.HTTPConnection(endpoint.hostname, endpoint.port, timeout=30)
        tunnel.set_tunnel(host, int(port))
        tunnel.connect()
        wake = tunnel.sock
    else:
        wake = socket.create_connection((host, int(port)), timeout=30)
    wake.sendall((token + '\n').encode())
    assert wake.recv(1) == b'R', 'notification handshake failed'
    wake.settimeout(None)
    return wake


def main():
    token = sys.argv[1]
    root = Path(sys.argv[2]) if len(sys.argv) > 2 else Path('/.sweeval-control')
    root.mkdir(mode=0o700, exist_ok=False)
    wake = connect_notifications(token) if os.environ.get('SWE_NOTIFY_ADDRESS') else None
    publish(root / 'boot.json', dict(token=token, pid=os.getpid(), schema=1))
    while True:
        requests = sorted(root.glob('*.request.json'))
        for path in requests:
            ident = path.name.removesuffix('.request.json')
            result_path = root / (ident + '.result.json')
            if result_path.exists():
                continue
            request = json.loads(path.read_text())
            if request.get('token') != token or request.get('id') != ident:
                continue
            if request.get('op') == 'stop':
                publish(result_path, dict(token=token, id=ident, stopped=True))
                return
            try:
                result = execute(request, root)
            except Exception as exc:
                result = dict(error=type(exc).__name__ + ': ' + str(exc))
            publish(result_path, dict(token=token, id=ident, **result))
        if wake is None:
            time.sleep(.02)
        else:
            try:
                connected = bool(wake.recv(4096))
            except ConnectionResetError:
                connected = False
            if not connected:
                wake.close()
                wake = connect_notifications(token)


if __name__ == '__main__':
    main()
