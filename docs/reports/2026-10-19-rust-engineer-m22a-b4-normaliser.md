# M22a-B4: `gorca-sem-atspi` normaliser (2026-10-19, rust-engineer)

Code: `crates/gorca-sem-atspi/src/{roles,states,attrs,record}.rs`. Tests: `src/*` unit tests and
`tests/{corpus,evidence,sweep}.rs`. Run `cargo test -p gorca-sem-atspi -- --nocapture` for the tables below.

## API
`normalise(&RawNode) -> SrcRecord`. `RawNode` = fetched `PropMask` + role `u32` + `StateBits` + borrowed
attribute pairs + name + description. `SrcRecord` = `Semantics` + name + `known: GroupMask` + raw role +
`RoleSource` + inline `TagName` + `IndexText` + `Notes`. No allocation except the strings the record keeps
(name, rare strings in one `Arc<SemText>`). Unknown vs Absent: see the dependency table in `record.rs`
(role needs ROLE; check/press/select/avail need STATES and ROLE; level/live need ATTRS and ROLE; popup is known
when ATTRS was fetched, or STATES with HAS_POPUP clear).

## Decisions made (all in code, pinned by tests)
- Role: first valid `xml-roles` token wins (multi-token fallback, abstract/invalid/DPUB tokens skipped and
  noted); engine-vs-ARIA rule in `resolve_role`: presentation/none loses to a focusable or interactive native
  role, `generic` never downgrades, a password field and the web document root are never relabelled.
- Engine refinements when no token: `text-input-type=search` -> SearchBox; Table + `layout-guess=true` ->
  Presentation (+ LAYOUT_TABLE); weak engine role (generic/static/unknown) + tag in {code,time,mark,figure,
  fieldset,em,strong} -> the HTML-AAM role (Blink exports these as static/section); non-web selectable list
  item -> Option (GTK listbox rows).
- States: INDETERMINATE -> `Check::Mixed` on checkables, `Press::Mixed` on toggles (toggle = raw toggle-button
  or PRESSED on a button); EXPANDED wins over COLLAPSED; availability only on disableable roles (SM-5);
  READ_ONLY = state, or missing EDITABLE on an engine text field (Gecko convention).
- `INVALID_ENTRY` on a REQUIRED web field with no `invalid` attribute is NOT `aria-invalid` (both engines raise it
  on empty required fields and unchecked required checkboxes); noted as `INVALID_ENTRY_DISCOUNTED`.
- `aria-current`: unrecognised token = true (noted); haspopup `true` = menu; level only on
  heading/treeitem/row/comment/static text (Blink exports level=1 on every listitem); heading falls back to the
  tag, then ARIA default 2; setsize 0/-1 = unknown size; alert/status/log carry their implicit live settings,
  `container-live` members get `root: false`.
- `colindextext`/`rowindextext` go to `SrcRecord.index_text` (SemText has no field for them).

## Corpus result (A2: 3170 recorded nodes, 195 patterns)
| Engine | rows | pass | gap rows | FAIL | node not exported |
|---|---|---|---|---|---|
| firefox | 156 | 147 | 6 | 0 | 3 |
| chromium | 156 | 151 | 2 | 0 | 3 |
| gtk4 | 39 | 30 | 9 | 0 | 0 |

Evidence test (states/attributes of `ok` rows reach the record): 91 checks, 0 failures; 14 engine rows skipped
(evidence not exported, the `inexpressible` candidates). Sweep: 3170 nodes, 0 Unknown roles with everything
fetched, invariants hold, Unknown/Absent follows the fetched mask on 7 subsets.

### Expected gaps (not failures), 17 field mismatches
Engine disagreement (A2 `disagree=true`): menuitem position counted per role in Gecko (1/3 vs 1/9, 4/9 vs 1/6,
3/9 vs 3/3); Blink figure name empty (figcaption); Blink dl = description-list vs expected list; Gecko orphan
option selectable. Engine does not export (status ok, role not on the wire): Gecko cannot say the name is the
placeholder (2 rows, Blink says `name-from: placeholder`); GtkSwitch = check-box; GtkSearchEntry = entry;
GtkColumnView = tree-table; GTK navigation/main/feed = filler, search = form (A2 report). Fixture artifacts:
gtk4.accprop.checked-mixed, gtk4.accprop.expanded. Deferred: `doc-footnote` (DPUB not in `SemRole` yet).
Expectation vs model: textbox.password / gtk4.entry.password expect `textbox`; the model has `PasswordBox`, so
the normalised role is `passwordbox` (override table in `tests/corpus.rs`).

## M-10 unmapped lists (code, with tests)
- ARIA 1.2 roles: all 82 core roles (incl. directory, none) map (`roles::ARIA_12_ROLES` test). Valid but unmapped (DPUB/graphics, 42
  tokens): `roles::unmapped_tokens()`.
- ARIA states/properties: `attrs::ARIA_PROPERTIES` (53 rows). Unmapped with reason: activedescendant, autocomplete,
  braille pair, colcount, controls, dropeffect, errormessage, flowto, grabbed, hidden, owns, rowcount,
  valuemax/min/now.
- AT-SPI states: `states::STATE_COVERAGE` (44 rows, 20 deliberately unmapped with reason).
- Engine roles with no model role: `roles::unmapped_engine_roles()` = invalid(0), calendar(5), desktop-icon(13),
  unknown(67), extended(70), ruler(74).

## Open for later tasks
- Relations (labelledby, errormessage, controls, flowto, details target) are not inputs yet: HAS_ERROR_MESSAGE,
  `error_text`, `details_target` stay empty until S3/B5 or a relations input is added.
- Quirk/PI/inexpressible tables are B5; the tag refinements above may move into quirk rows then.
- `SemText` has no slot for index texts or the invalid kind; GTK/Gecko/Blink never export the invalid kind anyway.
