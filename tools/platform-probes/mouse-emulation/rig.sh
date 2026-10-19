#!/usr/bin/env bash
# Private headless gnome-shell with the mouse-probe extension. Usage: rig.sh WORKDIR "VMONS" [extra gsettings keyfile lines]
# Writes WORKDIR/env (bus address, wayland display, shell pid). Foreground; kill only the pid in WORKDIR/shell.pid.
set -euo pipefail
W="$(realpath -m "$1")"; VM="${2:-1280x800}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rm -rf "$W"; mkdir -p "$W"/{config/glib-2.0/settings,data/gnome-shell/extensions,run,cache}
chmod 700 "$W/run"
cp -r "$HERE/ext/mouse-probe@gorca" "$W/data/gnome-shell/extensions/"
cat > "$W/config/glib-2.0/settings/keyfile" <<K
[org/gnome/shell]
enabled-extensions=['mouse-probe@gorca']
disable-user-extensions=false
welcome-dialog-last-shown-version='99'
[org/gnome/mutter]
experimental-features=['scale-monitor-framebuffer','xwayland-native-scaling']
K
export XDG_CONFIG_HOME="$W/config" XDG_DATA_HOME="$W/data" XDG_RUNTIME_DIR="$W/run" XDG_CACHE_HOME="$W/cache"
export GSETTINGS_BACKEND=keyfile DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
echo $$ > "$W/rig.pgid"   # run via setsid: this pid is the process-group leader
WL="gorca-mouse-$$"
args=(); for m in $VM; do args+=(--virtual-monitor "$m"); done
exec dbus-run-session -- bash -c '
  echo "BUS=$DBUS_SESSION_BUS_ADDRESS" > '"$W"'/env; echo "WL='"$WL"'" >> '"$W"'/env; echo "RT='"$W"'/run" >> '"$W"'/env
  gnome-shell --wayland --headless ${GORCA_RIG_NO_X11---no-x11} --sm-disable '"${args[*]}"' --wayland-display '"$WL"' &
  echo $! > '"$W"'/shell.pid
  wait'
