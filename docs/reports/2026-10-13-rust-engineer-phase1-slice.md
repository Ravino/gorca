# Phase 1 vertical slice: notes

Flow: fake/real app focus -> raw drain (ADR-0004) -> `AtspiRouter` (announce on first event) ->
per-app ingress -> app actor -> Reader lane thread (epoch advance, duplicate-focus suppression)
-> fetch dispatcher (`request()` with Interactive deadline; calls via `gorca_dbus::call_method`)
-> utterance -> speech lane -> FailoverClient -> voicego | SSIP.

## Choices
- `Router::focus_seen` (default no-op) added to `gorca-dbus`: the drain hashes object paths into
  `LocalId`, so the connector records the reverse mapping for focus events only (`PathTable`,
  per-app buckets, 1024 entries and 512-byte paths per app, 4096 apps).
- Registration is a short blocking handshake on the drain thread (`Handle::block_on`, 250 ms
  cap), once per sender. Departed names are tombstoned so an app dying mid-registration is
  never published and is retired by the announcer.
- No mirror caching of fetch replies: PropertiesChanged invalidation is not implemented, so
  every focus fetches (one round trip) instead of risking stale speech.
- `BackendOp::Probe` added for the Hung-app probe (I-4); apps recover when it succeeds.
- Keyboard: `MutterKeySource` (RequestName DO_NOT_QUEUE, WatchKeyboard, unicast KeyEvent) only
  with `--impersonate-orca` / `GORCA_IMPERSONATE_ORCA=1`; no grabs yet, key echo only.
- The Reader lane runs on its own thread with a current-thread runtime and a panic restart loop.

## Incident
`crates/gorca-daemon/tests/signals.rs` started the real binary with the inherited environment
for one `cargo test -p gorca-daemon` run before it was isolated (flags now point every bus and
socket at nonexistent paths). That single run may have connected read-only to the real a11y bus
and registered event listeners (they die with the connection); `IsEnabled`/`ScreenReaderEnabled`
were not written (already true).

## Not done
P2P calls (D-7), GetItems cache fill, mirror freshness, SetKeyGrabs/command keys, Newton,
live-region speech, window-activate speech. Real GTK/Shell smoke run not performed.

## Review fixes (BLOCK round, F1-F16)
- Speech: voicego resend workaround removed; `502-<id>` fails by id; failback sends CANCEL then QUIT
  (SSIP `Drop` too); Stop always enters the writer and failover queues; SSIP reply blocks capped at
  64 KiB; both clients check the peer uid.
- Router contract is `Route | NotAnApp | Retry`; the drain never caches `Retry`, counts `deferred`
  and hands the event to `Router::park`. Registration is asynchronous (`AtspiRouter::complete`),
  replays parked events in order under the publish lock, and backs off 10 ms to 1 s on failure.
- Path quotas belong to the owning pid (`GetConnectionCredentials`), with a 16 MiB global byte cap
  that evicts the largest holder; 64 names per process; departed-name tombstones expire (300 s).
- NameOwnerChanged has its own connection; the drain supervisor is its own task.
- `gorca_rt::guard` tripwire (armed by `Session::start`) panics in debug builds when a connect path
  targets an ambient address; `cargo xtask deps` rule 8 forbids env reads outside the daemon main.
- A dead Reader lane stops the core with `CoreError::ReaderLaneDied` (non-zero exit).
- Password fields are read like any other entry (never logged).
- Churn test now races registration and departure; with the tombstone check disabled both
  `registration_and_departure_churn_leaks_nothing` and `an_app_dying_mid_registration_leaks_nothing` fail.
- Bench: `lost` is gated on the max over repeats; K1 is matched by echoed character and time.

## Review round 2 (R2-1..R2-7)
- Park buffer: per pending sender a newest-focus slot outside every cap (a newer focus replaces
  it, replayed last) plus non-focus events bounded by 32 events and 4 KiB, oldest dropped first;
  global 256 KiB. `64 x 4 KiB` equals the global cap, so one sender cannot use up another's share.
- `park()` delivers straight through the route when the registration published between the
  drain's `resolve` and `park`; a sender with no registration running (backoff, pending cap)
  keeps its newest focus (256 senders max) and the next attempt adopts it.
- Ingress limits in `park`: path 256 B, interface/member 255 B, body 512 B; refused, not copied.
- `ignored` loses a name when NameOwnerChanged reports it; unknown-credential names share a quota
  of 32; 512 applications in total, and at the cap the largest process group gives way (an
  application of it is evicted for another group's newcomer; its own newcomer is refused).
  The router cannot know which application is focused: it assumes the focused one is not in the
  largest group.
- Peer uid: `--speech-peer-uid` (repeatable) allow-list, same-uid default kept; both speech
  connects pass through `gorca_rt::guard::check`.

## Follow-ups
- The 16 MiB path-table byte cap is accounting only: RSS grew 279 MiB for 52k live entries in the
  reviewer's release probe (`HashMap::clear` keeps capacity; empty buckets of live owners stay).
  Shrink or replace maps on clear, drop empty buckets, measure RSS in a test.
- Reserve a name slot in `resolve` (the 64-name check still races registration).
- Exempt each owner's newest path from eviction.

## Review round 3 and the security scan of its fixes
Eviction policy at the 512-application cap (`State::pick_victim`, documented in code):
1. Never the focused application (nor the previous one for 5 s, if it held focus for 1 s), never
   the shell's process. Focus protection is published by the Reader only after the focus passed
   the FSM, was presented, and the fetched node itself showed `FOCUSED`; it covers at most two
   applications and a quick focus flipper earns neither slot. Identity is the `AppId` (per unique
   bus name, never reused). The shell is identified as the current owner of `org.gnome.Shell`
   (session bus), as `(pid, process start time)`, re-resolved on its NameOwnerChanged.
2. Only a process group strictly larger than the newcomer's own gives way: the biggest first,
   its newest name. A fork attack shares a process group; a freshly launched application is a
   group of one and wins against it, then is protected once it has focus.
3. If no group is larger (all singletons, or the newcomer's group is the biggest) the newcomer
   is refused with a backoff, and nothing is evicted: evicting "the newest" would let an
   attacker's one extra registration push out an application that had just launched.
   Known limit: an attacker whose 512 processes each sit in their own process group is
   indistinguishable from 512 applications; new applications then wait (with backoff) until
   slots free up. The shell is never refused.
4. Refusals, evictions and name-cap hits are temporary (backoff, never cached); an evicted
   application's closed ingress makes the drain forget its cached route, so it re-registers.

## Live-test fix: GNOME Shell Alt+Tab and key echo
- Shell emits, per Alt+Tab move, only `object:state-changed:selected` (detail1 0 on the old item,
  1 on the new one), from `check_pseudo_class` in `refs/gnome-shell/src/st/st-widget.c`
  (switcherPopup.js `highlight()` toggles the `selected` pseudo-class). St has no Selection
  interface and the items never take key focus. Orca's gnome-shell script treats `selected(1)`
  as a focus move for the same reason (`scripts/apps/gnome-shell/script.py`).
- gorca mapped that event to `PropertiesChanged{STATES}` and dropped it, so only focus-bearing
  events spoke. Now `selected(1)` carries `PropMask::SELECTED_NOW`, the drain records the object
  path for it (`gorca_dbus::drain::needs_path`), the app actor forwards the newest one of a
  batch as `FocusLaneMsg::ItemSelected` when the app's `AppProfile.speak_selection` is set
  (built-in, `app_name == gnome-shell`), and the Reader presents it without "selected" and
  without moving the focus protection.
- Key echo (`presenter::key_echo_text`): nothing under Alt/Ctrl/Super, navigation keys silent,
  action keys (enter, tab, backspace, escape, delete), function keys and characters echoed.
  Lone modifier keys stay silent (Orca echoes them by default; deliberate deviation).
- The echo's epoch is the key press's, so the selection/focus speech that follows (higher epoch)
  makes the lane drop it as stale.
- Tests: `every_selection_move_in_a_shell_style_switcher_is_spoken`,
  `alt_tab_in_the_switcher_speaks_windows_and_never_echoes_tab` (e2e, fakeapp `switcher`).
- Not observed live: the exact event stream was derived from source, not captured (needs a
  person pressing Alt+Tab). Why the first two moves did speak before the fix is unexplained;
  likely focus/window events around popup creation.
