# Form labels on Tab, and Backspace in Chromium-engine editables (2026-10-19)

Branch `worktree-agent-a6e303420193c1fbb`, base main `a6f5387c`. Release build in `/var/tmp/gorca-lbl-target`.
All runs in private sessions (headless gnome-shell, private a11y bus), host load 12-18, swap 85-97 % (other agents).

## 1. Tab onto a form field: which name sources does gorca miss

### Method
* Pages: `tests/web/forms/pages/labels.html` (26 label sources, one control each) and
  `tests/web/forms/pages/login-real.html` (Google, GitHub, Telegram K and A, MUI, Bootstrap floating label, search shapes).
* Scenarios (M8 harness, `crates/gorca-bench/src/e2e/m8_steps.rs`): `M8-FLB` and `M8-FLR` focus the n-th focusable
  node in document order (`Act::FocusNth`, a new step that does not need a name) and require the label words;
  `M8-FLT` walks the login page with REAL Tab presses typed into the browser.
* `tests/web/m7/ax.py` `focusables` command dumps name, description, states, attributes and relations of every Tab stop;
  `m8.rs` writes the dump to `/var/tmp/gorca-fl-dump/<engine>-<doc>.json` (this is the data behind the table below).
* Orca reference (`refs/orca/src/orca/`): `generator.py` `_generate_accessible_label_and_name` (label from the LABELLED_BY
  relation, else the AT-SPI name, else the description as name, `_generate_accessible_name`);
  `scripts/web/speech_generator.py` `_generate_accessible_label` -> `script_utilities.infer_label_for` ->
  `label_inference.py` (text to the left, right, above, below the control, for entry / password / checkbox / combo /
  list box / radio whose name is empty); `generator.py` `_generate_accessible_placeholder_text` (placeholder-text /
  placeholder attribute spoken when the entry is empty); Orca speaks the group (legend) name when the focus enters a new
  fieldset.

### Result (3 repeats each, same in every repeat)
Firefox 153: `M8-FLB` 21 of 26 pass, `M8-FLR` 16 of 17. Chromium (Chrome for Testing): `M8-FLB` 22 of 26, `M8-FLR` 16 of 17.
Everything the browser turns into an AT-SPI name is spoken on focus: `<label for>`, wrapping label, `aria-label`,
`aria-labelledby` (one and two ids, hidden target, React-style sibling div), `title` only, `placeholder` only (input,
search, textarea), sr-only label, label + placeholder + title together, `aria-describedby` text after the name,
checkbox labels, icon button with `title`, role=searchbox, role=textbox contenteditable with `aria-label`, and the
Google / GitHub / Telegram A / MUI / Bootstrap markups. Firefox real Tab (`M8-FLT`, with the first-key priming) speaks the
same words in order (16 of 16 when no key is lost; Firefox drops 1-3 typed keys in some repeats, known RIG_KEYS flake).

The failures, with what the browser exposes (dump) and what Orca does:

| # | Markup | AT-SPI (Firefox / Chromium) | gorca says | Orca says | Root cause |
|---|---|---|---|---|---|
| 1 | `<div>Romeo label</div><input>` (unassociated visual label; React/Material forms, Telegram-like) | name `""`, no relation, no description / same | "edit" | label inferred from the text to the left or above | gorca has no label inference; name is the only source |
| 2 | `<label for style="display:none">` | name `""` / `""` | "edit" | same plus inference | not a bug (accname excludes it), inference would cover it |
| 3 | `<div contenteditable>` + sibling `<label>` (Telegram Web K phone field) | role 85 (section), states EDITABLE, FOCUSABLE, name `""` / same | "panel", no "focus mode" | "edit" in focus mode, label inferred | two gaps: no inference; the focus on an editable section is not treated as an edit (`web_on_focus` hit is true, so the mirror state for this node apparently lacks EDITABLE or the role maps wrongly; not diagnosed, `translate.rs` / `map_states`, C3 territory) |
| 4 | `aria-placeholder` on `role=textbox` | Firefox: name `""`, attribute `placeholder-text` / Chromium: name = placeholder (passes) | Firefox "edit, blank" | placeholder spoken | gorca never reads `placeholder-text` (registered by the C3 normaliser as `NameFrom::Placeholder` only for Blink's name) |
| 5 | input inside `<fieldset><legend>` | n/a (group name is an ancestor) | field name only | group label first when entering the group | no group (legend) announcement on focus into a different fieldset |
| 6 | `name="sierra_field_name"` only | name `""` | "edit" | "edit" | speculative expectation of mine, not a gap (remove) |

So the owner's symptom is NOT reproduced for the standard label sources on either engine: Tab onto a field with
`label for`, `aria-label`, `aria-labelledby`, `title` or `placeholder` speaks the label. What is missed is the
"nameless control with a visual label next to it" class (rows 1, 3, 4, 5), which Orca covers by inference. Unverified: the
exact markup of the owner's login pages (Google's current sign-in page and Telegram K were approximated, not fetched).
Also unverified: Chromium real-key Tab (`M8-FLT` on Chromium was run once with the first-key priming missing; the
Firefox run after the fix is the reference).

### C3 (M22a, worktree agent-aa830fe5493b2696e)
Its head commit is "W-6 registers, reparent and invalidation; web overlay reads fused entries; one flat state mapping".
It adds the `name_from` register (`gorca-sem-atspi/src/attrs.rs` `name_from_of`: Placeholder, Content, Author) and the S4
"inferred" stratum in `gorca-fuse`, but nothing in `browse/` or `present/` speaks a placeholder, infers a label or
announces a legend (grep for infer / placeholder / text_left finds no consumer). It does NOT fix rows 1, 3, 4, 5.

### Proposal for C3 (not done here: `present/flow.rs` and `browse/` are C3's)
In the focus / quick-nav presenter, for roles entry, password, combo box, check box, radio button, list box whose fused
name is empty, in this order (Orca `generator.py` + `label_inference.py`):
1. AT-SPI description as the name (Orca `_generate_accessible_name`);
2. the `placeholder-text` / `placeholder` attribute, spoken after the role when the entry is empty, never when it equals the name;
3. inferred label: the mirror's flat text of the previous inline run on the same line, else the closest text block
   above (cap about 80 chars, skip text that itself belongs to another control), computed at focus time from the mirror
   (S4 inferred, taint-aware);
4. the legend name (group ancestor with a name) when the previous focus was outside that group.
Row 3 additionally needs the mirror to pass EDITABLE for role-less contenteditable sections so `passes_through` gives focus mode.
`M8-FLB`/`M8-FLR` rows LB.sibling, LB.ceph, LB.legend, LR.tgk.phone are the acceptance tests (they fail on main today;
they are left unmarked so the gate shows them).

## 2. Backspace in Chromium-engine editables

Harness: `tests/edit/run_edit.sh` ED-CH* scenarios (3 repeats, Chrome for Testing), baseline binary = main `a6f5387c`
(results `bench/results/e2e/ed-ch`). New targets `ED-CHLA` / `ED-CHLB` use `tests/web/m3b/pages/composer.html`, two
contenteditable composers that rebuild their spans on every edit (A: `beforeinput` cancelled and the DOM rebuilt, the
Lexical path; B: native edit, then rebuild in `requestAnimationFrame`, the Draft/Quill path).

| Target | Single-character `BackSpace` | `Delete` | Backspace over a selection |
|---|---|---|---|
| ED-CHIN (`<input>`) | no failure in 3 repeats | no failure (one miss under load: a dropped typed letter) | n/a |
| ED-CHTA (`<textarea>`) | no failure in 3 repeats | no failure (one miss under load: a dropped typed letter) | spoken (whole text) 3 / 3 |
| ED-CHCE (plain contenteditable) | no failure in 3 repeats | no failure (one miss under load: a dropped typed letter) | still silent or "blank" (known open, `[known failing]`) |
| ED-CHLA (re-rendering composer, Chromium) | **0 / 3 (silent)** -> 3 / 3 after the fix | **0 / 3** -> 3 / 3 | not run |
| ED-CHLB (same, native edit then rebuild) | **0 / 3 (silent)** -> 3 / 3 after the fix | **0 / 3** -> 3 / 3 | not run |
| ED-CHLA/B on Electron 44 (same wrapper as the e2e rig) | 0 / 3 -> 3 / 3 | 0 / 3 -> 3 / 3 | not run |

So plain `<input>`, `<textarea>` and `contenteditable` speak single-character Backspace; the silent Backspace of the owner
(Signal composer, Electron) reproduces with an editor that replaces its text node on every edit. The plain
contenteditable Backspace-over-selection defect (unified-editing F5) is unchanged and is NOT fixed here (see below).

### Root cause (raw events, `tests/web/m9/probe_composer.py`, identical on Chromium and Electron 44)
For Backspace in "hello" the browser sends no `text-changed:delete` with text. It sends, in this order (A) or
interleaved (B):
```
editor: children-changed:remove, text-changed:delete(0,1,U+FFFC), text-changed:insert(0,1,U+FFFC), children-changed:add
NEW leaf (never seen before, new object id each time): text-changed:insert(0,N-1,"ell")  then  text-caret-moved(N-1)
```
i.e. a whole-node replacement. The reader (a) drops the insert because the node is unknown to the mirror and only
caret events may retarget (`text_accepts`, `Ancestry::Unknown`, text edit); (b) retargets on the caret event and
`retarget()` invalidates the window, so the old text is gone and nothing can say what was erased; the caret
event then speaks nothing. Net: silent. (Also seen: while typing the same editors the echo is "h, e, he, l, hel": every
char is followed by the growing word; pre-existing, still failing the `type` step of ED-CHLA/B; not touched.)

### Fix (small, isolated to the text layer)
`crates/gorca-core/src/text/{mod.rs,caret.rs}`:
* `Reader::leaf_replaced`: an Insert at offset 0 on an unknown node of the same app while a hosting rich editable is
  tracked is compared with the text the machine last read (`CaretState::leaf_removal`: the tracked leaf's whole
  window, or `leaf_memo` kept while the root's own churn events moved the tracking to the root). Only a PURE removal
  (what remains of the old text on both sides equals the new text) is taken; anything else (a clock, a log, typing)
  is untouched. The tracking retargets to the new node and the removal is fed to the existing machine as
  `Edit::Delete { offset, len, text }`, so key correlation (`held_del`), the erased-text speech (TE-5) and
  word/char choice are all reused.
* `erase_caret`: the caret event of the new node is absorbed (the root's churn replaces `absorb`, so the echo has its own slot).
* Tests: 3 in `text/tests.rs` (backspace on a rebuilt node; the same with root churn in between; a non-matching whole-text
  insert is not ours). `cargo test -p gorca-core --lib text::` 181 passed; `cargo clippy -p gorca-core -p gorca-bench
  --all-targets -- -D warnings` clean; `cargo fmt` applied.
* e2e after: ED-CHLA/B Backspace and Delete 3 / 3 on Chromium and Electron; ED-CHIN, ED-CHTA, ED-CHCE unchanged
  (their failures in these runs are dropped typed letters at load 13-18, the known typist flake; ED-CHIN had one
  repeat with 18 such failures, `bench/results/e2e/ed-fix2`).
* Limits: a leaf that holds a whole paragraph is diffed fully; a window that is not the whole leaf (very long text) is not;
  a Backspace across a leaf boundary (deleting the space between two word-spans of a Lexical paragraph) produces a removal
  in the previous leaf and an insert elsewhere and stays silent as before. Not run against a real Signal (owner's
  session is off limits); the composer page models its event shape, not its exact DOM.

### Backspace over a selection in plain contenteditable (F5)
Reproduced again: `ED-CHCE BackSpace selection` heard `[]` or `[one two | blank | blank]` (known failing, never gating).
Not fixed here; the same family of cause is plausible (block removal reported as `delete(U+FFFC)` structure events the
machine ignores via `is_structure`) but unverified.

## Files
Tests and harness: `tests/web/forms/pages/{labels,login-real}.html`, `tests/web/m3b/pages/composer.html`,
`tests/web/m9/probe_composer.py` (+ `run_m9.sh probe-composer`), `tests/web/m7/ax.py` (`focusables`),
`crates/gorca-bench/src/e2e/{m8.rs,m8_steps.rs,edit.rs,edit_model.rs,webtext.rs}` (`Act::FocusNth`, M8-FLB/FLR/FLT,
ED-CHLA/CHLB). Results: `bench/results/e2e/{lbl-*,ed-*,dbg-*}`.
Commands: `tests/web/m8/run_m8.sh <label> --only M8-FLB,M8-FLR,M8-FLT --no-fail`; `tests/web/m9/run_m9.sh m8 ...` (Chromium);
`tests/edit/run_edit.sh target/release/e2e run --only ED-CHLA,ED-CHLB --repeats 3 --reader daemon:<gorca>,--impersonate-orca
--reader-env GORCA_ED=1 --reader-env GORCA_ED_STRICT=1 --reader-env GORCA_M7=1` with `GORCA_M7_BROWSER=chromium` and
`GORCA_M7_CHROMIUM=<chrome | tools/electron-probe/electron-wrapper.sh>`.
