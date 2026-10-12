#!/bin/bash
# Start a PRIVATE session bus + a11y bus + registryd under a short private XDG_RUNTIME_DIR
# (sun_path limit 108 chars: at-spi2 p2p sockets live under $XDG_RUNTIME_DIR). Never touches the user's buses.
# usage: . private_a11y_env.sh up|down ; exports DBUS_SESSION_BUS_ADDRESS, XDG_RUNTIME_DIR, A11Y_ADDR
case "$1" in
up)
  export XDG_RUNTIME_DIR=/tmp/xg; mkdir -p -m 700 $XDG_RUNTIME_DIR
  unset WAYLAND_DISPLAY DISPLAY
  out=$(dbus-daemon --session --fork --print-address=1 --print-pid=1)
  export DBUS_SESSION_BUS_ADDRESS=$(echo "$out" | sed -n 1p); export PRIV_DBUS_PID=$(echo "$out" | sed -n 2p)
  /usr/libexec/at-spi-bus-launcher --launch-immediately >/dev/null 2>&1 & export PRIV_LAUNCHER_PID=$!
  sleep 1
  /usr/libexec/at-spi2-registryd --use-gnome-session >/dev/null 2>&1 & export PRIV_REG_PID=$!
  sleep 1
  gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled "<true>" >/dev/null
  gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus --method org.freedesktop.DBus.Properties.Set org.a11y.Status ScreenReaderEnabled "<true>" >/dev/null
  export A11Y_ADDR=unix:path=$XDG_RUNTIME_DIR/at-spi/bus
  ;;
down)
  kill $PRIV_REG_PID $PRIV_LAUNCHER_PID $PRIV_DBUS_PID 2>/dev/null ;;
esac
