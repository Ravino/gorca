# M18a wiring: plugins add facts to the fused model (document actor and app actor)

Date 2026-10-25, wasm-engineer. Branch `worktree-agent-a8189783046a42a6c`. ADR-0026 r4 + r5 notes
(`docs/adr/0026-wasm-plugin-model.md` sections 2.7 identity/threat model, 2.9 status and gaps).

## What exists

| Piece | Where |
|---|---|
| Plugin port of the fusion writers: bridge, revision token (PF-7/MQ-2), trigger rule (PF-4), S0-only rows (PF-2), wait governor, fact ops, stratum by tier | `crates/gorca-core/src/plugin_port.rs` (+ `tests.rs`, `testing.rs`) |
| Hooks | `doc_actor/fuse.rs` (`FuseStage::with_plugins`), `app_actor/fuse.rs` (`FuseWriter::set_plugins`), `Core::with_plugins` |
| Computed S1 patches in `FUSE` | `gorca-fuse` `Ivm::set_computed`, `tests/computed.rs` |
| Runtime join: packages, pins, scopes, slots, progressive filling, reply mapping | `crates/gorca-daemon/src/plugins.rs` |
| `[plugins] enabled` (default off), dirs from XDG / `GORCA_PLUGINS_*` | `gorca-config`, `gorca-daemon/src/{cli,lib,config_task}.rs` |
| WASI random/clock shim (deterministic) | `crates/gorca-plugin-host/src/wasi.rs` |
| Structured URL activation patterns | `crates/gorca-plugin-host/src/urlmatch.rs` |
| Verified application identity | `crates/gorca-connector-atspi/src/proc.rs`, `AppInfo::exe` |
| Document URL | `gorca-doc-source-atspi` `fetch_doc_url` (`DocURL` Gecko, `URI` Blink) -> `RouteChanged` hint |
| First-party plugin | `plugins/telegram-web` (Rust, wasm32-wasip2), `tools/build-plugins.sh`, `plugins/pins.txt` |

## Results

Private headless GNOME Shell world (`tests/web/telegram/run.sh`, real keys, mock SSIP), scenario `plugin`,
daemon release build, URL knob `GORCA_PLUGINS_URL_EXTRA` for the `file://` fixtures:

| Page | Browser | plugins off (control) | plugins on |
|---|---|---|---|
| A | Firefox | PASS: "BO Bob 14:59 ok thanks, button" | PASS: "Bob, ok thanks, 14:59, button"; messages are articles (quick nav) |
| A | Chromium | PASS | PASS (same) |
| K | Firefox | PASS: "Bob Bob 14:59 ok thanks, link" (doubled) | PASS: "Bob, ok thanks, 14:59, link" |
| K | Chromium | PASS | PASS (after the merged-text fallback) |

Raw runs: scratchpad `e2e-*` (not committed). The rig also showed three bugs that unit tests could not:
the revalidation overlay undid a patched name (spoken twice), the article flag did not follow the fused
role, and Chromium delivers Web K rows as one merged text run.

Unit and integration tests: `plugin_port` (10), doc and app writer hooks (6), `Ivm` computed patches (4),
URL patterns (7), identity (13), real wasm plugin through the pin list (8), 10 plugins with one slower
(no starvation: every plugin delivered 250 of 250 turns, refinements <= 1.6 ms), log redaction (1).

## Latency (PX-6) and the labelled conditions

`crates/gorca-daemon/tests/plugin_latency.rs`: the writer's focus turn (snapshot, send to 10 actors,
bounded wait, admission) with the real plugin plus nine copies, 400 turns 24 ms apart, against the same
bridge calls over a runner with no plugins. Rows are appended to `bench/results/plugins/px6-2026-10-25.txt`;
failing rows are kept.

**Not gate-quality.** Every run had loadavg 11-20 on 16 cores (other agents' benchmarks), deep C-states on
(`bench/b1/quiet-host.sh` needs the user and was not touched). All rows say `quiet_violated=true`. The runs
show: the bounded wait works (p50 41-110 us, the wait itself ends at D), a derive of the Telegram row costs
55-170 us of guest time per plugin, so ten of them in 0.3 ms depend on ten idle cores waking at once; facts
at first utterance were 0.3 - 12 % under load (and 60 % of the jobs in the quietest minute), with every
fact arriving as a refinement when the host was not oversubscribed. The gate (Delta p99 <= 0.3 ms, >= 99 %
overall, >= 97 % per plugin) is asserted only on a quiet release run; it has **not** been demonstrated.

## Gaps

See ADR-0026 section 2.9 "Gaps found while implementing" (12 items). The ones that need an owner decision:
no Patch source class in the frozen registry (1), fact values cannot carry message ids (2), flatpak identity
needs the portal (section 2.7), the 10-plugin cost: a Telegram derive at 55-170 us suggests B-2 for a
plugin reading a 12-node subtree is too small to share ten ways.
