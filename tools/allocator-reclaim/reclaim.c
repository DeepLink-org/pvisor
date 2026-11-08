#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define CHANNEL "PVISOR_RECLAIM_CHANNEL"
#define MAGIC UINT32_C(0x50565231)
static int channel_fd = -1;
static ino_t channel_inode;

static int valid_channel(void)
{
    struct stat st;
    return channel_fd >= 0 && !fstat(channel_fd, &st) &&
        st.st_ino == channel_inode && S_ISSOCK(st.st_mode);
}

static void fork_child(void)
{
    /* No allocation, thread creation, or inherited lock acquisition after fork. */
    if (valid_channel())
        close(channel_fd);
    channel_fd = -1;
}

static uint32_t load32(const unsigned char *p)
{
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void store32(unsigned char *p, uint32_t v)
{
    for (int i = 0; i < 4; i++)
        p[i] = (unsigned char)(v >> (8 * i));
}

static void *worker(void *unused)
{
    (void)unused;
    void *allocate = dlsym(RTLD_DEFAULT, "malloc");
    void *glibc_allocate = dlsym(RTLD_DEFAULT, "__libc_malloc");
    int (*trim)(size_t) = (int (*)(size_t))dlsym(RTLD_DEFAULT, "malloc_trim");
    const int supported = allocate && allocate == glibc_allocate && trim &&
        dlsym(RTLD_DEFAULT, "gnu_get_libc_version");
    for (;;) {
        unsigned char request[16], response[32] = {0};
        ssize_t size;
        do { size = recv(channel_fd, request, sizeof(request), MSG_TRUNC); }
        while (size < 0 && errno == EINTR);
        if (!valid_channel() || size != sizeof(request) || load32(request) != MAGIC || load32(request + 4) > 1)
            break;
        int result = supported && load32(request + 4) ? trim(0) : 0;
        store32(response, MAGIC);
        store32(response + 4, supported ? 0 : 1);
        store32(response + 8, (uint32_t)getpid());
        store32(response + 12, (uint32_t)syscall(SYS_gettid));
        for (int i = 0; i < 8; i++)
            response[16 + i] = request[8 + i];
        store32(response + 24, (uint32_t)result);
        store32(response + 28, supported ? 1 : 0);
        if (!valid_channel())
            break;
        do { size = send(channel_fd, response, sizeof(response), MSG_NOSIGNAL); }
        while (size < 0 && errno == EINTR);
        if (size != sizeof(response))
            break;
    }
    /* Keep the fd reserved until exec/exit so fork_child cannot close a reused fd. */
    return NULL;
}

__attribute__((constructor)) static void initialize(void)
{
    const char *value = getenv(CHANNEL);
    int fd, type;
    unsigned long long inode;
    long owner;
    char trailing;
    struct stat st;
    socklen_t length = sizeof(type);
    if (!value || sscanf(value, "%d:%llu:%ld%c", &fd, &inode, &owner, &trailing) != 3 ||
        fd < 3 || owner < 0 || fstat(fd, &st) || st.st_ino != inode || !S_ISSOCK(st.st_mode) ||
        getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &length) || type != SOCK_SEQPACKET)
        return;
    if (owner != 0 && owner != (long)getpid()) {
        close(fd);
        return;
    }
    char binding[96];
    if (snprintf(binding, sizeof(binding), "%d:%llu:%ld", fd, inode, (long)getpid()) >= (int)sizeof(binding) ||
        setenv(CHANNEL, binding, 1))
        return;
    channel_fd = fd;
    channel_inode = st.st_ino;
    if (pthread_atfork(NULL, NULL, fork_child))
        return;
    pthread_attr_t attributes;
    if (pthread_attr_init(&attributes))
        return;
    if (!pthread_attr_setdetachstate(&attributes, PTHREAD_CREATE_DETACHED) &&
        !pthread_attr_setstacksize(&attributes, 256 * 1024)) {
        sigset_t blocked, previous;
        sigfillset(&blocked);
        if (!pthread_sigmask(SIG_SETMASK, &blocked, &previous)) {
            pthread_t thread;
            (void)pthread_create(&thread, &attributes, worker, NULL);
            (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
        }
    }
    pthread_attr_destroy(&attributes);
}
