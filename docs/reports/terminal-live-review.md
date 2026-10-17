# Reviewing a live terminal (flat review while output streams)

Date: 2026-10-17. Author role: architect. Status: design proposal; draft ADR `docs/adr/0018-terminal-live-review.md`.
Code read at `bfbb3295` (worktree of main). Read-only: no code was changed.

## 1. User report

GNOME Terminal (VTE). Flat review by line/word/character works on a static screen. While output streams
(logs, sometimes hundreds of thousands of lines), every word or character review key jumps back to the
start of the line whenever the terminal updates.

## 2. Root cause (verified in code)

It is neither intended invalidation nor a deliberate re-snapshot policy. It is the terminal branch of the
RA-1 re-anchoring rule, which throws away the in-line offset, triggered by a model rebuild that happens on
almost every review key while output flows.

Chain, step by step:

1. **Every screen or caret change bumps the screen generation.** `ext-vte` `Session::on_edit`
   (`extensions/ext-vte/src/session.rs:722`), `on_caret` (`:768`, `:770`, also for a caret-only move) and
   `on_screen` (`:849`) all do `self.generation += 1`. A streaming program produces text-changed and
   caret-moved events continuously, so the generation changes between any two review keys.
2. **A review step rebuilds the model when the generation differs.** `exec_flat`
   (`crates/gorca-core/src/review/commands.rs:899`) calls `term_review_model`
   (`crates/gorca-core/src/review/terminal.rs:111`), which reuses `review.cur` only when
   `m.built_rev == snap.generation`; otherwise it builds a new `LineModel` (new `ModelId`) and calls
   `install_model`.
3. **The cursor is re-anchored to the new model by visible row, at offset 0.** `install_model`
   (`commands.rs:1193`) re-anchors any `FlatPos` whose `model` differs:
   `reanchor(model, p.anchor).or_else(|| nearest_line(..))`. For `LineSource::Terminal`, `reanchor`
   (`crates/gorca-core/src/review/flat.rs:90`) returns `nearest_line(model, a.y_mid)` directly ("a
   screen is a grid: the same row is the same place"), and `nearest_line` (`flat.rs:110`) returns
   `pos_at(model, best, 0, 0)`: **zone 0, offset 0**. `ZoneAnchor` (`cursor.rs:40`) carries
   `node, src_start, y_mid` only; the offset in `FlatPos.offset` is never carried over. `flat_pos`
   (`commands.rs:880`) has the same fallback for a position of an older model.
4. `flat_step(.., NextWord)` then runs from offset 0, so "next word" always reads the second word of the
   row and "next character" the second character: the user hears the line start again and again.

Two further defects sit on the same path:

- **Row anchoring drifts in content.** Even a correct offset would be applied to the *same visible row
  index*, but the text on that row moves up as the screen scrolls. With output at 10 lines/s the user's
  line is gone from under the cursor after one key; the next word comes from another line. A screen is a
  grid only for full-screen programs (vim, htop, mc, less); for a scrolling shell the identity of a line
  is its content and its absolute buffer row, not its visible row.
- **A review key can wait up to 400 ms during a flood.** `term_review_untrusted`
  (`crates/gorca-core/src/terminal.rs:287`, from the 2026-10-17 stale-screen fix, report
  `docs/reports/terminal-review-stale.md`) re-reads the whole screen and suspends the step
  (`TERM_WAIT`) whenever events were lost. Under a flood the coalescer loses/collapses events
  constantly, so most review keys pay a `GetText` round trip, against the 15 ms keypress-to-speech goal.

Related observations (not the bug, but they shape the design):

- In review mode terminal output speech is **dropped, not deferred**: `term_step` speaks only when
  `text_active()` (`terminal.rs:632`), which is false in `Mode::Review`. The user gets no hint that new
  output arrived while reviewing.
- Review word units are whitespace-only `SimpleUnits` (`review/segment.rs:51`), not the NVDA word rule in
  `gorca-text` (ADR-0007) used by caret word navigation. The Phase 3 "unified editing navigation" row
  (docs/plan/phase-3.md:64) makes the terminal caret/word model the reference for every editor; review
  must use the same `gorca-text` units, otherwise review word and caret word disagree in the terminal
  itself (`ls -l --color=auto`, `a/b/c`, punctuation runs).
- Existing tests never move the generation between two intra-line review keys
  (`review/tests.rs:1145..1333`), so the reset was never caught. The VS e2e gate (stale screen) measures
  line numbers, not in-line positions.

## 3. What VTE exposes (verified in `refs/vte`, installed libvte-2.91-0 0.80.1, gnome-terminal 3.56.2)

- **Visible screen only.** The accessible text is `get_text_displayed_a11y` = rows
  `scroll_delta .. scroll_delta + row_count` (`refs/vte/src/vte.cc:6906`). Scrollback is not
  reachable through the Text interface. NVDA on Windows consoles gets the whole buffer
  (see §4); we do not.
- **Scrolling is reported as edits.** `_vte_terminal_accessible_text_scrolled`
  (`refs/vte/src/vteaccess.cc:509`): a scroll by fewer rows than the screen emits a delete of the rows
  that left and an insert of the rows that came in (bottom-delete/top-insert for scroll-up, top-delete/
  bottom-insert for output), plus middle delete/insert when more changed. A scroll by **at least a whole
  screen** emits "delete everything, insert everything": every line that scrolled through in between was
  never exposed to any AT. At `cat`-of-a-large-file speeds this is the normal case, so **no AT-SPI reader
  can capture every line of a flood**; gaps must be first-class in any store.
- **No absolute row number in the text events.** The GTK4 path (`vteaccess-gtk4.cc:1065`) is the same
  shape through `gtk_accessible_text_update_contents`.
- **Candidate absolute row source (unverified on the bus):** VTE's vertical adjustment value is
  `scroll_delta`, an absolute ring row that only grows; `lower` is the first retained row
  (`row_data->delta()`), see `adjust_adjustments` (`vte.cc:1978`) and `queue_adjustment_value_changed`
  (`vte.cc:1934`). gnome-terminal attaches a `GtkScrollbar` to that adjustment; a GTK3 `GtkRange`
  accessible exposes it through the AT-SPI `Value` interface. If the scrollbar node is reachable (probe
  P-T1: also with "show scrollbar" off), `CurrentValue` gives the absolute top row and
  `MinimumValue` the trim point, which would make line identity exact even through floods.
- gnome-terminal does not scroll the view on output once the user scrolled back (scroll-on-output
  defaults to off; probe P-T3 confirms the user's profile), so a VTE-side "frozen view" exists, but it
  needs keys sent to the terminal (Shift+PgUp), which a reader must not inject.

## 4. Prior art

| Reader | Review position model during live output | Lesson |
|---|---|---|
| NVDA, Windows console (UIA) | The review position is a `TextInfo` wrapping a UIA text range of the **console buffer**; the range is buffer-anchored, so output and scrolling do not move it. `ConsoleUIATextInfo` only *bounds* moves to the visible range (`refs/nvda/source/NVDAObjects/UIA/winConsoleUIA.py:39-112`); at the FORMATTED API level the whole buffer is reviewable (`:390-401`). | Content/buffer anchoring is why NVDA review is stable in a streaming console. Our equivalent must be built by us (absolute rows + content hash). |
| NVDA, review follow | `review.handleCaretMove` moves the review position to the caret on caret events when `reviewCursor.followCaret` is on and the navigator is that object (`refs/nvda/source/review.py:159-190`). In a terminal the caret moves with every output line, so with follow-caret on the review position is moved to the caret on each caret event (inferred from the code; NVDA user reports not checked here). | gorca must not move review on caret events in a terminal while the user is reviewing. ADR-0006 TX-3 specifies `review.caret_follows` (no config key exists yet, `docs/config-reference.md`); when it lands, the terminal must ignore it while in review, or default it off. |
| NVDA, LiveText/Terminal | A monitor thread waits for `textChange`, re-reads all text, diffs (`difflib` per line or DMP per char), speaks new lines; a single new character is dropped as typed (`refs/nvda/source/NVDAObjects/behaviors.py:372-538`). No throttling or summarisation beyond the diff; floods are spoken line by line until the queue is cancelled. | Same family as our T-2 diff; NVDA has no flood summary; we already do better (burst tail). |
| Orca flat review | The context is rebuilt after any `text-inserted`/`caret-moved` on the reviewed object (`_context_invalidated`, `refs/orca/src/orca/flat_review_presenter.py:174-212`); the old location `(line, zone, word, char)` is restored **by index** with fallbacks to word 0, char 0, line 0 (`:337-357`). Location is invalidated outright when focus tracking is ON (`:150-172`). The terminal script still speaks insertions while flat review is active (`refs/orca/src/orca/scripts/terminal/script.py:85-99`). | Orca keeps word/char across rebuilds (we lose them) but anchors by visible line index, so it drifts in a scrolling terminal exactly like our row rule. |
| Odilia | No flat review and no terminal support (`refs/odilia`, grep). | Nothing to borrow. |
| VoiceOver (macOS Terminal) | Not in refs; from public behaviour: VO reads Terminal through an `AXTextArea` of the scrollback, and its VO cursor stays on content as output scrolls. Unverified here. | Consistent with NVDA: content anchoring. |

## 5. Design goals for live review

- G1 A review move never changes which line the cursor is on except by the user's own line move; a
  word/char move continues from the current offset, always.
- G2 No silent jumps: when the anchored line is gone (scrolled out of everything we hold, trimmed,
  screen cleared, discontinuity), the next step says so once ("line scrolled away", "screen cleared")
  and lands on a defined place (ADR-0006 RA-1 already requires this).
- G3 Review keys never wait on IPC in the hot path: speak from held state within the 15 ms budget,
  refresh in the background, correct only if the spoken line was wrong (ADR-0006 RV-1 style).
- G4 Bounded memory and CPU under 10k+ events/s and 100k+ line floods (ADR-0001 T-1/T-2 invariants).
- G5 Extension boundary: VTE specifics (prompt marks, scroll inference, alternate screen detection,
  OSC 133 later) live in `ext-vte`; the line store, anchors, review cursor and commands are generic core
  (a "line log" source usable by any live log view, not only terminals).
- G6 One word/char model: review units are the `gorca-text` units used by caret navigation (ADR-0007).

## 6. Options

### Option A — minimal fix: content anchors on the visible screen (Phase 2 polish, ~2-3 days)

Core only, no WIT change.

- A1 **Carry the offset and the line content in the anchor.** Extend `ZoneAnchor` with
  `line_hash: u64` (FNV of the trimmed line text, as `ext-vte/src/screen.rs:15`) and re-anchor terminal
  positions with `(line_hash, offset)`:
  1. estimate the scroll shift `s` between the old and the new model once per rebuild: the `s` in
     `0..=rows` that maximises `old.hash[i] == new.hash[i - s]` (bounded: rows <= `ReviewLimits.zones`,
     early exit on a full match; O(rows x candidates) with candidates = rows where the anchored hash
     occurs, typically 1). Ties (repeated lines, `yes` output) are broken by the estimated shift, then by
     nearest row;
  2. the line at `old_row - s` with the same hash: keep zone and offset;
  3. else the same hash anywhere, nearest to `old_row - s`;
  4. else the anchored row was rewritten in place (same row, no shift, different hash: progress bar,
     readline redraw): keep the row, clamp the offset;
  5. else the line scrolled off the visible screen: **keep the held line** (see A2).
- A2 **Intra-line moves never need the new model.** Keep, next to the cursor, the line text it is on
  (`held: Arc<str>`, already in `Line.text`). Word/char moves read the held line when its content is not
  on the screen any more; line moves go to the newest model: PrevLine/NextLine from a line that left the
  screen land on the top row with the prefix "scrolled," (one word, configurable), satisfying G2.
- A3 **Do not rebuild for caret-only generations.** Rebuild only when the screen text hash changed
  (`TermSnapshot` text hash computed once in `term_review_model`); a caret-only change updates
  `focus_line` without a new `ModelId`.
- A4 **Speak first, sync after (G3).** `term_review_untrusted` no longer suspends the step when the
  terminal is tracked and has a held screen: speak from the held screen, request the re-read, and correct
  only if the spoken line's content changed (RV-1 correction counter). The wait stays for the untracked
  and never-read cases (no screen at all).
- A5 **Same word rule as the caret.** `exec_flat` uses the `gorca-text` NVDA word units for
  `LineSource::Terminal` (G6; one line in `exec_flat`, behind the existing `TextUnits` trait).

Fixes the reported symptom completely while the line stays on screen and degrades honestly when it
scrolls away. Does not give scrollback review or new-output awareness.

### Option B — review snapshot ("freeze") on top of A (small, ~2 days after A)

A toggle (`review.terminal_freeze`, default key gorca+F12-style chord TBD in ADR-0005 keymap) that pins
the current held screen as the review model:

- frozen: review reads only the pinned snapshot; new output keeps being tracked by `ext-vte` but is not
  spoken; a polite count notice ("37 new lines") at most every `freeze_notice_s` (default 10 s, setting
  `off | count | tail`), at `Prio::Notification` (never interrupts review speech, M4b rule);
- unfreezing (toggle again, or any key that leaves review) re-anchors by content into the live screen
  (A1), and says "N new lines" once.

Cheap and valuable for "let me read this error while the build goes on", but on a visible-only model
the pinned snapshot is still one screen: the user cannot scroll past it.

### Option C — layered model: line store with absolute rows (Phase 3 milestone)

```
 layer 4  commands       review moves, jump-to-last-read, prompts, bookmarks, search, freeze
 layer 3  ReviewCursor   TermPos{epoch, row: AbsRow, col, hash}  (core, generic)
 layer 2  LineLog        ring of committed lines + live screen window, gaps, marks (core, generic)
 layer 1  continuity     abs_top, epoch breaks, alt-screen, prompt marks (ext-vte policy)
 layer 0  source         AT-SPI text events + screen reads (+ Value of the scrollbar, if P-T1 holds)
```

**Layer 1, continuity (ext-vte).** The extension already diffs every reading
(`screen.rs::diff`, `Diff.old_of`). It derives the scroll amount of each reading (rows that left the
top: from the delete-at-0 of a scroll edit, or from `old_of` shifts of a re-read) and publishes, in
`TermSnapshot` (WIT record, additive change):

```rust
pub struct TermSnapshot {
    pub text: String,
    pub caret: u32,
    pub generation: u64,
    // new:
    pub epoch: u32,          // bumped on any break in row continuity
    pub abs_top: u64,        // absolute row of screen row 0 within `epoch`
    pub surface: TermSurface,
}
pub enum TermSurface { Scrolling, Grid }   // Grid = alternate screen (vim, htop, less)
```

plus a new action carrying lines that left the screen:

```rust
TermAction::Committed { epoch: u32, first: u64, lines: Vec<String>, gap_before: Gap }
pub enum Gap { None, Known(u64), Unknown }   // rows that scrolled through unseen
```

`Gap::Known` comes from the scrollbar value (P-T1) or a VTE whole-screen scroll whose row count is
known; `Unknown` from a bulk diff with no match. Epoch breaks: resize, a reading with no matched line
and no known scroll, clear-screen detected as such (P-T2), alternate-screen entry/exit, tracking reset.

**Layer 2, `LineLog` (core, generic).** A ring of committed lines keyed by `AbsRow` within an epoch,
backed by chunked `Arc<[Line]>` segments (append-only, O(1) freeze, no copying), with:
- caps: `terminal.scrollback_lines` (default 20 000) and `terminal.scrollback_bytes` (default 16 MiB),
  oldest segments evicted first; eviction emits `Trimmed(upto)`;
- gap records (`Gap{rows: Known|Unknown}`), spoken as "N lines not captured" / "lines not captured";
- marks: prompt rows (from ext-vte prompt detection now, OSC 133 later), the last-spoken output row
  ("last read"), user bookmarks (bounded, 16);
- a view = committed lines of the current epoch + the live screen rows (always the newest truth).
Only completed lines are committed (a line is committed when it leaves the screen), so `\r` progress
bars never pollute the log: they live on the screen rows until they scroll.

**Layer 3, cursor.** `ReviewPos::Screen` keeps working for GUI surfaces; terminals get
`ReviewPos::Log(TermPos)` with `TermPos{epoch, row: AbsRow, col: u32 /* scalars, TU-1 */, hash: u64}`.
`TermSurface::Grid` keeps today's row anchoring (a TUI is a grid; htop row 3 is "the same place").

**Layer 4, commands and output policy.**
- moves over the log: line/word/char as today; Top = oldest retained row, Bottom = last screen row;
- `jump to last read` (row of the last spoken output line), `previous/next prompt`, `bookmark set/jump`,
  `find in terminal` (substring, case-insensitive, over the log; bounded scan, cancellable by the next
  key; ADR-0003 find UI reused when M15 lands);
- live output while reviewing: `terminal.review_output = mute | count | speak` (default `count`:
  polite "N new lines" notice, rate-limited, never interrupting review speech);
- flood output (not in review): `terminal.flood_speech = tail | count | count_and_tail | silent`
  (default `count_and_tail`: "1200 lines" then the burst tail, generalising today's `burst_tail`);
- freeze (Option B) becomes "pin the view at a row watermark"; with a log it no longer limits the
  user to one screen.

## 7. State machine (core, per tracked terminal, Option C; A is the subset without `Frozen`/log)

```rust
pub enum TermReview {
    /// Not reviewing this terminal.
    Off,
    /// Review cursor anchored to content; the view follows new output at its bottom.
    Anchored { pos: TermPos },
    /// View pinned at `watermark`; output after it is counted, not shown.
    Frozen { pos: TermPos, watermark: AbsRow, unseen: u64, notice_at: Instant },
    /// The anchored row is gone; the next step announces `why` and re-anchors.
    Lost { last: TermPos, why: LostWhy },
}
pub enum LostWhy { Trimmed, EpochBreak, Cleared, AltScreen, Untracked }
```

| From | Event | To | Effect |
|---|---|---|---|
| Off | review key on terminal | Anchored{pos at caret row (or last read)} | speak unit |
| Anchored | word/char move | Anchored{same row, new col} | speak; never touches row |
| Anchored | line move | Anchored{row ± 1 in view} | speak; Top/Bottom boundary |
| Anchored | snapshot, same epoch, row still held | Anchored (unchanged) | none |
| Anchored | snapshot, row rewritten in place (Grid or live row) | Anchored{col clamped, hash updated} | none (optional "changed" cue) |
| Anchored | `Trimmed(upto)` with row < upto | Lost{Trimmed} | none until next key |
| Anchored | epoch break | Lost{EpochBreak / Cleared / AltScreen} | none until next key |
| Lost | any review move | Anchored{oldest row (Trimmed) / caret row (others)} | say reason once, then the unit |
| Anchored | freeze toggle | Frozen{watermark = last view row} | "frozen" |
| Frozen | output committed | Frozen{unseen += n} | count notice if due (`count` policy) |
| Frozen | freeze toggle / leave review | Anchored (re-anchor by content) | "N new lines" |
| any | blur / focus change / untracked | Off (cursor kept per terminal for return, bounded 8 terminals) | none |
| any | ExitReview | Off | output policy back to focus mode |

Invariants (checked by the state model, §9):
- I1 A word/char move keeps `pos.row` and `pos.epoch` unless it crosses a line boundary by the move's
  own semantics.
- I2 `pos.col` changes only by the user's move or by a clamp to a rewritten line's length.
- I3 A row change not caused by the user is preceded, in the same utterance, by the `LostWhy` phrase
  (G2).
- I4 `TermPos.row` always refers to the line whose hash is `pos.hash` or the cursor is `Lost`; it never
  silently names a different line.
- I5 Review key handling does not await IPC (no `Suspend` on the terminal path when a held screen
  exists); key-to-speech p99 < 15 ms of gorca-side processing.
- I6 `LineLog` memory <= caps; a frozen view holds at most `scrollback_lines` rows; eviction of frozen
  rows past the cap turns the cursor `Lost{Trimmed}`.
- I7 Within an epoch `abs_top` is monotonic non-decreasing for `Scrolling` surfaces.

Failure modes and backpressure:
- Flood: the ingress coalescer collapses to one `Replaced` per frame (ADR-0001 T-1); ext-vte reads at
  most one screen per settle; `Committed` batches are per reading, never per event. A reading whose
  scroll exceeds the screen yields `Gap::Unknown` (or `Known(n)` with P-T1): we never pretend to have
  the lines.
- ext-vte faults or is disabled (WASM 3-strike rule): the core keeps the log as is, marks
  `Lost{Untracked}`, review continues over the held log, live rows are read by the core fallback (today's
  native stand-in).
- Hostile output (huge lines): per-line cap (`MAX_SCREEN_BYTES` per screen already; add 16 KiB per
  committed line, truncated with a marker), bytes cap on the log.
- Search over 20k lines is bounded (time slice 2 ms per chunk on the cpu pool, cancelled by the next
  review key; result spoken as a normal move).

## 8. Recommendation and phasing

- **Now (Phase 2 polish, before the acceptance walk-through): Option A (A1-A5).** Small, core-only, no
  WIT change, fixes the user's exact symptom and the line drift, and removes the 400 ms flood wait.
  Owner: rust-engineer; reviewer pass. Confidence high that it fixes the report.
- **Phase 3: Option C as milestone "M16a Live terminal review" inside M16 (terminal completeness),**
  after the `ext-vte` WASM port (C0 item 3) because it extends the terminal WIT; ADR-0018 (next free
  number; 0016/0017 are reserved by phase-3-features). Option B is delivered as part of C (freeze over
  the log), not separately, unless the user asks for it earlier; then it can ship on top of A in ~2
  days.
- **Probes before C (linux-platform-engineer, read-only on a private session):** P-T1 scrollbar `Value`
  on gnome-terminal 3.56 (reachable? equals `scroll_delta`? present when the scrollbar is hidden?
  value-changed event rate under `yes`); P-T2 what VTE emits for `clear`, Ctrl+L, `reset`, `tput smcup`
  (alternate screen) and a resize; P-T3 scroll-on-output default in the user's profile. No bake-off: the
  contested point (line identity from scroll inference vs from the scrollbar value) is settled by P-T1
  measurements, with scroll inference as the always-available fallback.

Not recommended: anchoring by visible row (today), Orca's index restore (drifts), NVDA's follow-caret
in terminals (yanks the cursor), injecting Shift+PgUp to scroll VTE (writes to the user's session).

## 9. Test plan

Unit (core, `review/terminal.rs` and `review/tests.rs`):
- T-A1 regression, must fail on main: held screen "alpha bravo charlie delta"; NextWord; a new
  generation (one line of output appended, screen scrolls by one); NextWord; NextWord → "bravo",
  "charlie", "delta" (main says "bravo", "bravo", "bravo").
- T-A2 caret-only generation → same `ModelId`, no rebuild.
- T-A3 line rewritten in place (`\r` progress) → same row, offset clamped.
- T-A4 line scrolled off the screen → word moves continue on the held text; NextLine says "scrolled,"
  then the top row.
- T-A5 repeated identical lines (`y` x 50) with scroll 3 → cursor stays on the same logical line (shift
  estimation), not the nearest equal hash.
- T-A6 lost events + held screen → speech within the step, no `Suspend`; a later reading that changes
  the spoken line produces exactly one correction.
- T-A7 word units: `ls -l --color=auto` review words equal caret words (`gorca-text`).

Model-based (`crates/gorca-core/src/statemodel/`): extend `world.rs` with a terminal content model that
gives every produced line a ground-truth id: generators `Output{n}`, `OutputFlood{n > rows}`,
`RewriteBottom`, `Clear`, `AltScreen{on}`, `Resize`, `Trim`; interleaved with review word/char/line moves
and freeze toggles. New invariants: `review-drift` (I1/I4: after a word/char move the spoken unit belongs
to the line with the cursor's ground-truth id, or a `LostWhy` phrase was spoken), `review-reset` (I2: the
column never returns to 0 without a StartOfLine move or a clamp), `review-wait` (I5: no terminal review
step suspends when a held screen exists). Default 500 cases in CI, 50k in the long run.

E2E (real gnome-terminal, `crates/gorca-bench/src/e2e/m6.rs`, new step set **VL**):
- producer: a small Go or Python program printing numbered lines with distinct words
  (`L000123 alpha bravo charlie delta echo`) at 10, 100 and 1000 lines/s, then a `seq 1 200000` burst,
  a `\r` progress bar, `clear`, and `less` (alternate screen);
- script: enter review, PrevLine x3, then NextWord x5 and NextChar x5 at 150 ms intervals while output
  continues;
- gates: `vl_word_wrong` 0 (each spoken word is the expected next word of the anchored line id),
  `vl_reset` 0 (no word move speaks the first word twice), `vl_lost_unannounced` 0, review key-to-speech
  p99 <= 15 ms at 100 lines/s and <= 15 ms + rig allowance during the burst, `vl_silent` 0, reader CPU
  and RSS within the M6 V2 limits; for C additionally `vl_gap_announced` (the burst produces a "not
  captured" phrase, never invented lines), log RSS <= cap.
- keep VS (stale screen) and VV/V2/V3 as regression gates; results labelled with host load
  (2026-10-16 low-load policy).

## 10. Risks and open questions

- A1 shift estimation is heuristic on screens of identical lines with no distinct neighbour; worst case
  the cursor lands on an equal line (same text, so the spoken words are still right). C removes this with
  `abs_top`.
- P-T1 unverified: if the scrollbar is not reachable, absolute rows come only from scroll inference,
  which breaks (honest gaps) on whole-screen scrolls.
- What VTE emits on `clear`/Ctrl+L (scrolls into scrollback vs erases) is unverified (P-T2); the epoch
  logic depends on it.
- Default policies (`review_output = count`, `flood_speech = count_and_tail`, "scrolled," prefix) are UX
  choices for the user to confirm.
- Russian wording of the new phrases goes through the M12 catalog.
- C changes the terminal WIT record; must land after the ext-vte WASM port to avoid two migrations.
