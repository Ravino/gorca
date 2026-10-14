# M7a implementation report: browse mode for static pages in Firefox

Branch `worktree-agent-a1d08cc29c61e0bae`. Spec: `docs/design/browse-mode.md`. Review: `docs/reports/m7a-review.md`.
Rig: `tests/web/m7/run_m7.sh <label>` (private session bus, a11y bus, headless gnome-shell, private Firefox profile per launch). Results: `bench/results/e2e/<label>/`; failing runs are kept.

## What was built

Doc actor and translator, document registry with cache warm-up, browse/focus state machine, cursor moves over `gorca-text`, line presenter, quick navigation (H 1-6 K U V F E B X R C L I G S Q D T O), Enter/Space, Insert+Space, Escape grab only in auto focus mode, checklist `docs/plan/checklists/M7a.md`. Not built: Escape pass-through for an open popup, Frame and Article quick navigation (M7b).

## Review fixes (this round)

| Finding | Fix | Test |
|---|---|---|
| H1 recursion in `identity.rs` | `rebind_subtree` and `insert_fresh_subtree` are iterative (explicit frames), capped at `MAX_DEPTH`, with a visited set against duplicate paths | `identity.rs`: 100k-deep chain, 3-node cycle, 50k-deep rebind, fan-out cap |
| Other recursion in the web path | scripted scan of gorca-doc, doc-source-atspi, doc_actor, browse: only the treap (`merge`/`split`/`find_*`, O(log n) expected) and `wire.rs` signature recursion (bus caps depth at 64) remain | |
| H2 cold field becomes browse mode | `WebMode::Loading` carries `known`: the grace timer flips to browse only when the focus snapshot was read and is not a field; `web_learn_focus` updates it when the presentation reply arrives | `a_cold_unknown_focus_stays_loading_with_the_grabs_off`, `a_better_reading_of_the_pending_focus_is_kept` |
| M1 moves bypass the depth cap | after `SyncChildren`, moved subtrees are walked iteratively and cut beyond `MAX_DEPTH` | `moving_a_subtree_under_a_deep_node_cannot_pass_the_depth_cap` |
| M2 no total caps | `gorca_doc_source_api::limits`: 500k nodes per document, 100k children per node, 256 KiB text per node, 200k parked ids (deferred lists plus orphans, by total ids); counted in `TranslatorStats.truncated` / `text_clipped`, first cut logged once | fan-out, parked lists, text clip tests |
| M3 lost notices | per-document notice queue (coalesces `Changed`, keeps first-useful and gone), `Gone` is awaited with a 2 s bound on retire, a browse key on a vanished document hands the keyboard back | `browse_keys_on_a_vanished_document_hand_the_keyboard_back` |
| M4 first line after navigation | tail TTL 2 s; when the root gets focus before it has a line, the first `Changed` with content speaks it (unless a key or another focus came first, or 2 s passed) | 2 tests in `browse/tests.rs` |
| M5 dropped web refinements | a changed document title is corrected; other differences are counted in `revalidations_suppressed`, no longer as agreed | existing flow tests |
| L1 unavailable | not spoken for static web roles (document, heading, paragraph, list, table, landmark, ...); controls keep it | `static_web_roles_are_not_called_unavailable_but_controls_still_are` |
| L2 Shift+digit | not done: Mutter grabs are keysym based, so physical-key grabs need a keymap-derived symbol table (M7b, with the Cyrillic layout gap) | |
| L3 keys after a mode switch | FM1 types a letter 100 ms after "focus mode" (gated: 0 lost) and at once (information); `fm_grab_ms` measures how long the browse grabs outlive the focus event (gated 15 ms) | |

## Numbers (release, `bench/results/e2e/m7a-fix-final/20261014T064245Z.txt`, 3 repeats, gorca-side)

| Gate | p99 | Limit |
|---|---|---|
| W1 quick nav | 0.46 to 0.97 ms | 5 ms |
| BR1 line / char / word | 0.44 / 0.42 / 0.38 to 0.73 ms | 5 ms |
| W3 tab switch | 0.25 ms | 15 ms |
| FM1 focus mode word | 0.28 to 0.52 ms | 15 ms |
| FM1 grab removal after the focus event (`fm_grab_ms`) | 1.0 to 1.3 ms | 15 ms |
| FM2 Escape | 0.50 ms | 5 ms |
| keys lost 100 ms after the switch | 0 | 0 |
| keys lost at once after the switch | 30 of 30 (information) | none |
| **W2b gorca-side** | **281 to 390 ms** | **150 ms: FAIL** |

The at-once loss is the unavoidable race of grab-based quick navigation: a key sent in the 1 to 6 ms between the focus event and the grab change is Ambiguous ("again"), never executed.

## W2b: where the time goes (per-phase, median of 3 trials, 10k-node page)

Entering a never-visited 10k-node document, from the focus event on the bus to the first line spoken: 285 ms wall.

| Phase | ms |
|---|---|
| warm-up wall (`warm_domains`) | 269 |
| of which inside D-Bus calls to Firefox | 237 (2 rounds x 3 `GetTree` of top-level subtrees = 10 232 nodes sampled, 13 probes) |
| of which our own sleeps between rounds | 26 |
| locate | 0.4 |
| first block fetch (444 nodes) | 12 |
| translate and publish (first batch) | 1.1 |
| speech after publish | the rest of the wall time (about 3) |

Conclusion: nearly all of the 285 ms is gorca's own warm-up protocol on the focus path. It fetches the whole document twice to see whether Firefox's cache has stabilised, while the real first block needs 12 ms. The gate is therefore measured on gorca-side time (wall minus locate and first-block fetch, which are the time Firefox needs to answer); it fails at 273 ms median. The per-phase numbers come from `gorca_phase` log lines (`RUST_LOG=...,gorca_phase=info`, written to `bench/results/e2e/<label>/reader-*.log`).

Why this is only seen in the rig: on a real desktop the attach-time warm-up runs while the window is active, before the user arrives. In the rig the window is inactive until the first click, so attach-time warm-up reports `NoDocument` and the warm-up runs on the focus path. A rig variant with the window active before attach was not built.

Recommended fix (not done, outside this round): make the warm-up sample a bounded part of the page (the first child once, with a node cap) and not two full passes of three subtrees; expected cost about 30 ms. The old wall-time results are kept in `bench/results/e2e/m7a-final*`, `m7-w2b`, `m7-all`.

## Rig notes

- A headless gnome-shell has no keyboard, so Firefox's window is never active. `tests/web/m7/rd.py` holds a Mutter RemoteDesktop session on the private compositor bus (click, key codes).
- Firefox shows the last URL it was given; Ctrl+PageUp/PageDown keep content focus, Alt+1 leaves focus in the browser chrome.
- A killed Firefox leaves a restore prompt: every launch uses a fresh profile directory.

## Not verified

W-V1..W-V5 and M-13 e2e were not run; the live checklist was not walked in a real Firefox session. Strict K2 to K4 and R1 were run before this round and not repeated after it (no input-lane code changed).
