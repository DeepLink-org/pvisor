/* B-ALLOCATOR-RECLAIM-ENG payload: retained free heap, normal allocation threads. */
#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

struct allocation { unsigned char *memory; size_t size; unsigned seed; };
static struct allocation blocks[4];
static unsigned char *guard;

static void *allocate(void *arg)
{
    struct allocation *block = arg;
    block->memory = malloc(block->size);
    if (!block->memory) abort();
    for (size_t i = 0; i < block->size; i++)
        block->memory[i] = (unsigned char)(1 + ((i * 17 + i / 4096 + block->seed) % 251));
    return NULL;
}

static void verify(const struct allocation *block)
{
    for (size_t i = 0; i < block->size; i++)
        if (block->memory[i] != (unsigned char)(1 + ((i * 17 + i / 4096 + block->seed) % 251)))
            abort();
}

static void fill(size_t bytes)
{
    pthread_t threads[4];
    for (unsigned i = 0; i < 4; i++) {
        blocks[i].size = bytes / 4;
        blocks[i].seed = 37 * i;
        if (pthread_create(&threads[i], NULL, allocate, &blocks[i])) abort();
    }
    for (unsigned i = 0; i < 4; i++) {
        if (pthread_join(threads[i], NULL)) abort();
        verify(&blocks[i]);
    }
}

static void marker(const char *phase)
{
    printf("{\"phase\":\"%s\",\"pid\":%ld}\n", phase, (long)getpid());
    fflush(stdout);
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "--idle")) {
        for (;;) pause();
    }
    if (argc != 2) return 2;
    char *end;
    unsigned long mib = strtoul(argv[1], &end, 10);
    if (*end || (mib != 64 && mib != 256)) return 2;
    if (!mallopt(M_TRIM_THRESHOLD, -1) || !mallopt(M_MMAP_MAX, 0) || !mallopt(M_ARENA_MAX, 1)) return 3;
    size_t bytes = mib * 1024 * 1024;
    fill(bytes);
    guard = malloc(65536);
    if (!guard) abort();
    memset(guard, 0xa5, 65536);
    marker("allocated");
    char command;
    for (;;) {
        ssize_t count = read(0, &command, 1);
        if (count < 0 && errno == EINTR) continue;
        if (count != 1) return 4;
        for (int i = 0; i < 65536; i++) if (guard[i] != 0xa5) abort();
        if (command == 'F') {
            for (int i = 0; i < 4; i++) {
                verify(&blocks[i]);
                free(blocks[i].memory);
                blocks[i].memory = NULL;
            }
            marker("freed");
        } else if (command == 'R') {
            fill(bytes);
            marker("refilled-verified");
        } else if (command == 'Q') {
            for (int i = 0; i < 4; i++) verify(&blocks[i]);
            marker("verified-exit");
            return 0;
        } else return 5;
    }
}
