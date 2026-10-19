# Review: Firefox Enter on a rewritten row, Ctrl+C with a text control focused (3a8a8ee0)

Date 2026-10-19, reviewer. Branch `worktree-agent-a65577ede15293492` (main `0d28ad61` merged). Implementation
report: `2026-10-19-web-a11y-ff-enter-ctrlc.md`. **Verdict: BLOCK the Enter change; Ctrl+C MERGE AFTER FIXES.**

| Check | Result |
|---|---|
| `git merge-tree main HEAD` | clean |
| clippy core, doc source, model (`--all-targets -D warnings`) | clean |
| tests core + doc source | 938 + 57 + 35 fake_peer pass |
| Owner's Telegram case | **not reproduced** (implementation report: Enter opened the chat every time); the Enter change fixes a hypothesis |

## E-1 (Critical) The armed Enter clicks an item that was never announced, and so does the source

`browse/commands.rs` `web_activate_cmd`: an Enter that announced a displaced item stores `(node, at)`. The
next Enter on the same cursor node within 6 s clears `displaced` **and** `announced_change`, then clicks,
whatever the item is now. The comparison of name and role that the F-3 security change required is skipped.
Probe (added to `browse/tests.rs`, then removed): Button "Reply" → page re-uses it as Button "Delete" → 1st Enter
says "Read button Delete now.", no click → page changes it to Link "Buy now" → **2nd Enter clicks, says
nothing**. The branch's own test `a_row_rewritten_again_and_again_still_clicks_on_the_second_enter` asserts
exactly this (announced "Dave, typing", clicked "Dave, online"). Bob→Alice and Delete→Confirm delete are
reopened the same way.

The source does the same: `click_ack` lets the next click on the same node through within 5 s **without** the
pre-click read, so a second change after the refusal is clicked unchecked. With both layers relaxed at once,
nothing stops the click.

**Required design:** arm with what was said: the activation target's `(role, name)` (and its toggle states),
not the line. The armed Enter clicks only if the target still has exactly those. Otherwise it announces again
and re-arms with the new values; a role change is never armed. In the source, replace `click_ack` with
"refuse only if the live `(role, name)` differs from the `(role, name)` the Reader sent with the click":
carry the announced signature in `DocAction::Click` instead of a time window. A row whose churn is in the
target's own name (a typing indicator inside the link's name) then needs the real Telegram tree before
any rule is loosened: capture it first. Volatile parts could be exempted by data (a quirk row), never by
time.

## C-1 (Medium) The focus is taken from the field at selection time, not at copy time, and never restored

`release_text_control` runs inside the selection mirror, which runs after every debounced browse selection
change (`web_timer` → `web_mirror_selection`), so the first Shift+arrow blurs the input. On real pages that
fires `blur`/`change` (validation messages, autosave, search dropdowns closing, SPA state), and the focus is not
put back: after Escape from a search field, select, copy, Tab starts from the document instead of after the field.
`SelectionSpec.focus_field` claims "when the selection is not inside it", but the Reader never checks that.
**Fix:** only with a non-empty selection that does not intersect the field; once per selection (not per
extension); record the field and offer the focus back on leaving the selection, or decide explicitly (an
ADR line) that the blur is acceptable. Mention it in the user-facing notes.

## C-2 (Low)

- `focus_key` keeps an unjoined key forever; harmless (it is looked up and filtered by document).
- `GrabFocus` reply is ignored (bool false = not taken); log it, as `action` does (R-6).

---

# Delta review (1ab53c20): MERGE

| Check | Result |
|---|---|
| merge-tree vs main | clean |
| clippy core + doc source, tests | clean; core 943, doc source 57 + 35 pass |
| E-1 probe Reply→Delete→Buy now | Enter 1 "Read button Delete now." (no click), Enter 2 "Read link Buy now now." (no click), Enter 3 clicks |
| Bob→Alice | Enter 1 announces Alice, Enter 2 clicks |
| Dave typing→online (churn between Enters) | announced twice, the click comes only after the current text was spoken |
| Source | time window gone; `ClickAnnounced{name}` makes the pre-click read compare the live name with the announced one; a plain `Click` still compares with the mirror |
| C-1 trigger | `keys.rs`: Control_L/R press → `web_ctrl_down`, which requires browse mode, `SelectionModel::Native` with a mirrored selection, a non-empty `sel`, a focused `passes_through` field joined to this document whose text range does not intersect the selection; the source acts on `input`/`textarea` only and reports `GrabFocus` false as `Refused`. No selection → nothing |

Residual (Low, accept or document):
- Any Control press satisfies the trigger under those conditions: Ctrl+F, Ctrl+L, Ctrl+Z, and above all the
  bare Control tap users press to stop speech. Each releases the field (one blur). Bounded by the strict
  preconditions; document it, or later move the release to the `c` press with Control held if the
  keyboard monitor delivers it before Firefox handles the chord.
- No focus restore: accepted. Putting the focus back on a field would switch browse mode to automatic focus mode,
  which is worse than a blur the user caused by copying.
- The owner's live Telegram case is still not reproduced; this round makes the rule safe, it does not prove
  the Telegram fix.
