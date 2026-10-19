// ei_pointer.c BUS X Y : direct RemoteDesktop session (no ScreenCast, no portal) -> ConnectToEIS -> libei sender,
// bind pointer+button+abs, print the abs regions, move to X,Y (logical px) and left-click. PRIVATE shell only.
// build: gcc -O2 ei_pointer.c -o ei_pointer $(pkg-config --cflags --libs libei-1.0 gio-unix-2.0)
#include <libei.h>
#include <gio/gio.h>
#include <gio/gunixfdlist.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <linux/input-event-codes.h>
static void die(const char*m,GError*e){fprintf(stderr,"%s: %s\n",m,e?e->message:"");exit(1);}
int main(int argc,char**argv){
  GError*e=NULL; GDBusConnection*bus=g_dbus_connection_new_for_address_sync(argv[1],G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT|G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,NULL,NULL,&e); if(!bus)die("bus",e);
  GVariant*r=g_dbus_connection_call_sync(bus,"org.gnome.Mutter.RemoteDesktop","/org/gnome/Mutter/RemoteDesktop","org.gnome.Mutter.RemoteDesktop","CreateSession",NULL,G_VARIANT_TYPE("(o)"),0,2000,NULL,&e); if(!r)die("CreateSession",e);
  const char*sp; g_variant_get(r,"(&o)",&sp); char*s=g_strdup(sp);
  GVariantBuilder ob; g_variant_builder_init(&ob,G_VARIANT_TYPE("a{sv}")); g_variant_builder_add(&ob,"{sv}","device-types",g_variant_new_uint32(3)); /* kbd|pointer */
  GUnixFDList*out=NULL; r=g_dbus_connection_call_with_unix_fd_list_sync(bus,"org.gnome.Mutter.RemoteDesktop",s,"org.gnome.Mutter.RemoteDesktop.Session","ConnectToEIS",g_variant_new("(a{sv})",&ob),G_VARIANT_TYPE("(h)"),0,2000,NULL,&out,NULL,&e); if(!r)die("ConnectToEIS",e);
  gint h; g_variant_get(r,"(h)",&h); int fd=g_unix_fd_list_get(out,h,&e);
  r=g_dbus_connection_call_sync(bus,"org.gnome.Mutter.RemoteDesktop",s,"org.gnome.Mutter.RemoteDesktop.Session","Start",NULL,NULL,0,2000,NULL,&e); if(!r)die("Start",e);
  struct ei*ei=ei_new_sender(NULL); ei_configure_name(ei,"gorca-probe"); if(ei_setup_backend_fd(ei,fd))die("ei fd",NULL);
  struct ei_device*abs=NULL,*btn=NULL; int ready=0;
  for(int i=0;i<400&&!ready;i++){ struct pollfd pf={ei_get_fd(ei),POLLIN,0}; poll(&pf,1,10); ei_dispatch(ei); struct ei_event*ev;
    while((ev=ei_get_event(ei))){ switch(ei_event_get_type(ev)){
      case EI_EVENT_SEAT_ADDED: ei_seat_bind_capabilities(ei_event_get_seat(ev),EI_DEVICE_CAP_POINTER,EI_DEVICE_CAP_POINTER_ABSOLUTE,EI_DEVICE_CAP_BUTTON,NULL); break;
      case EI_EVENT_DEVICE_ADDED: { struct ei_device*d=ei_event_get_device(ev); printf("device %s abs=%d btn=%d ptr=%d\n",ei_device_get_name(d),ei_device_has_capability(d,EI_DEVICE_CAP_POINTER_ABSOLUTE),ei_device_has_capability(d,EI_DEVICE_CAP_BUTTON),ei_device_has_capability(d,EI_DEVICE_CAP_POINTER));
        struct ei_region*rg; for(size_t k=0;(rg=ei_device_get_region(d,k));k++) printf("  region %u,%u %ux%u scale %.2f\n",ei_region_get_x(rg),ei_region_get_y(rg),ei_region_get_width(rg),ei_region_get_height(rg),ei_region_get_physical_scale(rg));
        if(ei_device_has_capability(d,EI_DEVICE_CAP_POINTER_ABSOLUTE)&&!abs)abs=ei_device_ref(d); if(ei_device_has_capability(d,EI_DEVICE_CAP_BUTTON)&&!btn)btn=ei_device_ref(d); break; }
      case EI_EVENT_DEVICE_RESUMED: if(abs) ready=1; break; default: break; } ei_event_unref(ev);} }
  if(!abs){puts("NO absolute pointer device");return 2;}
  ei_device_start_emulating(abs,1);
  if(btn&&btn!=abs) ei_device_start_emulating(btn,1);
  ei_device_pointer_motion_absolute(abs,atof(argv[2]),atof(argv[3])); ei_device_frame(abs,ei_now(ei)); ei_dispatch(ei); usleep(100000);
  struct ei_device*b=btn?btn:abs;
  ei_device_button_button(b,BTN_LEFT,true); ei_device_frame(b,ei_now(ei)); ei_device_button_button(b,BTN_LEFT,false); ei_device_frame(b,ei_now(ei)); ei_dispatch(ei); usleep(200000);
  puts("sent"); return 0; }
