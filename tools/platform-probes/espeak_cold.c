#include <espeak-ng/speak_lib.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1e3+t.tv_nsec/1e6;}
static double tfirst=-1; static int cb(short*w,int n,espeak_EVENT*e){ if(w&&n>0&&tfirst<0)tfirst=now(); return 0;}
int main(){ double a=now(); int sr=espeak_Initialize(AUDIO_OUTPUT_SYNCHRONOUS,0,NULL,espeakINITIALIZE_DONT_EXIT); espeak_SetSynthCallback(cb);
 double b=now(); espeak_SetVoiceByName("en-us"); double c=now();
 espeak_Synth("Hello",6,0,POS_CHARACTER,0,espeakCHARS_AUTO,NULL,NULL); espeak_Synchronize();
 printf("cold: init %.2f ms, setvoice %.2f ms, first synth first-audio %.2f ms\n",b-a,c-b,tfirst-c);
 tfirst=-1; double d=now(); espeak_Synth("Hello",6,0,POS_CHARACTER,0,espeakCHARS_AUTO,NULL,NULL); espeak_Synchronize(); printf("warm second: %.3f ms\n",tfirst-d);
 /* voice switch cost */
 double e=now(); espeak_SetVoiceByName("ru"); double f=now(); printf("switch voice to ru: %.2f ms\n",f-e);
 tfirst=-1; double g=now(); espeak_Synth("Привет",13,0,POS_CHARACTER,0,espeakCHARS_AUTO,NULL,NULL); espeak_Synchronize(); printf("ru first synth first-audio %.2f ms\n",tfirst-g);
 return 0;}
