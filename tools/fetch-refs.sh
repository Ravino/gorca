#!/usr/bin/env bash
# Fetch/update reference sources into refs/ (gitignored). Shallow; big browsers are sparse.
set -uo pipefail
cd "$(dirname "$0")/../refs"
clone_branch() { # name url branch [sparse dirs...]
  local name=$1 url=$2 br=$3; shift 3
  if [ -d "$name/.git" ]; then git -C "$name" pull -q --depth 1 && echo "updated $name" || echo "FAILED update $name"; return; fi
  if [ $# -gt 0 ]; then
    git clone -q --depth 1 -b "$br" --filter=blob:none --sparse "$url" "$name" && git -C "$name" sparse-checkout set "$@" && echo "ok $name@$br (sparse)" || echo "FAILED $name"
  else
    git clone -q --depth 1 -b "$br" "$url" "$name" && echo "ok $name@$br" || echo "FAILED $name"
  fi
}
clone() { # name url [sparse dirs...]
  local name=$1 url=$2; shift 2
  if [ -d "$name/.git" ]; then git -C "$name" pull -q --depth 1 && echo "updated $name" || echo "FAILED update $name"; return; fi
  if [ $# -gt 0 ]; then
    git clone -q --depth 1 --filter=blob:none --sparse "$url" "$name" && git -C "$name" sparse-checkout set "$@" && echo "ok $name (sparse)" || echo "FAILED $name"
  else
    git clone -q --depth 1 "$url" "$name" && echo "ok $name" || echo "FAILED $name"
  fi
}
# GNOME / AT-SPI stack
clone orca            https://gitlab.gnome.org/GNOME/orca.git &
clone at-spi2-core    https://gitlab.gnome.org/GNOME/at-spi2-core.git &
clone mutter          https://gitlab.gnome.org/GNOME/mutter.git &
clone gnome-shell     https://gitlab.gnome.org/GNOME/gnome-shell.git &
clone gtk             https://gitlab.gnome.org/GNOME/gtk.git gtk/a11y docs/reference &
clone xdg-desktop-portal https://github.com/flatpak/xdg-desktop-portal.git &
wait
# D-Bus, input, speech
clone dbus-broker      https://github.com/bus1/dbus-broker.git &
clone dbus             https://gitlab.freedesktop.org/dbus/dbus.git &
clone libei            https://gitlab.freedesktop.org/libinput/libei.git &
clone speech-dispatcher https://github.com/brailcom/speechd.git &
clone espeak-ng        https://github.com/espeak-ng/espeak-ng.git &
clone brltty           https://github.com/brltty/brltty.git &
clone voicego          https://github.com/Ravino/voicego.git &
wait
# Rust prior art and crates
clone odilia     https://github.com/odilia-app/odilia.git &
clone atspi-rs   https://github.com/odilia-app/atspi.git &
clone zbus       https://github.com/dbus2/zbus.git &
clone accesskit  https://github.com/AccessKit/accesskit.git &
clone ssip-client https://github.com/odilia-app/ssip-client-async.git &
clone wasmtime   https://github.com/bytecodealliance/wasmtime.git &
wait
# Newton (Wayland-native a11y architecture by Matt Campbell, AccessKit author)
mkdir -p newton
NW=https://gitlab.gnome.org/mwcampbell
clone newton/consumer          $NW/newton_consumer.git &
clone newton/atspi-compat      $NW/newton_atspi_compat.git &
clone newton/atspi-adapter     $NW/newton-atspi-adapter.git &
clone newton/test-consumer     $NW/newton-test-consumer.git &
clone_branch newton/mutter     $NW/mutter.git newton &
clone_branch newton/orca       $NW/orca.git newton &
clone_branch newton/gtk        $NW/gtk.git accesskit gtk/a11y &
clone_branch newton/libadwaita $NW/libadwaita.git newton &
wait
# Other screen readers (design reference)
clone nvda https://github.com/nvaccess/nvda.git &
# Terminal accessibility (VTE: GTK3 AtkText implementation used by GNOME Terminal)
clone vte https://gitlab.gnome.org/GNOME/vte.git &
# Browsers: accessibility code only
clone firefox  https://github.com/mozilla-firefox/firefox.git accessible widget/gtk &
clone chromium https://github.com/chromium/chromium.git ui/accessibility content/browser/accessibility content/renderer/accessibility third_party/blink/renderer/modules/accessibility &
clone qtbase   https://github.com/qt/qtbase.git src/gui/accessible &
wait
du -sh */ 2>/dev/null | sort -h
