// Scan-cost model for Arc<SemEntry> of sizes S (104/128/160/192) under glibc malloc.
// Each entry = 16 B Arc header + S payload, individually malloc'd (like Arc::new).
// Modes: seq = visit in allocation order (fresh mirror build); shuf = allocation order shuffled
// (after churn: CoW re-fuse reallocates entries); access = touch role (first line) + masks (last
// line) [hot read], or copy whole entry (fuse CoW).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
static double now(){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1e9+t.tv_nsec;}
static uint64_t rs=88172645463325252ULL; static uint64_t xr(){rs^=rs<<13;rs^=rs>>7;rs^=rs<<17;return rs;}
int main(int argc,char**argv){
  int S=atoi(argv[1]); int N=atoi(argv[2]); int shuf=atoi(argv[3]); int mode=atoi(argv[4]);
  int A=16+S;
  // churn heap a bit so addresses are realistic
  char **p=malloc(sizeof(char*)*N);
  for(int i=0;i<N;i++){p[i]=malloc(A); memset(p[i],i&0xff,A);}
  if(shuf){ // simulate CoW churn: free+realloc random half in random order, then visit in preorder
    for(int r=0;r<3;r++) for(int i=0;i<N;i++){int j=xr()%N; free(p[j]); p[j]=malloc(A); memset(p[j],j&0xff,A);}
  }
  if(shuf==2){ for(int i=N-1;i>0;i--){int j=xr()%(i+1); char*t=p[i];p[i]=p[j];p[j]=t;} }
  // evict: touch 64 MiB
  size_t EV=64<<20; char*ev=malloc(EV);
  char *dst=malloc(A);
  volatile uint64_t acc=0; double best=1e18;
  int reps = N<5000?200:(N<20000?40:10);
  for(int r=0;r<reps;r++){
    if(r%2==0){ for(size_t k=0;k<EV;k+=64) ev[k]++; } // cold on even reps
    double t0=now(); uint64_t a=0;
    if(mode==0){ for(int i=0;i<N;i++){ char*e=p[i]+16; a+=*(uint16_t*)e + *(uint32_t*)(e+S-8);} }
    else { for(int i=0;i<N;i++){ memcpy(dst,p[i],A); a+=dst[S-1];} }
    double t=now()-t0; acc+=a;
    if(r%2==0){ /* cold */ } 
    if(r>=2 && r%2==0 && t<best) best=t; // best cold
  }
  // warm
  double bw=1e18;
  for(int r=0;r<reps;r++){ double t0=now(); uint64_t a=0;
    if(mode==0){ for(int i=0;i<N;i++){ char*e=p[i]+16; a+=*(uint16_t*)e + *(uint32_t*)(e+S-8);} }
    else { for(int i=0;i<N;i++){ memcpy(dst,p[i],A); a+=dst[S-1];} }
    double t=now()-t0; acc+=a; if(t<bw) bw=t; }
  printf("%d %d %d %d cold_ns_per=%.2f warm_ns_per=%.2f cold_total_us=%.1f\n",S,N,shuf,mode,best/N,bw/N,best/1e3);
  return 0;}
