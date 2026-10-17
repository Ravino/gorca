# ADR-0018: Reviewing a live terminal

Status: Proposed (draft, 2026-10-17). Full design and evidence: `docs/reports/terminal-live-review.md`.
Amends: ADR-0006 RA-1 and LS-1 (terminal re-anchoring), ADR-0001 T-2 (`ext-vte` publishes row continuity).

## Context

Flat review in GNOME Terminal resets to the start of the line on every word/character key while output
streams. Cause: every screen or caret change bumps the `ext-vte` screen generation
(`extensions/ext-vte/src/session.rs:722,768,770,849`); a review step then rebuilds the terminal
`LineModel` (`crates/gorca-core/src/review/terminal.rs:111`), and `install_model`/`reanchor` re-anchor a
terminal position to the nearest visible row at zone 0, offset 0
(`crates/gorca-core/src/review/flat.rs:90-123`). Row anchoring also drifts in content as the screen
scrolls, and a review key may wait up to 400 ms for a screen re-read during floods
(`crates/gorca-core/src/terminal.rs:287`). VTE exposes only the visible screen and reports scrolls as
delete/insert edits, with whole-screen replacement when output outruns a frame
(`refs/vte/src/vteaccess.cc:509`, `refs/vte/src/vte.cc:6906`). NVDA's console review is stable because
its position is a buffer-anchored UIA range (`refs/nvda/source/NVDAObjects/UIA/winConsoleUIA.py:39-112`);
Orca restores `(line, zone, word, char)` by index and drifts like gorca
(`refs/orca/src/orca/flat_review_presenter.py:337-357`).

## Decision

1. **Terminal review positions are content-anchored, not row-anchored**, on scrolling surfaces. A
   word/char move never changes the cursor's line and never resets its column (only a clamp to a
   rewritten line's length). Row anchoring remains for grid surfaces (alternate screen: vim, htop, less).
2. **No silent jumps.** When the anchored line is gone (trimmed, cleared, continuity lost, scrolled out of
   everything held) the next step names the reason once and lands on a defined row (RA-1 made concrete).
3. **Review keys never wait on IPC when a held screen exists**: speak first, re-read in the background,
   correct only a wrong line (RV-1).
4. **Review words are `gorca-text` units**, the same as caret navigation (ADR-0007).
5. **Phase 2 (now): Option A**, core only, no WIT change: `ZoneAnchor` gains `line_hash`; terminal
   re-anchor keeps `(zone, offset)` on the same content using a bounded shift estimate; intra-line moves
   use the held line when it left the screen; no rebuild on caret-only generations; no flood wait.
6. **Phase 3 (M16a inside M16, after the `ext-vte` WASM port): Option C**, a layered model: `ext-vte`
   publishes row continuity (`epoch`, `abs_top`, `TermSurface::{Scrolling,Grid}`, `Committed` lines with
   explicit `Gap{None,Known,Unknown}`); the core owns a generic bounded `LineLog` (absolute rows, gaps,
   prompt/last-read/bookmark marks), `ReviewPos::Log(TermPos{epoch,row,col,hash})`, the `TermReview`
   state machine `{Off, Anchored, Frozen, Lost{why}}`, freeze, jump to last read, prompt navigation,
   find, and output policies `terminal.review_output` and `terminal.flood_speech`.

Interface sketch (additive WIT change, Phase 3):

```rust
pub struct TermSnapshot { pub text: String, pub caret: u32, pub generation: u64,
                          pub epoch: u32, pub abs_top: u64, pub surface: TermSurface }
pub enum TermSurface { Scrolling, Grid }
pub enum Gap { None, Known(u64), Unknown }
// TermAction::Committed { epoch: u32, first: u64, lines: Vec<String>, gap_before: Gap }
pub struct TermPos { pub epoch: u32, pub row: u64, pub col: u32, pub hash: u64 }
pub enum TermReview {
    Off,
    Anchored { pos: TermPos },
    Frozen { pos: TermPos, watermark: u64, unseen: u64, notice_at: std::time::Instant },
    Lost { last: TermPos, why: LostWhy },
}
pub enum LostWhy { Trimmed, EpochBreak, Cleared, AltScreen, Untracked }
```

Transition table and invariants I1-I7: report §7.

## Alternatives

- Keep row anchoring, carry only the offset: fixes the reset, not the drift; rejected.
- Orca-style index restore: drifts on scroll; rejected.
- NVDA follow-caret in terminals: moves review to the bottom on each output line; rejected while reviewing.
- Freeze only (Option B) without content anchors: still one screen, still resets after unfreeze; kept
  as a feature of C.
- Scroll VTE by injecting Shift+PgUp to reach scrollback: writes to the user's session; rejected.
- Line store inside `ext-vte`: duplicates review logic per app and blocks reuse for other live logs;
  rejected (store is generic core; continuity inference is extension policy).
- Absolute rows from the gnome-terminal scrollbar `Value` (VTE adjustment = `scroll_delta`): preferred
  source if probe P-T1 confirms it; scroll inference is the fallback. Decided by probe, no bake-off.

## Consequences

- Phase 2 fix is local to `gorca-core/src/review/{flat,terminal,commands}.rs`; existing VS/VV gates
  stay.
- Phase 3 extends the terminal WIT once (after the WASM port) and adds a bounded log (default 20k lines,
  16 MiB).
- A flood that outruns VTE's frame produces explicit "lines not captured" gaps: no AT-SPI reader can get
  those lines; upstream VTE scrollback exposure is a track-U item.

## Metrics to validate

- Unit regression T-A1 fails on main, passes after A.
- State model invariants `review-drift`, `review-reset`, `review-wait`: 0 violations in 50k sequences.
- E2E VL on real gnome-terminal: `vl_word_wrong` 0, `vl_reset` 0, `vl_lost_unannounced` 0, review
  key-to-speech p99 <= 15 ms at 100 lines/s, `vl_silent` 0; for C, `vl_gap_announced` on the 200k-line
  burst and log RSS within cap.
- No regression in VS (`vs_stale` 0), VV, V2 (CPU, socket queue) and V3 (Ctrl+C stop).

## Open questions

P-T1 scrollbar `Value` reachability; P-T2 VTE events for `clear`, Ctrl+L, `reset`, alternate screen,
resize; P-T3 scroll-on-output default; user confirmation of default output policies and phrases.
