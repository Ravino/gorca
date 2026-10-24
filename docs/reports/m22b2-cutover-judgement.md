# M22b2 cut-over judgement (browse, quick navigation, say-all)

Date: 2026-10-24. Judge: reviewer. Candidate: `cutover-browse` b1d99b71 (single commit, fast-forward of main e713e375).
Standard: as for M22b1 (docs/reports/m22b1-cutover-judgement.md).

## Verdict

**APPROVE PENDING FIREFOX M8**, with one recorded follow-up (F-1) and two pre-land checks (C-1, C-2).

## How it was checked

- Re-ran the deleted gate replay from the main build (`target/debug/deps/gorca_core-8a3f60140c870d7a`, built 08:46 from 128533a0)
  with `GORCA_CUTOVER_SURFACE={browse,quicknav} GORCA_CUTOVER_LANG={en,ru} GORCA_CUTOVER_SHUFFLE=0`, filter
  `browse::gate_tests::browse_emit`. Records: browse 15910, quicknav 3026 per language, as in the committed reports.
- Compared the gate's legacy and new columns token by token for every non-equal record.
- Compared the gate's new column with the goldens at edfedca1 (cut-over before the fixes) and b1d99b71 (after).
- No cargo build was run: the Firefox M8 run was in progress (loadavg 22), and a build would distort its timing gates.

## 1. Deviation rows (browse 175 records, quicknav 6, per language)

| row | records (en) | lost tokens | gained tokens | judgement |
|---|---|---|---|---|
| unavailable-item | browse 136 | none | `State:unavailable` (1 to 5 per range) | OK, adds a state the legacy dropped |
| canvas-graphic | browse 22 | none | `Role:graphic` | OK |
| alert-dialog-parity | browse 3 (Chromium) | none | `Confirm delete`, `dialog` | OK, Chromium now matches Firefox |
| alert-dialog-parity | quicknav 2 (Chromium node 96) | `Yes`, `button` | `Confirm delete`, `dialog` (node), nothing (node-from) | Accepted: Chromium now matches the Firefox legacy output byte for byte (container speaks its first line). See L-1. |
| name-cap-differs | browse 14, quicknav 4 | name tail after 1024 B | capped name | OK, cap is documented |

No row hides a lost role, state, level, link word or heading word. The only lost tokens are a button inside the
alertdialog on Chromium quick navigation, and they are lost exactly as on Firefox before the cut-over.

**L-1 (low, code hazard):** `Classifier::AlertDialogParity` (crates/gorca-present/src/diff.rs:566-578) clears both
residues whenever the range holds an alertdialog and either the new output says "dialog" or adds nothing. Any loss in such
a range is accepted. Today that covers only the two records above. The classifier goes away with the gate, so this needs no
fix, but do not copy the pattern into M22b3/b4 classifiers.

## 2. Goldens vs gate

- Before the fixes (edfedca1), the goldens equal the gate's new column for **every** record: browse 15910/15910 and quicknav
  3026/3026, in both en and ru, with 0 key and 0 text differences.
- b1d99b71 adds 1937 browse and 438 quicknav records (firefox/chromium `m8b-widgets`, `m8b-forms`, `errors`). The pinned
  counts are 17847 and 3464, and 17847 - 1937 = 15910.
- The stateful-roles-only replay and the error-text fix change 72 browse and 18 quicknav records against the gate. **Every
  change is an addition of error text; no token is lost.** So the stateful-only read loses nothing that the corpus contains.
- **F-1 (medium, regression vs legacy, never gated):** 50 browse records and 6 quicknav records now speak the error message
  **twice** when the error element sits in the same range as the field. Example, `chromium/m8-forms` 153 line:
  `Invalid | edit | invalid entry | Error text here | bad | Error text here`. This also happens in say-all (`block`: 22
  records), and in `firefox/forms` and `chromium/forms` ("Must be a number" said twice). Legacy said it once. The goldens
  blessed it with no deviation row. M8 does not catch it, because its checks are substring matches.
  The fix is to drop the fused `ErrorText` field when its target text lies inside the described range; `dedupe_names`
  in browse/present.rs is the natural place. The fix needs no legacy code. Record it in docs/decisions.md as a known
  regression and fix it within M22b3.

## 3. Deletion and say-all

- legacy.allow: all M22b2 rows are removed and none remain. There were **14** rows, not 15. `browse/ir.rs`, `aria.rs` and
  `gate_tests.rs` are deleted, and the `ir.rs` strings.allow entry is removed. `legacy_sem_tests.rs` is `#[cfg(test)]`
  test support. The name is misleading, but it is not a legacy path.
- Say-all (`browse/sayall.rs:103`) speaks `present::describe` -> `present::segments`, the same function the golden
  `block` case pins. Say-all goes through gorca-present.
- **C-1 (pre-land):** the resume note says the full workspace tests, clippy and `cargo xtask deps/strings/legacy` have not
  run since the fixes. Run them on b1d99b71 before the fast-forward.

## 4. Corpus adequacy (browse goldens, en)

| feature | records | note |
|---|---|---|
| tables | 105 `Role:table`, 126 rows/columns | m8-tables, tables; no column/row header role words in any record |
| lists | 193 | |
| landmarks | 767 | m8-landmarks |
| forms | m8-forms, m8b-forms, forms | |
| errors | 308 | `errors` page, both engines; error target in another frame is not covered (known gap) |
| iframes | 28 | m8-iframes |
| live regions | 518 role words | browse reading only; live announcements are a different surface |
| visited links | **0** | no corpus record and no positive M8 step (only "no visited links"). Unit tests in browse/present.rs:696,716 cover it through the real normaliser, and LINK is stateful (roles.rs:149) |

The corpus is adequate for this cut-over. **L-2:** add a visited link and a table with explicit column and row headers to
the corpus in M22b3.

**L-3 (low, pre-existing, equal in both presenters):** the line before the alertdialog's first text line is silent (line
308/338 in composite), and `line-from`/`node-from` into the dialog say only "Really delete?". Line or quick-nav entry
never announces "Confirm delete dialog". Track it as a browse-mode polish item.

## Conditions

- **Firefox M8 (blocking):** no step that passes on main may fail on b1d99b71. The steps that failed only on the cut-over
  before the fixes are `FB.qn.invalid` and `WG.tabs.line`; both must pass. `LT.search.go` needs three repeats if it fails.
- **C-1 (blocking):** the workspace tests, clippy and the xtask gates must be green on b1d99b71.
- **F-1 (non-blocking):** record the duplicate error text in docs/decisions.md and fix it in M22b3.
