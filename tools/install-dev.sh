#!/usr/bin/env bash
# Dev install: symlink the gorca user unit and register the Super+Alt+G GNOME
# custom keybinding. Idempotent. `--uninstall` removes both.
# The unit is never enabled; gorca is toggled manually. Orca is never touched.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
unit_src="$root/contrib/systemd/gorca.service"
unit_dst="$HOME/.config/systemd/user/gorca.service"
id_src="$root/contrib/systemd/gorca-identity.service"
id_dst="$HOME/.config/systemd/user/gorca-identity.service"
toggle="$root/tools/gorca-toggle.sh"
schema=org.gnome.settings-daemon.plugins.media-keys
base=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings
bind_name="gorca toggle"
bind_key="<Super><Alt>g"

list_paths() { gsettings get $schema custom-keybindings | grep -o "$base/custom[0-9]*/" || true; }
kb() { echo "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$1"; }
fmt_list() { # stdin: one path per line -> GVariant string array
    local out="" p
    while read -r p; do [ -n "$p" ] && out+="${out:+, }'$p'"; done
    echo "[$out]"
}
find_ours() {
    local p
    for p in $(list_paths); do
        if [ "$(gsettings get "$(kb "$p")" command)" = "'$toggle'" ]; then echo "$p"; return; fi
    done
}

if [ "${1:-}" = "--uninstall" ]; then
    systemctl --user stop gorca.service 2>/dev/null || true
    systemctl --user stop gorca-identity.service 2>/dev/null || true
    rm -f "$unit_dst" "$id_dst"
    systemctl --user daemon-reload
    ours=$(find_ours)
    if [ -n "$ours" ]; then
        list_paths | grep -vxF "$ours" | fmt_list | xargs -d '\n' -I{} gsettings set $schema custom-keybindings {}
        gsettings reset-recursively "$(kb "$ours")" 2>/dev/null || dconf reset -f "$ours"
    fi
    echo "gorca unit and keybinding removed"
    exit 0
fi

[ -x "$root/target/release/gorca" ] || echo "warning: $root/target/release/gorca not built (make release)" >&2
[ -f "$root/target/release/extensions/ext-gnome-shell/ext_gnome_shell.wasm" ] || echo "warning: the Shell policy component is not built (make ext); gorca falls back to the native build of the same policy" >&2

mkdir -p "$(dirname "$unit_dst")"
ln -sfn "$unit_src" "$unit_dst"
ln -sfn "$id_src" "$id_dst"
systemctl --user daemon-reload

ours=$(find_ours)
if [ -z "$ours" ]; then
    n=0
    while list_paths | grep -qxF "$base/custom$n/"; do n=$((n + 1)); done
    ours="$base/custom$n/"
    { list_paths; echo "$ours"; } | fmt_list | xargs -d '\n' -I{} gsettings set $schema custom-keybindings {}
fi
gsettings set "$(kb "$ours")" name "$bind_name"
gsettings set "$(kb "$ours")" command "$toggle"
gsettings set "$(kb "$ours")" binding "$bind_key"
echo "installed: $unit_dst; keybinding $bind_key at $ours"
