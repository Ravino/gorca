// Probe: RLIMIT_RTTIME soft-limit bump after SIGXCPU, as gorca configures it (soft=hard/2).
#define _GNU_SOURCE
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
static volatile sig_atomic_t xcpu = 0;
static void on_xcpu(int s){ (void)s; struct sched_param p={0}; sched_setscheduler(0, SCHED_OTHER|0x40000000, &p); xcpu++; }
static void lim(const char*w){ struct rlimit r; getrlimit(RLIMIT_RTTIME,&r); printf("%s: RLIMIT_RTTIME soft=%llu hard=%llu\n", w,(unsigned long long)r.rlim_cur,(unsigned long long)r.rlim_max); fflush(stdout);}
static double now(){struct timespec t; clock_gettime(CLOCK_MONOTONIC,&t); return t.tv_sec+t.tv_nsec/1e9;}
static int make_rt(void){ char cmd[256]; snprintf(cmd,sizeof cmd,"busctl --system call org.freedesktop.RealtimeKit1 /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 MakeThreadRealtimeWithPID ttu %d %ld 10 >/dev/null", getpid(), syscall(SYS_gettid)); return system(cmd); }
static void spin(const char*w, double max){ int pol=sched_getscheduler(0); printf("%s: policy=%d (2=RR)\n",w,pol); fflush(stdout); int x0=xcpu; double t0=now(); while(now()-t0<max && xcpu==x0){} printf("%s: spun %.0f ms, xcpu=%d policy now=%d\n",w,(now()-t0)*1e3,(int)xcpu,sched_getscheduler(0)); fflush(stdout);}
int main(){
  cpu_set_t s; CPU_ZERO(&s); CPU_SET(3,&s); sched_setaffinity(0,sizeof s,&s);
  struct rlimit r={100000,200000}; if(setrlimit(RLIMIT_RTTIME,&r)){perror("setrlimit");return 1;}
  struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_handler=on_xcpu; sa.sa_flags=SA_RESTART; sigaction(SIGXCPU,&sa,0);
  lim("start");
  if(make_rt()){printf("rtkit refused\n");return 2;}
  spin("spin1",0.5);
  lim("after first SIGXCPU");
  usleep(50000);
  if(make_rt()){printf("rtkit refused second\n");return 2;}
  spin("spin2",0.5);
  lim("after spin2");
  return 0;
}
