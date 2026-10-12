#include <espeak-ng/speak_lib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1e3+t.tv_nsec/1e6;}
static double t0, tfirst; static long nsamp; static int ncb;
static int cb(short *w,int n,espeak_EVENT*e){ if(w&&n>0){ if(tfirst<0) tfirst=now(); nsamp+=n;} ncb++; return 0;}
int main(int argc,char**argv){
  double ti=now();
  int sr=espeak_Initialize(AUDIO_OUTPUT_SYNCHRONOUS,0,NULL,espeakINITIALIZE_DONT_EXIT);
  printf("init: %.2f ms, samplerate %d\n",now()-ti,sr);
  espeak_SetSynthCallback(cb);
  espeak_SetVoiceByName("en-us");
  const char* texts[]={"a","hello world","The quick brown fox jumps over the lazy dog, and then keeps running for a while.", NULL};
  int rates[]={175,350,500};
  for(int r=0;r<3;r++){
   espeak_SetParameter(espeakRATE,rates[r],0);
   for(int i=0;texts[i];i++){
    double min=1e9,max=0,sum=0,tot=0; int N=30;
    for(int k=0;k<N;k++){
      tfirst=-1;nsamp=0;ncb=0; t0=now();
      espeak_Synth(texts[i],strlen(texts[i])+1,0,POS_CHARACTER,0,espeakCHARS_AUTO,NULL,NULL);
      espeak_Synchronize();
      double te=now(); double f=tfirst-t0; tot=te-t0;
      if(f<min)min=f; if(f>max)max=f; sum+=f;
    }
    printf("rate %3d text(%zu chars): first-audio-callback min %.2f avg %.2f max %.2f ms; total synth %.2f ms for %.0f ms audio\n",
      rates[r],strlen(texts[i]),min,sum/N,max,tot,nsamp*1000.0/sr);
   }
  }
  /* cancel latency: start long synth with callback returning 1 to abort */
  return 0;
}
