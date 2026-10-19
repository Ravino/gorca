#!/usr/bin/env bash
# run-as.sh WORKDIR a|b|c SCRIPT [ARGS...] : run a python3 script as the rig's caller A (capture+pointer), B (capture
# only) or C (not listed): the identity is the private binary WORKDIR/py-<role> (see test-rig.sh). Foreground, no service
# manager involved; PYTHONHOME makes the copied binary find the system stdlib and PyGObject.
set -u
W="$(realpath "$1")"; role="$2"; script="$3"; shift 3
. "$W/env"
case "$role" in a|b|c) ;; *) echo "role a|b|c" >&2; exit 2;; esac
export BUS="$BUS" WL="$WL" RT="$RT" W="$W" DBUS_SESSION_BUS_ADDRESS="$BUS" PYTHONUNBUFFERED=1 PYTHONHOME=/usr
exec timeout 900 "$W/py-$role" "$script" "$@" </dev/null
