// SPDX-License-Identifier: MPL-2.0

#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <linux/futex.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static const uint64_t MS = 1000000;
static const uint64_t SEC = 1000000000;

static uint64_t now(clockid_t clock) {
    struct timespec ts;
    assert(clock_gettime(clock, &ts) == 0);
    return (uint64_t)ts.tv_sec * SEC + (uint64_t)ts.tv_nsec;
}

static struct timespec timespec(uint64_t ns) {
    return (struct timespec){.tv_sec = ns / SEC, .tv_nsec = ns % SEC};
}

static void arm(int fd, uint64_t deadline, uint64_t period) {
    struct itimerspec timer = {.it_value = timespec(deadline),
                               .it_interval = timespec(period)};
    assert(timerfd_settime(fd, TFD_TIMER_ABSTIME, &timer, NULL) == 0);
}

static void expect_empty(int fd) {
    uint64_t count;
    assert(read(fd, &count, sizeof(count)) == -1 && errno == EAGAIN);
}

static void expect_expired(int fd) {
    uint64_t count;
    assert(read(fd, &count, sizeof(count)) == sizeof(count) && count >= 1);
}

static void wait_for(uint64_t delay) {
    uint64_t start = now(CLOCK_MONOTONIC);
    struct timespec ts = timespec(delay);
    assert(nanosleep(&ts, NULL) == 0);
    assert(now(CLOCK_MONOTONIC) - start >= delay);
}

struct counters {
    uint64_t irqs;
    uint64_t timer_softirqs;
};

static struct counters read_counters(void) {
    struct counters counts;
    FILE *stat = fopen("/proc/stat", "r");
    assert(stat);
    char *line = NULL;
    size_t capacity = 0;
    int seen = 0;
    while (getline(&line, &capacity, stat) != -1) {
        if (sscanf(line, "intr %" SCNu64, &counts.irqs) == 1)
            seen |= 1;
        if (sscanf(line, "softirq %*s %*s %" SCNu64, &counts.timer_softirqs) == 1)
            seen |= 2;
    }
    assert(seen == 3 && fclose(stat) == 0);
    free(line);
    return counts;
}

static void check_replacement(clockid_t clock) {
    int blocker = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    int fd = timerfd_create(clock, 0);
    assert(blocker >= 0 && fd >= 0);
    arm(blocker, now(CLOCK_MONOTONIC) + SEC, 0);
    arm(fd, now(clock) + 2 * SEC, 0);
    uint64_t deadline = now(clock) + MS / 5;
    arm(fd, deadline, 0);
    expect_expired(fd);
    assert(now(clock) >= deadline);
    expect_empty(blocker);
    arm(blocker, now(CLOCK_MONOTONIC) + 2 * MS, 0);
    arm(blocker, 0, 0);
    wait_for(5 * MS);
    expect_empty(blocker);
    assert(close(fd) == 0 && close(blocker) == 0);
}

static void check_busy_expiry(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    assert(fd >= 0);
    uint64_t deadline = now(CLOCK_MONOTONIC) + MS / 5;
    arm(fd, deadline, 0);
    uint64_t count;
    ssize_t size;
    do {
        size = read(fd, &count, sizeof(count));
        assert(size != -1 || errno == EAGAIN);
    } while (size == -1);
    assert(size == sizeof(count) && count == 1 && now(CLOCK_MONOTONIC) >= deadline);
    assert(close(fd) == 0);
}

static int compare(const void *a, const void *b) {
    uint64_t lhs = *(const uint64_t *)a, rhs = *(const uint64_t *)b;
    return (lhs > rhs) - (lhs < rhs);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    for (size_t i = 0; i < 3; i++) {
        const clockid_t clocks[] = {CLOCK_MONOTONIC, CLOCK_REALTIME, CLOCK_BOOTTIME};
        check_replacement(clocks[i]);
    }
    check_busy_expiry();
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    assert(fd >= 0);
    uint64_t deadline = now(CLOCK_MONOTONIC) + MS;
    arm(fd, deadline, 2 * MS);
    expect_expired(fd);
    expect_expired(fd);
    assert(now(CLOCK_MONOTONIC) >= deadline + 2 * MS);
    assert(close(fd) == 0);

    uint32_t futex = 0;
    struct timespec timeout = timespec(2 * MS);
    uint64_t start = now(CLOCK_MONOTONIC);
    assert(syscall(SYS_futex, &futex, FUTEX_WAIT_PRIVATE, 0, &timeout, NULL, 0) == -1);
    assert(errno == ETIMEDOUT && now(CLOCK_MONOTONIC) - start >= 2 * MS);

    struct counters start_counts = read_counters();
    wait_for(250 * MS);
    struct counters end_counts = read_counters();
    uint64_t idle_irqs = end_counts.irqs - start_counts.irqs;

    start_counts = read_counters();
    deadline = now(CLOCK_MONOTONIC) + 150 * MS;
    while (now(CLOCK_MONOTONIC) < deadline) {
    }
    end_counts = read_counters();
    uint64_t busy_irqs = end_counts.irqs - start_counts.irqs;
    uint64_t busy_scans = end_counts.timer_softirqs - start_counts.timer_softirqs;
    if (strcmp(argv[1], "deadline") == 0) {
        // The busy compatibility tick is still present, but cannot run
        // timer softirq unless there is an actual wall timer to expire.
        assert(idle_irqs < 40 && busy_irqs > 40 && busy_scans < 30);
    } else {
        assert(strcmp(argv[1], "periodic") == 0);
        assert(idle_irqs > 40 && busy_scans > 40);
    }

    enum { SAMPLES = 256 };
    uint64_t lateness[SAMPLES];
    fd = timerfd_create(CLOCK_MONOTONIC, 0);
    assert(fd >= 0);
    for (size_t i = 0; i < SAMPLES; i++) {
        deadline = now(CLOCK_MONOTONIC) + MS / 5;
        arm(fd, deadline, 0);
        expect_expired(fd);
        uint64_t observed = now(CLOCK_MONOTONIC);
        assert(observed >= deadline);
        lateness[i] = observed - deadline;
    }
    assert(close(fd) == 0);
    qsort(lateness, SAMPLES, sizeof(*lateness), compare);
    printf("WALL_DEADLINE_OK %s idle_irqs=%" PRIu64 " busy_irqs=%" PRIu64
           " busy_scans=%" PRIu64 " p50_late_ns=%" PRIu64
           " p95_late_ns=%" PRIu64 "\n",
           argv[1], idle_irqs, busy_irqs, busy_scans,
           lateness[SAMPLES / 2], lateness[SAMPLES * 95 / 100]);
}
