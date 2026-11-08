/* Static executable used to verify that ignored LD_PRELOAD never triggers injection. */
#include <unistd.h>
int main(void)
{
    if (write(1, "ready\n", 6) != 6)
        return 1;
    for (;;)
        pause();
}
