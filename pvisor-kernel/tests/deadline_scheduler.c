// SPDX-License-Identifier: MPL-2.0

#include <assert.h>
#include <inttypes.h>
#include <pthread.h>
#include <sched.h>
#include <semaphore.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ns(clockid_t clock) {
    struct timespec ts;
    assert(clock_gettime(clock, &ts) == 0);
    return (uint64_t)ts.tv_sec * 1000000000 + (uint64_t)ts.tv_nsec;
}

static atomic_uint turn;

// No blocking call or explicit yield while both threads are runnable.
static void contend(unsigned id) {
    uint64_t end = now_ns(CLOCK_MONOTONIC) + 2000000000;
    for (unsigned i = 0; i < 6; i++) {
        unsigned spins = 0;
        while (atomic_load_explicit(&turn, memory_order_acquire) != id) {
            if (++spins % 4096 == 0)
                assert(now_ns(CLOCK_MONOTONIC) < end);
        }
        atomic_store_explicit(&turn, 1 - id, memory_order_release);
    }
}

struct contender {
    int policy;
    struct sched_param param;
    sem_t ready;
    sem_t start;
};

static void *peer(void *arg) {
    struct contender *c = arg;
    assert(sched_setscheduler(0, c->policy, &c->param) == 0);
    assert(sem_post(&c->ready) == 0);
    assert(sem_wait(&c->start) == 0);
    contend(1);
    return NULL;
}

static void check_contention(int policy, int priority) {
    struct contender c = {.policy = policy, .param = {.sched_priority = priority}};
    assert(sem_init(&c.ready, 0, 0) == 0 && sem_init(&c.start, 0, 0) == 0);
    atomic_store_explicit(&turn, 0, memory_order_relaxed);
    pthread_t thread;
    assert(pthread_create(&thread, NULL, peer, &c) == 0);
    assert(sem_wait(&c.ready) == 0);
    assert(sched_setscheduler(0, policy, &c.param) == 0);
    assert(sem_post(&c.start) == 0);
    contend(0);
    assert(pthread_join(thread, NULL) == 0);
    const struct sched_param normal = {0};
    assert(sched_setscheduler(0, SCHED_OTHER, &normal) == 0);
    assert(sem_destroy(&c.ready) == 0 && sem_destroy(&c.start) == 0);
}

static volatile sig_atomic_t fired;

static void on_alarm(int sig) {
    fired = sig;
}

static uint64_t check_timer(int which, int sig) {
    assert(signal(sig, on_alarm) != SIG_ERR);
    sigset_t mask;
    assert(sigemptyset(&mask) == 0 && sigaddset(&mask, sig) == 0);
    if (which == ITIMER_REAL)
        assert(sigprocmask(SIG_BLOCK, &mask, NULL) == 0);
    const struct itimerval alarm = {.it_value = {.tv_usec = 30000}};
    fired = 0;
    uint64_t start = now_ns(CLOCK_MONOTONIC);
    assert(setitimer(which, &alarm, NULL) == 0);
    if (which == ITIMER_REAL) {
        sigset_t empty;
        assert(sigemptyset(&empty) == 0);
        while (fired == 0)
            sigsuspend(&empty);
        assert(sigprocmask(SIG_UNBLOCK, &mask, NULL) == 0);
    } else {
        while (fired == 0)
            assert(now_ns(CLOCK_MONOTONIC) - start < 1000000000);
    }
    assert(fired == sig);
    return now_ns(CLOCK_MONOTONIC) - start;
}

int main(void) {
    check_contention(SCHED_OTHER, 0);
    check_contention(SCHED_RR, 1);
    // Let the workqueue drain before asking an idle pool to deliver the alarm.
    assert(nanosleep(&(struct timespec){.tv_nsec = 250000000}, NULL) == 0);
    uint64_t real_alarm = check_timer(ITIMER_REAL, SIGALRM);
    uint64_t virtual_alarm = check_timer(ITIMER_VIRTUAL, SIGVTALRM);
    printf("DEADLINE_SCHEDULER_OK real_alarm_ns=%" PRIu64
           " virtual_alarm_ns=%" PRIu64 "\n",
           real_alarm, virtual_alarm);
}
