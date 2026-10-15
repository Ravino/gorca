# Platform probes (read-only measurements, 2026-10-12)

Run with the system `/usr/bin/python3` (needs `python3-gi`); none of them modifies the desktop.

* `cachebench.py <addr-file> :1.N ...` AT-SPI `Cache.GetItems`, `Peer.Ping`, `Properties.Get` timings on the a11y bus
  (address from `busctl --user call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress`).
* `termprobe.py <addr-file> :1.N` Text interface sizes/timings of a VTE terminal (reads, prints counts only).
* `pingbus.py` session-bus Peer.Ping RTT to gnome-shell / portal / Orca.
* `espeak_lat.c`, `espeak_cold.c` libespeak-ng synchronous first-sample latency
  (`gcc -O2 x.c -lespeak-ng`).
* `ssip_lat.py <socket>`, `ssip_prio.py <socket>` SSIP latency/priority tests against a PRIVATE speech-dispatcher:
  `speech-dispatcher -s -t 0 -c unix_socket -S <sock> -C <conf-copy-with-AudioOutputMethod "alsa" and AudioALSADevice "null" and an AddModule espeak-ng line> -P <pid> -L <logdir>`.
  Never point them at the user's daemon.

* `p9live.py BUS[@LIMIT] ...` (2026-10-13) read-only P-9 survey on the CURRENT a11y bus: GetItems vs bounded tree walk, idle staleness,
  MANAGES_DESCENDANTS nodes, P2P reachability/latency/socket modes.
* `p9probe.py APP.py [--nolisten] [--steps a,b]` scripted-mutation consistency test (GetItems + Cache signals vs ground-truth walk) against a
  PRIVATE session only (it registers as an event listener); apps in `p9apps/`. Needs the private rig described in
  `docs/reports/2026-10-13-p9-cache-p2p.md` section 6. Use `/usr/bin/python3 -I`.
* `p9enum.py` one-line summary per app (toolkit, child count, cache items, P2P address) on the current bus.

* `adr6/` (2026-10-14) ADR-0006 V-1..V-5 probes, PRIVATE session only (they refuse a bus not under `/tmp/dbus-*`): `rig.py` helpers
  (RemoteDesktop virtual keyboard via `NotifyKeyboardKeycode` with EVDEV keycodes, KeyboardMonitor, raw AT-SPI calls), `v1.py`/`v1b.py` keypad
  grabs, `v2*.py` extents (apps `app3.py`, `app4.py`, `appadw.py`), `v34.py` GetOffsetAtPoint + bounds-changed, `v4shell.py`, `v5.py`/`v5b.py` VTE.
  Report: `docs/reports/2026-10-14-adr0006-platform-checks.md`.
