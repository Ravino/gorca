# Firefox SPA: stale window title, Enter that goes to the wrong element

Date: 2026-10-18. Reported by the daily user on Telegram Web (React-style SPA). Corpus page:
`tests/web/m8/pages/chatlist.html`, scenario `M8-CL` (`crates/gorca-bench/src/e2e/m8_steps.rs`, fn `cl`).

## 1. gorca+T speaks the old title

Where the title comes from: `info.window_title` (`crates/gorca-core/src/commands/handlers.rs`,
`ask_node` / `answer_node`, `commands/info.rs::window_title`). It walks the mirror ancestry of the
focused node to the first `Window` or `Dialog` and speaks that snapshot's `name`.

Root cause: the walk uses `MirrorDirectory::lookup`, which returns whatever snapshot the mirror
holds, stale or not. A window name has no refresh path of its own. It only changes when an
`accessible-name` event for the frame arrives *and* something later re-fetches the node, and
Gecko fires `EVENT_NAME_CHANGE` for a title change on the document accessible
(`refs/firefox/accessible/generic/RootAccessible.cpp`, `DOMTitleChanged`), not reliably on the
frame. So the cached frame name can be as old as the last fetch. Alt+Tab is right because the Shell
extension reads the compositor's live title.

Fix: `gorca+T` re-reads the window node once (Interactive fetch, the connector's deadline), speaks
the fresh name, and falls back to the cached name when the fetch fails or times out (never silent,
never blocks). A mirror miss first fetches the focused node, then the window. The fetch result is
applied to the mirror by the existing `ReaderFetch` path, so the cache heals too.

Honest limit: in the private rig the old code also answered correctly (Firefox 153 delivered the
frame name change there, `CL.fresh.title`, `CL.same.title`, `CL.ttl.say` pass with and without the
fix). The live Telegram case could not be reproduced here; the fix removes the dependency on the
event rather than proving the missing event. The unit test
`window_title_is_fresh_after_a_page_changes_it_and_falls_back_to_the_cache` pins the contract.

## 2. Enter "goes into the void", then the account menu opens

Reproduced in the rig (`CL.foc.enter`, base run `cl-base2`: heard "Account menu, button | Account
menu opened"). Sequence from the AT-SPI tail: focus on the chat link, the page re-renders the list
(the focused node is destroyed), Firefox fires `Focus` on the **document** accessible, gorca treats
that as "returned to the page" and `web_place`s the browse cursor at the **top of the page**; the
next Enter clicks the first clickable thing there, the account menu button.

Root cause 1 (the user's symptom): `web_on_focus` (browse/commands.rs) moved the cursor to the
document root when the focus fell from a control of the same document to the document itself.
Fix: that transition (previous focus node in this document, now the root, Browse mode) is the page
destroying the focused node. Keep the cursor, say nothing. Return-to-the-page cases (tab or window
switch) are unaffected because their previous focus is in another document or none.

Root cause 2 (silent failure, latent): `DocAction::Click` errors were discarded (`spawn_action`
only reported `Focus` failures), so a click on an object the page had already replaced did nothing
and said nothing. The fix has three parts:

* `action()` classifies "object does not exist" bus errors (`UnknownObject`, `Defunct`,
  `NoSuchObject`) as the new `SourceError::Gone`.
* On `Gone` for Click/Focus the source refreshes the target's parents (`GetChildren` plus the
  identity ladder, up to 4 levels) and repeats the action **only if** the logical node continues in
  a live object with a new path and the same role and (non-empty) name. Otherwise the answer is
  `Gone` and nothing is clicked. Gecko paths are never reused (monotonic, seen in the rig: 152,
  then 166), so a dead path cannot hit another element by accident; the guard covers identity
  mis-pairing.
* A failed Click raises `DocNotice::ActionFailed`; the Reader says "that item is gone" or "can't
  activate that" (catalogue `web-item-gone`, `web-cant-activate`, en + ru).

NVDA comparison: `browseMode.py::_activatePosition` resolves the object at the caret
(`NVDAObjectAtStart`) and returns silently when there is none; its buffer lives in-process and is
re-synced on every mutation, and a focus move to the document does not move the virtual caret.
gorca now keeps the same caret behavior and, unlike NVDA, says so when the item is gone.

## Tests

* Unit: `commands::tests::window_title_*`, `browse::tests::focus_falling_to_the_document_*`,
  `browse::tests::a_failed_click_is_said_never_silent`, `tests/fake_peer.rs` (click on a live
  node, on a replaced node that follows identity, on a gone or re-contented node that is refused).
* E2E `M8-CL` (15+ steps, 3 repeats, run `cl-fix1`): fresh Tab+Enter, after re-render, cursor
  under re-render, bump to top, insert at top, focus destroyed (`CL.foc.enter`), item removed,
  title after open, title after a timer retitle.

## Not done / risks

* The stale title was not reproduced; only the contract and fallback are tested.
* A removed item under the cursor lets position restore settle the cursor on a neighbour (as
  NVDA does); Enter then opens that neighbour. The "gone" message is for the case the source
  itself proves the node dead.
* `CL.fresh.focus` was silent once on a cold first step in one run (rig warm-up, same as other
  first steps under load 11 to 16); not a product signal.
* Two experiment result dirs (`cl-title-nofix*`, passing old-code runs plus one cold-start flake)
  were removed after their numbers were recorded above.
