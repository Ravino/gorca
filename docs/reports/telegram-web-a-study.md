# Telegram Web A (and K) study: gorca in Firefox and Chromium

Date 2026-10-24, web-a11y-engineer. gorca 3db4ebea release binary (copied), private headless GNOME Shell world
(`tests/web/telegram/run.sh`, real keys through RemoteDesktop, mock SSIP). No login, no live session.
Orca 48.1 baseline: one Chromium run (`tests/web/chr0/signal_cmp`, scenarios `tg_list`, `tg_chat`).
The session was interrupted by a machine reboot: the matrix is partial (see Coverage). Raw runs: `/var/tmp/tg-study/runs/*/run.log`
(not in git; labelled by app-browser-scenario; `-scrollup`/`-scrollup2`/`-scrollup3` are reruns, the first is kept as a failing criterion, see F3).

## 1. Structure studied

* Live login screens (headless Chromium dump, `web.telegram.org/a/` and `/k/`): A shows `div#auth-qr-form > h1 "Log in to Telegram by QR Code" +
  ol > li`, a canvas and a spinner; K shows its own auth pages. The phone screen (source `AuthPhoneNumber.tsx`) is `h1`, `p.note`, a form with
  `InputText`, a checkbox and buttons: plain semantic HTML, fine for browse mode.
* A source (Ajaxy/telegram-tt, `/var/tmp/tg-study/tt`): almost no ARIA. No `aria-live` anywhere, no role on the message list. Chat row =
  `div.ListItem > div.ListItem-button[role=button][tabindex=0]` (no name attribute, the name comes from content: avatar initials, title h3,
  time, preview, badge), rows absolutely positioned (`top: N px`) in an `InfiniteScroll` that renders a window of the list. Message =
  `div.Message#message-ID` with no role or tabindex, inside `.message-date-group > .sticky-date` and sender groups. Composer =
  `div#editable-message-text[contenteditable][role=textbox][aria-label]`, placeholder in a sibling span.
* K source (morethanwords/tweb, `/var/tmp/tg-study/tweb`): more ARIA. `a.chatlist-chat[href]` in `ul[role=presentation]`, history is
  `div.scrollable[role=region][aria-label="Message history"]`, bubbles `div.bubble[role=article]` (tabindex=0 only with `?a11y=1`), a hidden
  `div[role=status]` connection announcer, composer `div[role=textbox]` with CSS placeholder.
* Fixtures: `tests/web/telegram/a.html`, `k.html` (windowed chat list, prepend on scroll, composer, `?f=incoming,flood,churn,typing,net`,
  `?dbg=1` puts window and scroll offset in the document title), `echo.html` (composer control), `model.js`, `tg_drive.py`, `run.sh`.

## 2. Findings

Priority: P1 breaks the owner's flow, P2 wrong or noisy, P3 polish. Browsers: FF = Firefox, CR = Chromium; both unless stated.

### (a) Generic core bugs

| # | Finding | Repro | Pri |
|---|---|---|---|
| F1 | After Escape closes the open chat (page hides/removes the pane, focus falls to the document), Tab says "no next focusable element" and Enter is silent: the Telegram "stops working until reload" bug reproduces (A and K, FF and CR). The cursor was in the removed pane, `lost_to_document` does not cover a cursor in a hidden/removed subtree | `run.sh A firefox chat` steps `esc 2`, `tab`, `tab`, `enter again` | P1 |
| F2 | Typing echo is doubled: two SPEAKs 2 ms apart, the second preceded by STOP ('h','h','i','i'). Also on a plain `<input>` (FF), so not a Telegram quirk. Not compared with a build that predates C3b | `run.sh E firefox echo OUT "?k=input"` and `?k=cerole`; A/K `type hi` | P1 |
| F3 | The page does not follow the browse cursor: 150 Up arrows through the history leave `scrollTop` unchanged (FF 2089, CR 1665) although `ScrollIntoView` (Component.ScrollTo(0) on the leaf) is issued. The virtual list never loads older messages. At the top of the loaded window the cursor runs on without any cue (Day 10, "online", then the chat list rows in reverse order). Hypothesis: ScrollTo on a text leaf is a no-op; fall back to the nearest block/ancestor, verify with the node extents, or send PageUp to the nearest scroller at an edge | `run.sh A firefox scrollup OUT "?dbg=1"` (`page:` lines) | P1 |
| F4 | Duplicate presentation after an unrelated mutation: Enter on a row speaks the row, then (Firefox) the preview text and the row again (the badge disappeared and the text node was replaced). Chromium repeats the row once. Fixture re-sets identical text (not differential like Teact), so part of this is fixture noise; the mirror still re-reads a name that did not change in a meaningful way | `list` scenario, step `enter (browse)` | P2 |
| F5 | K: Tab from the search field lands on the scroll container (`section`, no name), gorca speaks its entire content as the name ("browse mode, Alice Alice 15:00 ..."), a very long utterance. Chromium not run for K | `run.sh K firefox chat` step `tab 2` | P2 |
| F6 | Chromium: sending (Enter in the composer, page clears it) speaks "new line" twice; Firefox is silent | `run.sh A chromium chat` step `enter (send)` | P2 |
| F7 | Cursor stops are very fine-grained for messages: time, text, sender are three separate stops, read time-first when going up. No block-level (message) navigation | all `chat` runs | P3 |

### (b) Telegram-specific quirks (rows for the telegram-web plugin, ADR-0026)

| # | Quirk | Proposed row |
|---|---|---|
| T1 | A has no live region: incoming messages, sent messages and typing indicators are silent (14 s window with `incoming`, 12 s with a 30-message flood: 0 utterances; the mirror stays correct, the next Down reaches the new message) | plugin watch on `.Message` additions under `.messages-container`, announce sender and text, coalesce floods ("N new messages") |
| T2 | Chat row name is "AL Alice 15:00 see you soon 1": avatar initials first, time, no "unread" word. Orca 48.1 says the same (parity) | skip `.Avatar` text, say "1 unread" for `.ChatBadge`, optional time after the preview |
| T3 | K: link name doubled ("Alice Alice 15:00", avatar aria-label plus title) | skip `avatar-element`/`.dialog-avatar` label |
| T4 | A messages have no role, K bubbles are articles but not focusable without `?a11y=1` | message role row: `.Message` as a navigable item, not a focus-mode widget |
| T5 | Escape in the composer: gorca eats the first one (focus to browse), the second closes the chat (page handler), which triggers F1 | after the fix of F1 nothing; until then document the double Escape |
| T6 | Orca 48.1 (Chromium, A): same names, Escape closes the chat and later arrows are silent; its Tab after that is fine. So Orca has no better baseline here | none |

Pointer proposal in data: commented rows in `crates/gorca-config/product/web-apps.toml`; they need the plugin/watch schema (not available), so they are not active.

## 3. Not found

* Stale mirror after list re-sort (`?f=churn`, 7 s idle): 0 utterances in FF and CR, Enter opened the row under the cursor in both. Weak test: the cursor sat on row 0 in the first FF pass; the 28-Down version was not run after the reboot.
* Messages arriving while the cursor rests at the end: cursor order stays right (Down reaches the new message), no spam.
* Prepend on scroll-up could not be exercised because of F3. Chat-list window crossing (`listscroll`, 120 Downs) not rerun after the reboot; the first 60-Down run stayed inside the window.

## 4. Coverage and caveats

* Run: A FF {list, chat, scrollup x3, listscroll(60), incoming, flood, churn}; A CR {same, scrollup3}; K FF {list, chat, scrollup(70), listscroll(60)}; echo FF (cerole, input); Orca CR {tg_chat}. Not run: K Chromium, login screens under gorca, `typing`, `net`, `a11y=1`, Orca on Firefox and K.
* The `listscroll`/`scrollup` FAIL lines of the first runs are criterion errors (arrow budget below 3 stops per item), kept as recorded.
* Fixture rows re-set unchanged text on every render, unlike Teact (differential): affects F4 only.
* Orca keys did not drive browse mode in the rig; the Orca column is naming parity only.
