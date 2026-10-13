// M1 platform probes (P-7 grab settle, P-8 re-injection, P-10 key->KeyEvent latency).
// PRIVATE session only: refuses to run against a bus address under /run/user/ .
// Build: gcc -O2 -o m1probe m1probe.c $(pkg-config --cflags --libs libei-1.0 gio-unix-2.0 xkbcommon) -lm
// Usage: m1probe latency N | rtt N | settle-key N (SETTLE_INS=1 for Insert modifier) | settle-mod N | reinject | p13 | skew N | keymap | press KC...
// env: DBUS_SESSION_BUS_ADDRESS, GTKKEYS=path/to/gtkkeys.py, GORCA_WL_DISPLAY=<private wayland display>, SKEW_LO/SKEW_SPAN (ms)
#define _GNU_SOURCE
#include <gio/gio.h>
#include <gio/gunixfdlist.h>
#include <libei.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <math.h>
#include <xkbcommon/xkbcommon.h>

#define KC_A 30
#define KC_CAPS 58
#define KS_A 0x61
#define KS_CAPS 0xffe5
#define KS_INSERT 0xff63
#define KC_INSERT 110

static uint64_t now_ns(void){ struct timespec t; clock_gettime(CLOCK_MONOTONIC,&t); return t.tv_sec*1000000000ull+t.tv_nsec; }
static GDBusConnection *bus; static struct ei *ei; static struct ei_device *kbd; static char *sess_path;
typedef struct { uint64_t t; gboolean rel; guint32 state, keysym; guint16 keycode; } Ev;
static Ev evs[1<<20]; static int nev;
static int gtk_fd=-1; static GPid gtk_pid; static char gbuf[65536]; static int glen;
typedef struct { uint64_t t; gboolean press; guint32 keyval; } GEv; static GEv gevs[1<<16]; static int ngev; static gboolean gtk_focus;

static void on_key(GDBusConnection*c,const char*s,const char*o,const char*i,const char*n,GVariant*p,gpointer u){
  Ev e; e.t=now_ns(); guint32 uni; gboolean r; guint32 st,ks; guint16 kc;
  g_variant_get(p,"(buuuq)",&r,&st,&ks,&uni,&kc); e.rel=r;e.state=st;e.keysym=ks;e.keycode=kc;
  if(nev<(1<<20)) evs[nev++]=e; }
static void eipump(void){ struct pollfd pf={ei_get_fd(ei),POLLIN,0};
  if(poll(&pf,1,0)>0||1){ ei_dispatch(ei); struct ei_event*ev; while((ev=ei_get_event(ei))) ei_event_unref(ev);} }
static void gtkpump(void){ if(gtk_fd<0) return; struct pollfd pf={gtk_fd,POLLIN,0};
  while(poll(&pf,1,0)>0){ ssize_t n=read(gtk_fd,gbuf+glen,sizeof gbuf-glen-1); if(n<=0) break; glen+=n; gbuf[glen]=0;
    char*nl; char*st=gbuf; while((nl=strchr(st,'\n'))){ *nl=0; unsigned long long t; char k[16]; unsigned v;
      if(sscanf(st,"KEY %llu %15s %u",&t,k,&v)==3 && ngev<(1<<16)){ gevs[ngev].t=t; gevs[ngev].press=!strcmp(k,"press"); gevs[ngev++].keyval=v; }
      else if(!strcmp(st,"FOCUS")) gtk_focus=TRUE; st=nl+1; }
    glen-=st-gbuf; memmove(gbuf,st,glen); } }
static void pump(void){ while(g_main_context_iteration(NULL,FALSE)); eipump(); gtkpump(); }
static void spin_until(uint64_t t){ while(now_ns()<t) pump(); }
static void sleep_ms(int ms){ spin_until(now_ns()+ms*1000000ull); }
static void inject(uint32_t key,gboolean press,uint64_t*t){ ei_device_keyboard_key(kbd,key,press); if(t)*t=now_ns(); ei_device_frame(kbd,ei_now(ei)); ei_dispatch(ei); }
static void die(const char*m,GError*e){ fprintf(stderr,"FATAL %s: %s\n",m,e?e->message:""); exit(2); }
static GVariant* call(const char*dest,const char*path,const char*iface,const char*m,GVariant*p,const char*rt){
  GError*e=NULL; GVariant*r=g_dbus_connection_call_sync(bus,dest,path,iface,m,p,rt?G_VARIANT_TYPE(rt):NULL,0,2000,NULL,&e); if(!r) die(m,e); return r; }
static void set_grabs(GVariant*mods,GVariant*keys){ GVariant*r=call("org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager","org.freedesktop.a11y.KeyboardMonitor","SetKeyGrabs",g_variant_new("(@au@a(uu))",g_variant_ref_sink(mods),g_variant_ref_sink(keys)),NULL); g_variant_unref(r); }
static GVariant* mods_v(int n,...){ GVariantBuilder b; g_variant_builder_init(&b,G_VARIANT_TYPE("au")); va_list ap; va_start(ap,n); for(int i=0;i<n;i++) g_variant_builder_add(&b,"u",va_arg(ap,unsigned)); va_end(ap); return g_variant_builder_end(&b);}
static GVariant* keys_v(guint32 ks){ GVariantBuilder b; g_variant_builder_init(&b,G_VARIANT_TYPE("a(uu)")); if(ks) g_variant_builder_add(&b,"(uu)",ks,0u); return g_variant_builder_end(&b);}
static GVariant *E_MODS, *E_KEYS;
static void ungrab(void){ set_grabs(E_MODS,E_KEYS); }
static gboolean caps_state(void){ GVariant*r=call("org.gnome.Mutter.RemoteDesktop",sess_path,"org.freedesktop.DBus.Properties","Get",g_variant_new("(ss)","org.gnome.Mutter.RemoteDesktop.Session","CapsLockState"),"(v)");
  GVariant*v; g_variant_get(r,"(v)",&v); gboolean b=g_variant_get_boolean(v); g_variant_unref(v); g_variant_unref(r); return b; }

// async SetKeyGrabs with reply timestamp
static uint64_t t_reply;
static void on_reply(GObject*s,GAsyncResult*res,gpointer u){ t_reply=now_ns(); GError*e=NULL; GVariant*r=g_dbus_connection_call_finish(bus,res,&e); if(!r){fprintf(stderr,"async SetKeyGrabs: %s\n",e->message);} else g_variant_unref(r); }
static uint64_t set_grabs_async(GVariant*mods,GVariant*keys){ t_reply=0; uint64_t t=now_ns();
  g_dbus_connection_call(bus,"org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager","org.freedesktop.a11y.KeyboardMonitor","SetKeyGrabs",g_variant_new("(@au@a(uu))",g_variant_ref_sink(mods),g_variant_ref_sink(keys)),NULL,0,2000,NULL,on_reply,NULL);
  return t; }

static int cmpu(const void*a,const void*b){ uint64_t x=*(uint64_t*)a,y=*(uint64_t*)b; return x<y?-1:x>y; }
static void stats(const char*name,uint64_t*v,int n){ if(!n){printf("%s: no samples\n",name);return;} qsort(v,n,8,cmpu);
  printf("%s n=%d min=%.0fus p50=%.0fus p90=%.0fus p99=%.0fus p99.9=%.0fus max=%.0fus\n",name,n,v[0]/1e3,v[n/2]/1e3,v[(int)(n*0.9)]/1e3,v[(int)(n*0.99)]/1e3,v[(int)(n*0.999)>=n?n-1:(int)(n*0.999)]/1e3,v[n-1]/1e3); }

static void setup(void){
  const char*a=g_getenv("DBUS_SESSION_BUS_ADDRESS"); if(!a||strstr(a,"/run/user/")){ fprintf(stderr,"refusing: not a private bus (%s)\n",a?a:"unset"); exit(3);} 
  GError*e=NULL; bus=g_dbus_connection_new_for_address_sync(a,G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT|G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,NULL,NULL,&e); if(!bus) die("connect",e);
  E_MODS=g_variant_ref_sink(mods_v(0)); E_KEYS=g_variant_ref_sink(keys_v(0));
  GVariant*r=call("org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","RequestName",g_variant_new("(su)","org.gnome.Orca.KeyboardMonitor",0u),"(u)");
  guint32 rc; g_variant_get(r,"(u)",&rc); printf("RequestName org.gnome.Orca.KeyboardMonitor -> %u (1=primary owner)\n",rc);
  g_dbus_connection_signal_subscribe(bus,"org.freedesktop.a11y.Manager","org.freedesktop.a11y.KeyboardMonitor","KeyEvent","/org/freedesktop/a11y/Manager",NULL,G_DBUS_SIGNAL_FLAGS_NONE,on_key,NULL,NULL);
  call("org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager","org.freedesktop.a11y.KeyboardMonitor","WatchKeyboard",NULL,NULL);
  r=call("org.gnome.Mutter.RemoteDesktop","/org/gnome/Mutter/RemoteDesktop","org.gnome.Mutter.RemoteDesktop","CreateSession",NULL,"(o)"); g_variant_get(r,"(o)",&sess_path);
  GVariantBuilder ob; g_variant_builder_init(&ob,G_VARIANT_TYPE("a{sv}")); g_variant_builder_add(&ob,"{sv}","device-types",g_variant_new_uint32(1));
  GUnixFDList*out=NULL; r=g_dbus_connection_call_with_unix_fd_list_sync(bus,"org.gnome.Mutter.RemoteDesktop",sess_path,"org.gnome.Mutter.RemoteDesktop.Session","ConnectToEIS",g_variant_new("(a{sv})",&ob),G_VARIANT_TYPE("(h)"),0,2000,NULL,&out,NULL,&e); if(!r) die("ConnectToEIS",e);
  gint h; g_variant_get(r,"(h)",&h); int fd=g_unix_fd_list_get(out,h,&e); if(fd<0) die("fd",e);
  call("org.gnome.Mutter.RemoteDesktop",sess_path,"org.gnome.Mutter.RemoteDesktop.Session","Start",NULL,NULL);
  ei=ei_new_sender(NULL); ei_configure_name(ei,"m1probe"); if(ei_setup_backend_fd(ei,fd)) die("ei fd",NULL);
  uint64_t dl=now_ns()+3000000000ull; gboolean ready=FALSE;
  while(!ready&&now_ns()<dl){ ei_dispatch(ei); struct ei_event*ev; while((ev=ei_get_event(ei))){ switch(ei_event_get_type(ev)){
    case EI_EVENT_SEAT_ADDED: ei_seat_bind_capabilities(ei_event_get_seat(ev),EI_DEVICE_CAP_KEYBOARD,NULL); break;
    case EI_EVENT_DEVICE_ADDED: if(ei_device_has_capability(ei_event_get_device(ev),EI_DEVICE_CAP_KEYBOARD)) kbd=ei_device_ref(ei_event_get_device(ev)); break;
    case EI_EVENT_DEVICE_RESUMED: if(kbd) ready=TRUE; break; default: break; } ei_event_unref(ev);} g_main_context_iteration(NULL,FALSE); usleep(200);} 
  if(!ready) die("ei not ready",NULL); ei_device_start_emulating(kbd,1); ei_dispatch(ei);
  printf("EIS keyboard device ready (Mutter RemoteDesktop direct, no prompt)\n"); }

static void leave_overview(void){ GError*e=NULL; GVariant*r=g_dbus_connection_call_sync(bus,"org.gnome.Shell","/org/gnome/Shell","org.freedesktop.DBus.Properties","Get",g_variant_new("(ss)","org.gnome.Shell","OverviewActive"),G_VARIANT_TYPE("(v)"),0,2000,NULL,&e); if(!r) return;
  GVariant*v; g_variant_get(r,"(v)",&v); if(g_variant_get_boolean(v)){ uint64_t t; inject(125,TRUE,&t); sleep_ms(20); inject(125,FALSE,&t); sleep_ms(500); printf("left Overview with Super (headless shell starts in Overview, which blocks key focus)\n"); } }
static void start_gtk(void){ leave_overview(); const char*p=g_getenv("GTKKEYS"); if(!p) return; char*argv[]={"/usr/bin/python3",(char*)p,NULL}; char**env=g_get_environ(); env=g_environ_setenv(env,"GDK_BACKEND","wayland",TRUE); env=g_environ_setenv(env,"NO_AT_BRIDGE","1",TRUE); env=g_environ_setenv(env,"GTK_A11Y","none",TRUE); env=g_environ_setenv(env,"WAYLAND_DISPLAY",g_getenv("GORCA_WL_DISPLAY")?g_getenv("GORCA_WL_DISPLAY"):"gorca-test-0",TRUE);
  gint out; GError*e=NULL; if(!g_spawn_async_with_pipes(NULL,argv,env,G_SPAWN_DO_NOT_REAP_CHILD,NULL,NULL,&gtk_pid,NULL,&out,NULL,&e)) die("spawn gtk",e); gtk_fd=out; 
  uint64_t dl=now_ns()+8000000000ull; while(!gtk_focus&&now_ns()<dl) { pump(); usleep(500);} printf("gtk client focus=%d\n",gtk_focus); 
  if(!gtk_focus){ // nudge focus with a harmless key in the private session
    uint64_t t; inject(1,TRUE,&t); sleep_ms(5); inject(1,FALSE,&t); sleep_ms(800); printf("gtk focus after Esc nudge: %d\n",gtk_focus); } }
static int count_gtk(uint32_t kv,int since){ int c=0; for(int i=since;i<ngev;i++) if(gevs[i].press&&gevs[i].keyval==kv) c++; return c; }
static int gtk_saw(uint32_t keyval,int since){ for(int i=since;i<ngev;i++) if(gevs[i].press&&(gevs[i].keyval==keyval||gevs[i].keyval==(keyval&~0x20u))) return 1; return 0; }

static void do_latency(int n){
  ungrab(); uint64_t *lw=malloc(8*n); int nw=0; srand(7);
  for(int i=0;i<n;i++){ int base=nev; uint64_t t; inject(KC_A,TRUE,&t); uint64_t dl=t+50000000ull; 
    while(now_ns()<dl){ pump(); if(nev>base) break; } if(nev>base&&!evs[base].rel) lw[nw++]=evs[base].t-t;
    sleep_ms(2); inject(KC_A,FALSE,&t); sleep_ms(8+rand()%15); }
  stats("watch-only press->KeyEvent (EIS inject incl.)",lw,nw);
  // grabbed
  set_grabs(E_MODS,keys_v(KS_A)); uint64_t *lg=malloc(8*n); int ng=0;
  for(int i=0;i<n;i++){ int base=nev; uint64_t t; inject(KC_A,TRUE,&t); uint64_t dl=t+50000000ull;
    while(now_ns()<dl){ pump(); if(nev>base) break; } if(nev>base&&!evs[base].rel) lg[ng++]=evs[base].t-t;
    sleep_ms(2); inject(KC_A,FALSE,&t); sleep_ms(8+rand()%15); }
  stats("grabbed-key press->KeyEvent",lg,ng); ungrab(); }

static void do_settle_mod(int n){
  static const int deltas_full[]={0,50,100,200,300,500,750,1000,1500,2000,3000,5000}; static const int deltas_fast[]={0,25,50,100}; const int*deltas=g_getenv("SETTLE_FAST")?deltas_fast:deltas_full; int nd=g_getenv("SETTLE_FAST")?4:12;
  printf("# P-7 modifier grab (Caps_Lock as grabbed modifier; 'applied' = injected Caps press did NOT toggle CapsLockState)\n");
  for(int dir=g_getenv("SETTLE_DIR")?atoi(g_getenv("SETTLE_DIR")):0;dir<(g_getenv("SETTLE_DIR")?atoi(g_getenv("SETTLE_DIR"))+1:2);dir++){ printf("## %s\n# delta_us(from call send) trials applied reply_before_inject_and_applied reply_before_inject_but_NOT_applied\n",dir?"REMOVE modifier grab":"ADD modifier grab");
    for(int d=0; d<nd; d++){ int applied=0,rb_ok=0,rb_bad=0;
      for(int i=0;i<n;i++){
        ungrab(); if(dir) set_grabs(mods_v(1,KS_CAPS),E_KEYS); sleep_ms(120);
        if(caps_state()){ uint64_t t; inject(KC_CAPS,TRUE,&t); sleep_ms(5); inject(KC_CAPS,FALSE,&t); sleep_ms(150); }
        sleep_ms(560);
        gboolean before=caps_state();
        uint64_t tc=dir? set_grabs_async(E_MODS,E_KEYS): set_grabs_async(mods_v(1,KS_CAPS),E_KEYS);
        spin_until(tc+deltas[d]*1000ull); uint64_t t; inject(KC_CAPS,TRUE,&t); gboolean rb=t_reply&&t_reply<t; sleep_ms(5); inject(KC_CAPS,FALSE,&t); sleep_ms(80);
        gboolean toggled=caps_state()!=before; 
        gboolean ap = dir? toggled : !toggled; // add: applied if swallowed; remove: applied if key reached xkb
        applied+=ap; if(rb){ if(ap) rb_ok++; else rb_bad++; }
        // cleanup: ungrab and fix caps
        ungrab(); sleep_ms(120); if(caps_state()){ inject(KC_CAPS,TRUE,&t); sleep_ms(5); inject(KC_CAPS,FALSE,&t); sleep_ms(150);} }
      printf("%6d %4d %4d %4d %4d\n",deltas[d],n,applied,rb_ok,rb_bad); fflush(stdout); } } }

static void do_settle_key(int n){
  static const int deltas[]={0,100,300,1000,3000};
  gboolean ins=g_getenv("SETTLE_INS")!=NULL; uint32_t kc=ins?KC_INSERT:KC_A, kv=ins?KS_INSERT:KS_A;
  GVariant*(*addm)(void)=NULL; (void)addm;
  printf("# P-7 %s grab, detected by the GTK client (applied = client did NOT see the press)\n",ins?"MODIFIER (Insert)":"KEYSTROKE ('a' mask 0)");
  for(int dir=0;dir<2;dir++){ printf("## %s\n# delta_us trials applied reply_before_inject_and_applied reply_before_inject_but_NOT_applied\n",dir?"REMOVE":"ADD");
    for(unsigned d=0; d<sizeof deltas/sizeof*deltas; d++){ int applied=0,rb_ok=0,rb_bad=0;
      for(int i=0;i<n;i++){ ungrab(); if(dir) set_grabs(ins?mods_v(1,KS_INSERT):E_MODS,ins?E_KEYS:keys_v(KS_A)); sleep_ms(ins?620:30); int g0=ngev;
        uint64_t tc= dir? set_grabs_async(E_MODS,E_KEYS) : set_grabs_async(ins?mods_v(1,KS_INSERT):E_MODS,ins?E_KEYS:keys_v(KS_A));
        spin_until(tc+deltas[d]*1000ull); uint64_t t; inject(kc,TRUE,&t); gboolean rb=t_reply&&t_reply<t; sleep_ms(3); inject(kc,FALSE,&t); sleep_ms(60);
        int saw=count_gtk(kv,g0)>0; gboolean ap= dir? saw : !saw; applied+=ap; if(rb){ if(ap) rb_ok++; else rb_bad++; } }
      printf("%6d %4d %4d %4d %4d\n",deltas[d],n,applied,rb_ok,rb_bad); fflush(stdout);} }
  ungrab(); }

static void do_reinject(void){
  printf("# P-8 re-injection semantics (GTK client sees a?) gtk_events_so_far=%d\n",ngev); int g0;
  ungrab(); sleep_ms(50); g0=ngev; uint64_t t; inject(KC_A,TRUE,&t); sleep_ms(3); inject(KC_A,FALSE,&t); sleep_ms(300); printf("gtk events now %d\n",ngev); printf("no grab, inject a            -> app sees a: %d (expect 1)\n",gtk_saw(0x61,g0));
  set_grabs(E_MODS,keys_v(KS_A)); sleep_ms(30); g0=ngev; inject(KC_A,TRUE,&t); sleep_ms(3); inject(KC_A,FALSE,&t); sleep_ms(80); printf("grab a, inject a             -> app sees a: %d (expect 0: injected key is grabbed again)\n",gtk_saw(0x61,g0));
  int e0=nev; g0=ngev; uint64_t tu; uint64_t t0=now_ns(); set_grabs(E_MODS,E_KEYS); uint64_t t1=now_ns(); inject(KC_A,TRUE,&tu); sleep_ms(3); inject(KC_A,FALSE,&t); sleep_ms(80);
  printf("ungrab (sync, %.0fus) then inject a -> app sees a: %d (expect 1)\n",(t1-t0)/1e3,gtk_saw(0x61,g0)); (void)e0;
  // modifier chord: Caps grabbed, press Caps, then 'a' while held
  set_grabs(mods_v(1,KS_CAPS),E_KEYS); sleep_ms(30); g0=ngev; inject(KC_CAPS,TRUE,&t); sleep_ms(5); inject(KC_A,TRUE,&t); sleep_ms(3); inject(KC_A,FALSE,&t); sleep_ms(5); inject(KC_CAPS,FALSE,&t); sleep_ms(80);
  printf("Caps grabbed, Caps down + a  -> app sees a: %d (expect 0: chord keys swallowed)\n",gtk_saw(0x61,g0));
  g0=ngev; sleep_ms(600); inject(KC_A,TRUE,&t); sleep_ms(3); inject(KC_A,FALSE,&t); sleep_ms(80); printf("after Caps released, inject a-> app sees a: %d (expect 1: next key is not grabbed)\n",gtk_saw(0x61,g0));
  ungrab(); }


static int count_ev(uint32_t ks,int since){ int c=0; for(int i=since;i<nev;i++) if(!evs[i].rel&&evs[i].keysym==ks) c++; return c; }
static void hold_test(const char*name,uint32_t kc,uint32_t ks,GVariant*mods){
  ungrab(); if(mods) set_grabs(mods,E_KEYS); sleep_ms(700); int g0=ngev,e0=nev; uint64_t t;
  inject(kc,TRUE,&t); sleep_ms(1500); inject(kc,FALSE,&t); sleep_ms(150);
  printf("%-34s KeyEvent presses to gorca: %3d | GTK client presses (0xff63/0x%x): %3d\n",name,count_ev(ks,e0),ks,count_gtk(ks,g0)); }
static void do_p13(void){
  printf("# P-13 held key: compositor repeat (delay 500ms/interval 33ms expected in 1.5 s hold => ~30 repeats)\n");
  hold_test("Insert, NO grab",KC_INSERT,KS_INSERT,NULL);
  hold_test("Insert grabbed as modifier",KC_INSERT,KS_INSERT,mods_v(1,KS_INSERT));
  hold_test("'a', NO grab",KC_A,KS_A,NULL);
  ungrab(); set_grabs(mods_v(1,KS_INSERT),E_KEYS); sleep_ms(700); int g0=ngev,e0=nev; uint64_t t;
  inject(KC_INSERT,TRUE,&t); sleep_ms(30); inject(KC_A,TRUE,&t); sleep_ms(1500); inject(KC_A,FALSE,&t); sleep_ms(20); inject(KC_INSERT,FALSE,&t); sleep_ms(150);
  printf("%-34s KeyEvent a presses: %3d | GTK a presses: %3d | GTK Insert presses: %d\n","Insert held + 'a' held (chord)",count_ev(KS_A,e0),count_gtk(0x61,g0),count_gtk(KS_INSERT,g0));
  hold_test("Caps grabbed as modifier",KC_CAPS,KS_CAPS,mods_v(1,KS_CAPS)); ungrab(); if(caps_state()){ inject(KC_CAPS,TRUE,&t); sleep_ms(5); inject(KC_CAPS,FALSE,&t);} }

static void do_skew(int n){
  // gap = release1 -> press2 of grabbed modifier Insert; "passed" = GTK client received the second press. Mutter measures the gap with
  // its own event timestamps (EIS path: g_get_monotonic_time() on the compositor main thread); gorca measures KeyEvent receipt times.
  printf("# P-14 double-tap cutoff scan; columns: inject_gap_us recv_gap_us passed\n"); ungrab(); set_grabs(mods_v(1,KS_INSERT),E_KEYS); srand(11);
  for(int i=0;i<n;i++){ sleep_ms(900); uint64_t t,tr,tp; int base=nev,g0=ngev; inject(KC_INSERT,TRUE,&t); uint64_t tp1=t; sleep_ms(3); inject(KC_INSERT,FALSE,&tr);
    uint64_t g=(atoi(g_getenv("SKEW_LO"))*1000ull+rand()%(atoi(g_getenv("SKEW_SPAN"))*1000))*1000ull;
    spin_until(tr+g); inject(KC_INSERT,TRUE,&tp); sleep_ms(8); uint64_t tr2; inject(KC_INSERT,FALSE,&tr2); sleep_ms(60); if(g_getenv("SKEW_DEBUG")) printf("inj: P1@0 R1@%.1f P2@%.1f R2@%.1f\n",(tr-tp1)/1e6,(tp-tp1)/1e6,(tr2-tp1)/1e6);
    int ir=-1,ip=-1; for(int k=base;k<nev;k++){ if(evs[k].keysym==KS_INSERT){ if(evs[k].rel&&ir<0) ir=k; else if(!evs[k].rel&&ir>=0&&ip<0) ip=k; } }
    uint64_t rg= (ir>=0&&ip>=0)? evs[ip].t-evs[ir].t:0; printf("%llu %llu %d",(unsigned long long)((tp-tr)/1000),(unsigned long long)(rg/1000),count_gtk(KS_INSERT,g0)>0); if(g_getenv("SKEW_DEBUG")){ for(int k=g0;k<ngev;k++) printf(" [%s %u @%.1fms]",gevs[k].press?"P":"R",gevs[k].keyval,((double)gevs[k].t-(double)tp1)/1e6); } printf("\n"); fflush(stdout); }
  ungrab(); }

static void switch_layout(void){ uint64_t t; inject(125,TRUE,&t); sleep_ms(20); inject(57,TRUE,&t); sleep_ms(10); inject(57,FALSE,&t); sleep_ms(20); inject(125,FALSE,&t); sleep_ms(400); }
static int run_keymap_pass(const char*label,struct xkb_keymap*km,int group,gboolean numlock,gboolean shift){
  struct xkb_state*st=xkb_state_new(km); ungrab(); int mism=0,tot=0,shown=0; uint64_t t;
  for(int kc=1;kc<=247;kc++){ if(kc==125||kc==126||kc==58||kc==69||kc==42||kc==54||kc==70||(kc>=29&&kc<=29)||kc==97||kc==56||kc==100||kc==119||kc==99||kc==116) continue; // skip modifiers/locks/power keys
    int base=nev; if(shift) { inject(42,TRUE,&t); sleep_ms(2);} inject(kc,TRUE,&t); sleep_ms(2); inject(kc,FALSE,&t); if(shift){ sleep_ms(1); inject(42,FALSE,&t);} 
    uint64_t dl=now_ns()+30000000ull; while(now_ns()<dl){ pump(); } 
    for(int k=base;k<nev;k++){ if(evs[k].rel|| evs[k].keycode!=kc+8) continue; tot++;
      xkb_mod_mask_t mods=evs[k].state&0xff; xkb_state_update_mask(st,mods&~(2|16),0,mods&(2|16),0,0,group);
      xkb_keysym_t mine=xkb_state_key_get_one_sym(st,kc+8); if(kc==30||kc==79){ char nb[64]; xkb_keysym_get_name(evs[k].keysym,nb,64); printf("   sample evdev %d: mutter keysym=%s state=0x%x\n",kc,nb,evs[k].state);} 
      if(mine!=evs[k].keysym){ mism++; if(shown++<6){ char a[64],b[64]; xkb_keysym_get_name(mine,a,64); xkb_keysym_get_name(evs[k].keysym,b,64); printf("   MISMATCH evdev %d (xkb %d): gorca-xkb=%s mutter=%s state=0x%x\n",kc,kc+8,a,b,evs[k].state);} } break; } }
  printf("%-44s keys compared=%3d mismatches=%d\n",label,tot,mism); xkb_state_unref(st); return mism; }
static void do_keymap(void){
  struct xkb_context*ctx=xkb_context_new(XKB_CONTEXT_NO_FLAGS);
  const char*layout=g_getenv("KM_LAYOUT")?g_getenv("KM_LAYOUT"):"us,ru"; const char*model=g_getenv("KM_MODEL")?g_getenv("KM_MODEL"):"pc105+inet";
  struct xkb_rule_names rn={"evdev",model,layout,"",""}; struct xkb_keymap*km=xkb_keymap_new_from_names(ctx,&rn,0);
  if(!km){ printf("xkb compile failed\n"); return; }
  printf("# P-15 RMLVO evdev/%s/%s/'' compiled with xkbcommon %s\n",model,layout,"1.7.0");
  run_keymap_pass("group0 (us), plain",km,0,FALSE,FALSE);
  run_keymap_pass("group0 (us), Shift held",km,0,FALSE,TRUE);
  uint64_t t; inject(69,TRUE,&t); sleep_ms(5); inject(69,FALSE,&t); sleep_ms(100);
  run_keymap_pass("group0, NumLock ON",km,0,TRUE,FALSE);
  inject(69,TRUE,&t); sleep_ms(5); inject(69,FALSE,&t); sleep_ms(100);
  switch_layout(); 
  run_keymap_pass("group1 (ru), plain (assuming switched)",km,1,FALSE,FALSE);
  run_keymap_pass("group1 (ru), Shift held",km,1,FALSE,TRUE);
  run_keymap_pass("ru active, gorca ASSUMES group 0",km,0,FALSE,FALSE);
  switch_layout(); }

static void do_settle_neg(int n){
  static const int deltas[]={0,100,200,500,1000,2000,4000};
  printf("# P-7 control: inject Caps press FIRST, SetKeyGrabs([Caps]) call delta us later. swallowed = grab already effective for that key\n# delta_us trials swallowed\n");
  for(unsigned d=0;d<sizeof deltas/sizeof*deltas;d++){ int sw=0;
    for(int i=0;i<n;i++){ ungrab(); sleep_ms(120); if(caps_state()){ uint64_t t; inject(KC_CAPS,TRUE,&t); sleep_ms(5); inject(KC_CAPS,FALSE,&t); sleep_ms(150);} sleep_ms(560);
      gboolean before=caps_state(); uint64_t t,ti; inject(KC_CAPS,TRUE,&ti); spin_until(ti+deltas[d]*1000ull); set_grabs_async(mods_v(1,KS_CAPS),E_KEYS); sleep_ms(30); inject(KC_CAPS,FALSE,&t); sleep_ms(80);
      sw+= caps_state()==before; }
    printf("%6d %4d %4d\n",deltas[d],n,sw); fflush(stdout);} ungrab(); }

static void do_sanity(void){ uint64_t t; ungrab(); sleep_ms(100);
  for(int i=0;i<3;i++){ int b=nev; printf("caps before=%d\n",caps_state()); inject(KC_CAPS,TRUE,&t); sleep_ms(30); printf("  after press=%d\n",caps_state()); inject(KC_CAPS,FALSE,&t); sleep_ms(30); printf("  after release=%d\n",caps_state());
    for(int k=b;k<nev;k++) printf("  KeyEvent rel=%d state=0x%x keysym=0x%x kc=%u\n",evs[k].rel,evs[k].state,evs[k].keysym,evs[k].keycode); sleep_ms(700);} }

static void do_rtt(int n){ uint64_t *v=malloc(8*n); for(int i=0;i<n;i++){ uint64_t t0=now_ns(); set_grabs(mods_v(1,i&1?KS_INSERT:KS_CAPS),keys_v(i&1?0:KS_A)); v[i]=now_ns()-t0; sleep_ms(2);} stats("SetKeyGrabs reply RTT",v,n); ungrab(); }

static void do_press(int argc,char**argv){ uint64_t t; for(int i=2;i<argc;i++){ int kc=atoi(argv[i]); inject(kc,TRUE,&t); sleep_ms(20); inject(kc,FALSE,&t); sleep_ms(300);} }

int main(int argc,char**argv){ if(argc<2){fprintf(stderr,"usage\n");return 1;} setup();
  if(!strcmp(argv[1],"latency")) do_latency(argc>2?atoi(argv[2]):1000);
  else if(!strcmp(argv[1],"settle-mod")) do_settle_mod(argc>2?atoi(argv[2]):50);
  else if(!strcmp(argv[1],"settle-neg")) do_settle_neg(argc>2?atoi(argv[2]):20);
  else if(!strcmp(argv[1],"rtt")) do_rtt(argc>2?atoi(argv[2]):1000);
  else if(!strcmp(argv[1],"press")) do_press(argc,argv);
  else if(!strcmp(argv[1],"sanity")) do_sanity();
    else if(!strcmp(argv[1],"keymap")) do_keymap();
  else { start_gtk(); if(!strcmp(argv[1],"p13")) do_p13(); else if(!strcmp(argv[1],"skew")) do_skew(argc>2?atoi(argv[2]):300); else if(!strcmp(argv[1],"settle-key")) do_settle_key(argc>2?atoi(argv[2]):50); else if(!strcmp(argv[1],"reinject")) do_reinject(); }
  ungrab(); call("org.gnome.Mutter.RemoteDesktop",sess_path,"org.gnome.Mutter.RemoteDesktop.Session","Stop",NULL,NULL); if(gtk_fd>=0) kill(gtk_pid,SIGTERM); return 0; }
