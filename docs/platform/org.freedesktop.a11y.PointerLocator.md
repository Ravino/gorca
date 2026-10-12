# org.freedesktop.a11y.PointerLocator (Mutter main only; NOT in GNOME 48.7)

Source: `refs/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml`, `meta-a11y-manager.c`.
Live check on this host: `busctl --user introspect org.freedesktop.a11y.Manager /org/freedesktop/a11y/Manager`
lists only `org.freedesktop.a11y.KeyboardMonitor`; no PointerLocator. Tag `48.7` has no
`handle_query_pointer`. Treat as a future (GNOME 49+) feature; probe at runtime
(`test_pointer_locator` in libatspi does exactly that and tolerates `AccessDenied`).

Same bus name, object path and access-control rule as the KeyboardMonitor
(`org.freedesktop.a11y.Manager`, `/org/freedesktop/a11y/Manager`, caller must own
`org.gnome.Orca.KeyboardMonitor`).

| Member | Signature | Semantics |
|---|---|---|
| `QueryPointer()` | `` -> `a{sv} d d` = `app_data, rel_x, rel_y` | Toplevel under the pointer plus pointer position relative to it. `app_data` keys: `app-dbus-name` (s, name on the a11y bus), `toplevel-object-path` (o), `pid` (i). Error `org.freedesktop.a11y.UnknownToplevel` if the client is not introspectable. Registers the caller for the next `PointerPositionChanged`. |
| `PointerPositionChanged` (signal) | `` | Emitted once per query after the next pointer motion (unicast to each requester, then the requester is forgotten). It is a throttle: the AT re-queries in response. |

libatspi wiring: `a11y_manager_pointer_locator_signal_cb` re-queries on every signal and emits
`pointer-moved` with an `AtspiAccessible` resolved from `(app-dbus-name, toplevel-object-path)` or
from the pid.
