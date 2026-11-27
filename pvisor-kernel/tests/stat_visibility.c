/* SPDX-License-Identifier: MPL-2.0 */
/* Hardware probe for host mutations through a virtio-fs root. */
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <linux/magic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/vfs.h>
#include <time.h>
#include <unistd.h>

static void expect_fstatat(int dirfd, const char *path, int flags, off_t size,
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

static void expect_statx(int dirfd, const char *path, int flags, off_t size, mode_t mode)
{
    struct statx st;
    const unsigned mask = STATX_TYPE | STATX_MODE | STATX_SIZE;
    long rc = syscall(SYS_statx, dirfd, path, flags, mask, &st);
    if (size < 0)
        assert(rc == -1 && errno == ENOENT);
    else
        assert(rc == 0 && (st.stx_mask & mask) == mask &&
               st.stx_size == (unsigned long long)size && st.stx_mode == mode);
}

static void expect_stat(int dirfd, const char *path, int flags, off_t size, mode_t mode)
{
    expect_statx(dirfd, path, flags, size, mode);
    expect_fstatat(dirfd, path, flags, size, mode);
}

static int scan_metadata(const char *method)
{
    assert(chdir("/files") == 0);
    int flags = AT_SYMLINK_NOFOLLOW;
    if (!strcmp(method, "force"))
        flags |= AT_STATX_FORCE_SYNC;
    else if (!strcmp(method, "dont"))
        flags |= AT_STATX_DONT_SYNC;
    else
        assert(!strcmp(method, "statx") || !strcmp(method, "fstatat"));
    for (int i = 0; i < 512; ++i) {
        char path[32];
        snprintf(path, sizeof(path), "f%d", i);
        if (!strcmp(method, "fstatat"))
            expect_fstatat(AT_FDCWD, path, flags, i % 7, S_IFREG | 0644);
        else
            expect_statx(AT_FDCWD, path, flags, i % 7, S_IFREG | 0644);
    }
    puts("SCAN_OK");
    return 0;
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

static int wide_directory(unsigned count)
{
    assert(count > 0 && count <= 8192);
    assert(chdir("/wide") == 0);
    int held = open("held", O_RDONLY);
    int notify = inotify_init1(IN_NONBLOCK);
    int wd = inotify_add_watch(notify, "watched", IN_ATTRIB);
    assert(held >= 0 && wd >= 0);
    assert(mount("tmpfs", "mount", "tmpfs", 0, NULL) == 0);

    char name[32];
    struct timespec start, end;
    assert(clock_gettime(CLOCK_MONOTONIC, &start) == 0);
    for (unsigned i = 0; i < count; ++i) {
        snprintf(name, sizeof(name), "file%u", i);
        expect_fstatat(AT_FDCWD, name, 0, 0, S_IFREG | 0644);
    }
    assert(clock_gettime(CLOCK_MONOTONIC, &end) == 0);
    for (unsigned i = 0; i < count; ++i) {
        snprintf(name, sizeof(name), "absent%u", i);
        expect_fstatat(AT_FDCWD, name, 0, -1, 0);
    }

    assert(rename("held", "moved") == 0);
    char fd_path[64], target[64] = {0};
    snprintf(fd_path, sizeof(fd_path), "/proc/self/fd/%d", held);
    assert(readlink(fd_path, target, sizeof(target) - 1) > 0);
    assert(strcmp(target, "/wide/moved") == 0);
    expect_read(held, 0, "old");
    assert(rename("moved", "absent0") == 0);
    expect_stat(AT_FDCWD, "absent0", 0, 3, S_IFREG | 0644);

    assert(chmod("watched", 0600) == 0);
    struct inotify_event event;
    assert(read(notify, &event, sizeof(event)) == sizeof(event));
    assert(event.wd == wd && (event.mask & IN_ATTRIB));
    struct statfs fs;
    assert(statfs("mount", &fs) == 0 && fs.f_type == TMPFS_MAGIC);
    struct statx by_path, by_fd;
    int mounted = open("mount", O_DIRECTORY | O_RDONLY);
    assert(mounted >= 0);
    unsigned mask = STATX_BASIC_STATS | STATX_MNT_ID;
    assert(statx(AT_FDCWD, "mount", 0, mask, &by_path) == 0);
    assert(statx(mounted, "", AT_EMPTY_PATH, mask, &by_fd) == 0);
    assert((by_path.stx_mask & STATX_MNT_ID) && (by_path.stx_attributes & STATX_ATTR_MOUNT_ROOT));
    assert(by_path.stx_mnt_id == by_fd.stx_mnt_id && by_path.stx_ino == by_fd.stx_ino &&
           by_path.stx_dev_minor == by_fd.stx_dev_minor && by_path.stx_dev_major == by_fd.stx_dev_major);
    assert(close(mounted) == 0);
    assert(umount("mount") == 0);
    assert(close(notify) == 0 && close(held) == 0);
    printf("WIDE_STAT_OK %u %lld\n", count,
           (long long)(end.tv_sec - start.tv_sec) * 1000000000 + end.tv_nsec - start.tv_nsec);
    return 7;
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "--scan") == 0)
        return scan_metadata(argv[2]);
    if (argc == 3 && strcmp(argv[1], "--wide-dir") == 0)
        return wide_directory(strtoul(argv[2], NULL, 10));
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
    expect_statx(reader, "", AT_EMPTY_PATH, 2, S_IFREG | 0600);
    int fd = open("/subject", O_RDONLY);
    if (fd < 0)
        return 21;
    expect_read(fd, 0, "new!");
    expect_stat(fd, "", AT_EMPTY_PATH, 4, S_IFREG | 0640);
    return close(fd) || close(reader) || close(updater) || close(writer) ? 22 : 7;
}
