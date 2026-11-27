/* Firecracker runs this first userspace command as init. */
#include <linux/magic.h>
#include <stdio.h>
#include <sys/reboot.h>
#include <sys/vfs.h>
#include <termios.h>
#include <unistd.h>

int main(void) {
    const char marker[] = "VM_GUEST_COMMAND_STARTED\n";
    if (write(STDOUT_FILENO, marker, sizeof(marker) - 1) != (ssize_t)(sizeof(marker) - 1))
        return 1;
    struct statfs root;
    int init = getpid() == 1;
    if (statfs("/", &root) || root.f_type != (init ? EXT4_SUPER_MAGIC : FUSE_SUPER_MAGIC))
        return 1;
    if (puts("VM_GUEST_COMMAND_OK") < 0 || fflush(stdout))
        return 1;
    /* PID 1's serial queue must be empty before reboot prints to the same console. */
    if (init && tcdrain(STDOUT_FILENO))
        return 1;
    return init ? reboot(RB_AUTOBOOT) : 0;
}
