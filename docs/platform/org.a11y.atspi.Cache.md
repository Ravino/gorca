# org.a11y.atspi.Cache (per-application, a11y bus)

Source: `refs/at-spi2-core/xml/Cache.xml`, `atk-adaptor/accessible-cache.c`, `accessible-leasing.c`,
`refs/gtk/gtk/a11y/gtkatspicache.c`, `refs/qtbase/src/gui/accessible/linux/qspidbuscache.cpp`.
Live: every toolkit app on this host answered `GetItems` at `/org/a11y/atspi/cache`.

| Member | Signature | Notes |
|---|---|---|
| `version` | property `u` | |
| `GetItems()` | `` -> `a((so)(so)(so)iiassusau)` | bulk dump of the app's currently known objects. Item = (ref, app ref, parent ref, index-in-parent, child-count, interfaces, name, role, description, states). Qt may still use the pre-2015 signature (Cache.xml deprecation note). |
| `AddAccessible` (signal) | `((so)(so)(so)iiassusau)` | an object became known/visible to ATs |
| `RemoveAccessible` (signal) | `(so)` | object gone |
| `Ready` (signal, version >= 2) | `` | emitted by GTK4 when the cache can be queried (`gtkatspicache.c:358`) |

Semantics and caveats
* The atk-bridge cache is **lazy and lossy by design**: `AddAccessible` is emitted from an idle handler
  (`spi_idle_add(add_pending_items)`), objects can also be "leased" for 15 s (`LEASE_TIME_S`, expiry
  16 s) when only referenced through events, after which their D-Bus object is released: a client
  must treat any `(bus, path)` as possibly dead and expect `org.freedesktop.DBus.Error.UnknownObject`
  ("Object does not exist at path ...").
* Always-on events (`ChildrenChanged`, `StateChanged`, name/description/parent/role `PropertyChange`)
  are the app's way of keeping client caches coherent; see `org.a11y.atspi.Registry.md`.
* The cache signals are matched by libatspi through the `org.a11y.atspi.Cache` interface rule; a
  client must add `type='signal',interface='org.a11y.atspi.Cache'` itself.
* A cached item is a snapshot: states and child counts go stale between events. Orca's AXObject layer
  keeps its own caches with explicit dead/hung markers (hung expiry 1 s,
  `refs/orca/src/orca/ax_object.py` `_HUNG_TIMEOUT_SECONDS`).

Measured on this host (idle apps, `tools/platform-probes/cachebench.py`, Python GIO over the a11y bus):

| App | GetItems items | GetItems time | Ping rtt p50 / p99 | Properties.Get(Name) p50 / p99 |
|---|---|---|---|---|
| gnome-terminal `:1.14` | 334 | 5.2 ms (~101 KB text-form) | 0.119 / 1.5 ms | 0.148 / 0.63 ms |
| gedit `:1.17` | 236 | 4.3 ms | 0.138 / 0.87 ms | 0.119 / 0.74 ms |
| Firefox `:1.77` | 168 | 4.9 ms | 0.131 / 0.69 ms | 0.120 / 0.82 ms |
| Chrome `:1.76` | 4 | 0.5 ms | 0.067 / 0.62 ms | 0.132 / 1.1 ms |

(Chrome exposes only its top-level UI until an AT enables page accessibility; see
`org.a11y.Bus-and-Status.md`.)

## Which calls are synchronous round trips

In libatspi every `atspi_accessible_get_*`, `atspi_text_get_*`, `atspi_component_*` call is a blocking
D-Bus call on the calling thread with a **800 ms timeout**, extended to 15 s during an application's
first 15 s (`atspi-misc.c`: `method_call_timeout = 800`, `app_startup_time = 15000`,
`set_timeout()`); a timeout surfaces as "The process appears to be hung" (Orca marks the object hung).
Some values come from the libatspi-side cache (`ATSPI_CACHE_DEFAULT`: parent, children, name,
description, states, role, interfaces) after a `GetItems`; attributes, text, extents, relations,
actions are always round trips.
Where Orca blocks (all on its single GLib main loop thread):
* `event_manager.py::_enqueue_object_event -> _ignore()` (line ~654-724): role/state/focus queries per
  event, before the event is even queued.
* `input_event_manager.py::process_keyboard_event` (line ~129-170): on every key press it validates the
  active window with `AXUtilities.can_be_active_window(...)` (state/role queries) and may walk the tree
  (`find_active_window`) before the key is processed/spoken.
* libatspi `SetKeyGrabs`/`WatchKeyboard` to Mutter are sync with no deadline.
