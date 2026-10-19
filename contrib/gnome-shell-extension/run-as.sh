#!/usr/bin/env bash
# run-as.sh WORKDIR a|b|c SCRIPT [ARGS...] : run a python3 script as the rig's allow-listed caller A (capture+pointer),
# B (capture only) or C (not listed), in the matching transient systemd --user unit, with the rig environment.
set -u
W="$(realpath "$1")"; role="$2"; script="$3"; shift 3
. "$W/env"
case "$role" in a) n=1;; b) n=2;; c) n=3;; *) echo "role a|b|c" >&2; exit 2;; esac
unit="$(sed -n "${n}p" "$W/units")"
exec systemd-run --user --quiet --wait --pipe --collect --unit="$unit" \
  --setenv=BUS="$BUS" --setenv=WL="$WL" --setenv=RT="$RT" --setenv=W="$W" \
  --setenv=DBUS_SESSION_BUS_ADDRESS="$BUS" --setenv=PYTHONUNBUFFERED=1 \
  /usr/bin/python3 "$script" "$@" </dev/null
