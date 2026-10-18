#!/bin/bash
# Rig stand-in: tiny Electron app opening the last URL argument (binary renamed so the rig's
# "chrom" name filter finds it).
. "$(dirname "$0")/common.sh"
exec "$EP/dist2/chromelectron" --no-sandbox --disable-gpu "$EP/app" "$@"
