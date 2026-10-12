#!/bin/bash
# usage: run_firefox_atspi.sh <page.html> [atspi_tuned_walk args...]
# HEADED private Firefox (own profile, own session+a11y bus, tiny window) on the user's Wayland socket.
# Firefox headless has no ATK bridge, so AT-SPI against Gecko needs a real widget. Prints the active app before/after
# so a focus steal is visible (checked on the REAL a11y bus, read-only).
HERE=$(cd $(dirname $0) && pwd); PAGE=$1; shift
REAL_WL=${WAYLAND_DISPLAY:-wayland-0}; REAL_XDG=$XDG_RUNTIME_DIR
echo "{\"step\":\"active_before\",\"app\":\"$(/usr/bin/python3 $HERE/active_app.py)\"}"
. $HERE/private_a11y_env.sh up
export WAYLAND_DISPLAY=$REAL_XDG/$REAL_WL MOZ_ENABLE_WAYLAND=1
PROF=$(mktemp -d /tmp/ffprof.XXXX)
echo 'user_pref("browser.shell.checkDefaultBrowser", false); user_pref("browser.startup.homepage_override.mstone","ignore"); user_pref("datareporting.policy.dataSubmissionEnabled", false); user_pref("browser.aboutwelcome.enabled", false);' > $PROF/user.js
firefox --no-remote --new-instance -profile $PROF --width 300 --height 200 file://$PAGE >/dev/null 2>&1 &
FPID=$!
sleep 10
echo "{\"step\":\"active_after\",\"app\":\"$(/usr/bin/python3 $HERE/active_app.py)\"}"
/usr/bin/python3 $HERE/atspi_tuned_walk.py $A11Y_ADDR Firefox "$@"
echo "{\"step\":\"rss_mb\",\"firefox_total\":$(ps -eo rss,args | grep -F "$PROF" | grep -v grep | awk '{s+=$1} END {print int(s/1024)}')}"
kill $FPID 2>/dev/null; sleep 2; ps -eo pid,args | grep -F "$PROF" | grep -v grep | awk '{print $1}' | xargs -r kill 2>/dev/null
. $HERE/private_a11y_env.sh down; rm -rf $PROF
