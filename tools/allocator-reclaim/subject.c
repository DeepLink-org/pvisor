#define _GNU_SOURCE
#include <errno.h>
#include <malloc.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <dirent.h>

extern char **environ;

static int channel_descriptor(void)
{
    const char *binding = getenv("PVISOR_RECLAIM_CHANNEL");
    return binding ? atoi(binding) : -1;
}

static int thread_count(void)
{
    DIR *directory = opendir("/proc/self/task");
    if (!directory)
        return -1;
    int count = 0;
    struct dirent *entry;
    while ((entry = readdir(directory)))
        if (entry->d_name[0] != '.')
            count++;
    closedir(directory);
    return count;
}

static _Atomic unsigned long progress;

static void *worker(void *arg)
{
    (void)arg;
    for (;;) {
        volatile unsigned char *memory = malloc(65536);
        if (memory == NULL)
            abort();
        for (size_t i = 0; i < 65536; i += 4096)
            memory[i] = 0xa5;
        for (size_t i = 0; i < 65536; i += 4096) {
            if (memory[i] != 0xa5)
                abort();
        }
        free((void *)memory);
        atomic_fetch_add(&progress, 1);
        usleep(1000);
    }
    return NULL;
}

static void *short_worker(void *arg)
{
    (void)arg;
    volatile unsigned char *memory = malloc(4096);
    if (memory == NULL)
        abort();
    memory[0] = 0x5a;
    free((void *)memory);
    return NULL;
}

static void *churn(void *arg)
{
    (void)arg;
    for (;;) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, short_worker, NULL) != 0 ||
            pthread_join(thread, NULL) != 0)
            abort();
        usleep(10000);
    }
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--child-exec-check") == 0) {
        if (thread_count() != 1 || fcntl(channel_descriptor(), F_GETFD) < 0)
            return 5;
        puts("fork-exec-child-ok");
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "--vfork-exec-check") == 0) {
        if (thread_count() != 1 || fcntl(channel_descriptor(), F_GETFD) >= 0 || errno != EBADF)
            return 15;
        puts("vfork-exec-child-ok");
        return 0;
    }
    const int busy = argc == 2 && strcmp(argv[1], "--busy") == 0;
    const int multithread = argc == 2 && strcmp(argv[1], "--threads") == 0;
    const int thread_churn = argc == 2 && strcmp(argv[1], "--thread-churn") == 0;
    const int lifecycle = argc == 2 && (strcmp(argv[1], "--lifecycle") == 0 || strcmp(argv[1], "--after-exec") == 0);
    const int blocked_read = argc == 2 && strcmp(argv[1], "--blocked-read") == 0;
    if (argc > 1 && !busy && !multithread && !thread_churn && !lifecycle && !blocked_read)
        return 2;
    if (!mallopt(M_TRIM_THRESHOLD, -1) || !mallopt(M_MMAP_MAX, 0)) {
        fputs("mallopt setup failed\n", stderr);
        return 1;
    }
    const size_t size = 64 * 1024 * 1024;
    volatile unsigned char *memory = malloc(size);
    volatile unsigned char *live = malloc(4096);
    if (memory == NULL || live == NULL) {
        free((void *)memory);
        free((void *)live);
        return 1;
    }
    for (size_t i = 0; i < size; i++)
        memory[i] = 0x5a;
    for (size_t i = 0; i < 4096; i++)
        live[i] = 0xa5;
    free((void *)memory);
    if (multithread || thread_churn || lifecycle) {
        for (long i = 0; i < 4; i++) {
            pthread_t thread;
            if (pthread_create(&thread, NULL, worker, (void *)(i % 2)) != 0)
                return 1;
        }
        if (thread_churn) {
            pthread_t thread;
            if (pthread_create(&thread, NULL, churn, NULL) != 0)
                return 1;
        }
    }
    printf("ready pid=%ld progress=%p\n", (long)getpid(), (void *)&progress);
    fflush(stdout);
    if (blocked_read) {
        char byte;
        ssize_t size;
        do { size = read(0, &byte, 1); } while (size < 0 && errno == EINTR);
        if (size != 1 || byte != 'X')
            return 3;
        do { size = read(0, &byte, 1); } while (size < 0 && errno == EINTR);
        if (size != 0)
            return 4;
        puts("read-ok");
        return 0;
    }
    if (lifecycle) {
        char command;
        int fd = channel_descriptor();
        char *child_argv[] = { argv[0], "--child-exec-check", NULL };
        char *vfork_argv[] = { argv[0], "--vfork-exec-check", NULL };
        char *exec_argv[] = { argv[0], "--after-exec", NULL };
        while (read(0, &command, 1) == 1) {
            if (command == 'E') {
                execve("/proc/self/exe", exec_argv, environ);
                return 6;
            }
            if (command == 'C') {
                close(fd);
                if (dup2(1, fd) != fd)
                    return 7;
                puts("channel-closed");
                fflush(stdout);
                continue;
            }
            if (command != 'F' && command != 'X' && command != 'R' && command != 'V')
                return 8;
            pid_t child = command == 'V' ? vfork() : fork();
            if (child < 0)
                return 9;
            if (!child) {
                if (command == 'V') {
                    execve("/proc/self/exe", vfork_argv, environ);
                    _exit(16);
                }
                if (command == 'R')
                    _exit(fcntl(fd, F_GETFD) < 0 ? 10 : 0);
                if (fcntl(fd, F_GETFD) >= 0 || errno != EBADF)
                    _exit(11);
                if (command == 'X') {
                    /* Reuse the old fd: the preload constructor must leave it alone. */
                    if (dup2(1, fd) != fd)
                        _exit(12);
                    execve("/proc/self/exe", child_argv, environ);
                    _exit(13);
                }
                _exit(0);
            }
            int status;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status))
                return 14;
            puts("fork-parent-ok");
            fflush(stdout);
        }
        return 0;
    }
    for (;;) {
        for (size_t i = 0; i < 4096; i++) {
            if (live[i] != 0xa5) {
                fputs("live allocation corrupted\n", stderr);
                return 1;
            }
        }
        if (!busy)
            usleep(10000);
    }
}
