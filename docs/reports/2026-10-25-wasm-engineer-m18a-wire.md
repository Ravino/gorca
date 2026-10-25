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

**Not gate-quality, and the gate is not met on this host.** Every run had loadavg 11-20 on 16 cores (other
agents' benchmarks) and deep C-states on (`bench/b1/quiet-host.sh` needs the user and was not touched); all
rows say `quiet_violated=true`. Last row (release, load 10.9): writer turn p50 390 us, p99 653 us (off: p99
6 us), so **Delta p99 = 647 us against the 300 us gate**; facts at first utterance 29 % overall, 57 % for the
weakest plugin, against 99 % / 97 %; **every fact arrived as a refinement within 15 ms (100 %)** and no plugin
was masked. Reading: the wait itself ends at D = 0.3 ms by construction; a derive of the Telegram row is
55-170 us of guest time per plugin, so ten of them inside 0.3 ms need ten idle cores to wake together,
which a busy host with deep C-states does not give; the 350 us beyond D at p99 is late wake-up of the
writer plus admission. Two defects found by these runs are fixed: the wall-overrun rule demoted/masked
healthy plugins (the basis was the crowded fuel share, then wall time of a descheduled thread; now the per-call cap and
thread CPU time), and a plugin's interactive out-of-fuel is a refinement, not a strike. The earlier rows
in `bench/results/plugins/px6-2026-10-25.txt` are kept (they show the defect: 0.3 % in time, masked plugins).
The gate must be re-run on a quiet host (`sudo bench/b1/quiet-host.sh on`, release build, the user's
approval); until then the PX-6 gate is open.

## Latency options measured (M18a-2 review fixes), normal host idle states

Same test (`plugin_latency.rs`, release, 10 plugins, 400 focus turns 24 ms apart), loadavg 10-12 on 16 cores,
C2/C3 idle states enabled (the owner's configuration; `quiet-host.sh` not used). Rows are in
`bench/results/plugins/px6-2026-10-25.txt`, `debug_log=false`, all `quiet_violated=true`.

| Variant | turn p50 / p99 (us) | facts in time, overall / weakest | admit per turn |
|---|---|---|---|
| A baseline: parking writer, no pre-wake | 429 / 566 | 37 % / 62 % | 55 us |
| B pre-wake on key-down, parking writer | 404 / 4821 (outliers) | 57 % / 80 % | |
| E writer spins the window, no pre-wake | 333 / 426 | 5 % / 15 % | |
| **C pre-wake + writer spins the window (shipped)** | **335 / 434** | **71 % / 93 %** | 60 us |
| D = C + nine `refinement` plugins | 241 / 4338 (outliers) | real plugin 94 %, the nine are refinements by design | 53 us |

Reading: the pre-wake is what lifts coverage (the workers are running when the job arrives); the spinning
writer is what keeps the tail near D (a parked writer wakes late). Admission was ~100 us before the
message facts were resolved on the worker thread. The rare 4-5 ms outliers (max in B, C, D) are the
host scheduler preempting a busy-spinning thread on a loaded machine; they are in every variant that
spins and do not appear in A or E at 400 trials only by luck of the draw. **Delta p99 is 428 us against the
300 us gate and 71 % / 93 % against 99 % / 97 %: the gate is still open on this host**, every fact still
arrives as a refinement (100 % within 15 ms). Priority `refinement` removes plugins from the countdown (p50
drops to 241 us); it is the tool for anything heavier than a row name, and it does not change the first
utterance of the plugin that matters.

## Gaps

See ADR-0026 section 2.9 "Gaps found while implementing" (12 items). The ones that need an owner decision:
no Patch source class in the frozen registry (1), fact values cannot carry message ids (2), flatpak identity
needs the portal (section 2.7), the 10-plugin cost: a Telegram derive at 55-170 us suggests B-2 for a
plugin reading a 12-node subtree is too small to share ten ways.
