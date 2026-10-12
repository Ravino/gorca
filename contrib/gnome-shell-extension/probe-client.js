// usage: gjs client.js ping|windows|monitors|bench W H N
imports.gi.versions.Gio='2.0';
const {Gio, GLib} = imports.gi;
const bus = Gio.DBusConnection.new_for_address_sync(GLib.getenv('DBUS_SESSION_BUS_ADDRESS'),
  Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, null, null);
function call(m, args, sig, rsig) {
  return bus.call_sync('org.gorca.ShellHelper','/org/gorca/ShellHelper','org.gorca.ShellHelper',m,
    args ? new GLib.Variant(sig, args) : null, new GLib.VariantType(rsig), 0, 2000, null);
}
const cmd = ARGV[0];
try {
if (cmd==='ping') print(call('Ping',null,null,'(u)').print(false));
else if (cmd==='windows') print(call('GetWindows',null,null,'(aa{sv})').print(false));
else if (cmd==='monitors') print(call('GetMonitors',null,null,'(aa{sv})').print(false));
else if (cmd==='win') { const [id,x,y,w,h]=ARGV.slice(1).map(Number);
  const r=call('CaptureWindowArea',[id,x,y,w,h,{}],'(tiiiia{sv})','((uuusd)aya{sv})');
  print(r.get_child_value(0).print(false), r.get_child_value(2).print(false)); if(ARGV[6]) GLib.file_set_contents(ARGV[6], r.get_child_value(1).recursiveUnpack()); }
else if (cmd==='bench') {
  const [w,h,n]=ARGV.slice(1,4).map(Number); const enc=ARGV[4]||'raw'; const lat=[];
  for (let i=0;i<n;i++){ const t=GLib.get_monotonic_time();
    const r=call('CaptureArea',[100,100,w,h,{encoding:new GLib.Variant('s',enc)}],'(iiiia{sv})','((uuusd)ay)');
    lat.push(GLib.get_monotonic_time()-t);
    if(i===0) {print('info', r.get_child_value(0).print(false), 'bytes', r.get_child_value(1).n_children()); if(ARGV[5]) GLib.file_set_contents(ARGV[5], r.get_child_value(1).recursiveUnpack());} 
    GLib.usleep(20000); }
  lat.sort((a,b)=>a-b); const p=q=>lat[Math.min(n-1,Math.floor(q*n))];
  print(`${enc} ${w}x${h} n=${n} p50=${p(.5)}us p90=${p(.9)}us p99=${p(.99)}us max=${lat[n-1]}us`);
}} catch(e){ print('ERR', e.message); }
