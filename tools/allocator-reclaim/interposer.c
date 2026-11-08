/* Test only: preserve allocations but make the public malloc binding distinct. */
#include <stddef.h>
extern void *__libc_malloc(size_t);
void *malloc(size_t size) { return __libc_malloc(size); }
