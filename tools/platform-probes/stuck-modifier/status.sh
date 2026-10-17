#!/usr/bin/env bash
# usage: status.sh ADDR   -- PRIVATE session only: does org.a11y.Status Set write gsettings?
export DBUS_SESSION_BUS_ADDRESS="$1"
W=/tmp/scratch/ns/sh
export XDG_CONFIG_HOME=$W/config GSETTINGS_BACKEND=keyfile
show() {
  echo "  gsettings screen-reader-enabled=$(gsettings get org.gnome.desktop.a11y.applications screen-reader-enabled) toolkit-accessibility=$(gsettings get org.gnome.desktop.interface toolkit-accessibility)"
  echo "  bus: $(gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.GetAll org.a11y.Status)"
}
echo initial; show
echo "Set ScreenReaderEnabled=true"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status ScreenReaderEnabled '<true>'
sleep 1; show
echo "Set IsEnabled=true"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled '<true>'
sleep 1; show
echo "Set ScreenReaderEnabled=false"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status ScreenReaderEnabled '<false>'
sleep 1; show
echo "Set IsEnabled=false"
gdbus call --session -d org.a11y.Bus -o /org/a11y/bus -m org.freedesktop.DBus.Properties.Set org.a11y.Status IsEnabled '<false>'
sleep 1; show
