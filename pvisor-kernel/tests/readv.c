/* SPDX-License-Identifier: MPL-2.0 */
/* Static userspace probe for scatter reads on a writable virtio-fs root. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

#define CHECK(test) do { if (!(test)) { \
    fprintf(stderr, "line %d: %s (errno=%d)\n", __LINE__, #test, errno); exit(1); \
} } while (0)

enum { FIELDS = 8, WIDTH = 64, RECORD = FIELDS * WIDTH, RECORDS = 128 };
static unsigned char source[RECORDS * RECORD], warm[sizeof(source)];

/* Read one contiguous file record into separate, reverse-ordered slots.
   Check untouched bytes too, including fields beyond EOF. */
static void scatter(int fd, size_t offset, size_t available, int positioned)
{
    unsigned char slots[FIELDS][WIDTH + 1];
    struct iovec v[FIELDS + 1] = {{0}};
    memset(slots, 0xa5, sizeof(slots));
    for (size_t i = 0; i < FIELDS; i++)
        v[i + (i >= 2)] = (struct iovec){slots[FIELDS - 1 - i], WIDTH};
    ssize_t n = positioned ? preadv(fd, v, FIELDS + 1, offset) : readv(fd, v, FIELDS + 1);
    CHECK(n == (ssize_t)available);
    for (size_t i = 0; i < FIELDS; i++) {
        for (size_t j = 0; j < WIDTH; j++) {
            size_t pos = i * WIDTH + j;
            CHECK(slots[FIELDS - 1 - i][j] ==
                  (pos < available ? source[offset + pos] : 0xa5));
        }
        CHECK(slots[i][WIDTH] == 0xa5);
    }
}

/* The host protocol profile emits a cumulative checkpoint after 250 ms idle. */
static void checkpoint(int fd)
{
    struct stat st;
    CHECK(usleep(400000) == 0);
    CHECK(fstat(fd, &st) == 0 && st.st_size == sizeof(source));
}

static void check_boundaries(int fd, const char *path)
{
    size_t end = sizeof(source);
    CHECK(lseek(fd, end - WIDTH - 7, SEEK_SET) == (off_t)(end - WIDTH - 7));
    scatter(fd, end - WIDTH - 7, WIDTH + 7, 1);
    CHECK(lseek(fd, 0, SEEK_CUR) == (off_t)(end - WIDTH - 7));
    int shared = dup(fd);
    CHECK(shared >= 0);
    scatter(shared, end - WIDTH - 7, WIDTH + 7, 0);
    CHECK(close(shared) == 0);
    CHECK(lseek(fd, 0, SEEK_CUR) == (off_t)end);
    scatter(fd, end, 0, 0);
    scatter(fd, end + 1, 0, 1);

    CHECK(lseek(fd, INT64_MAX, SEEK_CUR) == -1 && errno == EINVAL);
    CHECK(lseek(fd, 0, SEEK_CUR) == (off_t)end);
    CHECK(lseek(fd, -1, SEEK_SET) == -1 && errno == EINVAL);
    CHECK(lseek(fd, 0, SEEK_CUR) == (off_t)end);
    int wo = open(path, O_WRONLY);
    unsigned char byte;
    struct iovec one = {&byte, 1}, empty = {NULL, 0};
    CHECK(wo >= 0);
    CHECK(preadv(wo, &one, 1, 0) == -1 && errno == EBADF);
    CHECK(preadv(fd, &empty, 1, 0) == 0);
    int pipefd[2];
    CHECK(pipe(pipefd) == 0);
    CHECK(preadv(pipefd[0], &empty, 1, 0) == -1 && errno == ESPIPE);
    CHECK(lseek(pipefd[0], 0, SEEK_CUR) == -1 && errno == ESPIPE);
    unsigned char msg[2] = {0};
    struct iovec pair[] = {{msg + 1, 1}, {msg, 1}};
    CHECK(write(pipefd[1], "xy", 2) == 2);
    CHECK(readv(pipefd[0], pair, 2) == 2 && !memcmp(msg, "yx", 2));
    CHECK(close(wo) == 0 && close(pipefd[0]) == 0 && close(pipefd[1]) == 0);

    long page = sysconf(_SC_PAGESIZE);
    CHECK(page > WIDTH && 2 * (size_t)page <= end);
    char *mapping = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mapping != MAP_FAILED && mprotect(mapping + page, page, PROT_NONE) == 0);
    struct iovec fault[] = {{mapping, WIDTH}, {mapping + page, WIDTH}};
    CHECK(lseek(fd, 0, SEEK_SET) == 0);
    CHECK(readv(fd, fault, 2) == WIDTH && !memcmp(mapping, source, WIDTH));
    CHECK(lseek(fd, 0, SEEK_CUR) == WIDTH);
    CHECK(readv(fd, fault + 1, 1) == -1 && errno == EFAULT);
    CHECK(lseek(fd, 0, SEEK_CUR) == WIDTH);
    struct iovec crossing = {mapping, 2 * (size_t)page};
    CHECK(lseek(fd, 0, SEEK_SET) == 0);
    CHECK(readv(fd, &crossing, 1) == page && !memcmp(mapping, source, page));
    CHECK(lseek(fd, 0, SEEK_CUR) == page);
    CHECK(munmap(mapping, 2 * page) == 0);
}

static uint64_t now(void)
{
    struct timespec ts;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &ts) == 0);
    return (uint64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

int main(int argc, char **argv)
{
    CHECK(argc == 3);
    int seek = !strcmp(argv[1], "profile-seek") || !strcmp(argv[1], "time-seek");
    int profile = !strcmp(argv[1], "profile") || !strcmp(argv[1], "profile-seek");
    int timing = !strcmp(argv[1], "time-scatter") || !strcmp(argv[1], "time-seek");
    CHECK(profile || timing || !strcmp(argv[1], "check"));
    int fd = open(argv[2], O_CREAT | O_EXCL | O_RDWR, 0600);
    CHECK(fd >= 0);
    for (size_t i = 0; i < sizeof(source); i++)
        source[i] = (i + i / WIDTH + 17 * (i / RECORD)) % 251;
    CHECK(write(fd, source, sizeof(source)) == sizeof(source));
    CHECK(pread(fd, warm, sizeof(warm), 0) == sizeof(warm));
    CHECK(!memcmp(warm, source, sizeof(source)));

    if (profile) {
        checkpoint(fd);
        puts(seek ? "SEEK_READ_BEGIN" : "SCATTER_BEGIN");
        fflush(stdout);
    }
    uint64_t start = now();
    for (size_t i = 0; i < RECORDS; i++) {
        size_t offset = ((i * 37) % RECORDS) * RECORD;
        if (seek) {
            unsigned char record[RECORD];
            CHECK(lseek(fd, offset, SEEK_SET) == (off_t)offset);
            CHECK(read(fd, record, RECORD) == RECORD && !memcmp(record, source + offset, RECORD));
        } else {
            scatter(fd, offset, RECORD, 1);
        }
    }
    uint64_t elapsed = now() - start;
    if (profile) {
        puts(seek ? "SEEK_READ_END" : "SCATTER_END");
        fflush(stdout);
        checkpoint(fd);
    } else if (!timing) {
        check_boundaries(fd, argv[2]);
    }
    CHECK(close(fd) == 0 && unlink(argv[2]) == 0);
    if (timing)
        printf("VM_GUEST_OK %s %" PRIu64 "\n", argv[1], elapsed);
    return 0;
}
