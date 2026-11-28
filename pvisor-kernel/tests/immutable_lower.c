/* SPDX-License-Identifier: MPL-2.0 */
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

enum { SIZE = 8192 + 17, MAPLEN = 16384 };

static void original(int fd)
{
    unsigned char bytes[SIZE + 1];
    memset(bytes, 0xa5, sizeof(bytes));
    struct iovec segments[] = {{bytes, 4089}, {bytes + 4089, sizeof(bytes) - 4089}};
    assert(preadv(fd, segments, 2, 0) == SIZE);
    for (int i = 0; i < SIZE; i++)
        assert(bytes[i] == i % 251);
    assert(bytes[SIZE] == 0xa5);
    assert(lseek(fd, 0, SEEK_END) == SIZE && read(fd, bytes, 1) == 0);
}

static void check_copyup(void)
{
    int fd = open("source", O_RDONLY);
    assert(fd >= 0);
    unsigned char *shared = mmap(NULL, MAPLEN, PROT_READ, MAP_SHARED, fd, 0);
    unsigned char *cow = mmap(NULL, MAPLEN, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    assert(shared != MAP_FAILED && cow != MAP_FAILED);
    assert(mmap(NULL, MAPLEN, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) == MAP_FAILED);
    assert(errno == EACCES);
    /* Neither the lower fd nor either mapping has faulted in any bytes yet. */
    int upper = open("source", O_TRUNC | O_RDWR);
    assert(upper >= 0 && write(upper, "stage", 5) == 5 && fsync(upper) == 0);
    struct stat st;
    struct statx sx;
    assert(!fstat(fd, &st) && st.st_size == SIZE);
    assert(!fstatat(fd, "", &st, AT_EMPTY_PATH) && st.st_size == SIZE);
    assert(!statx(fd, "", AT_EMPTY_PATH, STATX_SIZE, &sx) && sx.stx_size == SIZE);
    assert(!statx(AT_FDCWD, "source", 0, STATX_SIZE, &sx) && sx.stx_size == 5);
    original(fd);
    assert(!close(fd));
    assert(shared[4099] == 4099 % 251 && shared[SIZE - 1] == (SIZE - 1) % 251);
    assert(shared[SIZE] == 0 && shared[12287] == 0);
    cow[4099] = 252;
    assert(shared[4099] == 4099 % 251);
    assert(mprotect(shared, MAPLEN, PROT_READ | PROT_WRITE) == -1 && errno == EACCES);
    pid_t child = fork();
    assert(child >= 0);
    if (!child) {
        volatile unsigned char beyond = shared[12288];
        (void)beyond;
        _exit(1);
    }
    int status;
    assert(waitpid(child, &status, 0) == child && WIFSIGNALED(status) && WTERMSIG(status) == SIGBUS);
    char bytes[8];
    assert(pread(upper, bytes, sizeof(bytes), 0) == 5 && !memcmp(bytes, "stage", 5));
    assert(!close(upper));
    assert(!munmap(shared, MAPLEN) && !munmap(cow, MAPLEN));

    fd = open("victim", O_RDONLY);
    assert(fd >= 0 && rename("replacement", "victim") == 0);
    original(fd);
    assert(!unlink("victim"));
    original(fd);
    assert(!close(fd));
}

static void touch_large(void)
{
    /* Two resident 5 MiB files exceed the 256 MiB fixture VM's idle cache budget. */
    unsigned char buffer[32768];
    for (int i = 0; i < 2; ++i) {
        int fd = open(i ? "large1" : "large0", O_RDONLY);
        assert(fd >= 0);
        for (int offset = 0; offset < (5 << 20); offset += sizeof(buffer)) {
            assert(pread(fd, buffer, sizeof(buffer), offset) == sizeof(buffer));
            for (unsigned j = 0; j < sizeof(buffer); ++j)
                assert(buffer[j] == 0);
        }
        assert(!close(fd));
    }
}

static void mixed_transition(void)
{
    int lower = open("source", O_RDONLY);
    assert(lower >= 0);
    original(lower);
    assert(write(STDOUT_FILENO, "?", 1) == 1 && getchar() == '!');
    int live = open("source", O_RDONLY);
    char bytes[8];
    assert(live >= 0);
    ssize_t got = read(live, bytes, sizeof(bytes));
    if (got != 4)
        fprintf(stderr, "mutable shadow: expected 4 bytes, read %zd\n", got);
    assert(got == 4 && !memcmp(bytes, "live", 4));
    assert(!close(live));
    int stage = open("source", O_RDWR | O_TRUNC);
    assert(stage >= 0 && write(stage, "stage", 5) == 5 && !fsync(stage) && !close(stage));
    stage = open("source", O_RDONLY);
    assert(stage >= 0 && read(stage, bytes, sizeof(bytes)) == 5 && !memcmp(bytes, "stage", 5));
    assert(!close(stage) && !unlink("source"));
    assert(open("source", O_RDONLY) == -1 && errno == ENOENT);
    original(lower);
    assert(!close(lower));
}

int main(int argc, char **argv)
{
    assert(argc == 2 && !chdir("/workspace"));
    if (!strcmp(argv[1], "check")) {
        check_copyup();
    } else if (!strcmp(argv[1], "transition")) {
        mixed_transition();
    } else if (!strcmp(argv[1], "sparse1") || !strcmp(argv[1], "sparse3")) {
        for (int i = 0; i < argv[1][6] - '0'; ++i) {
            int fd = open("sparse", O_RDONLY);
            char byte = 1;
            assert(fd >= 0 && pread(fd, &byte, 1, (32 << 20) - 1) == 1 && byte == 0);
            assert(!close(fd));
        }
    } else if (!strcmp(argv[1], "evict") || !strcmp(argv[1], "hold")) {
        int held = open("source", O_RDONLY);
        assert(held >= 0);
        original(held);
        int keep = !strcmp(argv[1], "hold");
        if (!keep)
            assert(!close(held));
        touch_large();
        int fd = open("source", O_RDONLY);
        assert(fd >= 0);
        original(fd);
        assert(!close(fd));
        if (keep) {
            original(held);
            assert(!close(held));
        }
    } else {
        int shadow = !strncmp(argv[1], "shadow", 6);
        int rounds = atoi(argv[1] + (shadow ? 6 : 0));
        assert(rounds == 1 || rounds == 3);
        for (int i = 0; i < rounds; i++) {
            int fd = open("source", O_RDONLY);
            assert(fd >= 0);
            if (shadow) {
                char data[8];
                assert(read(fd, data, sizeof(data)) == 4 && !memcmp(data, "live", 4));
            } else {
                original(fd);
            }
            assert(!close(fd));
        }
    }
    puts("IMMUTABLE_LOWER_OK");
}
