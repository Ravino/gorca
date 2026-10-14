#!/usr/bin/env bash
# usage: fresh.sh start|stop ; prints bus address
L=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/lk
if [ "$1" = stop ]; then
  [ -f $L/shell.pid ] && { P=$(cat $L/shell.pid); kill -TERM -- -$P 2>/dev/null; sleep 1; kill -KILL -- -$P 2>/dev/null; rm -f $L/shell.pid; }
  exit 0
fi
bash $L/fresh.sh stop
setsid bash <repo>/contrib/gnome-shell-extension/headless-shell.sh $L/sh > $L/shell.log 2>&1 &
echo $! > $L/shell.pid
for i in $(seq 40); do
  sleep 0.5
  SP=$(pgrep -P $(cat $L/shell.pid) -f "gnome-shell --wayland" | head -1)
  [ -n "$SP" ] && ADDR=$(tr '\0' '\n' < /proc/$SP/environ 2>/dev/null | grep ^DBUS_SESSION_BUS_ADDRESS= | cut -d= -f2-)
  [ -n "${ADDR:-}" ] && grep -q "started at" $L/shell.log && break
done
sleep 2
echo $ADDR
