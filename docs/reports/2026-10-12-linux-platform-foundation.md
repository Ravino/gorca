# Linux platform foundation research (2026-10-12)

Author: linux-platform-engineer. Host: Debian 13, kernel 7.1.8+deb13-rt, GNOME Shell 48.7 (Mutter 48.7),
Wayland, dbus-broker 37, AT-SPI 2.56.2, Orca 48.1 running (not touched), speech-dispatcher 0.12.0,
espeak-ng 1.52, libvte-2.91 0.80.1, libei 1.3.901. Exact interface specs are in `docs/platform/`;
probe scripts in `tools/platform-probes/`. "Verified" = observed on the live system; "source" = read in refs/.

## 0. Constraints that force architectural choices

1. **Only one screen reader can hold the Wayland keyboard.** Mutter authorises `KeyboardMonitor`
   callers by checking that the caller's unique name owns the well-known name
   `org.gnome.Orca.KeyboardMonitor` (or unsafe-mode / a debug env var). Verified: an unprivileged call
   returns `Access denied`; `RequestName(org.gnome.Orca.KeyboardMonitor, DO_NOT_QUEUE)` returns EXISTS
   with Orca as owner. gorca must own that name (impersonating Orca's identity), so it cannot run next
   to Orca on a stock session; development needs Orca stopped, or a private Mutter / debug env var.
   Needs an upstream Mutter change for a clean story.
2. **Keys are a one-way, unicast D-Bus signal with no ack.** Mutter never waits for the AT; watch events
   cannot be consumed, only keys in the grab set (or `GrabKeyboard`) are swallowed, and a swallowed key
   can only be passed on again by injection (RemoteDesktop/EIS), which has its own duplicate-press and
   self-feedback rules. Windows that inhibit shortcuts (VM viewers, remote desktop clients) get no
   a11y key events at all.
3. **Everything on the AT-SPI side is blocking D-Bus unless gorca makes it async.** libatspi uses sync
   calls with an 800 ms timeout (15 s during an app's first 15 s); Orca calls role/state queries inside
   the event callback and on every key press (see section 2). gorca needs per-app actors, explicit
   deadlines, and a "hung" marker per app; the a11y bus offers no server-side help.
4. **The broker will not protect gorca from event storms.** The a11y bus has effectively unlimited
   per-uid quotas and same-uid senders are never throttled; a slow gorca makes dbus-broker buffer
   without bound and lag behind the UI, and when a quota is finally hit the receiver is disconnected
   (never silently dropped). gorca needs a dedicated, non-blocking socket reader feeding bounded,
   coalescing queues (drop/merge superseded events after reading).
5. **Terminals are the event-storm worst case.** VTE rebuilds and diffs the entire visible screen each
   frame and ships changed text inside the event; under heavy output that is ~tens of KB per event
   at up to the frame rate, paid in the app even if the AT ignores it. Needs per-object coalescing and
   a terminal-specific handler; also an assert in VTE aborts the whole terminal server on a negative
   text offset.
6. **Latency budget.** Measured pieces: session-bus RTT ~0.14 ms p50/0.9 ms p99; libespeak-ng time to
   first PCM <0.3 ms; speech-dispatcher SPEAK->BEGIN 0.7 ms p50 idle but 13 ms p50 / 20 ms max for
   cancel-and-replace; PipeWire default quantum 21.3 ms dominates audio start. A 15 ms p99 gorca-side
   budget is feasible for key->speech request if the echo path avoids AT-SPI round trips and
   avoids SSIP cancel-replace on the hot path (use direct espeak-ng or own audio stream with
   flush), and if every AT-SPI query on the key path is cache-served or deferred.
7. **Newton is not a dependency.** Patched Mutter/GTK needed, stale since mid-2024. Keep the connector
   abstraction; key handling should depend on a `KeyEvent` struct, not on the D-Bus interface name.
8. **Apps enable their a11y trees based on `org.a11y.Status`** (Chromium reads `IsEnabled` once at
   start). gorca must set `IsEnabled` and `ScreenReaderEnabled` itself, and GNOME's autostart for
   the reader is the `screen-reader-enabled` GSetting handled by gsd-a11y-settings (Orca is launched via
   `orca-autostart.desktop`).

## 1. Wayland keyboard path

Full contract: `docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`.

* Service `org.freedesktop.a11y.Manager` (gnome-shell), path `/org/freedesktop/a11y/Manager`, interface
  `org.freedesktop.a11y.KeyboardMonitor`: `WatchKeyboard()`, `UnwatchKeyboard()`, `GrabKeyboard()`,
  `UngrabKeyboard()`, `SetKeyGrabs(au modifiers, a(uu) keystrokes)`, signal `KeyEvent(b u u u q)`
  (released, state, keysym, unichar, keycode). Verified identical to `refs/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml`
  (`KeyboardMonitor` part); `PointerLocator` exists only in Mutter main, not on 48.7.
* Registration: no explicit register call. Any authorised caller's first method call creates a grabber
  entry keyed by its unique name; it is dropped when the name vanishes
  (`refs/mutter/src/backends/meta-a11y-manager.c`).
* Coexistence: Mutter supports many grabbers (all get events; any grab swallows). Authorisation is
  the limit: a single allow-listed name. See constraint 1.
* Orca: libatspi `AtspiDeviceA11yManager` (`refs/at-spi2-core/atspi/atspi-device-a11y-manager.c`) via
  `refs/orca/src/orca/ax_device_manager.py` (`Atspi.Device.new_full("org.gnome.Orca")`): requests
  `<app_id>.KeyboardMonitor`, sync `WatchKeyboard`, sync `SetKeyGrabs` debounced 10 ms with 4
  NumLock/CapsLock variants per grab, `GrabKeyboard` for learn mode. Orca never replays keys.
* Latency: Mutter emits from `meta_display_handle_event` before shortcut dispatch, on the compositor
  main thread, fire-and-forget. Measured proxy: Peer.Ping to `org.freedesktop.a11y.Manager` p50 0.136 ms,
  p99 0.874 ms, p99.9 1.3 ms, max 1.4 ms (2000 samples). A true KeyEvent measurement was impossible:
  Orca holds the name. Grabbed modifiers use a double-tap window equal to the key repeat delay.
* Alternatives (`docs/platform/libei-and-portals.md`): libei/RemoteDesktop/EIS = injection only;
  InputCapture portal = pointer-barrier-triggered, "no way to activate immediate capture";
  GlobalShortcuts = named shortcuts after a user dialog; evdev = observe-only (group `input`),
  never `EVIOCGRAB`. Surprise: `org.gnome.Mutter.RemoteDesktop.CreateSession` succeeded for an
  unprivileged client with no portal dialog (Start was not called).
* Newton mutter (`docs/platform/newton-mutter-keyboard-and-tree.md`): the `newton` branch of
  `refs/newton/mutter` has been rebased onto upstream and contains no Newton code; the real work is on
  `newton-before-rebase` (May 2024). It only snoops keys passively (`TODO: key grabs`), broadcast
  `KeyEvent` with the same signature, no access control.

## 2. AT-SPI pipeline

Specs: `org.a11y.Bus-and-Status.md`, `org.a11y.atspi.Registry.md`, `org.a11y.atspi.Cache.md`,
`dbus-broker-a11y.md`.

* Bus: `org.a11y.Bus.GetAddress` -> `unix:path=/run/user/1000/at-spi/bus`, a separate dbus-broker
  (`dbus-broker-launch --config-file=/usr/share/defaults/at-spi2/accessibility.conf --scope user`).
* Registry (`org.a11y.atspi.Registry`, `:1.1`): `RegisterEvent(s event, as props, s app)`,
  `DeregisterEvent`, `GetRegisteredEvents() -> a(ss)` (Orca = 38 entries live), signals
  `EventListenerRegistered`/`Deregistered`. Code emits `ssas` while XML says `ss`.
* Filtering exists at two levels: the broker (libatspi adds `type='signal',interface='org.a11y.atspi.Event.X'[,sender=][,member=]`
  matches) and the app (atk-bridge, GTK4, Qt read `GetRegisteredEvents` and drop unrequested events),
  except always-on cache-maintenance events (`ChildrenChanged`, `StateChanged`, name/description/
  parent/role changes). ATK text events still compute their text before the filter.
* Cache: `GetItems() -> a((so)(so)(so)iiassusau)`, `AddAccessible`, `RemoveAccessible`, `Ready` (v2).
  Lazy, idle-batched, objects leased 15 s; treat all refs as possibly dead. Measured `GetItems` on
  this desktop: 168-334 items in 4.3-5.2 ms (terminal, gedit, Firefox); Chrome exposes 4 until
  an AT enables it.
* Sync round trips: libatspi getters (800 ms timeout); Orca blocks in `_enqueue_object_event -> _ignore`
  (role/state/focus queries per event, `refs/orca/src/orca/event_manager.py:654-724`) and in
  `process_keyboard_event` (active-window validation per key press, `input_event_manager.py:129-170`),
  and in libatspi sync `SetKeyGrabs`/`WatchKeyboard` with no deadline. Measured idle RTTs on the a11y
  bus: Ping p50 0.07-0.14 ms, p99 0.6-1.5 ms; `Properties.Get(Name)` p50 0.12-0.15 ms, p99 0.6-1.1 ms.
* dbus-broker effects: no D-Bus stats interface (`GetAllMatchRules` unimplemented); effectively
  unlimited per-uid quotas on both buses; slow receivers are disconnected at quota, never silently
  dropped; no per-sender throttling inside one uid; config limit `max_match_rules_per_connection`
  50000 so wide registrations are allowed.

## 3. GNOME Terminal / VTE

Spec: `docs/platform/vte-terminal-accessibility.md`. `refs/vte` cloned (shallow) and added to
`tools/fetch-refs.sh`. Key points: GTK3/ATK AtkText of the visible screen only; per-frame (<= ~60 Hz,
`VTE_MAX_PROCESS_TIME` 100 ms budget) full snapshot + head/tail diff emitting one delete and one
insert plus a caret event; text payload produced inside the app by the ATK bridge; attribute run offsets
are UTF-8 byte offsets in GTK3 VTE; `CharacterCount` (31830) disagreed with `GetText` length (20995)
on the live terminal; `g_assert(start_offset >= 0)` can abort the terminal server.

## 4. Speech

Specs: `ssip-speech-dispatcher.md`, `espeak-ng-library.md`. Measured with a private speechd instance
(ALSA null sink, nothing audible, user daemon untouched) and a C harness on libespeak-ng
(synchronous callback mode):

| Metric | Result |
|---|---|
| SSIP SPEAK "a" -> queued reply | p50 0.10 ms, max 0.84 ms |
| SSIP SPEAK -> BEGIN (idle) | p50 0.7 ms, p90 1.1 ms, max 3.3 ms |
| SSIP CANCEL ack / CANCELED event | p50 0.09 ms / 3.6 ms (max 13.7 ms) |
| SSIP cancel + new TEXT message -> BEGIN | p50 13.1 ms, max 20.5 ms |
| libespeak-ng Synth -> first PCM callback | 0.01-0.12 ms warm (max 0.31 ms), 0.05-0.08 ms first call; init 0.5-3.3 ms |
| espeak-ng synth speed | 80 chars (4.5 s audio) in 2.4 ms (~1900x real time) |
| espeak voice switch en-us -> ru | 5.8-24.9 ms |
| PipeWire quantum (host default) | 1024 @ 48 kHz = 21.3 ms (adds to both paths) |

Cancel semantics: SSIP `STOP` = current message, `CANCEL` = current + queued, `PAUSE/RESUME` keep
state. `IMPORTANT` is never interrupted and queues behind itself; `TEXT` interrupts itself and is the
"newest wins" priority. Direct espeak-ng: return 1 from the synth callback, granularity one chunk
(60 ms default buffer, 300 ms in speechd's config), and flush your own audio stream.

## Unverified / open
* True end-to-end key->KeyEvent latency (needs a Mutter that lets us own the name, or Orca stopped).
* Behaviour when injected keys (RemoteDesktop/EIS) come back through `KeyEvent`; whether Mutter 48.7
  checks anything beyond session ownership for non-portal RemoteDesktop callers (CreateSession worked).
* VTE under real heavy output (not generated to avoid disturbing the user's terminal); payload
  figures in the VTE doc are estimates from live screen size and source.
* Real audio start latency through PipeWire/pulse (measured with a null sink only).
* Whether the numbers hold under CPU load or with a hung app (all measurements idle).
* `CharacterCount` vs `GetText` mismatch cause in VTE.
