# Telegram Web A chat list: keys, focus and the cold-focus trap (2026-10-26)

## telegram-tt source facts (refs/telegram-tt, shallow clone; src/components)

* `ui/ListItem.tsx`: wrapper `div.ListItem` (no tabindex, no role) around the button element, `a.ListItem-button`
  when the row has an `href` (chat rows: `#<chatId>`), else `div[role=button]`. The button has `tabIndex=0`.
* **A chat opens on `mousedown`, not on click**: `handleMouseDown` calls `handleClick` -> `onClick` for the main
  button. The `click` handler (`handleClickEvent`) only `preventDefault()`s. There is no keydown handler on the
  row. So a native Enter on the focused link produces a `click` that does nothing: **Enter only works through a
  synthesized mouse press** (AT-SPI `click`/`jump` action; Gecko dispatches mousedown/mouseup/click). Enter handed
  to the page (focus mode, or browse grabs off) is inert. This explains "Enter does nothing" whenever gorca is not
  in browse mode.
* `left/main/ChatList.tsx`: `div.chat-list.custom-scroll` (InfiniteScroll, no tabindex, no role). Global key
  handlers: only `Alt+ArrowUp/Down` (document `keydown` through `useHotkeys`, preventDefault) and, in the desktop
  app only, `Cmd/Ctrl+Digit`; `ChatFolders` `Ctrl+Shift+Digit`. No handler for plain letters, arrows or Enter in the
  list; the list is not a roving-tabindex widget. Rows are absolutely positioned (`top: Npx`) in a window.
* No `role`/`aria-*` on the list or rows (only `aria-disabled` on disabled buttons). The scroll container is
  keyboard-focusable only because Firefox focuses scrollable containers (the live dump showed `chat-list` and,
  at times, the row wrapper as FOCUSABLE).
* Focus on refresh: nothing in the app moves it; Firefox keeps the document focus (or the restored element).

## What a screen reader must do

1. Activate rows through the page's own click path (AT-SPI action), never by forwarding Enter.
2. Stay in browse mode when focus lands on a scroll container, a wrapper or a link in the list.
3. Never leave the grabs off waiting for a focus snapshot that is not coming.

## Reproduction in a private session

`tests/web/telegram/run.sh A2 firefox chatlist OUT "?focus=link|wrapper|container|none"` (page `a2.html`: telegram-tt
markup, mousedown opens the chat, no key handlers; scenario `chatlist`: F5, optional `TG_TABS=n` Tabs, H, I, Enter),
with `TG_PLUGINS=on` and the debug build. All four focus variants (and Tab from the document onto the scroll
container and the links) stay in browse mode, H speaks the heading, Enter opens the chat (title `OPENED n id`).
**The owner's failure did not reproduce in the rig.** Traces: `/var/tmp/tgcl/<variant>/gorca.log`
(`RUST_LOG=info,gorca_core::browse=debug`).

## A core defect found while looking for the cause

`Loading { known: false }` (a focus event whose node has no directory snapshot yet) ended only on a new focus
event: the grace timer needs `known: true`, and nothing read the directory again. A page that does not move the
focus again stayed with the browse grabs off ("keys are typed", Enter handed to the page) while every other
symptom matches. Test `a_cold_focus_that_turns_out_not_to_be_a_field_ends_in_browse_mode` failed before the fix
(mode stayed `Loading { known: false }` after the snapshot said link). Fix: `web_recheck_cold_focus` re-reads the
directory on each timer tick (30 s cap, trace `what="cold_focus_resolved"`). Not proven to be the live cause; the
owner's trace shows it as `FocusPending ... known=false` followed by no `FocusIn`, or `cold_focus_resolved`.

## Detached-node rebuilds

Live journal: `doc batch rejected ... not in the document tree` appears about every few minutes (07:23, 07:32,
07:40 on 2026-10-26), not on every chat-list interaction; the rig never produced one (no churn pattern did). The
`stage=` field in the deployed build will name the op.
