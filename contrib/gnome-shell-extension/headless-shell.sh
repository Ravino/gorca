#!/usr/bin/env bash
# Run a PRIVATE headless gnome-shell (own session bus, own Wayland socket, own settings
# keyfile) with gorca-helper@gorca enabled, for testing without touching the real session.
# Usage: headless-shell.sh WORKDIR [EXE=CGROUP-PATH ...]    (runs until killed)
# Each rule needs the executable AND the full cgroup path (e.g. /user.slice/.../app.slice/gorca.service).
set -euo pipefail
W="$(realpath -m "$1")"; shift
EXT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/gorca-helper@gorca"
rm -rf "$W"; mkdir -p "$W"/{config/glib-2.0/settings,config/gorca,data/gnome-shell/extensions,run,cache}
chmod 700 "$W/run"
cp -r "$EXT" "$W/data/gnome-shell/extensions/"; : > "$W/data/gnome-shell/extensions/gorca-helper@gorca/.dev"
[ -n "${GORCA_TRACE:-}" ] && : > "$W/data/gnome-shell/extensions/gorca-helper@gorca/.trace"
cat > "$W/config/glib-2.0/settings/keyfile" <<K
[org/gnome/shell]
enabled-extensions=['gorca-helper@gorca']
disable-user-extensions=false
K
rules=""; for e in "$@"; do rules="$rules{\"exe\":\"${e%%=*}\",\"cgroup\":\"${e#*=}\"},"; done
printf '{"allowed":[%s]}\n' "${rules%,}" > "$W/config/gorca/shell-helper.json"; chmod 600 "$W/config/gorca/shell-helper.json"
export XDG_CONFIG_HOME="$W/config" XDG_DATA_HOME="$W/data" XDG_RUNTIME_DIR="$W/run" XDG_CACHE_HOME="$W/cache"
export GSETTINGS_BACKEND=keyfile
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS
exec dbus-run-session -- gnome-shell --wayland --headless --no-x11 --sm-disable \
     --virtual-monitor "${GORCA_VMON:-1280x800}" --wayland-display gorca-test-0
