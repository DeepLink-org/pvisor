#!/usr/bin/env python3
"""Private VM LAN with QEMU's user networking, without changing host routes.

QEMU -machine none is only the network helper, not a second guest VM. The TAP
file descriptor is opened in a private user/network namespace and passed back
to the host helper, where ordinary outbound sockets provide DNS and NAT.
"""

import array
import fcntl
import os
import signal
import socket
import struct
import subprocess
import sys
from pathlib import Path


def namespace_child(control_fd, command):
    control = socket.socket(fileno=control_fd)
    for argv in (
        ['ip', 'link', 'set', 'lo', 'up'],
        ['ip', 'link', 'add', 'pvbench-br', 'type', 'bridge'],
        ['ip', 'tuntap', 'add', 'pvbench-tap', 'mode', 'tap'],
    ):
        subprocess.run(argv, check=True)
    tap = os.open('/dev/net/tun', os.O_RDWR)
    fcntl.ioctl(tap, 0x400454CA, struct.pack('16sH14x', b'pvbench-uplink', 0x0002 | 0x1000))
    for name in ('pvbench-tap', 'pvbench-uplink'):
        subprocess.run(['ip', 'link', 'set', name, 'master', 'pvbench-br'], check=True)
        subprocess.run(['ip', 'link', 'set', name, 'up'], check=True)
    subprocess.run(['ip', 'link', 'set', 'pvbench-br', 'up'], check=True)
    control.sendmsg([b'T'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [tap]))])
    if control.recv(1) != b'G':
        raise RuntimeError('Network helper did not become ready')
    os.close(tap)
    control.close()
    os.execvp(command[0], command)


def main():
    if sys.argv[1] == '--namespace-child':
        namespace_child(int(sys.argv[2]), sys.argv[3:])
        return
    host, child = socket.socketpair()
    command = ['unshare', '--user', '--map-root-user', '--net', '--', sys.executable,
               str(Path(__file__).resolve()), '--namespace-child', str(child.fileno()), *sys.argv[1:]]
    vm = subprocess.Popen(command, pass_fds=(child.fileno(),))
    child.close()
    helper = None
    fd = None
    try:
        host.settimeout(10)
        data, ancillary, _, _ = host.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
        if data != b'T':
            raise RuntimeError('Namespace TAP setup failed')
        descriptors = array.array('i')
        descriptors.frombytes(ancillary[0][2])
        fd = descriptors[0]
        helper = subprocess.Popen([
            'qemu-system-x86_64', '-machine', 'none', '-nodefaults', '-display', 'none',
            '-netdev', 'user,id=nat,net=10.77.0.0/24,host=10.77.0.1,dns=10.77.0.3',
            '-netdev', f'tap,id=link,fd={fd},vnet_hdr=off',
            '-netdev', 'hubport,id=natport,hubid=0,netdev=nat',
            '-netdev', 'hubport,id=linkport,hubid=0,netdev=link',
        ], pass_fds=(fd,), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
        host.sendall(b'G')
        host.close()
        status = vm.wait()
    finally:
        host.close()
        if fd is not None:
            os.close(fd)
        for proc in (vm, helper):
            if proc is not None and proc.poll() is None:
                proc.send_signal(signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
    sys.exit(status)


if __name__ == '__main__':
    main()
