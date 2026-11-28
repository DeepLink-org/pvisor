// SPDX-License-Identifier: MPL-2.0

#include <assert.h>
#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static const uint64_t MS = 1000000;

static uint64_t now(clockid_t clock) {
    struct timespec ts;
    assert(clock_gettime(clock, &ts) == 0);
    return (uint64_t)ts.tv_sec * 1000000000 + (uint64_t)ts.tv_nsec;
}

static uint64_t timeval_ns(struct timeval tv) {
    return (uint64_t)tv.tv_sec * 1000000000 + (uint64_t)tv.tv_usec * 1000;
}

static volatile sig_atomic_t received;
static void handle_signal(int signo) {
    received = signo;
}

static void burn_until_signal(int signo, int syscall_mix) {
    uint64_t end = now(CLOCK_MONOTONIC) + 5000 * MS;
    volatile uint32_t x = 1;
    long pid = getpid();
    while (!received) {
        if (syscall_mix) {
            for (int i = 0; i < 512; i++)
                assert(syscall(SYS_getpid) == pid);
        } else {
            for (int i = 0; i < 4096; i++)
                x = x * 1664525u + 1013904223u;
        }
        assert(now(CLOCK_MONOTONIC) < end);
    }
    assert(received == signo);
}

static void check_process_timer(int which, int signo, int contended) {
    assert(signal(signo, handle_signal) != SIG_ERR);
    received = 0;
    struct rusage before, after;
    assert(getrusage(RUSAGE_SELF, &before) == 0);
    uint64_t start = now(CLOCK_MONOTONIC);
    uint64_t cpu_start = now(CLOCK_PROCESS_CPUTIME_ID);
    const struct itimerval timer = {.it_value = {.tv_usec = 120000}};
    assert(setitimer(which, &timer, NULL) == 0);
    burn_until_signal(signo, which == ITIMER_PROF);
    uint64_t cpu = now(CLOCK_PROCESS_CPUTIME_ID) - cpu_start;
    uint64_t wall = now(CLOCK_MONOTONIC) - start;
    assert(getrusage(RUSAGE_SELF, &after) == 0);
    uint64_t user = timeval_ns(after.ru_utime) - timeval_ns(before.ru_utime);
    uint64_t system = timeval_ns(after.ru_stime) - timeval_ns(before.ru_stime);
    printf("CPU_TIMER %d wall_ns=%" PRIu64 " cpu_ns=%" PRIu64 " user_ns=%" PRIu64 "\n",
           which, wall, cpu, user);
    assert(fflush(stdout) == 0);
    assert(cpu >= 120 * MS && cpu < 160 * MS);
    if (which == ITIMER_VIRTUAL)
        assert(user >= 120 * MS && user < 160 * MS);
    else
        assert(system >= 10 * MS && user > 0);
    if (contended)
        assert(wall > cpu + 30 * MS);
}

static void check_thread_timer(void) {
    const int signo = SIGUSR1;
    assert(signal(signo, handle_signal) != SIG_ERR);
    received = 0;
    int id;
    struct sigevent event = {.sigev_notify = SIGEV_SIGNAL, .sigev_signo = signo};
    int clock = CLOCK_THREAD_CPUTIME_ID;
    // glibc remaps CLOCK_THREAD_CPUTIME_ID to a dynamic clock ID. Exercise
    // the native kernel's supported fixed-ID ABI directly.
    assert(syscall(SYS_timer_create, clock, &event, &id) == 0);
    uint64_t start = now(clock);
    const struct itimerspec timer = {.it_value = {.tv_nsec = 30 * MS}};
    assert(syscall(SYS_timer_settime, id, 0, &timer, NULL) == 0);
    burn_until_signal(signo, 0);
    uint64_t elapsed = now(clock) - start;
    assert(elapsed >= 30 * MS && elapsed < 65 * MS);
    assert(syscall(SYS_timer_delete, id) == 0);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    int contended = strcmp(argv[1], "contended") == 0;
    assert(contended || strcmp(argv[1], "ordinary") == 0);
    uint64_t start = now(CLOCK_THREAD_CPUTIME_ID);
    assert(nanosleep(&(struct timespec){.tv_nsec = 100 * MS}, NULL) == 0);
    assert(now(CLOCK_THREAD_CPUTIME_ID) - start < 10 * MS);
    check_process_timer(ITIMER_VIRTUAL, SIGVTALRM, contended);
    check_process_timer(ITIMER_PROF, SIGPROF, contended);
    check_thread_timer();
    puts("CPU_RUNTIME_OK");
}
