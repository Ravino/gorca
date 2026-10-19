#include <stdio.h>
#include <stdlib.h>
#include <malloc.h>
int main(){int s[]={104,120,128,144,160};for(int i=0;i<5;i++){size_t n=16+s[i];void*p=malloc(n);printf("entry %d -> Arc alloc %zu -> usable %zu\n",s[i],n,malloc_usable_size(p));}}
