#!/usr/bin/env bash
# Toggle the gorca user service. Bound to Super+Alt+G by tools/install-dev.sh.
# Feedback is spoken without depending on gorca itself (spd-say, then espeak-ng).
set -u

lock="${XDG_RUNTIME_DIR:-/tmp}/gorca-toggle.lock"
exec 9>"$lock" || exit 1
# A second press while the first is still running is dropped, not queued,
# so a double press cannot start and immediately stop the service.
flock -n 9 || exit 0

say() {
    local text=$1
    if command -v spd-say >/dev/null 2>&1 && timeout 5 spd-say -w -- "$text" >/dev/null 2>&1; then
        return 0
    fi
    if command -v espeak-ng >/dev/null 2>&1; then
        timeout 5 espeak-ng -- "$text" >/dev/null 2>&1
    fi
}

notify() {
    command -v notify-send >/dev/null 2>&1 && notify-send -a gorca -t 2000 gorca "$1" >/dev/null 2>&1
    return 0
}

if systemctl --user is-active --quiet gorca.service; then
    if systemctl --user stop gorca.service; then
        notify "gorca off"; say "gorca off"
    else
        notify "gorca failed to stop"; say "gorca stop failed"; exit 1
    fi
else
    if systemctl --user start gorca.service; then
        notify "gorca on"; say "gorca on"
    else
        notify "gorca failed to start"; say "gorca start failed"; exit 1
    fi
fi
