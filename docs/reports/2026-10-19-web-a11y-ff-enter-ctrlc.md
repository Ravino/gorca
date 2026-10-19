# Firefox: Enter that does nothing on a chat list, Ctrl+C that copies nothing (2026-10-19)

Rig: `tests/web/ffspa/` (page `tgapp.html`, drivers `drive_chat.py`, `drive_copy.py`, `run_all.sh`).
Real keys (Mutter RemoteDesktop) into Firefox 153 in the private headless shell, gorca release build,
speech read from the SSIP log, clipboard read back through the RemoteDesktop session. Host load 15-25
during every run (label: loaded).

## Bug 2: Ctrl+C copies nothing (root cause found, fixed)

Focus in an `<input>` or `<textarea>` (a chat composer or search box) while the browse selection is
mirrored into the page: Firefox copies the focused text control's own selection, so the document
selection never reaches the clipboard (`No selection owner available`), and gorca still says "selected".
Not the cause (all copy correctly): contenteditable composer, a page `copy` handler, focusable
bubbles, `user-select:none` time stamps (left out of the copy, though the virtual selection speaks them).

Fix: `SelectionSpec.focus_field` (the joined node of the last focus event, `focus_key`; `focus_node`
is stale after a join miss); the source reads its `tag` attribute and, for `input`/`textarea`, does
`GrabFocus` on the document before setting the ranges (`release_text_control`). The field keeps its
text; Enter/focus mode returns to it. A contenteditable host keeps its focus.
No gorca clipboard fallback was added: the Shell-extension route needs a login and user extensions are
disabled on the host; the Mutter RemoteDesktop/portal route lights the screen-sharing indicator
(ctrl-c-passthrough.md); Mutter 48 has no data-control.

## Bug 1: Enter does nothing (symptom reproduced, root cause not confirmed on Telegram)

Not reproduced (Enter opens the chat every time, 3 Tab/Enter rounds): unkeyed re-render, keyed rows,
DOM moves (Firefox recreates the nodes), narrow layout hiding the list, `<a>` and `div role=button`
rows, Escape consumed by gorca at the composer then Escape to the page. No join miss, no stale mirror,
no Loading state: Gecko's own events kept the mirror right, so a Gecko focus-miss repair is not
needed (CHR-0 F-1 stands).

Reproduced: a row the page rewrites every ~1 s (typing indicator, time stamp). Each rewrite under the
cursor marks the node displaced; Enter then only re-speaks the changed line, and the source's own
pre-click check (mirror name vs live name) refused the click too, so no Enter ever clicked ("link,
dave, typing" again and again). Baseline main: FAIL (2 Enters, no click); fixed: the first Enter
announces, the second clicks.
Fix: the Enter that announced a displaced item arms the next Enter on that node for 6 s
(`displaced_armed`), and the source lets the click through after it refused once for the same node
(`click_ack`, 5 s). Unproven on the live Telegram: if its failure is another mechanism this does not
cover it; `gorca_phase` lines `web_activate displaced=..` and `click_displaced` identify this one.
