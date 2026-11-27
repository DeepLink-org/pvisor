/* B-VM-GUEST-ENG: identical warm reads with and without a 1 ms think time. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { perror(#x); exit(1); } } while (0)

static uint64_t now(void) {
    struct timespec t;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &t) == 0);
    return (uint64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}

int main(int argc, char **argv) {
    CHECK(argc == 2);
    int spaced = !strcmp(argv[1], "spaced");
    CHECK(spaced || !strcmp(argv[1], "dense"));
    int fd = open("/work/packet", O_CREAT | O_EXCL | O_RDWR, 0600);
    unsigned char src[4096], dst[4096];
    for (size_t i = 0; i < sizeof(src); ++i) src[i] = i % 251;
    CHECK(fd >= 0 && write(fd, src, sizeof(src)) == sizeof(src) && fsync(fd) == 0);
    CHECK(pread(fd, dst, sizeof(dst), 0) == sizeof(dst) && !memcmp(src, dst, sizeof(dst)));
    uint64_t ns = 0;
    for (unsigned i = 0; i < 256; ++i) {
        if (spaced) CHECK(nanosleep(&(struct timespec){0, 1000000}, NULL) == 0);
        dst[i] ^= 0xff;
        uint64_t start = now();
        CHECK(pread(fd, dst, sizeof(dst), 0) == sizeof(dst));
        ns += now() - start;
        CHECK(!memcmp(src, dst, sizeof(dst)));
    }
    CHECK(close(fd) == 0);
    printf("VM_GUEST_OK %s %" PRIu64 "\n", argv[1], ns);
}
