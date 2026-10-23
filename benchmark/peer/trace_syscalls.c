/* Diagnostic helper for B-FOUR-PEER-ENG. Inject only into owned test processes.
 * Build: cc -shared -fPIC -O2 -pthread trace_syscalls.c -o trace_syscalls.so
 * Raw syscalls avoid dlsym recursion. This deliberately replaces libc's
 * cancellation wrappers, so it is diagnostic-only and never a timing arm.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

enum { RECV, SEND, RECVFROM, SENDTO, RECVMSG, SENDMSG, READ, WRITE, WRITEV, EPOLL, OPS };
static const char *names[] = {"recv", "send", "recvfrom", "sendto", "recvmsg", "sendmsg", "read", "write", "writev", "epoll_wait"};
struct count { _Atomic uint64_t calls, bytes, nanos, again; };
struct thread { pid_t tid; char name[16]; struct count counters[OPS]; };
static struct thread threads[256];
static _Atomic unsigned used;
static __thread struct thread *local;
static void checkpoint(struct thread *thread);

static uint64_t now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}
static void count(unsigned op, uint64_t started, ssize_t result, int error) {
    if (!local) {
        unsigned slot = atomic_fetch_add_explicit(&used, 1, memory_order_relaxed);
        if (slot >= 256) return;
        local = &threads[slot];
        local->tid = syscall(SYS_gettid);
        syscall(SYS_prctl, PR_GET_NAME, local->name);
    }
    struct count *c = &local->counters[op];
    atomic_fetch_add_explicit(&c->calls, 1, memory_order_relaxed);
    atomic_fetch_add_explicit(&c->nanos, now() - started, memory_order_relaxed);
    if (result > 0) atomic_fetch_add_explicit(&c->bytes, result, memory_order_relaxed);
    if (result < 0 && error == EAGAIN) atomic_fetch_add_explicit(&c->again, 1, memory_order_relaxed);
    // VM runners may terminate without ELF destructors. Retain cumulative
    // checkpoints while they are alive; analysis deduplicates by pid/tid/op
    // and takes the latest count (the unfinished tail is a lower bound).
    if (atomic_load_explicit(&c->calls, memory_order_relaxed) % 1024 == 0) checkpoint(local);
    errno = error;
}
static void reset_child(void) {
    memset(threads, 0, sizeof(threads));
    atomic_store(&used, 0);
    local = NULL;
}
__attribute__((constructor)) static void initialize(void) {
    pthread_atfork(NULL, NULL, reset_child);
}
static void checkpoint(struct thread *thread) {
    const char *directory = getenv("PVISOR_NET_PROFILE_DIR");
    if (!directory) return;
    char path[4096], line[1024];
    snprintf(path, sizeof(path), "%s/%d-%d.jsonl", directory, getpid(), thread->tid);
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd < 0) return;
    for (unsigned op = 0; op < OPS; ++op) {
        struct count *c = &thread->counters[op];
        uint64_t calls = atomic_load(&c->calls);
        if (!calls) continue;
        int length = snprintf(line, sizeof(line),
            "{\"pid\":%d,\"tid\":%d,\"thread\":\"%s\",\"op\":\"%s\",\"calls\":%lu,\"bytes\":%lu,\"nanos\":%lu,\"again\":%lu}\n",
            getpid(), thread->tid, thread->name, names[op], calls,
            atomic_load(&c->bytes), atomic_load(&c->nanos), atomic_load(&c->again));
        syscall(SYS_write, fd, line, length);
    }
    syscall(SYS_close, fd);
}
__attribute__((destructor)) static void finish(void) {
    const char *directory = getenv("PVISOR_NET_PROFILE_DIR");
    if (!directory) return;
    char path[4096], line[1024];
    snprintf(path, sizeof(path), "%s/%d.jsonl", directory, getpid());
    int fd = syscall(SYS_openat, AT_FDCWD, path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return;
    unsigned total = atomic_load(&used);
    for (unsigned i = 0; i < total && i < 256; ++i) {
        for (unsigned op = 0; op < OPS; ++op) {
            struct count *c = &threads[i].counters[op];
            uint64_t calls = atomic_load(&c->calls);
            if (!calls) continue;
            int length = snprintf(line, sizeof(line),
                "{\"pid\":%d,\"tid\":%d,\"thread\":\"%s\",\"op\":\"%s\",\"calls\":%lu,\"bytes\":%lu,\"nanos\":%lu,\"again\":%lu}\n",
                getpid(), threads[i].tid, threads[i].name, names[op], calls,
                atomic_load(&c->bytes), atomic_load(&c->nanos), atomic_load(&c->again));
            syscall(SYS_write, fd, line, length);
        }
    }
    syscall(SYS_close, fd);
}
#define TRACE(op, expression) uint64_t t = now(); ssize_t result = (expression); int error = errno; count(op, t, result, error); return result
ssize_t recv(int fd, void *buf, size_t len, int flags) { TRACE(RECV, syscall(SYS_recvfrom, fd, buf, len, flags, NULL, NULL)); }
ssize_t send(int fd, const void *buf, size_t len, int flags) { TRACE(SEND, syscall(SYS_sendto, fd, buf, len, flags, NULL, 0)); }
ssize_t recvfrom(int fd, void *buf, size_t len, int flags, struct sockaddr *address, socklen_t *size) { TRACE(RECVFROM, syscall(SYS_recvfrom, fd, buf, len, flags, address, size)); }
ssize_t sendto(int fd, const void *buf, size_t len, int flags, const struct sockaddr *address, socklen_t size) { TRACE(SENDTO, syscall(SYS_sendto, fd, buf, len, flags, address, size)); }
ssize_t recvmsg(int fd, struct msghdr *message, int flags) { TRACE(RECVMSG, syscall(SYS_recvmsg, fd, message, flags)); }
ssize_t sendmsg(int fd, const struct msghdr *message, int flags) { TRACE(SENDMSG, syscall(SYS_sendmsg, fd, message, flags)); }
ssize_t read(int fd, void *buf, size_t len) { TRACE(READ, syscall(SYS_read, fd, buf, len)); }
ssize_t write(int fd, const void *buf, size_t len) { TRACE(WRITE, syscall(SYS_write, fd, buf, len)); }
ssize_t writev(int fd, const struct iovec *vector, int count_) { TRACE(WRITEV, syscall(SYS_writev, fd, vector, count_)); }
int epoll_wait(int fd, struct epoll_event *events, int max, int timeout) { TRACE(EPOLL, syscall(SYS_epoll_wait, fd, events, max, timeout)); }
