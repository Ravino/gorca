# ADR-0007: Text, caret and typing echo, and the shared text layer

Status: **Accepted** (2026-10-14). Owner: architect. Milestone: M3 (`docs/plan/phase-2.md` §M3). The shared
text layer of §3 also serves M7 browse mode (`docs/design/browse-mode.md`), M4b (ADR-0006 §10, §15.2) and
M6 (terminal).

Inputs: `CLAUDE.md`; ADR-0001 §3.4 (epoch), §4.3 (TU-1…TU-3), §4.4 (`NodeEvent`, `TextDelta`,
`CaretOrigin`), §6.3 (ingress, R-TEXT, R-3), §7.4 (speech queue, S-1), §8.1 (`may_refine`); ADR-0004;
ADR-0005 (commands, `KeyIntent`, CR-1…CR-5, echo settings); ADR-0006 §10 (TX-1…TX-3 seams), §20
(Reader-lane-owned I/O tasks, replies matched by request id); `docs/design/modes.md` §4.2;
`docs/decisions.md` (2026-10-13: typing echo characters on and words off; password fields are spoken and
never logged; presentation-log redaction); `crates/gorca-core/src/{text/mod.rs, present/echo.rs,
app_actor.rs}`, `crates/gorca-model/src/text.rs`, `crates/gorca-ingress/src/table.rs`,
`crates/gorca-doc/src/web/flat.rs`.

## 0. Changelog

- 2026-10-14: first version, Accepted.
- 2026-10-14: TI-3 clarified (`docs/reports/m3-e2-flake.md`): an echo made stale by a newer key before it is sent
  (E-2, newest wins) is not "missing"; E2 counts only echoes lost in some other way.
  A typed key that trails its insert finds that insert spoken in an older epoch than its own; that speech is
  cancelled by the key, so the key echoes itself (`Claim::Superseded`) instead of staying silent.
  The lane counts every stale drop, including those the state machine makes after `submit`.
- 2026-10-14: M3 review fixes (`docs/reports/m3-review.md`, probes in `bench/results/e2e/m3-review/probes/`):
  - H1/H2 say-all: the speech lane has an explicit queued-utterance kind (`SpeechHandle::submit_say_all`).
    The first chunk is submitted normally (it replaces older speech); the following chunks queue behind it
    in FIFO order instead of replacing each other, so ordinary speech keeps newest-wins and the 150 ms clock
    fallback is gone (it also works with the speech server down at start). Progress is reported per
    tracked utterance id (replaces the `notify_progress` flag of the text above): the lane reports Started,
    Finished or Stopped for every tracked utterance, including one that is replaced or dropped, and a report
    settles earlier queued chunks (reports are ordered). The daemon forwards progress with a bounded
    `send` (1 s timeout) instead of `try_send`, so it is not lost. A run also ends by a watchdog (90 s without
    progress). A key after a finished or stale run never repositions the caret.
  - M1 (stop with an arrow): the caret follows the reading (`SetCaret` at the start of each chunk as it
    starts). A stopping key is then handled by the application from where the reading is, and gorca does not
    move the caret after the key; on a natural end it moves the caret to the end of the text. Chosen over
    placing the caret before forwarding because it has no window in which the app and gorca both write.
    Gate: e2e SA3a.
  - M2: selection-changed before caret-moved are paired inside the key window in either order.
  - M3: say-all and say-selection check the mode; outside the text modes they speak the cue "not available
    in this mode". Browse mode (M7a) plugs in at `text_mode_allows`.
  - M4: a typed key that matched nothing only holds its slot for its own insert, not for unrelated caret moves.
  - M5: `AppInfo.text_unit` (connector capability) -> `AppProfile.non_scalar_text` ->
    `MirrorDirectory::text_scalar`. Apps whose offsets are not scalars are not tracked until M3b.
  - Lows: wrong-way hold (an insert that beats its key by the wrong order); gaps in a flood no longer drop an
    event; `Debug` of types holding password text (`LazyText`, `TextUnitAt`, `TextWindowFill`,
    `TextSelections`, `Utterance`) is manual and redacts; line replies larger than 256 KiB are refused
    undecoded (`MAX_TEXT_REPLY_BYTES`) and a clipped line shortens its range.
- 2026-10-14: M3 for native apps built (`docs/plan/checklists/M3.md`). Where the code differs from the text
  above, the code is the decision:
  - `TextKey` is classified by `gorca_input_api::classify_text_key` and applied by the Reader to `PassToApp`
    keys; `KeyIntent` is unchanged (no `text_key` field), so no other lane or test constructor changes.
  - Caret, text and selection events travel on two new Reader channels instead of `FocusLaneMsg`
    (`TextMsg`, which carries payloads, so it is not `Copy`): `text` (events of the focused app, flood-prone,
    with a `dropped` counter that makes the Reader distrust its window) and `text_io` (fetch results and the
    speech lane's progress, so a flood cannot starve the answers). The app actor forwards only while its app is
    the published focused application (EV-3).
  - `text-changed` carries its text: `gorca-dbus` `map_signal` now puts the string payload (at most 1 KiB) of
    an insert or delete into `LazyText` (`field` 0, the bytes are the UTF-8 text). The ingress folds are
    unchanged.
  - `Utterance.sensitive` is not added: the Reader remembers the id of the latest secret utterance
    (`Reader::last_spoken_is_sensitive`, for the PW-3 refusal of `speech.copy_last`), and text speech writes
    nothing to the presentation log, so PW-2 holds by construction.
  - `AppInfo.text_unit` and `ActionReq::SetSelection` are not added: offsets are scalars (GTK, St, the fake app).
    Firefox focus-mode UTF-16 (TQ-1) stays with M3b. Descendant carets (contenteditable) likewise: the machine
    tracks the focused node itself.
  - The first say-all chunk goes alone (`say_all::FIRST_CHUNK_GATE`). The speech lane gives an utterance of a
    newer epoch the interrupting slot, and chunks submitted in the same breath replace each other there (lane
    test `same_epoch_utterances_submitted_together_replace_each_other_in_the_slot`); the other two follow when
    the server reports the first started, or after 150 ms.
  - The voicego and SSIP clients hold one utterance at the server (`max_in_flight = 1`), so SA2 is measured as
    the gap between the server's END of a chunk and the SPEAK of the next (p99 0.16 ms), not as "submitted
    before END at the server". The Reader's own three-chunk look-ahead is unit-tested.
  - EV-4 (a caret event that beats its key is held 5 ms) is extended to inserts and deletes, because the
    application's signal and the keyboard monitor's key travel on different sockets and the e2e harness showed
    the signal winning a few times per 10,000 keys: a short unexplained insert waits 5 ms for a typed key to claim
    it (the key's echo is then the insert's too), a delete waits 5 ms for an Erase or Cut key (otherwise it is
    dropped, as before), and a key that arrives after its insert was already spoken stays silent (150 ms).
    Touching unexplained inserts fold into one net insert (`gorca_text::coalesce::compose`, text read back from
    the window); while an earlier unexplained insert was just spoken they wait for the end of the 250 ms gap and
    are spoken once (EV-5), instead of being dropped.
  - `info.formatting` stays planned (M3b); `speech.punctuation` and `speech.caps` exist, `caps = "pitch"`
    speaks a capital like its lower-case letter until the speech clients can carry a pitch change.

- 2026-10-15: M3b built for Firefox fields (checklist `docs/plan/checklists/M7b.md`, report
  `docs/reports/m3b-m7b-impl.md`). Where the code differs from the text above, the code is the decision:
  - **TQ-1 is implemented as "no conversion".** Gecko's ATK strings carry a phantom U+FEFF after every astral scalar so
    that UTF-16 offsets equal character offsets of the string itself (`refs/firefox/accessible/atk/DOMtoATK.h`). The
    window therefore keeps the application's string in the application's own offsets. `gorca_text::seg::char_range`
    keeps a phantom with the scalar before it, word and soft-line rules follow, the verbalizer drops U+FEFF and U+FFFC,
    counts of "N characters" skip the phantom. A text-changed insert reports the UTF-16 length and a text without
    phantoms: the Reader puts them back (`with_phantoms`). `TextUnit::window_follows()` is true for scalars and Gecko,
    false for UTF-8 byte offsets (VTE, M6). `MirrorDirectory::text_scalar` is replaced by `text_window_follows`. The
    Reader never needs a scalar offset of a field, so there is no raw/scalar map, no `RawMap` in the window and no
    TX-1 run cache for fields (ADR-0008 Q-E4, measured in `crates/gorca-text/tests/qe4.rs`). The browse mirror still
    strips phantoms at its text-run layer, which re-reads whole runs, so it converts no offsets either.
  - **CE-1 rich editables.** In a contenteditable the focused object (Gecko: an `entry` or a document) has a text made of
    U+FFFC marks, one per block child; caret, text and selection events come from the paragraph the caret is in.
    The machine tracks a `root` (the focused object) and a current `node`: an event from another node of the same app
    while the root `hosts` embedded objects retargets the machine (`In::Retarget`), unless the mirror knows the node
    hangs outside the root. Window, caret, selection and held edits are bound to the node and go; the key slot and the
    typed-key expectations stay. The caret event right after focus (within 600 ms, no key waiting) only places the
    caret. A paragraph that goes away hands the machine back to the root. The connector remembers the object path of
    every caret event's node (`needs_path` now includes `CaretMoved`) so the paragraph can be read.
    Edits that are only U+FFFC marks (a block added or removed) are structure and are not spoken.
  - **Mode integration.** `text_mode_changed` runs after every Reader message: leaving focus or pass-through mode
    drops the window, entering it again drops it too and starts tracking a field that got the focus while the mode was
    browse (Enter on a field, Insert+Space). Gate FT1.
  - **`info.formatting`** is implemented: `TextQuery::Format` (`GetAttributeRun(offset, defaults)`) mapped by the
    connector's attribute-name table (`gorca-connector-atspi/src/text/format.rs`, GTK and Gecko names) to
    `TextFormat`; the Reader speaks font, size, bold, italic, underline, strikethrough, script, colours, alignment,
    language. Browse mode refuses ("not available in this mode"); formatting of the virtual cursor is M8.
  - Say-all in a contenteditable reads the paragraph the caret is in only (no paragraph chaining yet).

## 1. Context

Phase 1 and M1 speak focus, commands and key names. Nothing speaks the caret: arrows in a text field are
silent, typed characters are spoken only as key names (M1 key echo), selection is never reported, and
`text/` in `gorca-core` is an empty seam (`TextMsg` has no variants). The app actor drops
`CaretMoved`, `TextChanged` and `SelectionChanged` after invalidating the mirror
(`crates/gorca-core/src/app_actor.rs:706-711, 785-790`).

M7 (Firefox browse mode) needs the same reading units (character, word, line, sentence), the same
verbaliser and the same say-all engine over the document mirror's flat text. Building them twice would
give two different word rules and two say-all implementations. This ADR therefore defines one text
layer with two text spaces: a native one (AT-SPI `Text`, fetched on demand) and a document one (the
`gorca-doc` flat text, local).

### 1.1 Platform facts (verified in `refs/`)

| # | Fact | Source |
|---|---|---|
| T-F1 | `text-caret-moved` carries only the new offset: no granularity, no origin. Firefox drops user-origin information. | ADR-0001 §4.4; WEB §1.4 |
| T-F2 | GTK4 emits `TextCaretMoved` and then, in the same call, `TextSelectionChanged` when a selection existed before or exists after. The selection signal carries no ranges. | `refs/gtk/gtk/a11y/gtkatspitext.c:1040-1062`, `gtkatspicontext.c:861-889` |
| T-F3 | GTK4 `TextChanged` carries the inserted or deleted text in the event variant. Orca reads `event.any_data` from atk-bridge too. | `gtkatspicontext.c:836-858`; `refs/orca/src/orca/ax_utilities_event.py:560` |
| T-F4 | GTK4 implements `GetStringAtOffset` for char, word, sentence, line and paragraph granularities, `GetNSelections`, `GetSelection`, `SetCaretOffset`; the old `GetTextAtOffset` family returns `NotSupported`. | `gtkatspitext.c:53-61, 84-146, 315-330` |
| T-F5 | Firefox offsets are UTF-16 code units with a phantom U+FEFF after each astral character. GTK and Chromium use code points. | `refs/firefox/accessible/atk/DOMtoATK.h:10-28`; ADR-0001 TU-2 |
| T-F6 | Ingress folds text deltas in order (R-TEXT; delete followed by insert becomes `Replaced`), keeps the newest caret per node (last writer wins), and under `Flooded` keeps only the caret of the focused node. | ADR-0001 §6.3 R-2, R-3, R-TEXT; `crates/gorca-ingress/src/table.rs:713-735` |
| T-F7 | Mutter `KeyEvent` carries keysym and unichar for the current layout and level, no repeat flag, no timestamp. | ADR-0005 §1.1; `docs/reports/2026-10-13-m1-platform-measurements.md` |
| T-F8 | AT-SPI has no protected/secret state; only `ROLE_PASSWORD_TEXT` identifies a password field. | `docs/decisions.md` 2026-10-13 |

### 1.2 Prior art and its failures

| System | What it does | Take / avoid |
|---|---|---|
| NVDA editable text (`refs/nvda/source/editableText.py:70-192`) | Binds the arrow keys as scripts, sends the key, then **polls** the caret on the main thread (`_hasCaretMoved`, up to `caretMoveTimeoutMs = 100`, `config/configSpec.py:364`) and speaks the unit the key implies. At a boundary (no movement) it speaks the unit at the unchanged caret. | **Take** "the key decides the unit" and the boundary rule. **Avoid** the blocking poll: gorca never grabs arrows (ADR-0001 K-1) and never waits on the Reader. |
| Orca (`refs/orca/src/orca/input_event_manager.py:417-600`, `ax_utilities_event.py:560-640`) | Classifies every caret and text event by "the last key event" (`last_event_was_line_navigation`, …), with no time bound and no node check, and fetches text synchronously per event. | **Take** the reason table (navigation, selection, editing, paste). **Avoid** the unbounded "last key": a caret moved by a script seconds after an arrow is spoken as a character move. Correlation in gorca is bounded by time, node and epoch (CR-1…CR-4). |
| NVDA typing echo (`configSpec.py:204-206`) | `speakTypedCharacters = 1`, `speakTypedWords = 0`. | Same defaults (decision 2026-10-13). |
| NVDA say-all (`refs/nvda/source/speech/sayAll.py:217-430`) | Reads by reading chunk, keeps up to 10 lines buffered (`MAX_BUFFERED_LINES`), moves the caret through index-mark callbacks. | **Take** bounded look-ahead and mark-driven position. |
| Orca say-all (`refs/orca/src/orca/say_all_presenter.py:491-560`) | By line or sentence, with rewind and fast-forward. | Sentence chunks; rewind/fast-forward later. |
| Odilia | Caret events are printed, not presented. | Nothing to take. |

## 2. Decision (summary)

1. **One shared text layer** (§3): a new pure crate `gorca-text` (units, segmentation, offset
   conversion, selection diff, say-all chunking, the `TextSpace` trait), the verbaliser in
   `gorca-speech`, and presenters and the say-all engine in `gorca-core/src/text/`. Two text spaces
   implement `TextSpace`: `NativeText` (a per-focus text window over AT-SPI `Text`) and the document
   flat text (`gorca-doc`). Browse mode reuses everything except its line presenter.
2. **The key decides the unit.** The input lane tags each pass-through key with a `TextKey`
   (§4.1). The Reader's `CaretState` machine (§5) opens one correlation slot per key, bounded by time,
   node and epoch, and speaks the unit the key implies when the caret event arrives. Uncorrelated caret
   moves fall back to a delta rule (CR-4).
3. **No IPC on the hit path.** A text window around the caret (±2,048 scalars) and a small line cache
   answer character and word echo locally; a line echo on a miss costs one Interactive
   `GetStringAtOffset` (§6). Fetches are Reader-lane-owned tasks, matched by request id (ADR-0006 §20).
4. **Typing echo is spoken from the key event** (E1 ≤ 3 ms), then deduplicated against the
   `text-changed` insert by an expectation ring (§7). Inserts with no matching key (input methods,
   paste, autocorrect) are spoken from the event. Words are spoken from the text window when the
   separator is inserted.
5. **Selection is a diff** of fetched ranges, spoken as "‹text› selected / unselected"; a Shift+key
   fetches the selection immediately instead of waiting for the selection event (§8).
6. **Event routing** (§9): the app actor forwards caret and selection events of the focused app and
   text changes inside the focus subtree to the focus lane. Ingress coalescing is unchanged except one
   R-3 amendment.
7. **Password fields** are echoed like any field and never logged, copied or embedded (§10).
8. **Say-all** is one engine over any `TextSpace` (native, document, review) with bounded look-ahead,
   progress from the speech lane, and cursor placement on stop (§11).

## 3. The shared text layer

```
 input lane ── KeyIntent{text_key} ─┐
                                    ▼
 app actor ── FocusLaneMsg::{Caret,Text,Selection} ──► Reader
                                                     ├─ text/   CaretState (native), echo, selection,
                                                     │          SayAll engine, unit presenter
                                                     ├─ browse/ WebSession (M7), line presenter
                                                     └─ review/ (ADR-0006)
        all three use ─► gorca-text::TextSpace ◄─ implemented by
                              │                      • text/native.rs  NativeText  (window over AT-SPI Text)
                              │                      • gorca-doc web   WebDoc flat text (zero IPC)
                              │                      • review/         LineModel zones (M4b)
                              └─► gorca-speech::verbalize (char/word/line/spell, punctuation, caps)
```

### 3.1 `gorca-text` (new crate; pure: no tokio, no I/O)

Allowed dependencies: `gorca-model`, `unicode-segmentation`, `smallvec`. `gorca-doc` and `gorca-core`
depend on it; `gorca-connector-atspi` uses its offset conversions.

```rust
pub enum Unit { Char, Word, Line, Sentence, Paragraph }
pub enum Dir { Prev, Next }

/// Answer of a text space: ready now, or the data it needs first (native windows only).
pub enum Lookup<T> { Ready(T), Need(Need) }
pub enum Need {
    Window { around: u32 },            // scalar offset the window must cover
    Line { at: u32 },                  // toolkit line at offset (native Line unit, T-F4)
    Length,
}
pub enum Step { Moved(Range<u32>), Edge(Range<u32>) }   // Edge: no further unit; range = current one

pub trait TextSpace {
    fn len(&self) -> Option<u32>;                                       // scalars, if known
    fn unit_at(&self, at: u32, unit: Unit) -> Lookup<Range<u32>>;
    fn step(&self, at: u32, unit: Unit, dir: Dir) -> Lookup<Step>;
    fn read(&self, r: Range<u32>, out: &mut String) -> Lookup<()>;
}

pub mod seg {        // one implementation of every rule, shared by native, browse and review
    pub fn char_range(s: &[char], i: usize) -> Range<usize>;       // extended grapheme cluster
    pub fn word_range(s: &[char], i: usize) -> Range<usize>;       // NVDA/Orca rule (below)
    pub fn sentence_range(s: &[char], i: usize) -> Range<usize>;   // UAX #29 + abbreviation guard
    pub fn soft_lines(s: &[char], max: u16) -> impl Iterator<Item = Range<usize>>; // NVDA maxLineLength
}
pub mod units {      // TU-2 / TU-3
    pub struct SparseIndex;                       // scalar <-> UTF-8 byte, every 64 scalars
    pub fn raw_to_scalar(text: &str, raw: u32, unit: TextUnit) -> Option<u32>;
    pub fn scalar_to_raw(text: &str, scalar: u32, unit: TextUnit) -> Option<u32>;
}
pub mod sel {
    pub struct SelDelta { pub added: SmallVec<[Range<u32>; 2]>, pub removed: SmallVec<[Range<u32>; 2]> }
    pub fn diff(old: &[Range<u32>], new: &[Range<u32>]) -> SelDelta;
}
pub mod chunk {      // say-all
    pub fn next_chunk(space: &dyn TextSpace, from: u32, max: u32) -> Lookup<Range<u32>>;
}
```

- **TL-1 Word rule.** The rule of `crates/gorca-doc/src/web/flat.rs:37-80` (a word includes its trailing
  spaces; each punctuation scalar is its own word) moves into `gorca-text::seg::word_range` and becomes
  the only word rule. `gorca-doc` calls it. ADR-0006 `SimpleUnits` (`review/segment.rs`) is replaced by
  it (ADR-0006 TX-1).
- **TL-2 Char = grapheme.** A character step moves by one extended grapheme cluster; the offset stays a
  scalar offset (TU-1).
- **TL-3 Lines.** For native text the line is the toolkit's (`GetStringAtOffset(LINE)`), because only the
  toolkit knows its wrapping. For the document flat text a line is a hard line split into soft lines of
  at most `browse.max_line_length` scalars (default 100, NVDA `maxLineLength`,
  `refs/nvda/source/config/configSpec.py:217`). For review it is the `LineModel` line.
- **TL-4 Purity.** `gorca-text` functions never allocate on the step path except into a caller buffer,
  never panic on any offset (out-of-range offsets clamp, T-3), and are fuzzed.

### 3.2 Verbaliser (`gorca-speech/src/verbalize.rs`, M3 owns)

```rust
pub enum Say { Char, Word, Line, Text, Spell(Spell) }   // how a unit is spoken
pub enum Spell { Letters, Phonetic, CodePoint }
pub enum Punct { None, Some, Most, All }                 // speech.punctuation_cycle; default Some
pub struct VerbalizeOpts { pub punct: Punct, pub caps: Caps /* Pitch | Say | Off */, pub lang: Option<Lang> }
impl Verbalizer { pub fn unit(&self, s: &str, how: Say, o: &VerbalizeOpts) -> Verbalized; }
```

- **VB-1** A single character is spoken by name when it is a symbol, whitespace or control (`space`,
  `tab`, `blank` for a line break or end of text, `cap` or pitch change for capitals), whatever the
  punctuation level. Words and lines apply the punctuation level.
- **VB-2** A line or word that is only whitespace is spoken as `blank` (NVDA).
- **VB-3** Symbol tables are data (`gorca-speech/data/symbols-<lang>.toml`), English and Russian first.
- **VB-4** Verbalisation runs on the producer's thread (Reader for short units, the say-all source
  for chunks), never on the speech lane (ADR-0001 §3.3).

## 4. Types (interface sketch)

### 4.1 Input lane: `TextKey` (`gorca-input-api`, additive)

```rust
pub struct KeyIntent { /* existing fields */ pub text_key: Option<TextKey> }

pub enum TextKey {
    Move { unit: KeyUnit, dir: Dir, select: bool },   // select = Shift held
    SelectAll,                                         // Ctrl+A
    Typed { ch: char },                                // printable unichar, Return = '\n'
    Erase { dir: Dir, word: bool },                    // BackSpace / Delete, Ctrl = word
    Clipboard(ClipOp),                                 // Paste | Cut | Undo | Redo
}
pub enum KeyUnit { Char, Word, Line, Paragraph, LineEdge, DocEdge, Page }
```

Classification is a pure table over (keysym, modifiers without locks), on the input lane, only for
keys of class `PassToApp` (ADR-0001 §7.3). Keypad keys with Num Lock off map like their main-block
equivalents.

| Keys | No modifier | Ctrl | Shift adds |
|---|---|---|---|
| Left / Right | `Move{Char}` | `Move{Word}` | `select: true` |
| Up / Down | `Move{Line}` | `Move{Paragraph}` | `select: true` |
| Home / End | `Move{LineEdge}` | `Move{DocEdge}` | `select: true` |
| Prior / Next | `Move{Page}` | — | `select: true` |
| A | `Typed('a')` | `SelectAll` | — |
| BackSpace / Delete | `Erase{word: false}` | `Erase{word: true}` | — |
| Ctrl+V, Shift+Insert / Ctrl+X / Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z | — | `Clipboard(..)` | — |
| printable, no Ctrl/Alt/Super (AltGr allowed) | `Typed(unichar)` | — | — |

Alt or Super with any key gives `None` (application shortcuts).

### 4.2 Model and connector additions (`gorca-model`, `gorca-connector-atspi/src/text/`)

```rust
// gorca-model text.rs (additive)
pub enum TextQuery {
    // existing: GetText, CaretOffset, Attributes, VisibleLines
    /// One GetStringAtOffset(granularity) call. Offsets are raw (the app's TextUnit).
    AtOffset { node: NodeKey, offset: u32, unit: Unit },
    /// GetNSelections + GetSelection(i), i < 8.
    Selections { node: NodeKey },
}
// AppInfo (ADR-0001 §4.4) gains: pub text_unit: TextUnit   (connector: Gecko → Utf16Bom, else Scalar)
// ActionReq gains: SetCaret { node, offset_raw }, SetSelection { node, range_raw }
// gorca-model speech.rs: Utterance gains `pub sensitive: bool` (§10)
```

- **TQ-1 Raw offsets in, scalars out.** Event offsets (`CaretMoved.offset`, `TextDelta` offsets) and
  query offsets stay in the app's declared `TextUnit`. Conversion to scalars happens where the text is
  (`NativeText`'s window, the document mirror), with `gorca-text::units`. This amends ADR-0001 TU-1/TU-2
  (§18): a connector cannot convert a UTF-16 offset without the text in front of it.
- **TQ-2** Every query has a deadline and a class (Interactive 120 ms for presentation, Prefetch 300 ms
  for window warm-up), as ADR-0001 §6.4.

### 4.3 Reader messages and effects

```rust
// focus lane (app actor → Reader), new variants
pub enum FocusLaneMsg { /* existing */ 
    Caret { node: NodeKey, offset_raw: u32, origin: CaretOrigin, seq: u64 },
    Text  { node: NodeKey, delta: TextDelta, seq: u64 },
    Selection { node: NodeKey, seq: u64 },
}
// text/mod.rs
pub enum TextMsg {
    Fetched { req: TextReqId, result: Result<TextReply, FetchError> },
    SayAllProgress { utt: UttId, ev: Progress /* Started | Ended | Stopped | Mark(u32) */ },
}
pub enum TextReply { Window(WindowFill), Unit { range_raw: Range<u32>, text: Arc<str> },
                     Selections(SmallVec<[Range<u32>; 1]>), Caret(u32), Ack }
pub enum ReaderEffect { /* existing */ TextFetch { app: AppId, req: TextReqId, op: TextOp, class: Class } }
pub enum TextOp { Window { node, around_raw: u32, radius: u32 }, AtOffset { node, offset_raw: u32, unit: Unit },
                  Caret { node }, Selections { node }, SetCaret { node, offset_raw: u32 } }
```

## 5. Caret state machine (`gorca-core/src/text/caret.rs`)

```rust
pub enum CaretState {
    /// Focus is not a tracked text object.
    Off,
    /// Tracking a text object; nothing pending.
    Idle { t: Tracked },
    /// A TextKey was pressed on this object; waiting for its effect (CR-1).
    Awaiting { t: Tracked, key: TextKey, epoch: Epoch, edge_at: Instant, late_until: Instant },
    /// A presentation needs text that is not in the window; one fetch in flight.
    Fetching { t: Tracked, req: TextReqId, want: Present, epoch: Epoch, deadline: Instant },
}
pub struct Tracked {
    pub node: NodeKey,
    pub kind: TextKind,              // SingleLine | MultiLine; flags: EDITABLE, SECRET (§10)
    pub caret: Option<u32>,          // scalar
    pub sel: SmallVec<[Range<u32>; 1]>,
    pub win: TextWindow,             // §6
    pub expect: ExpectRing,          // §7, 16 entries
    pub own_moves: OwnMoves,         // SetCaret issued by gorca (say-all, review), 300 ms
}
pub struct Present { pub unit: Say, pub at: u32, pub reason: Reason }
```

`Tracked` is created when focus lands on a node whose role is text-capable (entry, password entry, text,
document, paragraph, label with `Text`; terminal is **excluded**, M6 owns it) or whose `Ifaces` has
`TEXT` and state `EDITABLE`. Roles whose Up/Down change a value (spin button, combo box, slider, list,
tree, menu) track Left/Right only; Up/Down there belong to the S1 value speech (Orca
`is_widget_controlled_by_line_navigation`).

### 5.1 Transitions

| From | Event | To | Effects |
|---|---|---|---|
| Off | `FocusChanged(n)`, `n` text-capable | Idle{t(n)} | Prefetch `TextOp::Caret` + `Window` (after the focus utterance is queued) |
| any | `FocusChanged(m)`, `m` not text-capable | Off | drop window (zeroed if SECRET), clear expectations |
| Idle, Awaiting, Fetching | `KeyIntent{text_key: Move \| Erase \| SelectAll \| Clipboard}` with focus = `t.node` | Awaiting{key, epoch, edge_at = at + 100 ms, late_until = at + 400 ms} | a pending fetch keeps running; its reply refills the window only (request id no longer wanted) |
| Idle, Awaiting | `KeyIntent{Typed(c)}` | same state | typing echo (§7); push `Expect{c}` |
| Awaiting{Move, select: false} | `Caret(n', off)`, `n'` in focus subtree | Idle, or Fetching | CR-2: unit from the key (table §5.2); speak if the window hits, else fetch |
| Awaiting{Move, select: true} | `Caret` | Fetching (`Selections`) | §8 |
| Awaiting | timer `edge_at` with no caret event | Idle | CR-3: speak the unit at the unchanged caret (boundary) |
| Idle (after CR-3) | `Caret` before `late_until`, same epoch | Idle/Fetching | CR-3b: speak the key's unit at the new offset; counted `caret_late` |
| Idle | `Caret` uncorrelated | Idle/Fetching | CR-4 |
| any tracked | `Caret` matching `own_moves` | same | suppressed (self-feedback, like `Injected` keys) |
| any tracked | `Text(delta)` | same | §7 (echo dedupe or speak), window splice |
| any tracked | `Selection` uncorrelated | Fetching (`Selections`) | §8 |
| Fetching | `Fetched{req}` matching | Idle | apply to window; speak `want` if its epoch is still current (E-3) |
| Fetching | deadline or error | Idle | speak nothing for chars/words; for a line speak the window's text around the caret if any, else nothing; count `text_fetch_timeout`; app health penalty |
| Fetching | `Fetched` for an older request | same | window refill only |

Invariants:

- **CS-1** At most one presentation fetch per Reader; a newer key supersedes it (ADR-0001 R-1). Under
  a held arrow (C2) every caret event speaks with a new epoch, so only the last position survives.
- **CS-2** The state machine is pure (`step(self, msg, now) -> (Self, Effects)`), exhaustive `match`,
  unit-tested with a fake clock (ADR-0001 §7).
- **CS-3** No caret utterance is spoken twice for one caret event; no caret utterance is spoken for a
  typing insert (the insert's caret move is absorbed by the matching `Expect`).

### 5.2 Correlation rules and unit choice

- **CR-1** One slot. A `TextKey` replaces any earlier slot. The slot binds the key to the focused node,
  the key's epoch (E-4) and two deadlines: `edge_at` (100 ms, NVDA `caretMoveTimeoutMs`) and
  `late_until` (400 ms, WF-11-style app latency bound). Focus change or mode change clears it.
- **CR-2 Correlated move.** A caret event on the focused node or a descendant (contenteditable puts the
  caret on inner paragraphs) before `late_until` consumes the slot. The unit:

| Key | Spoken |
|---|---|
| `Move{Char}` | the character at the new caret (TL-2); at end of text `blank` |
| `Move{Word}` | the word starting at the caret (TL-1) |
| `Move{Line}`, `Move{Page}` | the line at the caret (toolkit line, TL-3) |
| `Move{Paragraph}` | the paragraph (`GetStringAtOffset(PARAGRAPH)`) |
| `Move{LineEdge}` | the character at the caret (NVDA, Orca) |
| `Move{DocEdge}` | the line at the caret |
| `Erase{..}` | the deleted text (§7, TE-5); the caret move itself is not spoken |
| `Clipboard(Paste)` | the inserted text (§7, TE-3); the caret move is not spoken |

- **CR-3 Boundary.** No caret event by `edge_at`: the key did not move the caret (start or end of text,
  first or last line). Speak the unit at the current caret, as NVDA does. If a caret event still arrives
  before `late_until` (slow app), it is spoken with the same unit (CR-3b) and replaces the boundary
  utterance by epoch.
- **CR-4 Uncorrelated move** (script, mouse, input method, autocomplete, another gorca subsystem): if
  `|Δ| = 1` scalar and both offsets are on the same cached line, speak the character; otherwise speak the
  line. Moves caused by a `TextChanged` insert or delete within 50 ms at the same offset are absorbed (no
  speech). Moves equal to an `OwnMoves` entry are suppressed.
- **CR-5 Focus mode in a browser.** In a web document in focus mode the focused editable is an ordinary
  tracked node; Firefox offsets are converted through the window (TQ-1). In browse mode the CaretState is
  `Off` for document nodes: the virtual cursor owns presentation (browse-mode.md §8).

## 6. Native text access: the window and the line cache (`text/native.rs`)

```rust
pub struct TextWindow {
    pub base: u32,                 // scalar offset of text[0]
    pub text: String,              // UTF-8, ≤ 64 KiB
    pub idx: SparseIndex,          // TU-3
    pub raw: RawMap,               // raw (app unit) <-> scalar inside the window; identity for Scalar apps
    pub len: Option<u32>,          // total scalars, when known
    pub lines: ArrayVec<LineSpan, 16>,  // toolkit lines seen (AtOffset LINE replies), scalar ranges
    pub rev: u64,                  // bumped by every splice or refill
}
pub struct NativeText<'a> { win: &'a TextWindow }   // impl gorca_text::TextSpace
```

- **TW-1 Fill.** On focus (Prefetch) and whenever a lookup returns `Need::Window`, fetch
  `GetText(caret_raw − r, caret_raw + r)` with r = 2,048 units, clamped (T-3). One fetch, never more
  than one window fetch in flight per tracked node.
- **TW-2 Splice.** `Text{Insert/Delete}` with a decoded payload is applied to the window in raw units,
  then converted; lines after the edit shift, lines that overlap are dropped. `Replaced` or a payload-less
  delta invalidates the window (`text = ""`, refilled lazily). The mirror's own invalidation
  (`NAME|VALUE`, app_actor) is unchanged.
- **TW-3 Hit path.** Character and word units are answered from the window when the unit's range lies
  inside it with 1 scalar of margin (C1h, zero IPC). Lines are answered from `lines` when a cached span
  contains the offset and `rev` is unchanged since it was fetched; otherwise one `AtOffset{LINE}`
  (Interactive). This makes held Up/Down on a long document cost one call per new line, and Left/Right
  zero.
- **TW-4 Bounds.** One window per tracked node, at most 4 tracked windows kept (MRU, for Alt+Tab back and
  forth), 64 KiB each. Exact byte counters (`TextStats`).
- **TW-5** The Reader owns windows (PC-1: single writer). Fetch tasks run on the Reader lane's runtime
  (`JoinSet`, ADR-0006 §20) and return messages; the Reader never awaits.

## 7. Typing echo (`text/echo.rs`; `present/echo.rs` keeps key echo)

- **TE-1 Characters from the key.** A `KeyIntent{Typed(c)}` while focus is a tracked `EDITABLE` node and
  `echo.typed_chars = true` (default) is spoken immediately with the key's epoch (`Prio::Interrupt`,
  verbaliser `Say::Char`), before any text event (E1). With focus elsewhere the M1 key-echo policy is
  unchanged. Autorepeated characters are each echoed (NVDA).
- **TE-2 Dedupe.** Each echoed character pushes `Expect{node, ch, at, epoch}` into a 16-entry ring. A
  `Text{Insert}` in the focus subtree whose text equals the concatenation of the oldest unconsumed
  expectations (within 1 s) consumes them and is not spoken. This handles fast typing coalesced into one
  insert. Expectations older than 1 s are dropped silently (the app ignored the key: read-only field).
- **TE-3 Inserts without a key.** An insert that matches no expectation (input-method commit, paste,
  autocorrect, a script) is spoken as text when `echo.inserted_text = true` (default): up to 256
  scalars, then "and ‹N› more". Russian and other layouts are not special: the unichar comes from
  Mutter for the active group (T-F7); an IBus commit without a key event takes this path.
- **TE-4 Words.** With `echo.typed_words = true` (default off), when an insert consumes an expectation
  for a separator (whitespace, punctuation, `\n`), the word before it is read from the window (zero IPC)
  and spoken after the separator. If the window does not cover it, a `TypedWordBuffer` built from
  `Typed` keys (reset by any `Move`, `Erase{word}`, focus change) is the fallback.
- **TE-5 Erase.** In a tracked editable, `BackSpace` and `Delete` are **not** echoed as key names; the
  correlated `Text{Delete}` speaks the deleted text (payload, else the window before the splice). Nothing
  deleted (boundary) → nothing spoken.
- **TE-6 Return** is echoed as `new line` only when `echo.typed_chars` is on; the line that follows is
  not read automatically.
- **TE-7 Reasons** recorded on every text utterance for the presentation log and tests: `Typed`,
  `TypedWord`, `Erased`, `Inserted{Ime | Paste | Unknown}`, `CaretMove{unit, correlated}`, `Boundary`,
  `Selection`.

## 8. Selection (`text/selection.rs`)

- **SL-1 Model.** `Tracked.sel` is the last fetched selection set (scalars). The source of truth is
  `TextQuery::Selections` (Interactive); the selection event carries no ranges (T-F2).
- **SL-2 Triggers.** (a) A caret event correlated with `Move{select: true}` fetches selections
  immediately (in parallel with any text fetch); the later `Selection` event that brings no change is
  absorbed. (b) `SelectAll` fetches after its selection event or after `edge_at`. (c) An uncorrelated
  `Selection` event (mouse, script) fetches. Several events before the reply cost one fetch.
- **SL-3 Speech.** `sel::diff(old, new)`: newly covered text → "‹text› selected", uncovered text →
  "‹text› unselected"; both in one utterance when both exist. Texts over 512 scalars are spoken as
  "‹N› characters selected". `SelectAll` speaks "all selected" plus the count. A selection collapsed by a
  non-Shift move is not announced; the move's unit is spoken (NVDA).
- **SL-4** `info.selection` speaks the current selection text (fetched if the window does not cover it),
  or "no selection".

## 9. `text-changed`, `text-caret-moved` and their coalescing

- **EV-1 Ingress (unchanged).** Text deltas fold per node in order (R-TEXT), carets are last-writer-wins
  per node, selection events last-writer-wins per node (ADR-0001 §6.3).
- **EV-2 Amendment to R-3 (`Flooded`).** The kept caret slot becomes "the app's newest `Caret` slot,
  whatever its node", instead of "the caret of the focused node": in contenteditable content the caret
  lives on a descendant of the focused node (§18).
- **EV-3 App actor routing (new).** `route()` forwards to the focus lane: every `CaretMoved` and
  `SelectionChanged` of the app that holds focus; `TextChanged` only when the node is the focus node or a
  mirror-known descendant of it (≤ 16 parent hops, no IPC; an unknown ancestry is forwarded and the
  Reader decides). Everything else keeps today's behaviour (mirror invalidation only). Terminals: the
  arm forwards nothing for role `Terminal` until M6 claims it.
- **EV-4 Reader order.** Focus-lane messages are processed in arrival order; per node the ingress order
  is preserved (ORD-1). A `Caret` that arrives before the `KeyIntent` that caused it (lanes are separate
  threads) is held for at most 5 ms in `Idle` before CR-4 applies: if the key arrives inside that window
  the pair is correlated. This bound keeps C1 inside its target.
- **EV-5 Storms.** A text flood in the focused app (a log view appending) produces at most one utterance
  per Reader batch for uncorrelated inserts (the newest insert, truncated, `Prio::Text`, never
  interrupting user-caused speech younger than 1 s); everything else only updates the window.

## 10. Password fields

Decision 2026-10-13: password fields are spoken like any other field, and never logged.

- **PW-1** `TextKind::SECRET` is set when the role is `PasswordEntry` (T-F8). Typing echo, caret echo,
  selection and say-all behave exactly as for other fields; the user hears whatever text the app exposes
  (GTK shows bullets in the accessible text, some apps expose the characters).
- **PW-2** Every utterance built from a SECRET node carries `Utterance.sensitive = true`. The presentation
  log (`--bench-present-log`) writes it as `redacted(len=N,h=…)` with the existing `RedactKey`;
  `tracing` fields and `Debug` impls of `TextWindow`, `ExpectRing`, `TypedWordBuffer` and `Tracked`
  print only lengths for SECRET nodes.
- **PW-3** `speech.repeat_last` may repeat it (RAM only); `speech.copy_last` refuses with "not copied,
  protected text"; the perception layer never receives it (ADR-0002 X-11).
- **PW-4** Windows, expectation rings and word buffers of SECRET nodes are overwritten with zeros when the
  node loses focus or the window is dropped. Nothing about them is persisted.

## 11. Say-all (`text/sayall.rs`)

```rust
pub enum SayAll {
    Idle,
    Running {
        src: SayAllSrc,                    // Native(NodeKey) | Doc(DocId) | Review(ModelId)
        epoch: Epoch,
        next: u32,                          // scalar offset of the next chunk to prepare
        queued: ArrayVec<ChunkUtt, 3>,      // submitted, not yet Ended
        fetch: Option<TextReqId>,           // native window fetch for look-ahead
        last_started: Option<Range<u32>>,   // drives cursor placement
    },
}
pub struct ChunkUtt { pub utt: UttId, pub range: Range<u32> }
```

| From | Event | To | Effects |
|---|---|---|---|
| Idle | `text.say_all` (or `browse` say-all, `review.say_all`) | Running{next = cursor} | prepare chunks until 3 are queued or `Need` |
| Running | `Need::Window` | Running{fetch} | Prefetch fetch (300 ms); no speech gap filler |
| Running | `SayAllProgress{Started(u)}` | Running{last_started} | move the virtual position (browse cursor, review cursor) silently |
| Running | `SayAllProgress{Ended(u)}` | Running | pop; prepare the next chunk |
| Running | end of text | Idle | nothing spoken (NVDA) |
| Running | epoch advanced (any key, focus change, command) | Idle | place the cursor at `last_started.start`; native editable: one `SetCaret` (Background) when `text.say_all_moves_caret = true` (default); record it in `OwnMoves` |
| Running | fetch deadline | Idle | place the cursor as above; count `say_all_starved` |

- **SA-1 Chunks** are sentences (`seg::sentence_range`), at most 400 scalars, split at a word boundary
  when longer. Document chunks follow the browse line presenter's segments (objects spoken by role and
  name); review chunks are lines.
- **SA-2 Look-ahead.** Up to 3 chunk utterances are submitted with `Prio::Text`, the say-all epoch and
  `notify_progress = true`; the speech lane may hold 2 at the server (voicego, S-1), so there is no gap
  between chunks.
- **SA-3 Progress** comes from the speech lane as `TextMsg::SayAllProgress` (new, only for utterances
  with `notify_progress`). Backends without `Started` events report `Ended` only; the position then lags
  by one chunk.
- **SA-4 Stop** is the epoch (E-2): speech stops without a message; the state machine notices on the next
  message and places the cursor. Say-all never holds the Reader.

## 12. Commands and settings

Commands (all declared in ADR-0005 §3.2; M3 implements them; in Browse they act on the virtual cursor):
`text.say_all`, `text.say_char/word/line`, `text.spell_char/word/line`, `info.current_line` (×2 spell,
×3 phonetic), `info.selection`, `info.formatting` (attributes at the caret: font, size, bold, italic,
underline, colour; one `Attributes` query), `speech.punctuation_cycle`, `echo.typed_chars_toggle`,
`echo.typed_words_toggle`.

Settings (`gorca-config`, schema version bump):

| Key | Default | Meaning |
|---|---|---|
| `echo.typed_chars` | true | TE-1 |
| `echo.typed_words` | false | TE-4 |
| `echo.inserted_text` | true | TE-3 |
| `speech.punctuation` | `"some"` | VB-1 |
| `speech.caps` | `"pitch"` | `pitch` / `say` / `off` |
| `text.say_all_moves_caret` | true | §11 |
| `text.caret_edge_ms` | 100 | CR-3 |
| `text.caret_late_ms` | 400 | CR-3b |

## 13. How M3 serves browse mode, review and the terminal

| Consumer | Reuses | Own part |
|---|---|---|
| M7 browse mode | `gorca-text` (units, word rule, soft lines, chunking, selection diff), verbaliser, unit presenter for char/word, say-all engine (`SayAllSrc::Doc`), commands of §12 | `WebDoc` implements `TextSpace` (zero IPC, never `Need`); the line presenter speaks objects inside a line (browse-mode.md §8) |
| Firefox focus mode | the whole native path (§5–§8) on the focused editable | Gecko `TextUnit::Utf16Bom` via `AppInfo.text_unit` |
| M4b review | `TextSpace` over a `LineModel`; `AtOffset` for object review over whole texts (ADR-0006 TX-2); `CaretSettled` | ADR-0006 TX-3: `TextMsg` emits `CaretSettled{node, offset}` after every presented caret move |
| M6 terminal | verbaliser, `gorca-text`, typing echo dedupe (TE-2) | screen diff and prompt logic (`ext-vte`) |

## 14. Failure modes

| Failure | Behaviour | Counter |
|---|---|---|
| App hung during a fetch | deadline → nothing or window text (§5.1); health penalty | `text_fetch_timeout` |
| Caret event never comes | CR-3 boundary speech after 100 ms | `caret_edge` |
| Caret event very late (> 400 ms) | treated as uncorrelated (CR-4) | `caret_uncorrelated` |
| Event offsets beyond the known length | clamped (T-3); window refilled | `text_offset_clamped` |
| UTF-16 offset inside a surrogate pair or a phantom U+FEFF | rounded down to the scalar | `utf16_split` |
| Text flood in the focused app | EV-5 | `text_flood_coalesced` |
| `Flooded` app | window invalidated (dirty); next unit fetched | `text_window_dirty` |
| Selection fetch fails | speak the caret unit instead | `sel_fetch_failed` |
| Speech backend without progress events | say-all positions lag one chunk (SA-3) | — |

## 15. Invariants

- **TI-1** The hot path (character or word echo with a window hit, typing echo) performs no IPC (C1h, E1).
- **TI-2** Every text fetch has a deadline and a class; none is `Startup`.
- **TI-3** A typed character is spoken at most once per key press and at least once unless the user
  turned echo off (E2).
  An echo that a newer key made stale before it was sent (E-2: the epoch moved between the key and
  the send, in the lane or on a dropped fetch reply) is not "missing": the newer key's echo replaces it.
- **TI-4** No SECRET text reaches a log, a file, the clipboard or the perception layer (PW-2…PW-4).
- **TI-5** All state machines here (`CaretState`, `SayAll`) are enums with exhaustive `match` and pure
  `step` functions.

## 16. Latency gates and metrics

All gorca-side p99, median of interleaved repeats, also under L3-50k with Δp99 ≤ 1 ms, `lost = 0`,
`stale = 0` (phase-2 §2.1 conventions).

| Gate | Stimulus → end | Threshold (ceiling / target) |
|---|---|---|
| **C1** | arrow `KeyEvent` + `text-caret-moved` → SPEAK, one text fetch (fake app replying in 1 ms) | 15 ms / 5 ms |
| **C1h** (new) | same, window or line-cache hit | 3 ms / 1 ms, 0 IPC |
| **C2** | held arrow, 30 Hz for 3 s | 0 stale utterances; last position always spoken |
| **E1** | printable `KeyEvent` → SPEAK | 3 ms / 1 ms |
| **E2** (new) | 10,000-keystroke randomized session (Latin and Cyrillic, Backspace, paste, a simulated IME commit) | 0 double echoes, 0 missing echoes, 0 key-name echo of BackSpace/Delete in editables |
| **S2** (new) | Shift+arrow → SPEAK of the selection delta (fetch included) | 15 ms / 5 ms |
| **SA1** (new) | `text.say_all` → first chunk SPEAK (native, one fetch) | 15 ms / 5 ms |
| **SA2** (new) | say-all over 100 KB text on a healthy fake app | next chunk submitted before the previous `Ended` in ≥ 99 % of transitions |
| **SA3** (new) | key press during say-all → silence | L2 path (≤ 2 ms); cursor placed ≤ 50 ms |
| **PW** (new) | scripted session in a password field with the presentation log on | 0 plaintext occurrences in logs (grep of the typed string) |

## 17. Alternatives considered

| Alternative | Why not |
|---|---|
| Grab arrow keys and drive the caret like NVDA scripts | Violates K-1 (minimal grabs); swallowed keys need re-injection (K-2) and race with grab changes; the app already moves the caret. |
| Infer the unit from the offset delta only | A Ctrl+Right that moves one scalar is spoken as a character; Down on a short line looks like a character move. The key is the only reliable signal (Orca, NVDA). |
| Fetch the unit synchronously on every caret event (Orca) | One IPC per arrow on the hot path; a slow app delays every echo. The window makes Left/Right free. |
| Echo typed characters only from `text-changed` | Adds the app's event latency (several ms, more under load) to E1; misses read-only fields where NVDA still echoes. |
| Separate word rules for native, browse and review | Three behaviours for the same key; TL-1 makes one rule. |
| Store native text in the mirror | ADR-0001 §4.2: text is not in the snapshot; a 1 MB document would inflate every byte cap. |
| Per-chunk `SetCaret` during native say-all (NVDA) | One IPC per sentence for little gain; one `SetCaret` on stop gives the same end state. |

## 18. Consequences and amendments

- **ADR-0001 §4.3 TU-1/TU-2** amended (TQ-1): event and query offsets are raw in the app's declared
  `TextUnit` (`AppInfo.text_unit`); conversion to scalars happens in `gorca-text` next to the text
  (native window, document mirror). The core text API above that point is scalar.
- **ADR-0001 §6.3 R-3 / ADR-0004** (EV-2): `Flooded` keeps the app's newest `Caret` slot regardless of
  node. Test: B-2 conformance case for a flooded app with a caret on a non-focus node.
- **ADR-0001 §4.4**: `AppInfo.text_unit`; `ActionReq::{SetCaret, SetSelection}`.
- **ADR-0005**: `KeyIntent.text_key`; new settings of §12; `Utterance.sensitive`.
- **ADR-0006 §10**: TX-1 is fulfilled by `gorca-text` (`SimpleUnits` removed), TX-2 by
  `TextQuery::AtOffset`, TX-3 by `CaretSettled`.
- **`gorca-doc`**: `web/flat.rs` word rule moves to `gorca-text` (TL-1); `WebDoc` implements `TextSpace`.
- **Cost**: one Prefetch window fetch per focus on a text field (4 KiB), one Interactive call per new
  line; ~1 KB per expectation ring and buffers.

## 19. Verification items (linux-platform-engineer unless noted)

- **V-T1** GTK3 (atk-bridge) and St (Shell entries): order of `text-changed:insert` and
  `text-caret-moved` when typing, payload presence, and caret offsets after IME commits.
- **V-T2** GTK4 `GetStringAtOffset(LINE)` on a wrapped `GtkTextView`: visual or logical lines.
- **V-T3** Firefox entries and contenteditable: which object emits `text-caret-moved`, offsets with emoji
  (phantom U+FEFF), and whether `text-changed` carries the text.
- **V-T4** IBus (Russian layout, a CJK input method) on GNOME 48 Wayland: does a commit produce
  `KeyEvent`s through the Mutter monitor, or only `text-changed`?
- **V-T5** (test-engineer) Fake editable app scripts for E2 and C2, with configurable event order and
  latency, including caret-before-key arrival (EV-4).

## 20. Slices, files and parallel work

**T0 (first, 1–2 days, shared with M7, owned by M3):** `crates/gorca-text` with `Unit`, `Dir`,
`Lookup`, `TextSpace`, `seg::{char_range, word_range}` (word rule moved from `gorca-doc`), `units`,
`sel::diff`; `gorca-model` additions of §4.2 (types only). M7 starts its reading code on T0.

**M3a:** caret echo (CR-1…CR-5, window, line cache), typing echo (TE-1…TE-7), password handling, routing
(EV-2…EV-4), verbaliser levels, `text.say_char/word/line`, `info.current_line`; GTK3, GTK4, Shell. Gates
C1, C1h, C2, E1, E2, PW.
**M3b:** selection (§8, S2), say-all engine (§11, SA1–SA3), spelling and phonetic, `info.formatting`,
Firefox focus-mode text (UTF-16), `CaretSettled` for M4b.

| Files | Owner |
|---|---|
| `crates/gorca-text/**` | M3 (M7 reviews `seg` and `TextSpace`) |
| `gorca-core/src/text/**`, `gorca-core/src/present/text.rs`, `present/echo.rs` | M3 |
| `gorca-speech/src/verbalize.rs`, `gorca-speech/data/**` | M3 |
| `gorca-connector-atspi/src/text/**` | M3 |
| `gorca-input-api` (`TextKey`), `gorca-input-mutter` lane wiring | M3 |
| `gorca-model/src/text.rs`, `speech.rs` (additive) | M3 |
| `gorca-core/src/app_actor.rs` `route()` arms for Caret/Text/Selection | M3 (M7 adds `app_actor/doc_route.rs` and one call; no shared lines) |
| `gorca-core/src/commands/handlers.rs`, `commands/table.rs` | shared, additive: M3 owns `text.*`, `info.current_line/selection/formatting`, `echo.*`, `speech.punctuation_cycle` rows |
| `gorca-config` schema | shared, additive: M3 owns `echo.*`, `text.*`, `speech.punctuation/caps` |
| `gorca-daemon` `wire_text` | M3 |
| `bench/e2e` C1, C1h, C2, E1, E2, S2, SA1–SA3 | perf-engineer + test-engineer |

M3 has no dependency on M7. M7a depends only on T0.
