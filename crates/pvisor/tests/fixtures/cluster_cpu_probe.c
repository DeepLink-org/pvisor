/* Bounded, result-checked N-queens tool workload. Count n=13: OEIS A000170.
 * Absolute arrivals expose queuing delay rather than coordinated omission.
 * This runs inside the VM; no host process impersonates guest execution. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static uint64_t solve(uint32_t mask, uint32_t cols, uint32_t left, uint32_t right) {
    if (cols == mask) return 1;
    uint32_t available = mask & ~(cols | left | right);
    uint64_t count = 0;
    while (available) {
        uint32_t bit = available & (0u - available);
        available -= bit;
        count += solve(mask, cols | bit, (left | bit) << 1, (right | bit) >> 1);
    }
    return count;
}
static uint64_t now(clockid_t clock) {
    struct timespec t;
    if (clock_gettime(clock, &t)) { perror("clock_gettime"); exit(2); }
    return (uint64_t)t.tv_sec * 1000000000u + (uint64_t)t.tv_nsec;
}
static void touch(const char *path) {
    FILE *file = fopen(path, "w");
    if (!file || fputs("ready\n", file) < 0 || fclose(file)) { perror(path); exit(2); }
}
static void sleep_until(uint64_t deadline) {
    struct timespec t = { .tv_sec = deadline / 1000000000u,
        .tv_nsec = deadline % 1000000000u };
    int error;
    while ((error = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &t, NULL)) == EINTR) {}
    if (error) { errno = error; perror("clock_nanosleep"); exit(2); }
}
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    int foreground = strcmp(argv[1], "ls") == 0;
    /* A volatile input per solve prevents whole-workload constant folding or
     * hoisting identical pure searches out of the measurement loop. */
    volatile uint32_t board_mask = (1u << 13) - 1;
    unsigned steps = (unsigned)strtoul(argv[2], NULL, 10);
    if (steps < 20 || steps > 200) return 2;
    uint64_t expected = 0;
    FILE *expected_file = fopen("/env/expected", "r");
    if (!expected_file || fscanf(expected_file, "%" SCNu64, &expected) != 1 || fclose(expected_file)) return 2;
    FILE *records = fopen("/env/steps.jsonl", "w");
    if (!records || setvbuf(records, NULL, _IOLBF, 0)) return 2;
    touch("/env/ready");
    while (access("/env/go", F_OK)) sleep_until(now(CLOCK_MONOTONIC) + 5000000u);
    for (unsigned i = 0; i < 5; ++i) if (solve(board_mask, 0, 0, 0) != expected) return 3;
    touch("/env/warm");
    if (foreground) {
        /* The host releases measurement only after every background is warm. */
        while (access("/env/measure", F_OK)) sleep_until(now(CLOCK_MONOTONIC) + 5000000u);
    }
    uint64_t epoch = now(CLOCK_MONOTONIC);
    for (unsigned i = 0; foreground ? i < steps : access("/env/stop", F_OK) != 0; ++i) {
        uint64_t intended = foreground ? epoch + (uint64_t)i * 100000000u : now(CLOCK_MONOTONIC);
        if (foreground) sleep_until(intended);
        uint64_t begin = now(CLOCK_MONOTONIC), cpu_begin = now(CLOCK_THREAD_CPUTIME_ID);
        uint64_t answer = solve(board_mask, 0, 0, 0);
        uint64_t cpu_end = now(CLOCK_THREAD_CPUTIME_ID), end = now(CLOCK_MONOTONIC);
        if (answer != expected) return 3;
        if (fprintf(records, "{\"sequence\":%u,\"intended_ns\":%" PRIu64 ",\"begin_ns\":%" PRIu64
                ",\"end_ns\":%" PRIu64 ",\"cpu_ns\":%" PRIu64 ",\"solutions\":%" PRIu64 "}\n",
                i, intended, begin, end, cpu_end - cpu_begin, answer) < 0) return 2;
    }
    if (fclose(records)) return 2;
    touch("/env/done");
    /* Keep VM teardown and Bundle persistence outside the timed CPU envelope. */
    while (access("/env/release", F_OK)) sleep_until(now(CLOCK_MONOTONIC) + 5000000u);
    return 0;
}
