#include <stdint.h>
#include <stddef.h>
#include <sys/mman.h>
extern unsigned char _binary_payload_start[];
const size_t payload_size=64u*1024u*1024u;
void *payload_addr(void) { return _binary_payload_start; }
size_t payload_length(void) { return payload_size; }
int payload_mutate(uint64_t instance, unsigned percent) {
 if(percent!=25 && percent!=100) return -1;
 size_t pages=(payload_size/4096)*percent/100;
 if(mprotect(_binary_payload_start,pages*4096,PROT_READ|PROT_WRITE)) return -2;
 for(size_t p=0;p<pages;p++) {
  uint64_t x=20261006ull ^ ((instance|(1ull<<32))*0xd1b54a32d192ed03ull) ^ (p*0x9e3779b97f4a7c15ull);
  uint64_t *out=(uint64_t *)(_binary_payload_start+p*4096);
  for(size_t w=0;w<512;w++){x=x*6364136223846793005ull+1442695040888963407ull;out[w]=x;}
 }
 return 0;
}
