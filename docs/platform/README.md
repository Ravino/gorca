# Platform interface specs

Exact contracts verified on the dev host (Debian 13, GNOME Shell 48.7, Mutter 48.7, AT-SPI 2.56.2,
dbus-broker 37, speech-dispatcher 0.12.0, espeak-ng 1.52, libvte-2.91 0.80.1). One file per interface.
Findings, evidence and measurements: `docs/reports/2026-10-12-linux-platform-foundation.md`.
Probe scripts: `tools/platform-probes/`.

| File | Contract |
|---|---|
| `org.freedesktop.a11y.KeyboardMonitor.md` | Wayland keyboard watch/grab (Mutter), access control, Orca usage |
| `org.freedesktop.a11y.PointerLocator.md` | Pointer query (Mutter main only, absent on 48.7) |
| `libei-and-portals.md` | RemoteDesktop (Mutter + portal), libei, InputCapture, GlobalShortcuts, evdev |
| `newton-mutter-keyboard-and-tree.md` | Newton prototype: keys and tree transport |
| `org.a11y.Bus-and-Status.md` | a11y bus discovery, IsEnabled/ScreenReaderEnabled |
| `org.a11y.atspi.Registry.md` | Event registration, two-layer filtering, event signal shapes |
| `org.a11y.atspi.Cache.md` | Cache bulk interface, lease behaviour, sync-call map, measured timings |
| `dbus-broker-a11y.md` | How dbus-broker shapes the a11y bus (quotas, slow readers) |
| `vte-terminal-accessibility.md` | GNOME Terminal/VTE Text events and large-output behaviour |
| `ssip-speech-dispatcher.md` | SSIP protocol, priorities, cancel, measured latency |
| `espeak-ng-library.md` | Direct libespeak-ng use and latency, SSIP comparison |
