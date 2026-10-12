# Newton (Matt Campbell's Wayland-native a11y prototype): what it does for keys and trees

Sources: `refs/newton/mutter` (branch `newton` tip `f79e74c`, 2024-06-28, which after a rebase equals
upstream plus nothing Newton-specific; the Newton commits live on `origin/newton-before-rebase`, tip
`b123605` 2024-05-17, fetched into the same clone), `refs/newton/consumer` (last commit
`76b2ee2`, 2024-06-29), `refs/newton/atspi-adapter` (`21b4d54`, 2024-01-29), `refs/newton/orca`,
`refs/newton/gtk` (AccessKit branch). All activity stops mid-2024; nothing in this host's Mutter 48.7
implements it (live `org.freedesktop.a11y.Manager` only has `KeyboardMonitor`).

## Mechanism
* Transport for tree data is D-Bus plus an fd, not a Wayland-visible AT protocol: commit `bd1ad7e`
  "New prototype, using D-Bus for the AT (consumer) protocol". Mutter owns `org.freedesktop.a11y.Manager`
  at `/org/freedesktop/a11y/Manager`; the consumer (`refs/newton/consumer/lowlevel/src/proxy.rs`) calls
  `GetKeyboardFocus() -> u`, `RequestSurfaceUpdates(u surface_id, o sink_path)`, `DoAction(u surface_id, h fd)`
  and exports `org.freedesktop.a11y.UpdateSink` objects at `/UpdateSink/<n>` with
  `Update(h fd)` (+ signal `Closed`). The fd carries an AccessKit `TreeUpdate` serialised as **JSON**
  (`serde_json::from_slice::<TreeUpdate>` in `state.rs`; read with `read_to_end`).
* Signals: `KeyboardFocusChanged(u)`, `PointerMoved(u,d,d)`, `SurfaceDestroyed(u)`, `KeyEvent(b u u u q)`.
* Wayland side: new protocol `a11y-consumer-v1` from a forked `wayland-protocols` `accessibility`
  branch; apps (GTK with AccessKit, egui) push tree updates to Mutter, which forwards to the AT.

## Keyboard in the Newton branch
* `cf01efc` "Prototype passive key snooping in the Newton AT protocol": `meta_a11y_manager_snoop_key_event()`
  called from `meta_display_handle_event()` for every key press/release, emits the **broadcast** signal
  `org.freedesktop.a11y.Manager.KeyEvent(b released, u state, u keysym, u unichar, q keycode)`
  with the same argument list as upstream `KeyboardMonitor.KeyEvent`. Code comment: `/* TODO: key grabs */`;
  it returns FALSE so nothing is ever swallowed. No access control.
* The later consumer proxy (`proxy.rs`) already declares `GrabKeyboard`, `UngrabKeyboard`,
  `SetKeyGrabs(au, a(uu))` on the same `org.freedesktop.a11y.Manager` interface: Newton planned to
  fold upstream's KeyboardMonitor semantics into its own interface. Upstream instead split them
  into `org.freedesktop.a11y.KeyboardMonitor` (the interface gorca will actually use on GNOME 48/49) on the
  same bus name/path. Interface names overlap in a confusing way: a Newton-aware client and an
  upstream client both talk to `org.freedesktop.a11y.Manager`/`/org/freedesktop/a11y/Manager` but to
  different interfaces.
* Orca branch `refs/newton/orca` uses Python package `newton_atspi_compat` (from `newton_consumer`) to
  consume Newton trees directly, bypassing AT-SPI; `newton-atspi-adapter` translates to AT-SPI
  (no text support at the time).

## Consequences
* Newton cannot be a runtime dependency on GNOME 48: it needs patched Mutter, GTK and wayland-protocols,
  and the prototype is stale. The connector abstraction is still the right design (a Newton connector
  would reuse `KeyEvent`), but the interface is not stable and not shipped.
* The keyboard path for gorca is the upstream `org.freedesktop.a11y.KeyboardMonitor`. If Newton ever
  lands, expect its Manager interface to carry the same `KeyEvent` signature, so key handling code
  should depend on a `KeyEvent{released,state,keysym,unichar,keycode}` struct, not on the interface name.
