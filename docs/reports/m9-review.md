# M9 review (Chromium/Chrome), round 1

2026-10-17, reviewer. Branch `worktree-agent-a5bd784f7a1f0f564`, head `db34b59` (code head `ebead6e`).
Review worktree: `.claude/worktrees/m9-review` (detached at `db34b59`).
Probes: `bench/results/e2e/m9-review/probes/m9-review-probes.patch` (debug-log probes, unit and fake-peer
probes, and the candidate FCE1 fix). E2E results: `bench/results/e2e/m9-review-seq1/` (head plus log
probes) and `bench/results/e2e/m9-review-seq2-fix/` (head plus probes plus the candidate fix).

## Verdict: MERGE AFTER FIXES

The wake call, the static-leaves run layer, the zero-length-delete repair and the settle window are sound
and data-driven (ADR-0008 holds: core logic has no Chromium names, only shape rules and connector numbers).
The merge is blocked by three things: a merge compile break; the FCE1 "browser stall", which is a gorca
text-layer bug and not a browser stall; and the off-screen frame wake, whose page scrolling can be left
unrestored and outlives its document.

## Findings (ranked)

### H-1 FCE1 key loss is a gorca bug (text layer), not Chromium dropping keys. Pre-existing, exposed by Chromium.
- Claim in `m9-impl.md` section 3 ("the lost keys never appear as `text key` in the reader log") is false.
  The impl's own artifact `bench/results/e2e/m9i-fce1b/reader-20261017T034006Z.log` (lines 6437-6480) logs
  every lost key as `text key` and every caret, edit and delete event on the paragraphs. Nothing is fetched
  or said for them.
- Mechanism (verified with log probes, seq1):
  1. Chromium's `text-caret-moved(0)` on the contenteditable is handled before the focus event (separate
     lanes). The mode is still Browse, so the event is dropped (`PROBE caret: text not active mode=Browse`).
  2. The focus then switches the mode to Focus, and `text_track` sends the window prefetch. In the same turn,
     `text_after` -> `text_mode_changed` (`crates/gorca-core/src/text/mod.rs:1168`) sends `In::Dirty`, which bumps
     the window revision. The prefetch reply is therefore always stale: `PROBE stray reply fresh=false` on
     **every** track in both runs (30/30).
  3. `Tracked::hosts` is learned only from a fresh window fill (`note_hosts`, `caret.rs:1287/1730`). In the
     normal order, the browser's caret-placing event triggers a second fetch that sets it. When that event
     came first, `hosts` stays false for the whole focus. `text_accepts` (`text/mod.rs` `!t.hosts`) then
     refuses every caret, edit and selection event of the paragraphs (`PROBE accepts: refused (kind/hosts)`,
     21 per lost repeat). Right, Backspace and Up are silent; typed `x`/`y` still echo from the key.
- Rate: seq1 (head, load 10-19) FCE1 lost 15 keys in **4 of 5** repeats. All 4 had the caret-before-focus
  order, and the 1 good repeat had focus first.
- Product impact: Tab into a Chromium contenteditable (Gmail compose, GitHub comment) can leave arrows and
  Backspace mute for that whole focus. This is silent key loss.
- Unit repro: `text::tests::probe_m9r::probe_m9r_mode_flip_in_the_focus_turn_loses_the_paragraph` FAILS. Its
  control (no mode flip) passes.
- Candidate fix (in the patch): in `stray()`, a stale window fill still runs `note_hosts`, because whether the
  root hosts embedded objects does not depend on the window revision. With it, the probe passes, 131/131
  `text::` tests pass, and seq2 (load about 20) gives FCE1 lost 0 and doubles 0 in **5 of 5** repeats,
  including a repeat with the caret-before-focus order (log `04:51:26.513`). FS2 lost 0, FSA1/3 lost 0.
- Better fix, owner rust-engineer: (a) do not send `In::Dirty` for a tracking started in the same turn (the
  prefetch is already current), and (b) in `text_accepts`, accept `Ancestry::Inside` without `hosts`. Also
  keep the fix's regression test.

### H-2 Merge with main does not compile (`--all-targets`, `cargo test --workspace`)
- `git merge-tree main <branch>` conflicts only in `docs/decisions.md`. However, main's new
  `crates/gorca-daemon/tests/term_parity.rs:77` and `crates/gorca-ext-host/examples/x1_term.rs:44` build
  `AppInfo` without `page_events_settle_ms`, which gives E0063 after the merge. Fix: add
  `page_events_settle_ms: 0` to both.
- `present/flow.rs`: both sides added the identical `description: false` at the same line, so it merges to
  one field. There is no double fix.

### M-1 Off-screen frame wake: the page is left scrolled when no "stateful" node is showing
- `source.rs` `note_sleeping_frames`: `scroll_back` is the first showing node with an `is_stateful` role
  (links, buttons, list items, entries, cells). Headings and paragraphs are not stateful roles. An
  article whose first screen holds only text, with an embedded cross-site frame below (a typical blog
  with a video), is scrolled to the frame and **never scrolled back**.
- Probe: `probe_m9r_frame_wake_scrolls_the_page_back` FAILS: `calls=[(frame, 6)]`, no restore.
- Even when a node is found, `ScrollTo(TOP_LEFT)` aligns that node to the viewport top. It does not
  restore the previous scroll offset (it shifts by the node's y).
- The scroll-back runs unconditionally after all frames. This takes about 0.8 s per non-populating frame
  and up to 12 frames, so about 10 s typical and about 59 s worst case at the 300 ms call timeout. It
  overrides any scroll the user or gorca's own browse cursor made in that window, so the viewport and the
  browse cursor (and the M8 Tab grab logic that depends on the viewport) can diverge.
- Side effects not measured: IntersectionObserver lazy loads, infinite scroll, ad impressions and
  autoplay-on-visible are triggered by every page load that has off-screen OOPIFs (ads are typical: up to
  12 per page).
- Fix: wake a sleeping frame on demand, when the browse cursor or quick nav reaches it (the user is there,
  so no scroll-back is needed). If the eager wake is kept, restore with saved scroll offsets
  (`Component.GetExtents` on the document before and after) and skip the restore if the viewport moved
  since.

### M-2 Frame wake task outlives its document (missing cancellation)
- `wake_frames` uses a detached `tokio::spawn` that is not tied to `ctx.shutdown`.
- Probe `probe_m9r_frame_wake_stops_with_the_source` FAILS: all 4 frames of a closed document are still
  scrolled after shutdown.
- Navigating away mid-wake keeps issuing `ScrollTo` on old paths for seconds, and it uses the source's
  round-robin lanes, which the next document's build shares.
- Fix: `tokio::select!` on `ctx.shutdown.cancelled()` (or put the task in a JoinSet owned by `Live`).

### L-1 Frame-document focus suppression is engine-agnostic and silences Firefox Tab stops (unmeasured)
- `browse/commands.rs` +944: any nested `DocumentWeb` (Cat::FRAME) taking focus in browse mode is silent.
  Firefox stops Tab on frame documents (M8 R-5).
- That Tab is now mute, and a later legitimate focus of the same frame document can be swallowed by a
  stale `suppress_focus`.
- The corpus cannot see this: the rig never delivers Tab to the browser (M8 note).
- Fix: suppress only if a descendant focus follows within a short window (defer), or gate behind a
  connector row.

### L-2 Settle window also mutes the first real page after a browser launch
- `page_events_settle_ms` = 3000 from app appearance drops `HubMsg::Loaded` (DocRouter) and auto-read
  (`is_settling`).
- When Chrome is started after gorca, the page it opens in its first 3 s is a real load but gets no
  auto-read (Firefox does).
- A >3 s initial build (100k-node page: 27 s in the baseline) is then treated as a page load.
- Accept for now, and document it in the checklist.

### L-3 FP1 F4 harness change: justified, but it moves Alt+Down coverage to the checklist
- `rd.py` sends Alt down, Down, Down up, Alt up (a real chord), and gorca does not take it (`took` false).
  Whatever eats the following Escape is between Chromium and the page, outside gorca's path in this rig.
  The change does not hide a gorca bug.
- Not verified that the cause is a Chromium menu gesture. The checklist row (Alt+Down on the Combo field)
  must stay.

### L-4 Doc-source quirk selection by name
- `factory.rs::is_blink` (pre-existing) now switches three quirks by app-name sniffing, separate from the
  connector's `Toolkit::classify`. The two copies can drift, and the `SourceConfig.quirks` comment ("never
  decided here from a name") is not true.
- Fix: pass the connector's row values through `AppInfo`, as `text_insert_slack` already is.

### Info
- Wake call: one `GetAttributes` and one `GetRelationSet` with a 5 s Startup deadline, once per
  connection, in the AppServer's JoinSet. It is bounded and safe.
- For Electron, the cost is the app building its full tree. This is intended (Orca does the same) but
  unmeasured, and only on the checklist.
- Observed in seq1 FS2: the field entry is spoken as "focus mode, Select, edit", then "..., edit,
  abcdefghijklmnopqrstuvwxyz", then the line again (`lost-fs2-1792210878-state.txt`). That doubles the field
  value at entry. Not checked against Firefox.
- Chromium `GetStringAtOffset` returns an inverted range at the end of the text (FSA, offset 6000), so no
  line is spoken for a caret at the end. Not M9 scope.

## Measurements

| Run | Build | Load | FCE1 lost (max / repeats lost) | FS2 lost max | FSA1/3 lost |
|---|---|---|---|---|---|
| m9-review-seq1 | db34b59 + log probes | 10-19 | 15 / 4 of 5 | 4 (fc_extra 2) | 0 / 0 |
| m9-review-seq2-fix | + `stray` hosts fix | about 20 | 0 / 0 of 5 | 0 | 0 / 0 |

Sequence: `--only FS2,FSA1,FSA3,FCE1 --repeats 5`, `M7_RUST_LOG=...,gorca_text=debug`, CfT 153,
`M9_AT=isenabled`, governor performance.

Gates at `db34b59` (no probes): `cargo fmt --check` OK; `cargo clippy --workspace --all-targets -D warnings`
OK; `cargo test --workspace`: all green, 0 failed (load 25).

## Must fix before merge
H-1 (fix and its regression test), H-2, M-1, M-2. L-1 should be fixed or measured on Firefox with a real Tab.
