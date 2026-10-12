#!/usr/bin/env bash
# Developer helper for the gorca-helper@gorca GNOME Shell extension.
#   dev.sh install     copy the extension into ~/.local/share/gnome-shell/extensions (this user only)
#   dev.sh enable      gnome-extensions enable (see note on disable-user-extensions)
#   dev.sh disable     gnome-extensions disable
#   dev.sh uninstall   disable + remove the files (+ optionally the config: --purge)
#   dev.sh status
#   dev.sh sync        re-copy files (with a .dev marker, helper.js is re-imported on next enable)
# Nothing here restarts the shell or needs root.
set -euo pipefail
UUID=gorca-helper@gorca
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$UUID"
DST="$HOME/.local/share/gnome-shell/extensions/$UUID"
CFG="${XDG_CONFIG_HOME:-$HOME/.config}/gorca/shell-helper.json"

case "${1:-}" in
install|sync)
    mkdir -p "$DST"
    cp -f "$SRC"/metadata.json "$SRC"/extension.js "$SRC"/helper.js "$DST"/
    [ "${GORCA_HELPER_DEV:-1}" = 1 ] && : > "$DST/.dev"
    echo "installed to $DST (dev marker: ${GORCA_HELPER_DEV:-1})"
    ;;
enable)
    if [ "$(gsettings get org.gnome.shell disable-user-extensions)" = true ]; then
        echo "NOTE: org.gnome.shell disable-user-extensions is true; user extensions will not load." >&2
        echo "      set it with: gsettings set org.gnome.shell disable-user-extensions false" >&2
    fi
    gnome-extensions enable "$UUID"
    ;;
disable) gnome-extensions disable "$UUID" ;;
uninstall)
    gnome-extensions disable "$UUID" 2>/dev/null || true
    rm -rf "$DST"
    [ "${2:-}" = --purge ] && rm -f "$CFG"
    echo "removed $DST (the running shell keeps the module cached until its next restart)"
    ;;
status)
    gnome-extensions info "$UUID" || true
    busctl --user status org.gorca.ShellHelper 2>&1 | head -3 || true
    ;;
*) sed -n 2,9p "$0"; exit 2 ;;
esac
