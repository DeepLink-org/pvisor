/* First userspace result, before VM teardown. Firecracker runs the probe as init. */
#include <linux/magic.h>
#include <stdio.h>
#include <sys/reboot.h>
#include <sys/vfs.h>
#include <unistd.h>

int main(void) {
    struct statfs root;
    int init = getpid() == 1;
    if (statfs("/", &root) || root.f_type != (init ? EXT4_SUPER_MAGIC : FUSE_SUPER_MAGIC))
        return 1;
    if (puts("VM_STARTUP_READY") < 0 || fflush(stdout))
        return 1;
    return init ? reboot(RB_AUTOBOOT) : 0;
}
