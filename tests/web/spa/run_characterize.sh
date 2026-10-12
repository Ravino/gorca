#!/bin/bash
# usage: run_characterize.sh chrome|firefox [characterize.py args...]
# Starts the static server (if needed) and a PRIVATE session + a11y bus, then runs the characterization.
# chrome: headless=new, no window. firefox: headed inside a private headless gnome-shell (no focus stealing).
HERE=$(cd $(dirname $0) && pwd); B=$1; shift
REAL_WL=${WAYLAND_DISPLAY:-wayland-0}; REAL_XDG=$XDG_RUNTIME_DIR
[ -f $HERE/dist/react/main.js ] || (cd $HERE && node build.mjs)
curl -s --noproxy '*' -o /dev/null http://127.0.0.1:8089/react/ || { (cd $HERE && node serve.mjs 8089 >/dev/null 2>&1 &) ; sleep 1; }
. $HERE/../probes/private_a11y_env.sh up
if [ "$B" = firefox ]; then
  # Firefox has no ATK bridge when headless, so it needs a Wayland compositor. Use a PRIVATE headless gnome-shell (own session bus,
  # in-memory gsettings): the window is always visible and the user's focus/desktop is never touched.
  export REAL_ACTIVE_BEFORE="$(XDG_RUNTIME_DIR=$REAL_XDG DBUS_SESSION_BUS_ADDRESS=unix:path=$REAL_XDG/bus /usr/bin/python3 $HERE/../probes/active_app.py 2>&1)"
  export GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland
  env -u WAYLAND_DISPLAY -u DISPLAY gnome-shell --headless --wayland --no-x11 --wayland-display=gorca-wl --virtual-monitor 1280x800 >$XDG_RUNTIME_DIR/shell.log 2>&1 &
  SHELL_PID=$!
  for i in $(seq 50); do [ -S $XDG_RUNTIME_DIR/gorca-wl ] && break; sleep 0.2; done; sleep 3
  export WAYLAND_DISPLAY=gorca-wl MOZ_ENABLE_WAYLAND=1
fi
/usr/bin/python3 $HERE/driver/characterize.py --browser $B "$@"
rc=$?
if [ "$B" = firefox ]; then
  echo "real_active_before: $REAL_ACTIVE_BEFORE"
  echo "real_active_after:  $(XDG_RUNTIME_DIR=$REAL_XDG DBUS_SESSION_BUS_ADDRESS=unix:path=$REAL_XDG/bus /usr/bin/python3 $HERE/../probes/active_app.py 2>&1)"
  kill $SHELL_PID 2>/dev/null; sleep 1
fi
for p in $(ps -eo pid,args | grep -E "gorca-(cdp|ff)-" | grep -v grep | awk '{print $1}'); do kill $p 2>/dev/null; done
. $HERE/../probes/private_a11y_env.sh down
exit $rc
