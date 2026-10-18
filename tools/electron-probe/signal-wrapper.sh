#!/bin/bash
# Rig stand-in: runs the installed Signal with the rig's private profile (and the force flag when
# the rig passes it); page URLs are ignored.
extra=(); for a in "$@"; do case "$a" in --user-data-dir=*|--force-renderer-accessibility) extra+=("$a");; esac; done
exec /opt/Signal/signal-desktop "${extra[@]}" --ozone-platform=wayland --no-sandbox
