# Mouse click in Firefox "does nothing" (owner report, 2026-10-23)

## Verdict
Injection and coordinate mapping are correct. The route target is wrong: it is the centre of the focused
object, not the virtual-cursor item.

## Evidence
- Live journal (read-only): `move stage=(876.0,605.5) win=953217793 outcome=done`, then `click b1 stage=(876.0,605.0)`.
  The live monitor is 3072x1920 at scale 1.7534 (GetCurrentState, read-only), so the logical screen is 1752 px wide;
  876 is exactly its horizontal centre, and the point sits in the middle of the content area. Earlier clicks at
  (863,985) show the same pattern: a big object's centre.
- `Pending::Route` / item click use `review_target()` (crates/gorca-core/src/commands/mouse.rs). Outside
  `Mode::Review` it returns the AT-SPI *focus* node. In `Mode::Browse` (Firefox documents open in browse mode) the focus
  node is the document (or whatever last held DOM focus), not the virtual cursor, so the pointer goes to the
  document centre. `win=0` on the click is only the placeholder of `PointerButton` (click at the current pointer; it
  carries no target). It is irrelevant.
- Hypotheses 1 and 2 refuted in a private headless shell, Firefox, route (`movewin`, PointerMove) + 2 s gap +
  `click L` (PointerButton, no motion), page_menu.html button: PASS at scale 1.0, 1.25 and 1.7534 (3072x1920 mode,
  the owner's). Resolver target and helper window matching are right under fractional scale. New:
  `contrib/gnome-shell-extension/testclients/test_routeclick.py`
  (`MODE=test_routeclick contrib/gnome-shell-extension/test-mapping.sh SCALE WORKDIR firefox`; `GORCA_RIG_VM` sets the
  virtual monitors, e.g. `"3072x1920 1280x800"` for fractional 1.7534).
  Press/release in the same tick with no motion before the press works in Firefox.

## Fix (not small, not done)
In Browse mode `review_target()` must return the NodeKey (and offset) of the virtual cursor's source node. Doc nodes
are `DocNodeId` (doc-local) and the source ids are `SrcNodeId`; there is no accessor from a `WebCursor` to a
`NodeKey` yet, so it needs a doc-actor/connector lookup (architect + rust-engineer). Interim option: in Browse mode
answer "No location" instead of clicking the document centre (a silent mis-click is worse). Tab-focused links work
today because focus is then the link.

## Risks
The live page/focus were not observed (live a11y bus untouched), so "focus was the document" is inferred from the
coordinates, not observed. Gecko first-DoAction drops are a separate issue.
