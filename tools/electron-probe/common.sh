# Shared by the probe wrappers. The probe directory holds data that is exec'd (the Electron
# build) and read as configuration, so it must be a private directory owned by us.
# The e2e rig runs the browser with a private HOME: take the real one from the user database.
REAL_HOME=$(getent passwd "$(id -u)" | cut -d: -f6)
EP=${ELECTRON_PROBE_DIR:-${REAL_HOME:-$HOME}/.cache/electron-probe}
mkdir -p -m 700 "$EP" 2>/dev/null
perm=$(stat -c %a "$EP" 2>/dev/null)
if [ ! -d "$EP" ] || [ ! -O "$EP" ] || [ -L "$EP" ] || [ "${perm: -2}" != 00 ]; then
  echo "electron-probe: $EP must be a real directory owned by you with no group/world access" >&2
  exit 78
fi
# key=value data, never sourced: WRAP_BIN, WRAP_DELAY_MS only.
WRAP_BIN=electron; WRAP_DELAY_MS=0
if [ -f "$EP/wrap.conf" ] && [ -O "$EP/wrap.conf" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      WRAP_BIN) WRAP_BIN=$v ;;
      WRAP_DELAY_MS) case "$v" in ''|*[!0-9]*) ;; *) WRAP_DELAY_MS=$v ;; esac ;;
    esac
  done < "$EP/wrap.conf"
fi
