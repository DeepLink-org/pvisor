/* B-FS-DIAG only. Delay the first backing write to A without changing bytes. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>
static atomic_int held;
static void event(char file, const char *phase) {
    const char *path = getenv("CURVE_PIPELINE_EVENTS");
    if (!path) return;
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    char line[160];
    int n = snprintf(line, sizeof line, "{\"file\":\"%c\",\"phase\":\"%s\",\"ns\":%lld}\n",
                     file, phase, (long long)now.tv_sec * 1000000000LL + now.tv_nsec);
    int out = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (out >= 0) { (void)write(out, line, n); close(out); }
}
static char begin(int fd) {
    char link[64], path[4096];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    if (n <= 0) return 0;
    path[n] = 0;
    char *base = strrchr(path, '/');
    char kind = base && !strcmp(base, "/pipeline-a") ? 'a' :
                base && !strcmp(base, "/pipeline-b") ? 'b' : 0;
    if (!kind) return 0;
    event(kind, "begin");
    if (kind == 'a' && !atomic_exchange(&held, 1)) {
        struct timespec delay = {.tv_sec = 1, .tv_nsec = 500000000};
        while (nanosleep(&delay, &delay)) {}
    }
    return kind;
}
ssize_t pwrite64(int fd, const void *buf, size_t n, off64_t offset) {
    ssize_t (*real)(int,const void*,size_t,off64_t) = dlsym(RTLD_NEXT,"pwrite64");
    char kind = begin(fd);
    ssize_t out = real(fd,buf,n,offset);
    if (kind) event(kind,"end");
    return out;
}
ssize_t pwritev(int fd, const struct iovec *vec, int n, off_t offset) {
    ssize_t (*real)(int,const struct iovec*,int,off_t) = dlsym(RTLD_NEXT,"pwritev");
    char kind = begin(fd);
    ssize_t out = real(fd,vec,n,offset);
    if (kind) event(kind,"end");
    return out;
}
ssize_t pwritev64(int fd, const struct iovec *vec, int n, off64_t offset) {
    ssize_t (*real)(int,const struct iovec*,int,off64_t) = dlsym(RTLD_NEXT,"pwritev64");
    char kind = begin(fd);
    ssize_t out = real(fd,vec,n,offset);
    if (kind) event(kind,"end");
    return out;
}
