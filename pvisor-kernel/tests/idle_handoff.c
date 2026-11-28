#include <assert.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int64_t now_ns(void) {
    struct timespec now;
    assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
    return (int64_t)now.tv_sec * 1000000000 + now.tv_nsec;
}

int main(int argc, char **argv) {
    char byte;
    assert(write(STDOUT_FILENO, "BLOCK\n", 6) == 6);
    assert(read(STDIN_FILENO, &byte, 1) == 1 && byte == 'A');
    if (argc == 2 && strcmp(argv[1], "measure") == 0) {
        assert(write(STDOUT_FILENO, "DONE\n", 5) == 5);
        return 7;
    }

    int64_t start = now_ns();
    assert(write(STDOUT_FILENO, "TIMER\n", 6) == 6);
    struct timespec delay = {.tv_nsec = 600000000};
    assert(nanosleep(&delay, NULL) == 0);
    assert(now_ns() - start >= delay.tv_nsec);
    assert(write(STDOUT_FILENO, "TIMER_OK\n", 9) == 9);
    assert(read(STDIN_FILENO, &byte, 1) == 1 && byte == 'B');
    assert(write(STDOUT_FILENO, "DONE\n", 5) == 5);
    return 7;
}
