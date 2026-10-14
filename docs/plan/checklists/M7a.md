# M7a live checklist: browse mode for static pages in Firefox

Run on the real GNOME 48 desktop with `tools/gorca-toggle.sh` (Super+Alt+G), your normal Firefox,
Orca off. The gorca key is Insert (and Caps Lock unless it switches your layout). Browse mode
starts by itself when focus is in a page; it says "browse mode" or "focus mode" when the mode
changes. Firefox needs no setting; if it was started before gorca, open a page (or switch tabs)
once after starting gorca.

Compare every row with what NVDA says for the same keys on the same page. Note where gorca is
slower, louder, wrong, or silent.

## Pages

A Wikipedia article, a GNOME Help page, an MDN page, a login form, a news front page. Also
`tests/web/m7/pages/article.html` (open it with `file://`).

## Entering a page

| Step | Expected |
|---|---|
| Alt+Tab to Firefox with a page focused | "browse mode", the page title, then the line at the cursor |
| Load a new page (click a link, or Enter on a link in browse mode) | within about a second: "browse mode", the new title, the first line |
| Switch tabs and back (Ctrl+PageDown, Ctrl+PageUp) | the title, then the line where the cursor was in that tab |
| Focus the address bar (Ctrl+L) and type | letters are typed, nothing is swallowed; no browse keys while you are in the chrome |

## Reading

| Step | Expected |
|---|---|
| Down / Up | next / previous line, with "link", "heading level 2", "button", "edit" in front of what they belong to; blank lines are skipped |
| Left / Right | previous / next character; a space is "space", an empty line is "blank" |
| Ctrl+Left / Ctrl+Right | previous / next word |
| Home / End | first / last character of the line is spoken |
| Ctrl+Home / Ctrl+End | first / last line of the page |
| Page Up / Page Down | 25 lines up / down |
| Down at the very end, Up at the very start | "bottom", "top"; the cursor stays |
| Hold Down | lines keep up with the key, the last one is spoken in full, nothing old is spoken after you let go |

## Quick navigation (Shift = backwards)

| Step | Expected |
|---|---|
| H | next heading: "heading level N", the text; no wrap: "no next heading" |
| 1 .. 6 | next heading of that level (Shift+1 .. Shift+6 backwards) |
| K, U, V | next link, unvisited link, visited link ("visited link ...") |
| F, E, B, X, R, C | next form field, edit field, button, check box, radio button, combo box |
| L, I, G, S, Q, D, T, O | list, list item, graphic, separator, block quote, landmark, table, embedded object |
| Quick navigation onto a field | stays in browse mode (no "focus mode") |
| The page scrolls to the target | yes, after a short pause when keys are held |

## Links, buttons, forms

| Step | Expected |
|---|---|
| Enter or Space on a link, on a button, on a check box | follows it, presses it, toggles it |
| Tab onto a link | browse mode, the link is read, the cursor is on it |
| Tab onto a text field | "focus mode", the field is read; typing works and is echoed as in any edit |
| Escape in the field | "browse mode", the line of the field; the field keeps the focus |
| Escape on a drop-down list that is open | KNOWN GAP: gorca takes the Escape ("browse mode") and the list stays open; passing the first Escape needs the expanded state (M7b) |
| Enter on a text field in browse mode | "focus mode", focus moves into the field |
| Insert+Space | toggles focus mode by hand ("focus mode" / "browse mode"); Escape does not leave manual focus mode, Insert+Space does |
| Tab out of the field onto a link | "browse mode" |

## Safety (must never happen)

| Step | Expected |
|---|---|
| Type in the address bar, in a field, in a web app's text area | every key arrives; browse keys are never grabbed while you are in a field |
| Pass-next-key (Insert+F2) then a letter | the letter reaches the page |
| Learn mode (Insert+1) in a page | keys are described, nothing moves |
| Kill the tab (Ctrl+W) | silence or the next tab's title; no stuck keys, no repeated "loading" |
| Open a 10 000-line page | focus speech is immediate; browse keys say "loading" only until the first screen is built |

## Latency gates (private Firefox, not your session)

`tests/web/m7/run_m7.sh <label>` runs B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2 in private sessions (a private headless gnome-shell plus a RemoteDesktop session for clicks and keys; W2b gate 400 ms, see docs/decisions.md).
Gorca-side numbers of the hot paths: `cargo test --release -p gorca-core --lib browse::latency -- --ignored --nocapture`.
Results land in `bench/results/e2e/<label>/`.
