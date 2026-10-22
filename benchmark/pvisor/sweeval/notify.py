"""Host file notifications wake a blocking guest worker over its real NIC.

Only command arrival is signalled. There are no model-wait declarations and no
periodic guest polling timer. The existing mailbox owns payloads and identity.
"""
import ctypes
import json
import os
import select
import socket
import struct
import threading
import time
from pathlib import Path


class Notifier:
    def __init__(self, root, token, host='127.0.0.1', busy_keepalive_s=60):
        self.root, self.token = root, token
        self.busy_keepalive_s = busy_keepalive_s
        self.listener = socket.socket()
        self.listener.bind((host, 0))
        self.listener.listen(1)
        self.listener.settimeout(.2)
        self.address = '%s:%s' % self.listener.getsockname()
        self.stopping = threading.Event()
        self.error = None
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            while not self.stopping.is_set():
                self.serve_connection()
        except Exception as exc:
            if not self.stopping.is_set():
                self.error = str(exc)
                (Path(self.root).parent.parent.parent / 'notify-error.json').write_text(json.dumps({'error':self.error}))

    def serve_connection(self):
        fd = None
        try:
            while not self.stopping.is_set():
                try:
                    conn, _ = self.listener.accept()
                    break
                except socket.timeout:
                    continue
            else:
                return
            with conn:
                conn.settimeout(10)
                hello = bytearray()
                while not hello.endswith(b'\n') and len(hello) < 128:
                    part = conn.recv(1)
                    if not part:
                        raise RuntimeError('notification peer disconnected during handshake')
                    hello.extend(part)
                if hello.decode().strip() != self.token:
                    raise RuntimeError('notification peer identity mismatch')
                libc = ctypes.CDLL(None, use_errno=True)
                fd = libc.inotify_init1(os.O_CLOEXEC | os.O_NONBLOCK)
                if fd < 0 or libc.inotify_add_watch(fd, os.fsencode(self.root), 0x80 | 0x08) < 0:
                    raise OSError(ctypes.get_errno(), 'cannot watch command mailbox')
                # Watch before readiness and first scan: publication cannot fall
                # between a scan and arming the notification source.
                conn.sendall(b'RW')
                last_check = time.monotonic()
                while not self.stopping.is_set():
                    readable, _, _ = select.select([fd, conn], [], [], .2)
                    if time.monotonic() - last_check >= self.busy_keepalive_s:
                        last_check = time.monotonic()
                        # Only a running tool needs this: virtual TCP otherwise
                        # expires after five idle minutes during long builds.
                        # No heartbeat during the Host Agent's model wait.
                        if any(not path.with_name(path.name.replace('.request.json', '.result.json')).exists()
                               for path in self.root.glob('*.request.json')):
                            conn.sendall(b'W')
                    if conn in readable:
                        if not conn.recv(1):
                            return
                    if fd not in readable:
                        continue
                    events = os.read(fd, 65536)
                    offset, wake = 0, False
                    while offset < len(events):
                        _, mask, _, size = struct.unpack_from('iIII', events, offset)
                        name = events[offset+16:offset+16+size].split(b'\0')[0]
                        offset += 16 + size
                        wake |= name.endswith(b'.request.json') or bool(mask & 0x4000)
                    if wake:
                        conn.sendall(b'W')
        except (ConnectionResetError, BrokenPipeError):
            # Virtual TCP may expire while a long tool runs. A reconnect gets
            # a fresh watch and initial scan; the mailbox remains authoritative.
            pass
        finally:
            if fd is not None and fd >= 0:
                os.close(fd)

    def close(self):
        self.stopping.set()
        self.thread.join(2)
        self.listener.close()
