/* SPDX-License-Identifier: MPL-2.0 */
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t received;
static sigset_t usr1;

static void record_signal(int signo)
{
    if (signo != SIGUSR1)
        _exit(1);
    ++received;
}

static void install(void (*handler)(int), int flags)
{
    struct sigaction sa = {.sa_handler = handler, .sa_flags = flags};
    sigemptyset(&sa.sa_mask);
    assert(sigaction(SIGUSR1, &sa, NULL) == 0);
}

static void check_temporary_mask(void)
{
    sigset_t old, temporary, current, pending;
    assert(pthread_sigmask(SIG_BLOCK, &usr1, &old) == 0);
    assert(pthread_sigmask(SIG_SETMASK, NULL, &temporary) == 0);
    sigdelset(&temporary, SIGUSR1);

    /* No signal: normal ppoll return must still restore the old mask. */
    assert(ppoll(NULL, 0, &(struct timespec){0}, &temporary) == 0);
    assert(pthread_sigmask(SIG_SETMASK, NULL, &current) == 0);
    assert(sigismember(&current, SIGUSR1) == 1);

    assert(kill(getpid(), SIGUSR1) == 0);
    assert(sigpending(&pending) == 0 && sigismember(&pending, SIGUSR1) == 1);
    assert(received == 0);
    assert(sigsuspend(&temporary) == -1 && errno == EINTR);
    assert(received == 1);
    assert(pthread_sigmask(SIG_SETMASK, NULL, &current) == 0);
    assert(sigismember(&current, SIGUSR1) == 1);
    assert(pthread_sigmask(SIG_SETMASK, &old, NULL) == 0);
}

static volatile sig_atomic_t notify_fd;

static void notify_parent(int signo)
{
    record_signal(signo);
    if (write(notify_fd, "!", 1) != 1)
        _exit(1);
}

static void wait_for_blocked_read(pid_t child)
{
    char path[64], line[512];
    snprintf(path, sizeof(path), "/proc/%d/stat", child);
    for (;;) {
        FILE *f = fopen(path, "r");
        assert(f && fgets(line, sizeof(line), f));
        assert(fclose(f) == 0);
        char *end = strrchr(line, ')');
        assert(end && end[1] == ' ' && end[2] != 'Z');
        if (end[2] == 'S')
            return;
        sched_yield();
    }
}

static void check_interrupted_read(int flags)
{
    int data[2], events[2], status;
    char byte;
    assert(pipe(data) == 0 && pipe(events) == 0);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        close(events[0]);
        close(data[1]);
        notify_fd = events[1];
        install(notify_parent, flags);
        assert(write(events[1], "R", 1) == 1);
        ssize_t n = read(data[0], &byte, 1);
        if (flags == SA_RESTART)
            assert(n == 1 && byte == 'D');
        else
            assert(n == -1 && errno == EINTR);
        _exit(0);
    }
    close(data[0]);
    close(events[1]);
    assert(read(events[0], &byte, 1) == 1 && byte == 'R');
    wait_for_blocked_read(child);
    assert(kill(child, SIGUSR1) == 0);
    assert(read(events[0], &byte, 1) == 1 && byte == '!');
    if (flags == SA_RESTART)
        assert(write(data[1], "D", 1) == 1);
    assert(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    close(events[0]);
    close(data[1]);
}

enum { ROUNDS = 128 };

static void *syscall_loop(void *arg)
{
    int ack = *(int *)arg;
    pid_t pid = getpid();
    assert(pthread_sigmask(SIG_UNBLOCK, &usr1, NULL) == 0);
    for (int i = 0; i < ROUNDS; ++i) {
        assert(write(ack, "R", 1) == 1);
        while (received == i)
            assert(syscall(SYS_getpid) == pid);
        assert(received == i + 1);
    }
    return NULL;
}

static void check_concurrent_delivery(void)
{
    int ack[2];
    pthread_t thread;
    sigset_t old;
    received = 0;
    assert(pipe(ack) == 0);
    /* Only the syscall thread can receive the process-directed signals. */
    assert(pthread_sigmask(SIG_BLOCK, &usr1, &old) == 0);
    assert(pthread_create(&thread, NULL, syscall_loop, &ack[1]) == 0);
    for (int i = 0; i < ROUNDS; ++i) {
        char byte;
        assert(read(ack[0], &byte, 1) == 1 && byte == 'R');
        if (i % 2 == 0)
            assert(kill(getpid(), SIGUSR1) == 0);
        else
            assert(pthread_kill(thread, SIGUSR1) == 0);
    }
    assert(pthread_join(thread, NULL) == 0);
    assert(received == ROUNDS);
    assert(pthread_sigmask(SIG_SETMASK, &old, NULL) == 0);
    close(ack[0]);
    close(ack[1]);
}

int main(void)
{
    sigemptyset(&usr1);
    sigaddset(&usr1, SIGUSR1);
    assert(pthread_sigmask(SIG_UNBLOCK, &usr1, NULL) == 0);
    install(record_signal, 0);
    check_temporary_mask();
    check_interrupted_read(0);
    check_interrupted_read(SA_RESTART);
    check_concurrent_delivery();
    puts("SIGNAL_RETURN_OK");
    return 0;
}
