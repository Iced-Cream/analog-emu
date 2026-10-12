/* memset/memcpy for the freestanding build (gcc emits calls to them). */
#include <stddef.h>
void* memset(void* d, int c, size_t n) { unsigned char* p = d; while (n--) *p++ = (unsigned char)c; return d; }
void* memcpy(void* d, const void* s, size_t n) { unsigned char* p = d; const unsigned char* q = s; while (n--) *p++ = *q++; return d; }
