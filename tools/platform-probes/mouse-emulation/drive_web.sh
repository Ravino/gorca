#!/usr/bin/env bash
# drive_web.sh WORKDIR firefox|chromium : launch browser in the private shell, dump extents; click is done by hand afterwards.
set -u
W="$1"; . "$W/env"; B="$2"
export DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$RT" HOME="$W/home" 
mkdir -p "$HOME"
P="gdbus call --session -d org.gorca.MouseProbe -o /org/gorca/MouseProbe -m org.gorca.MouseProbe"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>" >/dev/null
if [ ! -f "$W/reg.pid" ]; then /usr/libexec/at-spi2-registryd >"$W/reg.log" 2>&1 < /dev/null & echo $! | tee -a "$W/pids" > "$W/reg.pid"; sleep 1.5; fi
export WAYLAND_DISPLAY="$WL" GDK_BACKEND=wayland GNOME_ACCESSIBILITY=1
PAGE="file://$(dirname "$(realpath "$0")")/page.html"
case "$B" in
 firefox) MOZ_ENABLE_WAYLAND=1 /usr/bin/firefox-esr --no-remote --profile "$W/ffprof" --new-window "$PAGE" > "$W/web.log" 2>&1 & ;;
 chromium) /usr/bin/chromium --ozone-platform=wayland --force-renderer-accessibility --user-data-dir="$W/crprof" --no-first-run --no-default-browser-check "$PAGE" > "$W/web.log" 2>&1 & ;;
esac
echo $! | tee -a "$W/pids" > "$W/web.pid"; sleep 12; $P.HideOverview >/dev/null; sleep 1
echo "meta frames: $($P.Frames)"
/usr/bin/python3 -I "$(dirname "$0")/extents2.py" "${3:-$B}"
