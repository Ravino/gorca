# org.freedesktop.a11y.KeyboardMonitor (Mutter, GNOME 48.7)

Verified live (2026-10-12, GNOME Shell 48.7, session bus) with
`busctl --user introspect org.freedesktop.a11y.Manager /org/freedesktop/a11y/Manager`
and against `refs/mutter` (main, and tag `48.7` fetched into the same clone).

| Item | Value |
|---|---|
| Bus | session bus |
| Service (owner) | `org.freedesktop.a11y.Manager` (gnome-shell, flags ALLOW_REPLACEMENT) |
| Object path | `/org/freedesktop/a11y/Manager` |
| Interface | `org.freedesktop.a11y.KeyboardMonitor` |
| XML | `refs/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml` |
| Implementation | `refs/mutter/src/backends/meta-a11y-manager.c` |
| Hook into input | `refs/mutter/src/core/events.c` (`meta_display_handle_event`, key branch) |

## Methods and signals

| Member | Signature | Semantics |
|---|---|---|
| `WatchKeyboard()` | `` -> `` | Deliver every key press/release as `KeyEvent`. Events are still handled normally by the compositor and apps. |
| `UnwatchKeyboard()` | `` -> `` | Undo watch. Grabs from `SetKeyGrabs` stay active. |
| `GrabKeyboard()` | `` -> `` | Deliver every key and swallow it (apps and shell shortcuts never see it; Caps/Num Lock state does not change). |
| `UngrabKeyboard()` | `` -> `` | Undo grab. |
| `SetKeyGrabs(modifiers, keystrokes)` | `au a(uu)` -> `` | Replaces this client's grab set. `modifiers`: XKB keysyms that are grabbed as modifiers (pressed alone they are swallowed; any key pressed while one is down is grabbed). `keystrokes`: `(keysym, modifier_mask)` pairs, matched by **exact equality** of the Clutter state mask. |
| `KeyEvent` (signal) | `b u u u q` = `released, state, keysym, unichar, keycode` | **Unicast** (destination = the grabber's unique name), path `/org/freedesktop/a11y/Manager`. `state` is the Clutter/X11-style modifier mask with mouse-button bits stripped (Shift=1<<0, Lock=1<<1, Ctrl=1<<2, Alt=1<<3, Mod2/NumLock=1<<4, ...). `keycode` is the XKB keycode (evdev+8), truncated to 16 bits. |

There are no properties. Introspection is open to anyone; calls are gated (below).

## Access control (the hard constraint)

`check_access()` in `meta-a11y-manager.c` runs as `g-authorize-method` on every method.
`meta_dbus_access_checker_is_sender_allowed()` (`refs/mutter/src/backends/meta-dbus-access-checker.c`)
returns true only if

1. the context runs with `--unsafe-mode`, or
2. `org.gnome.Mutter.DebugControl.A11yManagerWithoutAccessControl` is true (env
   `MUTTER_DEBUG_A11Y_MANAGER_WITHOUT_ACCESS_CONTROL=1` at shell start, or the property when the
   DebugControl service is exported with `--debug-control`), or
3. the caller's **unique name currently owns the well-known name `org.gnome.Orca.KeyboardMonitor`**
   (`meta_dbus_access_checker_allow_sender (..., "org.gnome.Orca.KeyboardMonitor")` in `on_bus_acquired`).

Live proof: `busctl --user call org.freedesktop.a11y.Manager /org/freedesktop/a11y/Manager org.freedesktop.a11y.KeyboardMonitor WatchKeyboard`
from an unprivileged client returns `Call failed: Access denied`.
`RequestName("org.gnome.Orca.KeyboardMonitor", DO_NOT_QUEUE)` while Orca runs returns `3` (EXISTS);
owner is Orca's connection `:1.501`. The 48.7 tag contains the identical allow-list (diff of
`meta-a11y-manager.c` between 48.7 and main only adds `PointerLocator`).

Consequences:
* gorca must own `org.gnome.Orca.KeyboardMonitor` to use the interface. Orca and gorca cannot both be
  authorised at the same time (one owner; Orca does not set ALLOW_REPLACEMENT).
  A queued request (`flags=0`) makes gorca the owner when Orca exits.
* A proper fix is upstream: a second allow-listed name or a generic policy. Until then a dev/test
  setup needs the debug env var or a private Mutter.
* The checker is name-ownership based; any process that owns the name passes (no pid/uid check).

## Multi-client behaviour

Mutter keeps a list of grabbers keyed by the caller's unique name (`ensure_key_grabber`), created
lazily by the first call, removed when the name vanishes (`g_bus_watch_name_on_connection`).
Several authorised clients can coexist: every client with watch/grab interest gets its own
`KeyEvent`; an event is swallowed if **any** client grabs it (`meta_a11y_manager_notify_clients`).
Grabbed keypresses are tracked per keysym so the matching release is swallowed too.

## Delivery order and latency characteristics (source reading)

* `meta_display_handle_event()` calls `meta_a11y_manager_notify_clients()` for key events **before**
  shell/keybinding/client dispatch (`events.c`, around line 150-170), on the compositor main thread.
  It emits the signal and does not wait for any reply: Mutter never blocks on the AT, and a slow AT
  cannot stall the compositor except through broker backpressure (see `dbus-broker-a11y.md`).
* The evdev read happens on Mutter's input thread; the event is then queued to the main thread.
* Key events are **not** delivered when the focused window inhibits shortcuts
  (`meta_window_shortcuts_inhibited`, e.g. VM viewers, remote-desktop clients, xwayland keyboard grab):
  `should_inhibit_a11y` skips the a11y manager entirely. A screen reader is blind to the keyboard there.
* Grabbed modifiers (Insert, Caps Lock ...) use a double-tap rule: the first press+release of a
  grabbed modifier alone is swallowed; a second press within `repeat_delay` is passed through to apps
  (`is_a11y_modifier_first_click`, `meta-seat-impl.c`). `SetKeyGrabs` modifier updates are applied on
  the input thread via a GTask (not synchronous with the D-Bus reply).
* Virtual-device (RemoteDesktop/EIS) key repeats of an already-pressed key are dropped
  (`update_button_count` in `meta_seat_impl_notify_key_in_impl`).
* Measured proxy: session-bus `Peer.Ping` round trip to `org.freedesktop.a11y.Manager` (gnome-shell
  main loop), 2000 samples from Python/GIO: p50 0.136 ms, p99 0.874 ms, p99.9 1.3 ms, max 1.4 ms
  (`tools/platform-probes/pingbus.py`). A one-way unicast signal is below that. The key->gorca hop is
  therefore expected to be sub-millisecond at p99 on an idle desktop; it could not be measured
  end to end because an unauthorised client cannot subscribe (and Orca holds the name).

## How Orca uses it

Orca does not call D-Bus directly; it uses libatspi `AtspiDeviceA11yManager`
(`refs/at-spi2-core/atspi/atspi-device-a11y-manager.c`) through `refs/orca/src/orca/ax_device_manager.py`
(`Atspi.Device.new_full("org.gnome.Orca")`, so the name is `<app_id>.KeyboardMonitor`).
* `constructed`: `RequestName(<app_id>.KeyboardMonitor, flags=0)`, then **sync** `WatchKeyboard`
  (default libgio timeout 25 s), then subscribes to `KeyEvent` on the proxy.
* Grabs: libatspi coalesces changes with a 10 ms timer (`REFRESH_KEYS_TIMEOUT`) then issues a **sync**
  `SetKeyGrabs` (`g_dbus_proxy_call_sync`, timeout -1). For each grab it adds 4 variants of the
  modifier mask (plain, +NumLock, +CapsLock, +both) because Mutter matches the mask exactly.
* libatspi remaps Mod2 (1<<4) to ATSPI NUMLOCK in `state`; virtual modifiers (Orca's Insert/KP_Insert
  and Caps Lock) are allocated from a private range of the state mask in libatspi.
* `GrabKeyboard`/`UngrabKeyboard` are sync calls too (Orca uses them for its "learn mode").
* Watch events cannot be consumed; only keys in the grab set (or `GrabKeyboard`) are swallowed.
  Orca never needs to replay a grabbed key because its grab set is only its own command keys.

## Design notes for gorca

* Use async zbus calls with explicit deadlines for all of the above (Orca/libatspi use sync calls with
  no deadline).
* `KeyEvent` is a unicast signal: subscribe with a match rule on sender `org.freedesktop.a11y.Manager`,
  interface `org.freedesktop.a11y.KeyboardMonitor`, member `KeyEvent` (the destination is your unique
  name so no extra rule is needed for delivery, but zbus needs a handler/stream).
* To pass a grabbed key through to the app, the only mechanism is injection (RemoteDesktop/EIS, see
  `libei-and-portals.md`). Prefer `WatchKeyboard` plus a minimal `SetKeyGrabs` set so most keys
  are never swallowed.
