#!/bin/sh
# Runs after Ubuntu's normal multi-user and cloud-init targets, never as PID 1.
set -eu
mode=ready
provision=0
for arg in $(cat /proc/cmdline); do
    case "$arg" in
        pvbench.mode=*) mode=${arg#pvbench.mode=} ;;
        pvbench.provision=1) provision=1 ;;
    esac
done
if [ "$provision" = 1 ]; then
    mkdir -p /mnt/reference-payload
    mount -o ro /dev/vdb /mnt/reference-payload
    cp -a /mnt/reference-payload/. /
    umount /mnt/reference-payload
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends python3 git ripgrep nodejs npm build-essential
    dpkg-query -W > /root/reference-packages.txt
    printf 'REFERENCE_PROVISIONED\n'
    systemctl reboot --no-wall
    exit 0
fi
/usr/bin/python3 - <<'PY'
import json, os, subprocess
from pathlib import Path

def state(unit):
    return subprocess.check_output(['systemctl', 'show', unit, '--property=ActiveState', '--value'], text=True).strip()
value = {
    'os_release': Path('/etc/os-release').read_text(),
    'kernel': os.uname().release,
    'pid1': Path('/proc/1/comm').read_text().strip(),
    'multi_user': state('multi-user.target'),
    'network_online': state('network-online.target'),
    'cloud_final': state('cloud-final.service'),
    'ssh_socket': state('ssh.socket'),
    'failed_units': subprocess.check_output(['systemctl', '--failed', '--no-legend', '--no-pager'], text=True),
}
assert value['pid1'] == 'systemd'
assert all(value[k] == 'active' for k in ('multi_user', 'network_online', 'cloud_final', 'ssh_socket')), value
print('REFERENCE_OS_READY ' + json.dumps(value), flush=True)
PY
if [ "$mode" = ready ]; then
    printf 'REFERENCE_READY\nREFERENCE_RESULT {"mode":"ready","correctness":"passed"}\n'
else
    cd /work
    export PVISOR_REFERENCE_TOOL_ROOT=/
    export PVISOR_REFERENCE_TOOLCHAIN=/opt/toolchain
    export PVISOR_REFERENCE_HARNESS=/bench/harness
    /usr/bin/python3 /bench/reference_workload.py --mode "$mode"
fi
printf 'REFERENCE_EXIT 0\n'
systemctl reboot --no-wall
