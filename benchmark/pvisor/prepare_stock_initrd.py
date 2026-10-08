#!/usr/bin/env python3
"""Prepare a minimal initrd for an installed Fedora stock kernel (B-STARTUP).

Load the exact installed virtio_mmio module, mount /dev/vda, and execute the
common /bench/init. No kernel build, host initramfs modification or rootfs edit.
Requires built-in virtio/PCI/block/ext4 and a dependency-free virtio_mmio module.
"""
import argparse
import gzip
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import time


SOURCE = r'''
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
static void check(int rc, const char *what) {
    if (rc < 0) { perror(what); exit(1); }
}
int main(void) {
    check(mount("devtmpfs", "/dev", "devtmpfs", 0, NULL), "devtmpfs");
    check(mount("proc", "/proc", "proc", 0, NULL), "proc");
    char cmdline[4096], params[4096] = "", *state;
    FILE *cmd = fopen("/proc/cmdline", "r");
    if (!cmd || !fgets(cmdline, sizeof(cmdline), cmd)) return 1;
    fclose(cmd);
    // Module parameters from the kernel cmdline must be applied at load time.
    // QEMU adds virtio_mmio.device entries for its actual MMIO devices.
    for (char *p = strtok_r(cmdline, " \n", &state); p; p = strtok_r(NULL, " \n", &state)) {
        if (!strncmp(p, "virtio_mmio.device=", 19)) {
            if (strlen(params) + strlen(p + 12) + 2 >= sizeof(params)) return 1;
            strcat(params, p + 12); strcat(params, " ");
        }
    }
    int fd = open("/virtio_mmio.ko", O_RDONLY);
    check(fd, "open module");
    struct stat st;
    check(fstat(fd, &st), "stat module");
    char *module = malloc(st.st_size);
    if (!module) return 1;
    size_t done = 0;
    while (done < (size_t)st.st_size) {
        ssize_t n = read(fd, module + done, st.st_size - done);
        if (n <= 0) return 1;
        done += n;
    }
    check(syscall(SYS_init_module, module, st.st_size, params), "load virtio_mmio");
    close(fd); free(module);
    for (int i = 0; i < 500 && access("/dev/vda", F_OK); i++) usleep(10000);
    check(mount("/dev/vda", "/newroot", "ext4", 0, NULL), "mount root");
    check(mount("/dev", "/newroot/dev", NULL, MS_MOVE, NULL), "move dev");
    check(umount("/proc"), "unmount proc");
    check(chdir("/newroot"), "chdir");
    check(chroot("."), "chroot");
    char *args[] = {"/bench/init", NULL};
    execv(args[0], args);
    perror("exec common init");
    return 1;
}
'''


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def prepare(release, output):
    start = time.perf_counter()
    config = Path('/boot') / f'config-{release}'
    text = config.read_text()
    for option in ('VIRTIO', 'VIRTIO_PCI', 'VIRTIO_BLK', 'EXT4_FS', 'DEVTMPFS', 'BLK_DEV_INITRD'):
        if f'CONFIG_{option}=y\n' not in text:
            raise ValueError(f'stock kernel needs built-in {option}')
    module = Path('/lib/modules') / release / 'kernel/drivers/virtio/virtio_mmio.ko.xz'
    depends = subprocess.check_output(['modinfo', '-F', 'depends', str(module)], text=True).strip()
    if depends:
        raise ValueError(f'unsupported module dependencies: {depends}')
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    root = output / 'root'
    root.mkdir()
    for name in ('dev', 'proc', 'newroot'):
        (root / name).mkdir()
    source = output / 'init.c'
    source.write_text(SOURCE)
    command = ['gcc', '-static', '-Os', '-o', str(root / 'init'), str(source)]
    subprocess.run(command, check=True, capture_output=True)
    compressed_module = output / module.name
    compressed_module.write_bytes(module.read_bytes())
    (root / 'virtio_mmio.ko').write_bytes(lzma.decompress(compressed_module.read_bytes()))
    files = ['.', 'dev', 'proc', 'newroot', 'init', 'virtio_mmio.ko']
    archive = subprocess.run(['cpio', '--null', '-o', '--format=newc', '--owner=0:0'],
                             input=b'\0'.join(s.encode() for s in files) + b'\0', cwd=root,
                             capture_output=True, check=True)
    (output / 'initrd.cpio.gz').write_bytes(gzip.compress(archive.stdout, mtime=0))
    record = dict(kernel_release=release, purpose='stock module loader; common ext4 userspace',
                  build_command=command, compiler=subprocess.check_output(['gcc', '--version'], text=True),
                  module_source=str(module), module_package=subprocess.check_output(['rpm', '-qf', str(module)], text=True).strip(),
                  config_source=str(config), config_sha256=digest(config), preparation_seconds=time.perf_counter()-start,
                  artifacts={str(p.relative_to(output)): digest(p) for p in (source, compressed_module, root / 'init', root / 'virtio_mmio.ko', output / 'initrd.cpio.gz')})
    (output / 'receipt.json').write_text(json.dumps(record, indent=2) + '\n')
    return output / 'initrd.cpio.gz'


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kernel-release', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(prepare(args.kernel_release, args.output))
