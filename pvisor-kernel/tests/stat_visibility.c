/* SPDX-License-Identifier: MPL-2.0 */
/* Hardware probe for host mutations through a virtio-fs root. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
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

static void expect_read(int fd, off_t offset, const char *expected)
{
    unsigned char bytes[16], want[sizeof(bytes)];
    size_t len = strlen(expected);
    if (len > sizeof(bytes))
        abort();
    memset(bytes, 0xa5, sizeof(bytes));
    memset(want, 0xa5, sizeof(want));
    memcpy(want, expected, len);
    ssize_t got = pread(fd, bytes, sizeof(bytes), offset);
    if (got != (ssize_t)len || memcmp(want, bytes, sizeof(bytes))) {
        fprintf(stderr, "unexpected pread (fd=%d, offset=%ld, got=%zd, want=%zu)\n",
                fd, (long)offset, got, len);
        exit(23);
    }
}

static void await_host(const char *step)
{
    puts(step);
    fflush(stdout);
    if (getchar() != '\n')
        exit(20);
}

static void expect_linux_transport(const char *expected)
{
    glob_t paths = {0};
    if (glob("/sys/bus/virtio/drivers/virtiofs/virtio*/features", 0, NULL, &paths) ||
        paths.gl_pathc != 1) {
        fprintf(stderr, "expected one bound Linux virtio-fs device\n");
        exit(25);
    }
    FILE *f = fopen(paths.gl_pathv[0], "r");
    char bits[80];
    if (!f || !fgets(bits, sizeof(bits), f) || strlen(bits) < 24 ||
        strlen(expected) != 2 || strncmp(bits + 22, expected, 2)) {
        fprintf(stderr, "expected negotiated virtio-fs bits 22,23 = %s\n", expected);
        exit(25);
    }
    fclose(f);
    globfree(&paths);
}

int main(int argc, char **argv)
{
    if (argc == 2)
        expect_linux_transport(argv[1]);
    expect_stat(AT_FDCWD, "/subject", 0, 3, S_IFREG | 0644);
    expect_stat(AT_FDCWD, "/link", AT_SYMLINK_NOFOLLOW, 7, S_IFLNK | 0777);
    expect_stat(AT_FDCWD, "/link", 0, 3, S_IFREG | 0644);

    int reader = open("/subject", O_RDONLY);
    int updater = open("/subject", O_RDWR);
    int writer = open("/subject", O_WRONLY);
    if (reader < 0 || updater < 0 || writer < 0)
        return 21;
    expect_read(reader, 0, "old");
    expect_read(updater, 1, "ld");
    char byte;
    if (pread(writer, &byte, 1, 0) != -1 || errno != EBADF)
        return 24;
    await_host("rewrite");
    expect_read(updater, 0, "hot");
    expect_read(reader, 1, "ot");
    await_host("rewrite-scatter");
    char first = 0, rest[4] = {0};
    struct iovec v[] = {{&first, 1}, {rest, sizeof(rest)}};
    if (preadv(reader, v, 2, 0) != 3 || first != 'i' || memcmp(rest, "ov\0", 4))
        return 26;
    await_host("resize");
    expect_read(reader, 3, "rgrown");
    expect_read(updater, 9, "");
    expect_stat(AT_FDCWD, "/subject", 0, 9, S_IFREG | 0600);
    await_host("grow-seek");
    if (lseek(reader, -1, SEEK_END) != 11)
        return 27;
    await_host("truncate");
    expect_read(updater, 2, "");
    expect_read(reader, 0, "ok");
    if (lseek(reader, 0, SEEK_CUR) != 11 || lseek(reader, 0, SEEK_END) != 2)
        return 27;
    await_host("remove");
    expect_stat(AT_FDCWD, "/subject", 0, -1, 0);
    expect_stat(AT_FDCWD, "/link", 0, -1, 0);
    expect_stat(AT_FDCWD, "/link", AT_SYMLINK_NOFOLLOW, 7, S_IFLNK | 0777);
    await_host("recreate");
    expect_stat(AT_FDCWD, "/subject", 0, 4, S_IFREG | 0640);
    expect_stat(AT_FDCWD, "/link", 0, 4, S_IFREG | 0640);

    expect_read(reader, 0, "ok");
    int fd = open("/subject", O_RDONLY);
    if (fd < 0)
        return 21;
    expect_read(fd, 0, "new!");
    expect_stat(fd, "", AT_EMPTY_PATH, 4, S_IFREG | 0640);
    return close(fd) || close(reader) || close(updater) || close(writer) ? 22 : 7;
}
