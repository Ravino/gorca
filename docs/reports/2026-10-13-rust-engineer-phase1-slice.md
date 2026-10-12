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
