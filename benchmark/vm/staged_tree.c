/* B-VM-GUEST-ENG mechanism probe: publish a new tree to a caller-owned directory. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <inttypes.h>
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

int main(int argc, char **argv) {
    CHECK(argc == 2 || (argc == 3 && !strcmp(argv[2], "observe")));
    int local = !strcmp(argv[1], "local"), observe = argc == 3;
    CHECK(local || !strcmp(argv[1], "shared"));
    CHECK(setgroups(0, NULL) == 0 && setgid(1000) == 0 && setuid(1000) == 0);
    int out = open("/output", O_DIRECTORY | O_RDONLY);
    CHECK(out >= 0);

    int ram = -1;
    if (local) {
        struct statfs fs;
        ram = open("/dev/shm", O_DIRECTORY | O_RDONLY);
        CHECK(ram >= 0 && fstatfs(ram, &fs) == 0 && fs.f_type == TMPFS_MAGIC);
    }
    unsigned char block[SMALL], buf[BULK];
    uint64_t start = now();
    int pending = new_tree(out, ".pending");
    int work = local ? new_tree(ram, "stage") : pending;
    for (int i = 0; i < FILES; ++i) {
        char name[16];
        snprintf(name, sizeof(name), "part-%d", i);
        int fd = openat(work, name, O_CREAT | O_EXCL | O_WRONLY, 0600);
        CHECK(fd >= 0);
        for (int j = 0; j < BLOCKS; ++j) {
            memset(block, (i * 31 + j) & 255, sizeof(block));
            CHECK(write(fd, block, sizeof(block)) == sizeof(block));
        }
        CHECK((local || fsync(fd) == 0) && close(fd) == 0);
    }
    checkpoint(observe, "generated");
    if (local) {
        for (int i = 0; i < FILES; ++i) {
            char name[16];
            snprintf(name, sizeof(name), "part-%d", i);
            int src = openat(work, name, O_RDONLY);
            int dst = openat(pending, name, O_CREAT | O_EXCL | O_WRONLY, 0600);
            CHECK(src >= 0 && dst >= 0);
            ssize_t n;
            while ((n = read(src, buf, sizeof(buf))) > 0)
                CHECK(write(dst, buf, n) == n);
            CHECK(n == 0 && fsync(dst) == 0 && close(dst) == 0 && close(src) == 0);
        }
        CHECK(close(work) == 0 && close(ram) == 0);
    }
    CHECK(fsync(pending) == 0 && close(pending) == 0);
    checkpoint(observe, "prepared");
    CHECK(renameat2(out, ".pending", out, "ready", RENAME_NOREPLACE) == 0);
    checkpoint(observe, "renamed");
    CHECK(fsync(out) == 0 && close(out) == 0);
    printf("TREE_PUBLISHED_NS %" PRIu64 "\n", now() - start);
    return 0;
}
