/* A real guest process: dirty anonymous RAM, keep an FD open, and verify the
 * entire allocation before exit. Every heartbeat touches every resident page. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static uint64_t pattern(size_t i) { return UINT64_C(0x91c6d52f78a403be) ^ (i * UINT64_C(0x9e3779b97f4a7c15)); }

int main(void) {
    const size_t bytes = 256UL * 1024 * 1024, count = bytes / sizeof(uint64_t);
    uint64_t *memory = malloc(bytes);
    if (!memory) return 2;
    for (size_t i = 0; i < count; ++i) memory[i] = pattern(i);
    FILE *counter = fopen("counter", "w");
    if (!counter) return 3;
    for (unsigned iteration = 1; access("stop", F_OK); ++iteration) {
        for (size_t i = 0; i < count; i += 4096 / sizeof(uint64_t))
            if (memory[i] != pattern(i)) return 4;
        fprintf(counter, "%d %u\n", getpid(), iteration);
        fflush(counter);
        usleep(50000);
    }
    for (size_t i = 0; i < count; ++i)
        if (memory[i] != pattern(i)) return 5;
    fclose(counter);
    free(memory);
    FILE *verified = fopen("verified", "w");
    if (!verified) return 6;
    fputs("all 256 MiB verified\n", verified);
    return fclose(verified) != 0;
}
