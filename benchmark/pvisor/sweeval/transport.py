"""Host side of the resident worker protocol, shared by SweEval and mini."""
import json
import os
import select
import shutil
import stat
import time
import uuid
from pathlib import Path


def event(path, name, **values):
    row = dict(host_monotonic_ns=time.monotonic_ns(), event=name, **values)
    with Path(path).open('a') as stream:
        stream.write(json.dumps(row) + '\n')


def publish(path, data):
    temp = path.with_suffix('.tmp')
    with temp.open('x') as stream:
        json.dump(data, stream)
        stream.flush()
        os.fsync(stream.fileno())
    temp.chmod(0o600)
    os.replace(temp, path)


class RunnerLiveness:
    """Linux pidfd bound to a Host-provided runner identity, never a guest PID.

    A readable pidfd notifies exit even while the outer driver is finalizing or
    blocked. Check start time after opening to reject PID reuse during handoff.
    Missing pidfd support/identity fails startup; never silently falls back to
    testing the service wrapper. The caller owns and closes this object.
    """
    def __init__(self, identity):
        self.pid = int(identity['pid'])
        self.fd = None
        self.poll = select.poll()
        try:
            self.fd = os.pidfd_open(self.pid)
            self.poll.register(self.fd, select.POLLIN)
            text = Path(f'/proc/{self.pid}/stat').read_text()
            fields = text.rsplit(')', 1)[1].split()
            if int(fields[19]) != int(identity['start_time_ticks']):
                raise RuntimeError('VM runner identity changed before pidfd attachment')
            if not self():
                raise RuntimeError('VM runner exited before pidfd attachment')
        except BaseException:
            self.close()
            raise

    def __call__(self):
        return self.fd is not None and not self.poll.poll(0)

    def close(self):
        if self.fd is not None:
            self.poll.unregister(self.fd)
            os.close(self.fd)
            self.fd = None


class Client:
    def __init__(self, directory, token, events, is_alive=None):
        self.root, self.token, self.events = Path(directory), token, events
        self._fd = None
        self.is_alive = is_alive

    def directory_fd(self):
        if self._fd is None:
            self._fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        return self._fd

    def read(self, name):
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                     dir_fd=self.directory_fd())
        with os.fdopen(fd, 'rb') as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise ValueError('IPC artifact is not a regular file')
            return stream.read().decode(errors='replace')

    def exists(self, name):
        try:
            os.stat(name, dir_fd=self.directory_fd(), follow_symlinks=False)
            return True
        except FileNotFoundError:
            return False

    def write(self, name, data):
        temporary = name + '.' + uuid.uuid4().hex + '.tmp'
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=self.directory_fd())
        with os.fdopen(fd, 'w') as stream:
            json.dump(data, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, name, src_dir_fd=self.directory_fd(), dst_dir_fd=self.directory_fd())

    def payload(self, source):
        name = uuid.uuid4().hex + '.payload'
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=self.directory_fd())
        with os.fdopen(fd, 'wb') as output, Path(source).open('rb') as stream:
            shutil.copyfileobj(stream, output)
        return name

    def cancel(self, ident):
        try:
            fd = os.open(ident + '.cancel', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=self.directory_fd())
            os.close(fd)
        except FileExistsError:
            pass

    def close(self):
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None

    def call(self, command='', *, timeout=60, cwd='/', env=None, user=None, op='exec', cancel_event=None):
        ident = uuid.uuid4().hex
        result_name = ident + '.result.json'
        event(self.events, 'tool_start', execution_id=ident, operation=op)
        self.write(ident + '.request.json', dict(id=ident, token=self.token, op=op, command=command,
                                  timeout=timeout, cwd=cwd, env=env or {}, user=user))
        deadline = time.monotonic() + timeout + 15
        while not self.exists(result_name):
            if self.is_alive is not None and not self.is_alive():
                event(self.events, 'tool_transport_error', execution_id=ident,
                      reason='VM process exited before command acknowledgement')
                raise RuntimeError('VM process exited; inspect driver.log and run-result.json')
            if cancel_event is not None and cancel_event.is_set():
                self.cancel(ident)
            if time.monotonic() >= deadline:
                self.cancel(ident)
                event(self.events, 'tool_transport_error', execution_id=ident)
                raise TimeoutError('guest did not acknowledge command ' + ident)
            time.sleep(.02)
        data = json.loads(self.read(result_name))
        if data.get('token') != self.token or data.get('id') != ident:
            raise RuntimeError('guest response identity mismatch')
        event(self.events, 'tool_end', execution_id=ident,
              return_code=data.get('return_code'), timed_out=data.get('timed_out', False))
        if 'error' in data:
            raise RuntimeError(data['error'])
        for name in ('stdout', 'stderr'):
            filename = ident + '.' + name
            data[name] = self.read(filename) if self.exists(filename) else ''
        return data

    def cancel_pending(self):
        if not self.root.exists() and self._fd is None:
            return
        for name in os.listdir(self.directory_fd()):
            if not name.endswith('.request.json'):
                continue
            ident = name.removesuffix('.request.json')
            if not self.exists(ident + '.result.json'):
                self.cancel(ident)
