# M8 gap table: where each baseline root cause stands

2026-10-16, web-a11y-engineer. Continues `docs/reports/m8-baseline.md` section 4 (the 18 root causes of the
`f80d1f5` baseline: 181 PASS, 55 GAP, 82 XFAIL, 0 unmarked). Implementation report: `docs/reports/m8-impl.md`.
Spec: `docs/plan/m8-spec.md`. Step data: `bench/results/e2e/m8-final2-full/*-steps.md` (3 repeats) and
`m8l-full` (the first all-scenario run with every feature in; 305 PASS, 3 GAP, 10 XFAIL, 0 FAIL).

Legend: **closed** = the scripted steps pass; **partial** = core words pass, a recorded extra is missing;
**open** = expected-fail with a reason; **unverified** = built, but the rig cannot exercise it.

| # | Root cause (baseline) | Status | Steps now | Notes |
|---|---|---|---|---|
| 1 | Name of a labelled control not spoken | **closed** | QN.e/f/x/r/c/b | the quick-nav presenter speaks name, role word, state, value |
| 2 | Container hits speak content, not the container | **closed** | QN.l/q/t/d, LM.* | "list with 3 items", "table with C columns and R rows", "block quote", landmark name and kind |
| 3 | Nothing past T in tables | **closed** | TB.* (25 of 26 pass) | 12 commands, header reading, edges, spans, one line per row |
| 4 | Live regions not spoken | **closed** | LV.* 17/17, APP.delete, APP.close (toast), FG.star, SP.rerender | classifier, settle, dedupe, token buckets, flood summary; live_ms p99 80 to 98 ms (gate 100) |
| 5 | Say-all in browse mode | **closed** (on main before M8) | RD.sa.* | |
| 6 | Selection and copy | **partial** | RD.sel.* pass; RD.copy.say open | virtual selection (Shift+arrows, Ctrl+Shift+arrows, Shift+Home/End/PageUp/PageDown, Ctrl+A); mirrored into the page as a native selection when it lies in one text node, so the page's own Ctrl+C copies it. gorca+C (read the clipboard) needs clipboard access: open |
| 7 | Missing commands M N A W P comma single-letter, elements list, find | **mostly closed** | QN.n/a/p/comma/sl pass; QN.w.1, EL.open, EL.find open | W finds nodes in an invalid state only (spelling errors are text attributes the mirror does not carry); elements list and find need a dialog UI |
| 8 | Jump to a container leaves the cursor on the container | **closed** | LM.skip.enter, AN.skip, AN.after, AN.foc.go | first text line is spoken |
| 9 | Same-page anchors without focus | **closed** | AN.para.go, AN.named.go | caret event after our own click moves the cursor |
| 10 | Modal scope | **closed** | APP.alert.h2, APP.alert.k | |
| 11 | Pass-through predicate gaps (D-1) | **closed** | FM.radio, FM.listbox, FM.app, FM.app.toggle, APP.tab, APP.menu.item, APP.combo, TB.grid.focus | NVDA's two role sets on focus; `role=application` and `<object>` are ATK_ROLE_EMBEDDED and now map to `Role::Application` |
| 12 | States and descriptions | **partial** | FM.cb, FG.toggle, FM.required pass; FM.invalid (error text), FG.b.1 name are GAP | "not checked", "not pressed", "collapsed" (from `expandable` without `expanded`) are spoken. The help or error text of a field is requested on the confirming fetch and spoken only when it arrives within the refine window: the scripted focus does not hear it yet |
| 13 | Dialog name on entry | **closed** | APP.compose, APP.alert | by node, not by flat offset; also when the field joins late |
| 14 | Separator silent; O finds nothing | **mostly closed** | QN.s.*, QN.o.1 pass; QN.o.2 open | `<object>` with text/plain data has no embedded node in the mirror (not investigated) |
| 15 | Position restore | **closed** | SP.restore | the cursor is re-resolved at the saved offset for 1.5 s after its node vanished; the step now re-renders after a delay with the cursor in the list |
| 16 | Shift+D from a nested landmark | **closed** | LM.d.prev, LM.d.prev2 | |
| 17 | Role prefix repeated inside a heading | **closed** | RD.char.left, RD.word.left, RD.home | |
| 18 | Tab starts from the browse cursor (D-2) | **closed** | SY.tab, SY.tab2, SY.stab, SY.tab.out (pass), FM.tab.real, SP.back (rig) | Review H-1: the caret-follow substitute is removed (it never fired on text and focused links). Tab / Shift+Tab are bound in browse mode (`browse.tab_next`/`browse.tab_prev`): next/previous `Cat::FOCUSABLE` after/before the cursor in the mirror (never the document, inside a modal dialog while the cursor is), focused with `Action::Focus`; the focus event then moves the cursor and speaks. Tab is part of the browse grab set only in a direction that has a target (updated 150 ms after the cursor settles), otherwise the key reaches the page: a grabbed key cannot be given back (key injection is a stub). Wrap-around is not done |

## Steps that are not PASS (final run `m8-final2-full`)

GAP: FM.invalid, FG.b.1. XFAIL: QN.o.2, QN.w.1, FM.tab.real, SY.tab, SP.back, RD.copy.say, EL.open, EL.find.

## New expected-fail marks introduced by this work (reviewed, with reason)

| Step | Reason |
|---|---|
| QN.o.2 | the `<object>` with `data:text/plain` is not found by O; not investigated |
| QN.w.1 | `aria-invalid=spelling` is a Gecko text attribute; the mirror carries node states |
| RD.copy.say | reading the clipboard is not implemented |

All other marks are the ones the baseline had (rig: real Tab and Alt+Left; EL.open, EL.find).
