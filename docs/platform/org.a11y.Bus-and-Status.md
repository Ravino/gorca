# org.a11y.Bus and org.a11y.Status (bootstrap of the accessibility bus)

Verified live (`busctl --user introspect org.a11y.Bus /org/a11y/bus`) and in
`refs/at-spi2-core/bus/at-spi-bus-launcher.c`.

| Item | Value |
|---|---|
| Name / path (session bus) | `org.a11y.Bus` `/org/a11y/bus` (activated by `at-spi-bus-launcher`) |
| `org.a11y.Bus.GetAddress()` | `` -> `s`, live value `unix:path=/run/user/1000/at-spi/bus` |
| `org.a11y.Status.IsEnabled` | property `b`, read/write, emits `PropertiesChanged` (live: true) |
| `org.a11y.Status.ScreenReaderEnabled` | property `b`, read/write, emits change (live: true) |

* The a11y bus is a **separate** dbus-broker instance:
  `dbus-broker-launch --config-file=/usr/share/defaults/at-spi2/accessibility.conf --scope user`, a
  child of `at-spi-bus-launcher` (ppid check on this host). Details in `dbus-broker-a11y.md`.
* Apps decide whether to enable accessibility from these properties/settings: Chromium reads
  `org.a11y.Status.IsEnabled` once at startup (`refs/chromium/ui/accessibility/platform/atk_util_auralinux.cc`,
  `ShouldEnableAccessibility`: env var, then `IsEnabled`, then GSettings
  `org.gnome.desktop.interface toolkit-accessibility`). Live evidence: Chrome exposed only 4 cache
  items vs 168 for Firefox on this host. gorca must set `ScreenReaderEnabled=true` (and `IsEnabled`)
  when it starts, as Orca/gsd do.
* GNOME launches the screen reader from GSettings `org.gnome.desktop.a11y.applications screen-reader-enabled`
  (watched by `gsd-a11y-settings`); on this host Orca comes from `orca-autostart.desktop` in a
  gnome-session scope (`app-gnome-orca\x2dautostart-<pid>.scope`).
* Registry daemon: activatable name `org.a11y.atspi.Registry` on the a11y bus,
  `Exec=/usr/libexec/at-spi2-registryd --use-gnome-session`.
