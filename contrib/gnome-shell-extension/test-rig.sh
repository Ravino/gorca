#!/usr/bin/env bash
# Private headless gnome-shell with gorca-helper@gorca enabled (dev marker) and a generated allow-list, for
# test-pointer.sh and the MO-0 spikes. Touches nothing in the real session.
#   test-rig.sh WORKDIR ["WxH WxH ..."]      foreground; run it with setsid; teardown: tools/platform-probes/mouse-emulation/teardown.sh WORKDIR
# Writes WORKDIR/env (BUS, WL, RT, A11Y), WORKDIR/rig.pgid (process group = this script), WORKDIR/shell.pid,
# WORKDIR/units (transient systemd --user unit names usable as allow-listed callers: A has capture+pointer,
# B has capture only, C is not listed at all) and WORKDIR/shell.log.
# Env: GORCA_RIG_NO_X11=  (empty) enables XWayland; GORCA_RIG_KEYFILE_EXTRA="key=value\n..." appended under [org/gnome/mutter].
set -euo pipefail
W="$(realpath -m "$1")"; VM="${2:-1280x800}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rm -rf "$W"; mkdir -p "$W"/{config/glib-2.0/settings,config/gorca,data/gnome-shell/extensions,run,cache}
chmod 700 "$W/run"
cp -r "$HERE/gorca-helper@gorca" "$W/data/gnome-shell/extensions/"
# test-only companion (HideOverview/ShowOverview/Notify/MoveFrame/Frames); the helper itself never needs it
cp -r "$HERE/../../tools/platform-probes/mouse-emulation/ext/mouse-probe@gorca" "$W/data/gnome-shell/extensions/"
: > "$W/data/gnome-shell/extensions/gorca-helper@gorca/.dev"
[ -n "${GORCA_TRACE:-}" ] && : > "$W/data/gnome-shell/extensions/gorca-helper@gorca/.trace"
cat > "$W/config/glib-2.0/settings/keyfile" <<K
[org/gnome/shell]
enabled-extensions=['gorca-helper@gorca','mouse-probe@gorca']
disable-user-extensions=false
welcome-dialog-last-shown-version='99'
[org/gnome/mutter]
experimental-features=['scale-monitor-framebuffer','xwayland-native-scaling']
${GORCA_RIG_KEYFILE_EXTRA:-}
K
ID="$$"
UID_N="$(id -u)"
CG="/user.slice/user-$UID_N.slice/user@$UID_N.service/app.slice"
EXE="$(readlink -f /usr/bin/python3)"
printf 'gorca-motest-%s-a\ngorca-motest-%s-b\ngorca-motest-%s-c\n' "$ID" "$ID" "$ID" > "$W/units"
cat > "$W/config/gorca/shell-helper.json" <<J
{"allowed":[
 {"exe":"$EXE","cgroup":"$CG/gorca-motest-$ID-a.service","caps":["capture","pointer"]},
 {"exe":"$EXE","cgroup":"$CG/gorca-motest-$ID-b.service"}
]}
J
chmod 600 "$W/config/gorca/shell-helper.json"
export XDG_CONFIG_HOME="$W/config" XDG_DATA_HOME="$W/data" XDG_RUNTIME_DIR="$W/run" XDG_CACHE_HOME="$W/cache"
export GSETTINGS_BACKEND=keyfile DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
echo $$ > "$W/rig.pgid"   # run via setsid: this pid is the process-group leader
WL="gorca-ptest-$$"
args=(); for m in $VM; do args+=(--virtual-monitor "$m"); done
exec dbus-run-session -- bash -c '
  echo "BUS=$DBUS_SESSION_BUS_ADDRESS" > '"$W"'/env; echo "WL='"$WL"'" >> '"$W"'/env; echo "RT='"$W"'/run" >> '"$W"'/env
  gnome-shell --wayland --headless ${GORCA_RIG_NO_X11---no-x11} --sm-disable '"${args[*]}"' --wayland-display '"$WL"' >'"$W"'/shell.log 2>&1 &
  echo $! > '"$W"'/shell.pid
  wait'
