/* SPDX-License-Identifier: MPL-2.0 */
/* Hardware probe for newfstatat through the native virtio-fs root. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static void expect_stat(int dirfd, const char *path, int flags, off_t size,
                        mode_t mode)
{
    struct stat st;
    long rc = syscall(SYS_newfstatat, dirfd, path, &st, flags);

    if (size < 0 ? rc == -1 && errno == ENOENT
                 : rc == 0 && st.st_size == size && st.st_mode == mode)
        return;
    fprintf(stderr, "unexpected stat of '%s' (flags=%#x, rc=%ld, errno=%d)\n",
            path, flags, rc, errno);
    exit(19);
}

static void await_host(const char *step)
{
    puts(step);
    fflush(stdout);
    if (getchar() != '\n')
        exit(20);
}

int main(void)
{
    expect_stat(AT_FDCWD, "/subject", 0, 3, S_IFREG | 0644);
    expect_stat(AT_FDCWD, "/link", AT_SYMLINK_NOFOLLOW, 7, S_IFLNK | 0777);
    expect_stat(AT_FDCWD, "/link", 0, 3, S_IFREG | 0644);

    await_host("resize");
    expect_stat(AT_FDCWD, "/subject", 0, 9, S_IFREG | 0600);
    await_host("remove");
    expect_stat(AT_FDCWD, "/subject", 0, -1, 0);
    expect_stat(AT_FDCWD, "/link", 0, -1, 0);
    expect_stat(AT_FDCWD, "/link", AT_SYMLINK_NOFOLLOW, 7, S_IFLNK | 0777);
    await_host("recreate");
    expect_stat(AT_FDCWD, "/subject", 0, 4, S_IFREG | 0640);
    expect_stat(AT_FDCWD, "/link", 0, 4, S_IFREG | 0640);

    int fd = open("/subject", O_RDONLY);
    if (fd < 0)
        return 21;
    expect_stat(fd, "", AT_EMPTY_PATH, 4, S_IFREG | 0640);
    return close(fd) < 0 ? 22 : 7;
}
