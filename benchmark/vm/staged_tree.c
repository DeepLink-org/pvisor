/* B-VM-GUEST-ENG mechanism probe: publish a new tree to a caller-owned directory. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <inttypes.h>
#include <limits.h>
#include <linux/magic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <time.h>
#include <unistd.h>

#define CHECK(expr) do { if (!(expr)) { \
    fprintf(stderr, "%s:%d: %s (errno %d)\n", __FILE__, __LINE__, #expr, errno); exit(1); \
} } while (0)

enum { FILES = 8, SMALL = 1024, BLOCKS = 512, BULK = 256 * 1024 };

static uint64_t now(void) {
    struct timespec t;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &t) == 0);
    return (uint64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}

static void checkpoint(int observe, const char *phase) {
    if (observe) {
        puts(phase);
        CHECK(fflush(stdout) == 0 && getchar() == '\n');
    }
}

static int new_tree(int parent, const char *name) {
    CHECK(mkdirat(parent, name, 0700) == 0);
    int fd = openat(parent, name, O_DIRECTORY | O_NOFOLLOW | O_RDONLY);
    CHECK(fd >= 0);
    return fd;
}

static void copy_part(int src, off_t offset, int dst) {
    unsigned char buf[BULK];
    _Static_assert((BLOCKS * SMALL) % BULK == 0, "whole bulk blocks");
    for (size_t n = 0; n < BLOCKS * SMALL; n += sizeof(buf)) {
        CHECK(pread(src, buf, sizeof(buf), offset + n) == sizeof(buf));
        CHECK(write(dst, buf, sizeof(buf)) == sizeof(buf));
    }
}

static int part(int dir, int i, int flags) {
    char name[16];
    snprintf(name, sizeof(name), "part-%d", i);
    int fd = openat(dir, name, flags | O_NOFOLLOW, 0600);
    CHECK(fd >= 0);
    return fd;
}

static void publish(int pending, int out, int observe) {
    CHECK(fsync(pending) == 0 && close(pending) == 0);
    checkpoint(observe, "prepared");
    CHECK(renameat2(out, ".pending", out, "ready", RENAME_NOREPLACE) == 0);
    checkpoint(observe, "renamed");
    CHECK(fsync(out) == 0 && close(out) == 0);
}

/* Host fixture: fd pinned before VM start; cwd is private and not shared. */
static void import(int src, int observe) {
    struct stat st;
    CHECK(fstat(src, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == FILES * BLOCKS * SMALL);
    int out = open(".", O_DIRECTORY | O_RDONLY);
    CHECK(out >= 0);
    int pending = new_tree(out, ".pending");
    for (int i = 0; i < FILES; ++i) {
        int dst = part(pending, i, O_WRONLY | O_CREAT | O_EXCL);
        copy_part(src, i * BLOCKS * SMALL, dst);
        CHECK(fsync(dst) == 0 && close(dst) == 0);
    }
    publish(pending, out, observe);
}

int main(int argc, char **argv) {
    if (argc >= 2 && !strcmp(argv[1], "import")) {
        CHECK(argc == 3 || (argc == 4 && !strcmp(argv[3], "observe")));
        char *end;
        long fd = strtol(argv[2], &end, 10);
        CHECK(end != argv[2] && !*end && fd >= 0 && fd <= INT_MAX);
        import((int)fd, argc == 4);
        return 0;
    }
    CHECK(argc == 2 || (argc == 3 && (!strcmp(argv[2], "observe") || !strcmp(argv[2], "start"))));
    int observe = argc == 3 && !strcmp(argv[2], "observe");
    int packed = !strcmp(argv[1], "pack"), local = packed || !strcmp(argv[1], "local");
    CHECK(local || !strcmp(argv[1], "shared"));
    int ram = -1;
    if (local) {
        struct statfs fs;
        int shm = open("/dev/shm", O_DIRECTORY | O_RDONLY);
        CHECK(shm >= 0 && fstatfs(shm, &fs) == 0 && fs.f_type == TMPFS_MAGIC);
        ram = new_tree(shm, "producer");
        CHECK(fchown(ram, 1000, 1000) == 0 && close(shm) == 0);
    }
    CHECK(setgroups(0, NULL) == 0 && setgid(1000) == 0 && setuid(1000) == 0);
    int out = open("/output", O_DIRECTORY | O_RDONLY);
    CHECK(out >= 0);
    checkpoint(argc == 3 && !observe, "begin");
    unsigned char block[SMALL];
    uint64_t start = now();
    int pending = packed ? -1 : new_tree(out, ".pending");
    int work = local ? new_tree(ram, "stage") : pending;
    for (int i = 0; i < FILES; ++i) {
        int fd = part(work, i, O_CREAT | O_EXCL | O_WRONLY);
        for (int j = 0; j < BLOCKS; ++j) {
            memset(block, (i * 31 + j) & 255, sizeof(block));
            CHECK(write(fd, block, sizeof(block)) == sizeof(block));
        }
        CHECK((local || fsync(fd) == 0) && close(fd) == 0);
    }
    checkpoint(observe, "generated");
    if (local) {
        int pack = -1;
        if (packed) {
            pack = openat(out, "pack", O_WRONLY | O_NOFOLLOW);
            struct stat st;
            CHECK(pack >= 0 && fstat(pack, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 0);
        }
        for (int i = 0; i < FILES; ++i) {
            int src = part(work, i, O_RDONLY);
            int dst = packed ? pack : part(pending, i, O_CREAT | O_EXCL | O_WRONLY);
            copy_part(src, 0, dst);
            CHECK(close(src) == 0);
            if (!packed) CHECK(fsync(dst) == 0 && close(dst) == 0);
        }
        CHECK(close(work) == 0 && close(ram) == 0);
        if (packed) {
            CHECK(fsync(pack) == 0 && close(pack) == 0 && close(out) == 0);
            checkpoint(observe, "submitted");
            puts("PACK_SUBMITTED");
            return 0;
        }
    }
    publish(pending, out, observe);
    printf("TREE_PUBLISHED_NS %" PRIu64 "\n", now() - start);
    return 0;
}
