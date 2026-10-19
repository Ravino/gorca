# M22a-C1a: concurrent `ATTRS` on the focus fetch, and the M-6b cold-focus measurement

## What changed

- `gorca-connector-atspi`: `fetch_node` joins `Accessible.GetAttributes` into the same
  `tokio::join!` batch as role, state, name, description, parent and child count (one more
  pipelined call, no extra round trip). `have` carries `ATTRS` only when the call was answered;
  a D-Bus error reply is an answer (empty attributes), a timeout leaves the node poorer.
  `RawNode.attrs` keeps only the attributes the core understands (`decode::attr_key`, the
  well-known `web_attrs` keys), at most 32, values clipped to 256 bytes. `class` and `id` are
  never kept, so a Chromium `class` of tens of KB costs wire time only, not memory.
- `gorca-core`: `FOCUS_FIELDS` gains `ATTRS`; `NAV_MASK` (review and object navigation) gains
  `ATTRS`. `FOCUS_FIELDS_LAZY` and `focus_fields()` are the M-6b fallback: `GORCA_LAZY_ATTRS=1`
  (read by `gorca-daemon`) takes `ATTRS` out of the cold focus fetch again.
- Web relations (`labelled-by`, `details`, `error-message`) are NOT added to the focus fetch:
  `fetch_node` has no generic `GetRelationSet` path and nothing consumes them before C1b/C2.
  The existing `error-message` read (invalid controls) is unchanged.
- Semantics consequence: a mirror node that never had `ATTRS` (a `Cache.GetItems` row) is now a
  `Partial { missing: ATTRS }` focus hit instead of a full `Hit`. It is still spoken from memory
  first (ATTRS is non-blocking, only NAME/ROLE force a fetch), tagged `may_refine`, and the
  revalidation fills it. Test `a_partial_node_missing_only_non_blocking_fields_is_spoken` pins it.

## Harness

New e2e scenario `CF` (`crates/gorca-bench/src/e2e/web.rs`, run through
`tests/web/m7/run_m7.sh <label> --only CF`, engine by `GORCA_M7_BROWSER=firefox|chromium|gtk4`):
150 text fields never fetched before, each trial focuses the next one and measures focus event
arrival on the bus to the first speech (gorca side). Firefox and Chromium: `tests/web` page
generated in the harness dir, every field with a `class` of about 5 KB; focus by `GrabFocus`.
GTK4: `tests/harness/c1a_gtk4.py`, a real GTK4 window; GTK4 answers `GrabFocus` with
`NotSupported`, so Tab through the compositor's virtual keyboard moves the focus.
A/B arm: `--reader-env GORCA_LAZY_ATTRS=1` is the "before" (pre-C1a mask) with the same binary.
`tests/web/m7/ax.py` gained `list` and the `gtk4` engine.

## M-6b numbers (private sessions, 147 samples per run, load average 7 to 12, other agents running)

Interleaved on/off, `--repeats 1`, fresh application per run.

| Engine | runs per arm | p50 off / on (ms) | p95 off / on | p99 median off / on | delta p99 |
|---|---|---|---|---|---|
| GTK4 (real) | 3 | 0.47 / 0.50 | 0.8 / 0.8 | 0.83 / 0.83 | +0.00 to +0.05 ms |
| Chromium 154 (5 KB `class`) | 3 | 3.80 / 3.84 | - | 5.81 / 6.13 | +0.3 ms |
| Firefox ESR | 11 | 3.77 / 3.76 | 13.95 / 13.95 | 16.8 / 16.8 | 0.0 ms |

Gate: delta p99 <= 2 ms: **PASS on all three**, the lazy fallback is not needed. Raw rows:
`bench/results/e2e/c1a-{gtk4,firefox,chromium}-{on,off}-r*`. Firefox tail: p99 per run is
noisy in both arms (14 to 42 ms), p95 is a flat 14 ms in both arms: a Firefox-side latency step
that is not related to C1a (present with `ATTRS` off). Worth its own look: it is the closest
thing to the 15 ms budget in the cold focus path.

Fake app (`L1`, cold focus of 1200 distinct nodes, gorca side `l1g_ms`) and key to speech (`K1`),
3 interleaved rounds of 3 repeats: L1 p99 on 5.888/5.905/5.920 vs off 5.889/5.865/5.892 ms;
K1 p99 on 0.909/0.866/0.919 vs off 0.903/0.878/0.884 ms (deltas under 0.04 ms).
Logs under `bench/results/e2e/c1a-lk-*`.

## Open

- `GetAttributes` was not counted on the wire in the e2e runs; the unit test
  `fetch_asks_for_attributes_only_when_wanted` proves the call against a fake application, the
  e2e deltas show it costs nothing measurable.
- Firefox hung-tree cases (`<input type=date>`) were not exercised: the deadline of the batch
  bounds a slow `GetAttributes` like any other call, but nothing was measured for it.
