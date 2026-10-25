/* B-FS-ENG guest phase clocks. Validation is outside every phase timer. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>

#define BLOCK (1U << 20)
#define LARGE (64U << 20)
#define SMALL (64U << 10)
#define COUNT 128
static unsigned char expected[BLOCK];
static const char *names[12];
static uint64_t elapsed[12];
static unsigned metrics;

static void require(int ok, const char *what) {
    if (!ok) { fprintf(stderr, "DAX_BENCH_FAIL %s: %s\n", what, strerror(errno)); exit(2); }
}
static uint64_t now(void) {
    struct timespec t;
    require(clock_gettime(CLOCK_MONOTONIC_RAW, &t) == 0, "clock");
    return (uint64_t)t.tv_sec * 1000000000 + (uint64_t)t.tv_nsec;
}
static void metric(const char *name, uint64_t start, unsigned repetitions) {
    names[metrics] = name;
    elapsed[metrics++] = (now() - start) / repetitions;
}
static void pattern(uint32_t seed) {
    uint32_t x = seed;
    for (unsigned i = 0; i < BLOCK; i += 4) {
        x ^= x << 13; x ^= x >> 17; x ^= x << 5;
        memcpy(expected + i, &x, 4);
    }
}
static void verify(const unsigned char *buf, size_t len, uint32_t seed) {
    pattern(seed);
    for (size_t i = 0; i < len; i += BLOCK)
        require(memcmp(buf + i, expected, len - i < BLOCK ? len - i : BLOCK) == 0, "content");
}
static int opened(const char *name) {
    char path[128];
    snprintf(path, sizeof(path), "/tmp/data/%s", name);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    require(fd >= 0, "open");
    return fd;
}
static void read_range(int fd, unsigned char *buf, size_t offset, size_t len) {
    for (size_t i = 0; i < len; i += SMALL)
        require(pread(fd, buf + i, SMALL, (off_t)(offset + i)) == SMALL, "pread");
}
static unsigned char *buffer(size_t n) {
    unsigned char *p = malloc(n);
    require(p != NULL, "malloc");
    memset(p, 0, n); /* pre-fault destination outside the timer */
    volatile unsigned char *resident = p;
    for (size_t i = 0; i < n; i += 4096) resident[i] = 0xa5;
    resident[n - 1] = 0xa5;
    __asm__ volatile("" : : "r"(p) : "memory");
    return p;
}
static void sequential(void) {
    int fd = opened("sequential");
    unsigned char *buf = buffer(LARGE);
    uint64_t t = now();
    read_range(fd, buf, 0, LARGE);
    metric("read_first", t, 1);
    verify(buf, LARGE, 101);
    t = now();
    for (unsigned i = 0; i < 4; ++i) read_range(fd, buf, 0, LARGE);
    metric("read_repeat", t, 4);
    verify(buf, LARGE, 101);
    require(close(fd) == 0, "close"); free(buf);
}
static uint64_t scan(volatile unsigned char *p) {
    uint64_t sum = 0;
    for (size_t i = 0; i < LARGE; i += 4096) sum += p[i];
    return sum;
}
static void mapped(void) {
    int fd = opened("mapped");
    uint64_t t = now();
    unsigned char *p = mmap(NULL, LARGE, PROT_READ, MAP_PRIVATE, fd, 0);
    require(p != MAP_FAILED, "mmap");
    uint64_t sum = scan(p);
    metric("mmap_first", t, 1);
    verify(p, LARGE, 202);
    uint64_t expected_sum = 0;
    for (size_t i = 0; i < LARGE; i += 4096) expected_sum += expected[i % BLOCK];
    require(sum == expected_sum, "mmap first sum");
    sum = 0; t = now();
    for (unsigned i = 0; i < 16; ++i) sum += scan(p);
    metric("mmap_repeat", t, 16);
    require(sum == expected_sum * 16, "mmap repeat sum");
    require(munmap(p, LARGE) == 0 && close(fd) == 0, "unmap/close");
}
struct reader { int fd; unsigned char *buf; size_t offset; };
static void *reader(void *input) {
    struct reader *r = input;
    read_range(r->fd, r->buf, r->offset, LARGE / 2);
    return NULL;
}
static void parallel_pass(int fd, unsigned char *buf) {
    pthread_t workers[2];
    struct reader args[2] = {{fd, buf, 0}, {fd, buf + LARGE / 2, LARGE / 2}};
    for (unsigned i = 0; i < 2; ++i)
        require(pthread_create(&workers[i], NULL, reader, &args[i]) == 0, "thread create");
    for (unsigned i = 0; i < 2; ++i)
        require(pthread_join(workers[i], NULL) == 0, "thread join");
}
static void parallel(void) {
    int fd = opened("parallel");
    unsigned char *buf = buffer(LARGE);
    uint64_t t = now();
    parallel_pass(fd, buf);
    metric("parallel_first", t, 1);
    verify(buf, LARGE, 303);
    t = now();
    for (unsigned i = 0; i < 4; ++i) parallel_pass(fd, buf);
    metric("parallel_repeat", t, 4);
    verify(buf, LARGE, 303);
    require(close(fd) == 0, "close"); free(buf);
}
static void small_pass(unsigned char *buf) {
    for (unsigned i = 0; i < COUNT; ++i) {
        char name[64]; snprintf(name, sizeof(name), "small/f%03u", i);
        int fd = opened(name);
        read_range(fd, buf + i * SMALL, 0, SMALL);
        require(close(fd) == 0, "small close");
    }
}
static void small_verify(unsigned char *buf) {
    for (unsigned i = 0; i < COUNT; ++i) verify(buf + i * SMALL, SMALL, 1000 + i);
}
static void small(void) {
    unsigned char *buf = buffer(COUNT * SMALL);
    uint64_t t = now(); small_pass(buf);
    metric("small_first", t, 1); small_verify(buf);
    t = now(); small_pass(buf);
    metric("small_repeat", t, 1); small_verify(buf);
    free(buf);
}
static void metadata_pass(void) {
    uint64_t size = 0;
    for (unsigned i = 0; i < 2048; ++i) {
        char path[128]; struct stat s;
        snprintf(path, sizeof(path), "/tmp/data/meta/d%02u/f%04u", i / 64, i);
        require(stat(path, &s) == 0 && S_ISREG(s.st_mode), "metadata");
        size += (uint64_t)s.st_size;
    }
    require(size == 2048U * 256, "metadata size");
}
static void metadata(void) {
    uint64_t t = now(); metadata_pass(); metric("metadata_first", t, 1);
    t = now(); metadata_pass(); metric("metadata_repeat", t, 1);
}
int main(int argc, char **argv) {
    require(argc == 2 && (!strcmp(argv[1], "off") || !strcmp(argv[1], "on")), "mode");
    int dax = !strcmp(argv[1], "on");
    require(mount("tmpfs", "/tmp", "tmpfs", 0, "size=16m") == 0, "tmpfs");
    require(mkdir("/tmp/data", 0700) == 0, "mountpoint");
    require(mount("pvisor-dax", "/tmp/data", "virtiofs", MS_RDONLY,
                  dax ? "dax=inode" : "dax=never") == 0, "virtiofs mount");
    struct statx s;
    require(statx(AT_FDCWD, "/tmp/data/sequential", 0, STATX_BASIC_STATS, &s) == 0, "statx");
    require((s.stx_attributes_mask & STATX_ATTR_DAX) != 0 &&
            !!(s.stx_attributes & STATX_ATTR_DAX) == dax, "DAX selection");
    struct utsname kernel; require(uname(&kernel) == 0, "uname");
    /* Independent files avoid priming the later class through the earlier one.
       A 128 MiB DAX window deliberately includes normal mapping reclamation. */
    metadata(); small(); sequential(); mapped(); parallel();
    require(umount("/tmp/data") == 0, "unmount");
    printf("DAX_BENCH_RESULT {\"mode\":\"%s\",\"dax\":%s,\"guest_kernel\":\"%s\",\"metrics_ns\":{",
           argv[1], dax ? "true" : "false", kernel.release);
    for (unsigned i = 0; i < metrics; ++i)
        printf("%s\"%s\":%" PRIu64, i ? "," : "", names[i], elapsed[i]);
    printf("},\"verified_bytes\":%u,\"metadata_files\":2048}\n", 3 * LARGE + COUNT * SMALL);
    fflush(stdout);
    return 0;
}
