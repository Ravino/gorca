# M9 fix round verification (18df6725..e5becadb)

Date: 2026-10-17. Reviewer. Scope: fix commits 594afa89, fc23362a, 5a4bc508, b261b4f9, a64b618b, eff0ad4c,
bd4b4007 and the browse/present.rs resolution in merge e5becadb.

Verdict: **MERGE AFTER FIXES** (one small must-fix, F-1).

## Commands
- `cargo test -p gorca-core -j4 -- --test-threads=4`: 697 passed, 0 failed, 1 ignored.
- `cargo test -p gorca-doc-source-atspi --test fake_peer -- wake`: 5 passed (frame_wake_* incl. exact restore,
  user-scroll yield, stops with the source).
- Clippy not re-run (reported green; no code change since).

## Findings

### F-1 (Medium, must-fix) M-2 only half fixed: the frame wake is not cancelled when the document ends
`crates/gorca-doc-source-atspi/src/source.rs:768` spawns `wake::run` with `ctx.shutdown` (a child of the
per-document token). Nobody cancels that token when the document ends from the source side:
`crates/gorca-core/src/doc_actor/mod.rs:463` creates `src_shutdown` and after the main loop only calls
`src_task.abort_all()` (aborting the attach future does not reach the detached `tokio::spawn`);
`crates/gorca-core/src/doc_registry.rs` `on_doc_ended` removes the token without `cancel()`. The only cancellers are
`HubMsg::Gone` (app exit, line 525) and reader shutdown.
Scenario: Chromium page with off-screen iframes, the user navigates or closes the tab within the wake (up to
12 frames x ~0.8 s + call timeouts): the source returns `Detached`, the actor retires, the wake keeps issuing
`ScrollTo`/`GetChildren`/`ScrollToPoint` against the old document. Mostly fails fast on defunct paths, but the
commit's claim "ends with its document" is false and `frame_wake_stops_with_the_source` only covers a direct
token cancel. Fix: `src_shutdown.cancel()` right after `'main` in doc_actor `run` (and/or `t.cancel()` in
`on_doc_ended`); add an actor-level test.

### F-2 (Low) Frame-focus deferred announcement drops the mode word / tail
`browse/commands.rs` `web_frame_focus_timer` re-presents `p.snap` without `apply_web_extra`, and logs it as
`PresentSource::MissReply`. A browse-mode word queued on the swallowed focus utterance is lost. Cosmetic.

### F-3 (Low, unverified) Wake restore assumes instant scroll
`wake.rs` samples the reference 60 ms after `ScrollTo` and treats any later movement as a user scroll. A page
with CSS `scroll-behavior: smooth` (if Blink honours it for AT scrolls) would read as "moved by the user" and the
page is left scrolled. Also no reference among the first 16 nodes in view (page already scrolled) means no wake.
Not reproduced; worth one e2e probe.

### F-4 (Low) early caret replay accepts an Unknown-ancestry node
`text/mod.rs` `replay_early_caret` goes through `text_accepts`, which retargets on `Ancestry::Unknown` for caret
events. A stale browse-mode caret (<1.5 s) on an unmirrored node could retarget a just-focused contenteditable when
the browser sends no fresh caret on focus. Mirrored nodes are `Outside` and rejected, so Firefox is safe.

### Nits
- `proc.rs` `age_ms_at`: `format!(..)[..2]` slices by byte; use `.get(..2)?`. `/proc/uptime` is ASCII, no live panic.
- `own_start_ticks()` first call is a sync `/proc/self/stat` read on the executor (once).

## Checked, no issue
- Every wake IPC carries a `Deadline`; `age_ms_async` has a 250 ms timeout; no new `unwrap` on runtime paths.
- `tracked_this_turn` is consumed and reset in the same turn; `early_caret` cleared on untrack, accepted caret, expiry.
- `frame_focus` timer re-arms correctly, is void when focus moved on (Chromium) or a key bumped the epoch.
- `WebEngine` from the connector row; `is_gecko`/`is_blink` no longer sniff names; Firefox classification unchanged.
- Fresh-launch rule: only apps started after the reader and younger than the window; pure-function test covers edges.
- Merge in `browse/present.rs`: `t(M::WebFrame)` kept, holder title logic kept; Gecko internal frame names give the same
  result.
- K-1..K-3 in m9-impl.md section 5.4 match docs/reports/m9-gate-triage.md (main) honestly, including K-2 on main FF.
