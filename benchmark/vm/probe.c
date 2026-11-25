/* Guest workload for B-VM-GUEST-ENG. One operation per fresh guest. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <linux/magic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/vfs.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHECK(expr) do { if (!(expr)) { \
    fprintf(stderr, "probe failed: %s (errno %d)\n", #expr, errno); exit(1); \
} } while (0)

static uint64_t now(void) {
    struct timespec t;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &t) == 0);
    return (uint64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}

static uint64_t cpu(void) {
    uint32_t x = 1;
    uint64_t start = now();
    for (int i = 0; i < 8000000; ++i) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
    }
    __asm__ volatile("" : "+r"(x)); /* Keep the result inside the timed interval. */
    uint64_t ns = now() - start;
    CHECK(x == 0x9a1c5f10);
    return ns;
}

static uint64_t syscall_getpid(void) {
    pid_t pid = getpid();
    uint64_t start = now();
    for (int i = 0; i < 131072; ++i)
        CHECK(syscall(SYS_getpid) == pid);
    return now() - start;
}

static uint64_t pipe_roundtrip(void) {
    enum { ROUNDS = 4096 };
    int to_child[2], reply[2], status;
    CHECK(pipe(to_child) == 0 && pipe(reply) == 0);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        close(to_child[1]);
        close(reply[0]);
        for (int i = 0; i <= ROUNDS; ++i) {
            unsigned char token;
            CHECK(read(to_child[0], &token, 1) == 1);
            token ^= 0xff;
            CHECK(write(reply[1], &token, 1) == 1);
        }
        _exit(0);
    }
    close(to_child[0]);
    close(reply[1]);
    uint64_t start = 0;
    for (int i = 0; i <= ROUNDS; ++i) {
        unsigned char sent = (unsigned char)i, got;
        if (i == 1) /* First exchange establishes readiness outside the clock. */
            start = now();
        CHECK(write(to_child[1], &sent, 1) == 1 && read(reply[0], &got, 1) == 1);
        CHECK((sent ^ got) == 0xff);
    }
    uint64_t ns = now() - start;
    CHECK(close(to_child[1]) == 0 && close(reply[0]) == 0);
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    return ns;
}

static unsigned char *buffer(size_t size, unsigned seed) {
    unsigned char *p = malloc(size);
    CHECK(p != NULL);
    for (size_t i = 0; i < size; ++i)
        p[i] = (unsigned char)(i * 31 + i / 4096 + seed);
    return p;
}

static uint64_t memory(void) {
    size_t size = 16 * 1024 * 1024;
    unsigned char *src = buffer(size, 37), *dst = buffer(size, 5);
    uint64_t start = now();
    for (int i = 0; i < 16; ++i) {
        memcpy(dst, src, size);
        __asm__ volatile("" : : "r"(dst) : "memory");
    }
    uint64_t ns = now() - start;
    CHECK(memcmp(src, dst, size) == 0);
    free(src);
    free(dst);
    return ns;
}

static uint64_t map_touch(void) {
    const size_t size = 32 * 1024 * 1024, page = 4096;
    CHECK(sysconf(_SC_PAGESIZE) == (long)page);
    uint64_t start = now();
    unsigned char *p = mmap(NULL, size, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED);
    for (size_t i = 0; i < size; i += page)
        p[i] = 1 + (i / page) % 251;
    __asm__ volatile("" : : "r"(p) : "memory");
    uint64_t ns = now() - start;
    for (size_t i = 0; i < size; i += page)
        CHECK(p[i] == 1 + (i / page) % 251 && p[i + 1] == 0);
    CHECK(munmap(p, size) == 0);
    return ns;
}

static uint64_t file_io(int measure_read, int durable) {
    const size_t size = 4 * 1024 * 1024, chunk = 4096;
    unsigned char *src = buffer(size, 37), *dst = buffer(size, 5);
    int fd = open("payload", O_CREAT | O_EXCL | O_RDWR, 0600);
    CHECK(fd >= 0);
    uint64_t start = now();
    for (size_t i = 0; i < size; i += chunk)
        CHECK(write(fd, src + i, chunk) == (ssize_t)chunk);
    if (durable)
        CHECK(fsync(fd) == 0);
    uint64_t write_ns = now() - start;
    CHECK(lseek(fd, 0, SEEK_SET) == 0);
    start = now();
    for (size_t i = 0; i < size; i += chunk)
        CHECK(read(fd, dst + i, chunk) == (ssize_t)chunk);
    uint64_t read_ns = now() - start;
    CHECK(read(fd, dst, 1) == 0 && memcmp(src, dst, size) == 0);
    CHECK(close(fd) == 0 && unlink("payload") == 0);
    free(src);
    free(dst);
    return measure_read ? read_ns : write_ns;
}

enum access_pattern { WARM_SEQ_READ, SHUFFLED_READ, SHUFFLED_OVERWRITE };

static uint64_t positioned_io(enum access_pattern op, int durable) {
    enum { BLOCKS = 1024, CHUNK = 4096, SIZE = BLOCKS * CHUNK };
    unsigned char *src = buffer(SIZE, 37), *dst = buffer(SIZE, 5);
    unsigned order[BLOCKS];
    int fd = open("payload", O_CREAT | O_EXCL | O_RDWR, 0600);
    CHECK(fd >= 0);
    for (uint32_t i = 0; i < BLOCKS; ++i) {
        order[i] = i;
        memcpy(src + i * CHUNK, &i, sizeof(i));
        CHECK(write(fd, src + i * CHUNK, CHUNK) == CHUNK);
    }
    if (durable)
        CHECK(fsync(fd) == 0);
    for (size_t i = 0; i < SIZE; i += CHUNK)
        CHECK(pread(fd, dst + i, CHUNK, i) == CHUNK);
    CHECK(memcmp(src, dst, SIZE) == 0);
    if (op != WARM_SEQ_READ) {
        uint32_t state = 1729;
        for (unsigned i = BLOCKS - 1; i > 0; --i) {
            state = state * 1664525u + 1013904223u;
            unsigned j = (state >> 8) % (i + 1), tmp = order[i];
            order[i] = order[j];
            order[j] = tmp;
        }
    }
    if (op == SHUFFLED_OVERWRITE)
        for (size_t i = 0; i < SIZE; ++i)
            src[i] ^= 0xff;
    memset(dst, 0, SIZE);
    uint64_t start = now();
    for (unsigned i = 0; i < BLOCKS; ++i) {
        size_t offset = order[i] * CHUNK;
        if (op == SHUFFLED_OVERWRITE)
            CHECK(pwrite(fd, src + offset, CHUNK, offset) == CHUNK);
        else
            CHECK(pread(fd, dst + offset, CHUNK, offset) == CHUNK);
    }
    if (op == SHUFFLED_OVERWRITE && durable)
        CHECK(fsync(fd) == 0);
    uint64_t ns = now() - start;
    if (op == SHUFFLED_OVERWRITE)
        for (size_t i = 0; i < SIZE; i += CHUNK)
            CHECK(pread(fd, dst + i, CHUNK, i) == CHUNK);
    struct stat st;
    CHECK(fstat(fd, &st) == 0 && st.st_size == SIZE && memcmp(src, dst, SIZE) == 0);
    CHECK(close(fd) == 0 && unlink("payload") == 0);
    free(src);
    free(dst);
    return ns;
}

static uint64_t metadata(void) {
    struct stat st;
    uint64_t start = now();
    for (int i = 0; i < 1024; ++i) {
        int fd = open("pending", O_CREAT | O_EXCL | O_WRONLY, 0600);
        CHECK(fd >= 0 && close(fd) == 0);
        CHECK(rename("pending", "ready") == 0);
        CHECK(stat("ready", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 0);
        CHECK(unlink("ready") == 0);
    }
    uint64_t ns = now() - start;
    CHECK(stat("ready", &st) == -1 && errno == ENOENT);
    CHECK(stat("pending", &st) == -1 && errno == ENOENT);
    return ns;
}

static int compare_names(const void *a, const void *b) {
    return strcmp(a, b);
}

static uint64_t dirscan(void) {
    enum { FILES = 512, PASSES = 16, WIDTH = sizeof("f0000") };
    char expected[FILES][WIDTH], names[FILES][WIDTH];
    for (int i = 0; i < FILES; ++i) {
        CHECK(snprintf(expected[i], WIDTH, "f%04d", i) == WIDTH - 1);
        int fd = open(expected[i], O_CREAT | O_EXCL | O_WRONLY, 0600);
        CHECK(fd >= 0 && close(fd) == 0);
    }
    uint64_t ns = 0;
    for (int pass = 0; pass < PASSES; ++pass) {
        size_t count = 0;
        uint64_t start = now();
        DIR *dir = opendir(".");
        CHECK(dir != NULL);
        errno = 0;
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, ".."))
                continue;
            CHECK(count < FILES && strlen(entry->d_name) == WIDTH - 1);
            memcpy(names[count++], entry->d_name, WIDTH);
        }
        CHECK(errno == 0 && closedir(dir) == 0);
        ns += now() - start;
        CHECK(count == FILES);
        qsort(names, FILES, WIDTH, compare_names);
        CHECK(memcmp(names, expected, sizeof(expected)) == 0);
    }
    for (int i = 0; i < FILES; ++i)
        CHECK(unlink(expected[i]) == 0);
    return ns;
}

int main(int argc, char **argv) {
    int init = getpid() == 1;
    const char *metric = init ? getenv("BENCH_METRIC") : argc == 2 ? argv[1] : NULL;
    CHECK(metric != NULL);
    struct statfs fs;
    CHECK(statfs("/", &fs) == 0);
    CHECK(fs.f_type == (init ? EXT4_SUPER_MAGIC : FUSE_SUPER_MAGIC));
    uint64_t ns;
    if (!strcmp(metric, "cpu-integer")) {
        ns = cpu();
    } else if (!strcmp(metric, "syscall-getpid")) {
        ns = syscall_getpid();
    } else if (!strcmp(metric, "ipc-pipe-roundtrip")) {
        ns = pipe_roundtrip();
    } else if (!strcmp(metric, "memory-copy")) {
        ns = memory();
    } else if (!strcmp(metric, "memory-map-touch")) {
        ns = map_touch();
    } else if (!strncmp(metric, "disk-", 5)) {
        CHECK(chdir("/work") == 0);
        if (!strcmp(metric, "disk-write-fsync"))
            ns = file_io(0, 1);
        else if (!strcmp(metric, "disk-warm-read"))
            ns = positioned_io(WARM_SEQ_READ, 1);
        else if (!strcmp(metric, "disk-shuffled-read"))
            ns = positioned_io(SHUFFLED_READ, 1);
        else if (!strcmp(metric, "disk-shuffled-overwrite-fsync"))
            ns = positioned_io(SHUFFLED_OVERWRITE, 1);
        else
            return 1;
    } else {
        CHECK(mount("tmpfs", "/ram", "tmpfs", 0, NULL) == 0);
        CHECK(chdir("/ram") == 0 && statfs(".", &fs) == 0 && fs.f_type == TMPFS_MAGIC);
        if (!strcmp(metric, "tmpfs-read"))
            ns = file_io(1, 0);
        else if (!strcmp(metric, "tmpfs-write"))
            ns = file_io(0, 0);
        else if (!strcmp(metric, "tmpfs-shuffled-read"))
            ns = positioned_io(SHUFFLED_READ, 0);
        else if (!strcmp(metric, "tmpfs-shuffled-overwrite"))
            ns = positioned_io(SHUFFLED_OVERWRITE, 0);
        else if (!strcmp(metric, "tmpfs-metadata"))
            ns = metadata();
        else if (!strcmp(metric, "tmpfs-dirscan"))
            ns = dirscan();
        else
            return 1;
    }
    CHECK(ns > 0);
    printf("VM_GUEST_OK %s %" PRIu64 "\n", metric, ns);
    CHECK(fflush(stdout) == 0);
    if (init)
        CHECK(reboot(RB_AUTOBOOT) == 0);
    return 0;
}
