# GNOME Terminal / VTE accessibility (AT-SPI Text on a terminal widget)

Sources: `refs/vte` (shallow clone of GNOME/vte main; the host runs `libvte-2.91-0` 0.80.1 with
`gnome-terminal` 3.56.2, i.e. **GTK3 + ATK**; the tag was fetched into the same clone and
`src/vteaccess.cc` / `src/vte.cc` at `0.80.1` were read). `refs/vte/src/vteaccess.cc` (GTK3/ATK) and
`vteaccess-gtk4.cc` (GtkAccessibleText, GTK4 VTE) are separate implementations; the GTK4 one is not what
GNOME Terminal 3.56 uses. Bridge side: `refs/at-spi2-core/atk-adaptor/event.c`. Orca side:
`refs/orca/src/orca/scripts/terminal/`, `ax_utilities_event.py`, `ax_utilities_text.py`.

## What is exposed (verified live on gnome-terminal `:1.14`)

* One accessible per terminal widget, role `TERMINAL` (60), interfaces `Accessible, Action, Component,
  Text, Collection` (no EditableText, no Terminal-event interface). Example path
  `/org/a11y/atspi/accessible/163`.
* The `Text` content is **only the visible screen** (`Terminal::get_text_displayed_a11y`: rows
  `scroll_delta .. scroll_delta + row_count - 1`, `preserve_empty=false`), not scrollback. Offsets are
  character offsets into that snapshot and shift on every scroll.
* Live numbers (`tools/platform-probes/termprobe.py`): `CharacterCount` property 31830,
  `CaretOffset` 20994, `GetText(0,-1)` returned 20995 chars / 253 newlines (the count and the text
  length disagree: treat `CharacterCount` as unreliable, re-derive from `GetText`; unexplained, likely a
  bytes-vs-chars or whitespace-padding quirk). Timings idle: `CharacterCount` 0.7-1.1 ms,
  `GetText(0,-1)` p50 0.41 ms / max 1.1 ms (x50), `GetAttributeRun` 1.06 ms, `GetTextAtOffset` 0.63 ms.
* Text attribute runs from GTK3 VTE are in **UTF-8 byte offsets**, not characters; Orca converts
  (`ax_utilities_text.py` ~l.1369).
* `Event.Terminal` signals (LineChanged, ...) are declared in AT-SPI but VTE never emits them.

## How changes are emitted

1. PTY output is parsed in `process_timeout` (frame-clock scheduler callback); each tick processes
   until a time budget (`VTE_MAX_PROCESS_TIME` = 100 ms, `m_max_input_bytes` adapts so a tick takes
   about that long) and then `emit_pending_signals()` fires the buffered `text-inserted/deleted/modified`
   once for the whole tick (`vte.cc` ~l.10976-10995, 11033-11062). So the emission rate is bounded by the
   frame rate (<= ~60/s), independent of output speed.
2. `_vte_terminal_accessible_text_modified()` (`vteaccess.cc` l.~379): marks the snapshot invalid,
   rebuilds it (`get_text_displayed_a11y` for the whole screen + per-byte attribute list + char-offset
   array + line-break array), then **diffs old vs new by common head/tail only** and emits one
   `text-changed::delete` and one `text-changed::insert` covering everything between the common prefix
   and suffix, then `text-caret-moved` (caret recomputed by a linear scan of the snapshot) *after* the
   text events.
3. `_vte_terminal_accessible_text_scrolled(n)` handles scrolling separately: partial scroll emits
   delete(bottom rows) + insert(top rows) (plus delete/insert "middle" if other cells changed);
   a scroll of >= row_count emits delete(entire old text) + insert(entire new text).
4. The ATK bridge turns `text-changed::*` into `object:text-changed:{insert,delete}` with
   `detail1=offset`, `detail2=length` and, in `any_data`, the affected text obtained by calling
   `atk_text_get_text(offset, offset+length)` *inside the application* (`text_changed_event_listener`,
   event.c:979-1011), before the `signal_is_needed` filter. VTE only passes `(start,count)`, so the
   payload is produced by the bridge.
5. Special case: backspacing over a trailing space emits a synthetic 1-char delete + insert at the old
   caret position (`text_modified`, "Check if we just backspaced over a space", l.~424-440) before the
   normal diff. Ordinary typing is one `text-changed::insert` of the new character(s) (diff of old vs
   new snapshot) followed by `text-caret-moved`; the order text-then-caret is deliberate
   (`text_caret_moved_pending`). Not measured live (no safe way to type into the user's terminal).

## Known problems with large output

* **Per-frame full-screen cost, app side**: each tick under `cat bigfile` rebuilds the full visible
  snapshot (O(rows x cols) allocations: text, per-byte attributes, char offsets, linebreaks; offsets
  helpers `xy_from_offset` are O(lines)), and, because scrolled output shares almost no common
  prefix/suffix, the delta is nearly the whole screen. On this terminal that is ~21-32 K characters per
  event, delivered twice (delete + insert), up to ~60 times/s: a worst case of the order of 2-4 MB/s of
  signal payload per matching listener (estimate from sizes above; not measured under load because
  generating output would have disturbed the user's terminal).
* **Cost is paid even if AT-SPI listeners ignore it**: the bridge extracts the text before the
  listener filter; with Orca registered for `Object:TextChanged:*` the terminal also becomes the
  hottest event source on the bus.
* **Orca's mitigations are heuristic** (`ax_utilities_event.py` l.~308-390, 880-925): a 1 s window to
  recognise "terminal repaint" after up/down line navigation, `TERMINAL_REPAINT_EVENT_WINDOW_SECONDS`,
  dedupe of re-presented lines, `_is_terminal_being_flat_reviewed`. Insertions wider than the caret line
  are silenced as `AUTO_INSERTION_UNPRESENTABLE`. There is no structural information (no prompt/command
  boundary, no OSC 133) in the AT-SPI tree.
* **Assert-based crash hazard**: `vte_terminal_accessible_get_text` has
  `g_assert((start_offset >= 0) && (end_offset >= -1))` (0.80.1 l.1012). A client passing a negative
  start offset aborts gnome-terminal-server (killing every terminal tab). gorca must clamp offsets and
  never pass stale/negative values (crash isolation of the *app* is our responsibility here).
* Because offsets are screen-relative and the snapshot is rebuilt lazily on each call
  (`update_private_data_if_needed`), a client that reads text after a delay sees a different snapshot
  than the one the event described. Prefer using `any_data` of the event plus the event's offsets, and
  avoid follow-up `GetText` calls for terminals.

## Design implications for gorca (terminal handler)

* Treat each terminal as its own actor with a bounded mailbox; coalesce `text-changed` bursts per
  object to the last N ms (the frame cadence is 16 ms) and drop superseded insert/delete pairs.
* For typing echo use the key event path (Mutter `KeyEvent`) and the single-character insert signature
  instead of waiting for caret events.
* Consider registering `Object:TextChanged:*` per application (`app_bus_name` argument of
  `RegisterEvent`) only for the focused terminal and a catch-up poll (`GetText`) when it regains focus.

## Measured on a real VTE (2026-10-15, M6)

gnome-terminal 3.56.2 with libvte 0.80.1 in a private headless gnome-shell, 100x30 window, bash, driven by
`tests/term/vte_probe.py` (see `tests/term/README.md`; the recorded session is
`extensions/ext-vte/tests/fixtures/`). This corrects or settles several points above.

* **Text**: every one of the 30 rows, each followed by `'\n'`, empty rows included (`"$ \n" + 29 x "\n"`, 32 scalars).
  A written trailing blank is kept (`"$ cho \n"`); only unwritten cells are dropped. `CharacterCount` equalled the
  scalars of `GetText(0, -1)` and the caret is a scalar offset (the 31830/20995 mismatch of the first probe did not
  reproduce). Offsets in `Text` are scalars; only attribute runs are bytes, so the connector table says `Scalar`.
* **Typed character**: `text-changed:insert off=N len=1` with the character as payload, then `text-caret-moved N+1`, 2 to
  5 ms after the key reached the compositor. A key and its echo are that close: the key event and the screen change
  can arrive in either order.
* **BackSpace**: `delete off len=1` whose payload is the deleted character, then the caret. **Left/Right/Home/End**: caret
  only. **Tab** completion: an insert of the added text. **Ctrl+U**: a delete whose payload is what went. **History
  Up/Down**: an insert / delete of the command text.
* **Enter with output**: `delete off len=1 "\n"` immediately followed by `insert off len=N "output\n$ "` and the caret:
  the T-1 pair. A partial scroll (`seq 1 40`) is two deletes and two inserts with offsets that apply in order.
* **Flood** (`seq 1 100000000`, `yes`): one `delete` of the whole screen text and one `insert` of the whole new text per
  frame, 110 to 165 events/s, 5 to 30 KB/s of payload at 30x100, gnome-terminal-server at 60 to 82 % of a core. The
  last row is often a partial line (the program is mid-write).
* **Only registered events are emitted**: atk-bridge drops `text-changed`, `text-caret-moved` and
  `text-selection-changed` unless a listener registered them (`Registry.RegisterEvent`). A reader started alone heard none
  until `REGISTERED_EVENTS` listed them (connector, `events/session.rs`).
* **P-11 settled by source**: `RegisterEvent`'s `app_bus_name` is stored by registryd (`registry.c:1011`) but the
  `EventListenerRegistered` signal carries only `(sender, event, properties)` and the code sets the destination on the
  incoming call, not on the signal (`registry.c:1028`); atk-bridge parses the same three fields
  (`atk-adaptor/bridge.c:793`). Per-application registration therefore does not reach the application: D-5 is not
  available, a flooding background terminal is handled by T-4 (`Flooded`).
* **Focus**: the terminal widget got `state-changed:focused` only when the first key reached it on a headless seat; the
  Reader's focus presentation may be served from a snapshot without `FOCUSED` (see `docs/reports/m6-impl.md`, risks).
