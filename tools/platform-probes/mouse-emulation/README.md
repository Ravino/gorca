# mouse-emulation probes (2026-10-19)

PRIVATE headless shell only; report: `docs/reports/mouse-emulation-design.md`. Probe extension `ext/mouse-probe@gorca` exports
`org.gorca.MouseProbe` (virtual pointer, window frames, monitors). `rig.sh WORKDIR "1280x800 1200x900"` starts the shell
(`GORCA_RIG_NO_X11=` enables XWayland); `apply_scale.py BUS 1 1.5` sets monitor 2 to 1.5; `drive*.sh WORKDIR` run clients
(`client.py` click logger, `client_btn.py 3|4` button, `page.html` for browsers) and print results; `extents*.py` dump AT-SPI
extents (needs a registryd, the drive scripts start one and record `reg.pid`); `ei_pointer.c` direct RemoteDesktop + libei
absolute pointer; `rd_probe.py` direct RD D-Bus calls; `lat.py` round trips.
Teardown: `teardown.sh WORKDIR` (kills only recorded PIDs in `pids`/`*.pid` and the rig process group in `rig.pgid`). Never kill by pattern.
