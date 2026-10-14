# M4a live checklist: object navigation and flat review

Run on the real GNOME 48 desktop with `tools/gorca-toggle.sh` (Super+Alt+G); gorca must be the only
screen reader. Specification: `docs/adr/0006-review-and-object-navigation.md` (section 15.1 acceptance 3,
section 20 for what the code does). Keys are the NVDA desktop layout unless said otherwise. The unmodified numpad
review keys work with **Num Lock off** only (K-6a, as in NVDA and Orca): with Num Lock on the keypad types digits
and Home/End into the application untouched, and review is reached through the `gorca+` chords. Check this: with
Num Lock on, `7` on the numpad types a 7 in a text field.
`gorca` = Insert (or KP_Insert with Num Lock off) or Caps Lock.

Settings are not in `gorca.toml` yet: simple review, review-follows-focus and the screen/object default are
constants, changed at run time by `review.simple_toggle` (unbound) and `gorca+7`.

## Keys

| Step | Keys | Expected |
|---|---|---|
| Review line / word / char | KP7 / KP8 / KP9 (lines), KP4 / KP5 / KP6 (words), KP1 / KP2 / KP3 (chars) | previous / current / next; first press in a window builds the model (about 100 ms), then instant |
| Spell | KP8, KP5, KP2 twice | line / word / character spelled letter by letter (third press: same) |
| Top, bottom, line start, line end | Shift+KP7, Shift+KP9, Shift+KP1, Shift+KP3 | first line, last line, first and last character of the line |
| Object navigation | gorca+KP8 parent, gorca+KP2 first child, gorca+KP6 next, gorca+KP4 previous, gorca+KP3 next in flow, gorca+KP9 previous in flow, gorca+KP5 current (twice: spelled name) | the object spoken like focus; "No next", "No previous", "No containing object", "No objects inside" at the ends |
| Review kind | gorca+KP7 / gorca+KP1 | toggles object review (reads the object's own text) and screen review |
| Review to focus | gorca+KP_Minus | review leaves, the focused object is spoken |
| Move focus to review | gorca+Shift+KP_Minus | the object under the review cursor takes focus; in GTK4 (no `GrabFocus`) it is selected in its list, else "cannot move focus here" |
| Activate | gorca+KP_Enter | the object's default action runs, nothing is spoken |
| Status bar | gorca+End (laptop: gorca+Shift+End) | the status bar text; "no status bar found" in apps without one |
| Review follows focus | gorca+7 | toggles; on (default), moving focus ends review |
| Orca preset | `keyboard.preset = "orca"`: KP_Subtract toggles review, KP_Home/Up/Prior lines, Orca+Ctrl+arrows objects | same commands |
| Laptop layout | `keyboard.layout = "laptop"`: gorca+Up / Down lines, gorca+Left / Right chars, gorca+Shift+Up / Down / Left / Right objects | same commands |

## Applications (ADR-0006 15.1 acceptance 3)

| App | Check |
|---|---|
| Settings, Wi-Fi panel | lines read in visual order; the Wi-Fi switch line says its name once; object next / previous reach every switch and row |
| Settings, Sound panel | sliders read with their value; the output list is reachable by object navigation |
| Files, list view | each row is a line; object navigation walks rows; gorca+End finds the status text if the view has one |
| Files, path bar | the buttons read left to right on one line |
| Text Editor, document | document lines read as lines (a wrapped paragraph as several); the status bar line is found |
| A GTK4 dialog (for example save) | the dialog title is not a line; the buttons read on one line; the entry says its name, "edit" and its text |
| Shell top bar | review reads the panel items in order (Shell stage coordinates); object navigation reaches each button |
| Quick settings | tiles read as lines; activate (gorca+KP_Enter) toggles a tile |
| A notification banner | its text is reachable by review while it is shown |
| GNOME Terminal | review reads the visible screen lines (generic path; M6 polishes it) |
| A hung application (SIGSTOP a test app you started yourself) | review keys speak "not responding" or the degraded neighbour within a fraction of a second and the reader stays usable; do not stop a system process |

Anything that reads in the wrong order, misses a visible control, or speaks twice for one key is a bug: note the
app and the command and run `gorca+F1` (node info) on the object.

## Gates (automatic)

`bench/e2e/run.sh m4a-r1 --only R1,R1s,RS --reader daemon:<abs>/target/release/gorca,--impersonate-orca --reader-env GORCA_M4=1`

| Gate | Result on 2026-10-14 (load 11 to 20, powersave governor) |
|---|---|
| R1 p99 <= 3 ms | 0.43 ms median of 5 repeats (`m4a-final2`, p50 0.20 ms); 1.02 ms median of 3 (`m4a-r1-full`, busier host) |
| R1s p99 <= 4 ms (under L3-50k) | 0.43 ms median of 5 (`m4a-final2`), 0.83 ms (`m4a-r1-full`); one repeat of five had p99 3.5 ms |
| `lost` = 0, `not_consumed` = 0 | 0 in every repeat of `m4a-final2` and `m4a-r1-full`. **One repeat of `m4a-final` (R1, 1 of 3) had `lost` = 2 with p99 3.0 ms at load 14: kept, not explained (suspected host scheduling; not reproduced in 10 further repeats)** |
| RS (correctness through the real connector) | 0 failures (`m4a-final`) |
| R2 cold first step, fake app | 7 to 9 ms warm daemon (`m4a-final`), about 100 ms in the first runs (informational, naive fetch strategy) |
| R1 unit test (2 000 steps, 600 zones) | `review::tests::r1_a_step_on_a_big_valid_model_is_fast_and_pure`: 3 ms release bound, 30 ms debug bound |

Results: `bench/results/e2e/{m4a-r1,m4a-r1-full,m4a-rs,m4a-final,m4a-final2}/` (the `m4a-rs` directory also holds the
first RS runs that failed before the fixes of the same day).
