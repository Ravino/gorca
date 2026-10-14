# M3 review: native caret, typing echo, selection, say-all

Branch `worktree-agent-a15d2cdf7665c02f6`, head `54a6fef`, against `main` `0bfe951`
(3 commits, 111 files, +10.7k lines). Reviewer: adversarial review r1, 2026-10-14.

## Verdict: MERGE AFTER FIXES

The caret machine is a clean, pure, exhaustive enum state machine. All buffers are bounded, every D-Bus
text call runs under a deadline, offsets are clamped, and no `unwrap` sits on a runtime path. The typing echo
dedupe (key-first, insert-first via the 5 ms hold, late key via `flushed_ins`) is sound in the orders the
harness exercises. The gates pass. The defects are in say-all bookkeeping, a few event-order holes in the
caret machine, and a mode check that matters once M7a merges. None of them is a crash or a secret leak.

## Gates (run in the worktree, `CARGO_TARGET_DIR` = worktree `target/`)

Host: load average 3.9 to 9.4 (a B-1 soak2 judge run was on cores 0 to 15); disk 20 GB free (80 % used).

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets -D warnings` | pass |
| `cargo test --workspace` | pass: 1285 passed, 0 failed, 14 ignored |
| `tools/check-deps.sh` | pass |
| e2e quick rerun (`--quick`, 1 round, private session, load 8 to 9.5) | 0 FAIL: C1 p99 0.74 ms, C1h 0.33 ms, E1 0.38 ms (0 double), E2 0 missing/double/wrong, S2 1.33 ms, SA1 1.25 ms, SA2 gap p99 0.17 ms with ahead ratio 1.0, SA3 stop 0.36 ms / caret 0.88 ms / 0 wrong, PW 0 plaintext. Result kept in the worktree at `bench/results/e2e/m3-review-quick/20261014T062415Z.{json,txt}`. The author's numbers reproduce within noise |

## Probes (failing tests written by the reviewer)

The probes are in `bench/results/e2e/m3-review/probes/`. `probe_text.rs` is appended to
`crates/gorca-core/src/text/tests.rs` and `probe_lane.rs` goes inside `mod tests` of
`crates/gorca-speech/src/lane.rs`. Run them with `cargo test -p gorca-core --lib probe_` and
`cargo test -p gorca-speech --lib probe_`. All 5 fail at 54a6fef. The worktree was restored with
`git checkout` afterwards.

| Probe | Observed |
|---|---|
| P1 `probe_selection_event_before_caret_event` | Shift+Right with `text-selection-changed` before `text-caret-moved`: spoken `["e"]` (the character after the caret), not `"h selected"` |
| P2 `probe_say_all_lost_progress_report` | one lost `Ended`: at the end of the text no natural finish (no SetCaret); 60 s later a Down key emits `SetCaret{offset:5}` |
| P3 `probe_unrelated_caret_move_absorbed_by_stale_expectations` | typed key whose insert never comes, then a 1-char caret move 300 ms later with no key: spoken `[]` |
| P4 `probe_caret_beats_key_while_previous_slot_open` | Right at end of text (slot open), Left's caret beats the Left key: spoken `["d","d"]` |
| P5 `probe_gate_chunks_lost_when_server_down_at_start` | speech server down at say-all start, chunks 2 and 3 submitted after the 150 ms clock gate: server got `[1, 3]`, `superseded: 1`, no `Stopped(2)` reported |

## Findings, ranked

### H1. Say-all bookkeeping leaks phantom chunks: the reading never ends and a later key yanks the caret back

- Where: `crates/gorca-core/src/text/say_all.rs:246-276` (`progress`, `is_done`),
  `crates/gorca-core/src/text/mod.rs:705-740` (`say_all_finish`, `text_after`), `crates/gorca-daemon/src/wire.rs:61`
  (`let _ = text_io.try_send(...)`), `crates/gorca-speech/src/lane.rs:148-163` (a slot replacement reports nothing).
- Failure: `Run.queued` is popped only by `Ended`/`Stopped` reports. There are two ways to lose a report:
  - the `text-progress` forwarder drops it when `text_io` (64, shared with fetch results) is full;
  - the lane replaces an utterance in its interrupting slot (`superseded`) without any report (P5).

  In both cases a phantom stays in `queued`. Then `is_done()` is never true. The look-ahead shrinks by one per
  phantom (with three phantoms, say-all stalls silently). `SayAll::Running` survives the end of the text, so the
  natural finish (caret to the end) never happens. The next key press, even minutes later, runs
  `say_all_finish(false)`, which sends `SetCaret` to the start of the last chunk and overrides what the user
  just did (P2: `SetCaret{offset:5}` 60 s after the reading ended).
- The ADR changelog says "say-all then lags one chunk"; the code does not match that.
- Fix:
  - (a) Have the lane report `Stopped(id)` for every utterance it supersedes in the slot, discards as stale or
    drops from the inbox (`stale_dropped`, `superseded`, `inbox_dropped`, `Discard`).
  - (b) Give `Run` a watchdog: if no progress arrives for N × chunk-estimate (or a fixed 10 s), finish it.
  - (c) Never move the caret from a run whose last progress is older than the watchdog.
  - (d) Give progress its own channel, or make `text_io` drop progress before fetch results.

### H2. The first-chunk-alone workaround fixes the symptom; the gate's clock fallback is unsound

- Where: `say_all.rs:30` (`FIRST_CHUNK_GATE`), `say_all.rs:161-175`, `lane.rs:148-163`.
- The diagnosis is right. `interrupts = Interrupt || epoch > inflight_epoch` sends every chunk of the new
  epoch into the single slot until one has been *sent*, and later ones overwrite earlier ones.
- Sending chunk 1 alone and waiting for `Started` is sound. Opening the gate after 150 ms "when the lane has
  certainly taken it" is not: `inflight_epoch` advances only on a successful `send()`. While the client is
  Down or reconnecting (voicego restart, SSIP failover), the gate opens by the clock, chunks 2 and 3 enter the
  slot together, and chunk 2 is lost with no report (P5: server received `[1, 3]`). That feeds H1.
- Fix: the right fix is in the lane. Same-epoch, non-`Interrupt` utterances should queue in FIFO order behind
  the slot occupant instead of replacing it: compare against the slot's epoch as well as `inflight_epoch`.
  Alternatively, mark utterances `Prio::Text` + `queue_behind_epoch`. Then say-all can submit 3 at once, as the
  ADR says, and the gate goes away. At a minimum, open the gate only on `Started`/`Ended`, never by the clock.

### M1. Stopping say-all with a movement key races the stop's SetCaret

- Where: `text/mod.rs:705-733`, ADR-0007 §11 row "epoch advanced".
- Failure: the caret stays at the say-all start until the reading stops. The user's key (Down, Ctrl+Right)
  reaches the application over Wayland at the same time as gorca's `SetCaret` over D-Bus.
  - If the key wins, the app moves the caret from the old position. gorca speaks that line (the key's
    correlated caret event), then `SetCaret` silently moves the caret to the sentence start (the own-move
    absorbs the event). The user hears line X and the caret ends at line Y.
  - If `SetCaret` wins, it works.
- SA3 only tests Escape (`crates/gorca-bench/src/e2e/m3.rs:1001`), so the race is unmeasured.
- Fix: move the caret as chunks start (`Started` → `SetCaret` at a Background budget, as Orca and NVDA do in
  editable text), so any key acts from the reading position. Or at least add an SA3 variant that stops with
  Down and checks which line is spoken and where the caret ends.

### M2. Selection speech is lost when `selection-changed` precedes `caret-moved`

- Where: `crates/gorca-core/src/text/caret.rs:807` (`Fetching → Idle.on_caret` drops a `Want::Selection`
  fetch).
- Failure (P1): a Shift+Right `Awaiting` slot gets the selection event first and issues a selection fetch
  (`Fetching{Want::Selection}`). The caret event then drops that fetch and goes to `Idle`, where it is held as
  uncorrelated. 5 ms later it is spoken as a character ("e"), and the selection reply is discarded as stray, so
  `t.sel` is also stale for the next delta.
- GTK4 emits caret first (`refs/gtk/gtk/a11y/gtkatspitext.c:1057-1061`). The order in GTK3 (atk-bridge
  property notifies) and St/Clutter is unverified, and the checklist itself says St "may differ".
- Fix: in `on_caret`, `Fetching{want: Selection}` should keep the fetch and only update `t.caret`.

### M3. `text.say_all` and `info.selection` ignore the mode (clash with M4a review, and with M7a browse)

- Where: `text/mod.rs:565` (`text_say_selection`) and `:601` (`say_all_start`): no `text_active()` check,
  unlike `text_say_unit`.
- Failure: the tracked entry survives a mode change (untrack happens only on a focus change). Two cases:
  - In Review mode (M4a), or in Browse mode after M7a merges (Escape from Firefox focus mode leaves focus on
    the entry), gorca+Down starts a native say-all of the entry instead of the document or review say-all.
  - When that run stops, its `SetCaret` moves the entry's caret.
- Fix: gate both on `text_active()`, or let the mode dispatch choose the say-all source (ADR §11 `SayAllSrc`).

### M4. Typing expectations absorb unrelated caret moves for up to 1 s

- Where: `caret.rs:814-822`.
- Failure (P3): an expectation remains in the `ExpectRing` whenever a typed key's insert does not match it.
  Examples: IME preedit keys (pinyin "nihao" → commit "你好", where `Consume::Unmatched` leaves all 5), a
  rejected character (a numeric field), or a typed key in a non-editable text (`expect.push` at `caret.rs:694`).
  For up to `EXPECT_TTL_MS` = 1000 ms afterwards, any caret move forward by ≤ `expect.len()` scalars is
  absorbed silently, a mouse click or a script move included.
- Fix: absorb only if the caret move lands where the pending typed characters would put it *and* arrives within
  `ABSORB` (50 ms) of the newest expectation. Clear the ring on `Unmatched`.

### M5. Firefox focus mode after M7a: UTF-16 offsets and descendant carets

- `trackable()` (`text/mod.rs:204`) tracks any `TextEntry`, or editable `Document`/`Paragraph`, in
  `Mode::Focus`. After M7a, auto focus mode in Firefox is `Mode::Focus`, so Gecko entries are tracked.
- Gecko offsets are UTF-16 (TQ-1, deferred to M3b). One emoji makes the window splice (scalar count) and the
  app's offsets disagree by 1 per astral character. Arrows then speak the wrong character, and BackSpace speaks
  from the window at the wrong offset when the delete has no payload.
- A contenteditable sends caret events on descendants, which `text_is_tracked` ignores, so arrows are silent
  in rich editors.
- Fix before merging both: skip tracking for apps whose `AppProfile` is a web engine until M3b, or carry
  `text_unit` per app.

### L1. Caret that beats its key while the previous slot is open: double speech

- Where: `caret.rs:845-897`.
- Failure (P4): Right at the end of the text (no caret event, slot open 100 ms) is followed by Left, whose caret
  event beats the Left key. The event is presented with Right's unit, and then Left's slot reaches its edge and
  speaks the boundary unit again: `["d","d"]`.
- Fix: when a new `Move` key arrives within `HOLD` of a caret just presented by the previous slot, treat that
  caret as the new key's.

### L2. A non-touching unexplained insert during the flood gap drops the held one

- Where: `caret.rs:1573` (`flush_held_insert` before a new hold), `:1604-1615`.
- `speak_inserted` returns false inside `INSERT_GAP`, so an earlier held insert is dropped, not deferred. This
  contradicts the EV-5 changelog line "spoken once ... instead of being dropped".
- Fix: keep the old held insert (or append "and more") when flooding.

### L3. Password-bearing types derive `Debug`

- Types: `Edit`, `HeldIns`, `HeldDel` (`caret.rs:145-310`), `TextMsg`, `Reply`, `TextWindowFill`,
  `TextUnitAt`, `TextSelections` and `Utterance`.
- None of them is logged today: a grep of all `tracing::` call sites in core, speech, dbus, connector and
  daemon finds no Debug of these. `Spoken`, `Tracked`, `TextWindow`, `Run` and `Act` redact correctly.
- One future `?msg` trace in the Reader loop would leak typed passwords. The PW gate only matches ≥ 8-char
  substrings (`m3.rs:1136-1138`), so it would not catch a per-character leak.
- Fix: manual redacting `Debug` for `Edit`/`HeldIns`/`HeldDel`/`TextMsg::Edit`/`Reply`. Make the PW gate also
  search for the sequence of single-character log fields.
- `Tracked::scrub` (`caret.rs:357`) does not clear `held_ins`/`flushed_ins`/`held_del`, which hold password
  text. They are dropped soon after, but PW-4 says to overwrite.

### L4. `GetStringAtOffset` reply size is unbounded on the wire

- Where: `connector-atspi/src/text/mod.rs:135-146`.
- The line or paragraph string is fully deserialised before `clip(64 KiB)`. A multi-MB minified line costs a
  transient multi-MB copy in the connector task. This is bounded by the D-Bus message limit, isolated from the
  Reader, and it runs under a deadline.
- The clipped `TextUnitAt.range` then no longer matches its text length in `add_line`. This is harmless today,
  because units are read from the window and not from `LineSpan.text`.
- Fix: none required now. Note it for when `LineSpan.text` becomes a source.

### L5. The ADR deviation is not recorded: progress is reported for every utterance

- ADR SA-3 says progress is reported only for `notify_progress` utterances.
- `lane.rs:333-341` reports Started/Ended/Stopped for all speech into `text_io`. That adds 2 to 3 Reader
  wake-ups per key echo, and it competes with fetch results for the 64 slots (it also feeds H1).
- Fix: filter to say-all ids (a flag on `Utterance`, or an id set in the lane).

### Checked and found sound

- **Key decides unit.** `classify` (`gorca-input-api/src/text_key.rs`) uses Ctrl for word or paragraph,
  Home/End for line edge, Ctrl+Home for the document edge, PgUp/PgDn for page, and folds the keypad keys. Alt
  and Super are excluded, and Ctrl letters use the physical keycode, so the mapping works in any layout.
- **5 ms hold.** It is bounded: `held`, `held_ins` and `held_del` are single `Option` slots, and
  `deadline()` arms the Reader timer for each of them. With no key, an uncorrelated caret or short insert waits
  `HOLD` (5 ms) plus timer latency. It does not leak: the hold is cleared on timer, key, untrack and focus
  change. Flood inserts wait at most 250 ms by design.
- **Echo exactly once.** This holds for key-then-insert, insert-then-key within 5 ms (`Claim::Held`), and
  insert-then-key within 150 ms (`Claim::AlreadySpoken`). Exceptions: an app slower than 1 s (expectation
  expired), where the echo is doubled, and the M4 absorb hole.
- **BackSpace/Delete.** Deleted text comes from the payload or, before the splice, from the window. An erase key
  is never named; nothing is spoken at a boundary.
- **Paste.** Paste is spoken with the key's epoch, truncated at 256 plus "and N more".
- **IME commit.** A commit without keys is spoken once. With keys, see M4 and V-T4.
- **Deadlines.** Every text call (`window`: 3 calls; `selections`: up to 17 calls under one deadline;
  `at_offset`; `SetCaret`) runs under `Deadline::for_class`. At most 16 text tasks run at once; beyond that the
  task answers `Busy` immediately.
- **Size bounds.** The window is ≤ 16 Ki scalars; reply text is clipped to 64 KiB on char boundaries; the
  text-changed payload is ≤ 1 KiB; selection text ≤ 1024 scalars; an utterance ≤ 4000 scalars. `revs` holds
  at most 8, `own_moves` at most 4, say-all keeps 3 chunks. No map grows without bound.
- **UTF-8 and offsets.** All slicing is on `Vec<char>` with `.get()` or guarded ranges; wire offsets go
  through `nonneg`/`min(len)`; `clip` respects char boundaries. Cyrillic and emoji are safe under the
  scalar-offset (GTK) assumption; see M5 for Gecko.
- **Recursion.** Only one-level re-dispatch (`Fetching → Idle.on_caret`, `on_key → on_caret`); no unbounded
  recursion.
- **`unwrap`.** None outside tests in the new code; `debug_assert_eq!` (`caret.rs:1653`) prints counts only.

## Merge conflicts with M7a (`177f39c`; the worktree also has uncommitted fixes)

`git merge-tree 54a6fef 177f39c` reports content conflicts in:

- `crates/gorca-core/src/app_actor.rs`: `text: Option<TextLink>` next to `web: Option<DocRouter>`, and the
  `with_text`/`with_web` builders;
- `reader.rs`: queue constants, `ReaderEffect::TextFetch` next to `::Web`, `ReaderSenders`/`ReaderInbox`
  fields, the `Reader` fields;
- `runtime.rs`: imports, the `Shared` fields, the actor wiring, the effect match;
- `lib.rs`: exports;
- `commands/spec.rs` and `commands/tests.rs`: the implemented-command counts 69 and 122. After the merge the
  count must be recomputed; it is neither number;
- `gorca-bench/src/bin/e2e.rs` and `e2e/scenarios.rs`;
- `Cargo.lock`, `docs/decisions.md` and the rust-engineer memory.

`present/flow.rs`, `commands/handlers.rs`, `compile.rs`, `table.rs`, `keys.rs`, `mode.rs` and `daemon/*`
auto-merge. The conflicts are all additive; the semantic clashes are M3 and M5 above. Also check after the
merge:

- the inbox `next()` priority order of `text_io`/`text` against `docs`;
- `begin_focus` calling `text_untrack` before the web focus join;
- whether browse-mode keys reach `text_key`. They should not: they are grabbed, and `text_active` is false in
  `Mode::Browse`.
