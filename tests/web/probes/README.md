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
