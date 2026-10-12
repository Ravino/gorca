# Read-only AT-SPI probes for browsers

Run with the system Python (`/usr/bin/python3`, has `gi` + `Atspi`), not the project venv.
All probes only read the tree of an already running browser on the live a11y bus. They print roles,
counts and timings, never page text. Usage: `probe.py "<app name>" <index> <bus-name>`
(for example `p2p_vs_bus_latency.py Firefox 0 :1.77`). Find bus names with
`busctl --address=unix:path=/run/user/1000/at-spi/bus list`.

| Probe | Measures |
|---|---|
| rtt_single_calls.py | per-call latency of Accessible methods on the showing document |
| text_calls_latency.py | Text.GetText / GetStringAtOffset / GetRangeExtents latency |
| bulk_walk.py | cost of a naive full-tree walk (1 GetChildren + 5 property calls per node) |
| p2p_vs_bus_latency.py | same call through the bus daemon vs the app's peer-to-peer socket |
| collection_matches.py | Collection.GetMatches by role over the whole document |
| cache_getitems.py | what org.a11y.atspi.Cache.GetItems returns for an app |
| hypertext_invariant.py | count of U+FFFC in Text vs child count (tree/text consistency) |

## Source-layer probes (2026-10-12, docs/reports/2026-10-12-web-source-layer.md)

These launch their OWN browser (temp profile; private session+a11y bus under `/tmp/xg` where AT-SPI is needed).
They never touch the user's running browsers or buses. Page generator: `tests/web/pages/gen_large.py N out.html`;
cross-site iframe / closed shadow / canvas page: `tests/web/pages/frames/`.

| Probe | Measures |
|---|---|
| cdp_probe.py | Chromium CDP over `--remote-debugging-pipe`: getFullAXTree time/size, queryAXTree, DOMSnapshot, nodesUpdated behaviour |
| accname_lib_probe.py | dom-accessibility-api name agreement vs Chromium AX names, in-page cost |
| marionette_client.py | tiny Marionette client; launches headless Firefox with `-remote-allow-system-access` |
| firefox_a11y_probe.py | privileged chrome-JS walk of the parent-process RemoteAccessible mirror (nsIAccessibilityService) |
| firefox_content_actor_probe.py + ff_actor/ | privileged JSWindowActor in the CONTENT process walking LocalAccessible; compact serialization; shipping cost |
| firefox_events_probe.py | `accessible-event` observer in the parent: event types/counts per mutation |
| firefox_frames_probe.py | cross-site iframe, closed shadow root, canvas fallback visible in the privileged walk |
| private_a11y_env.sh | private dbus session + a11y bus + registryd (short XDG_RUNTIME_DIR: sun_path limit 108) |
| atspi_tuned_walk.py | P2P + pipelined async + multi-process walk, `Collection.GetTree`, GetMatches |
| run_chrome_atspi.sh / run_firefox_atspi.sh | wrappers: private env + browser + atspi_tuned_walk (Firefox is HEADED, tiny window, can briefly take focus) |
| active_app.py | read-only focus check on the real a11y bus |
