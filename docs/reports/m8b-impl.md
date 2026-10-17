# M8b: ARIA names, roles, states and descriptions at NVDA parity

Date 2026-10-17. Decision line in `docs/decisions.md`; live checklist section "ARIA names, roles, states
and descriptions (M8b)" in `docs/plan/checklists/M8.md`.

## What was built

| Piece | Where |
|---|---|
| Words (NVDA role labels, state order, aria-current, haspopup, level, error) | `crates/gorca-core/src/aria.rs` |
| Browse lines and quick nav: authored name replaces visible text, ARIA items and containers announced, description after a pause, error text | `crates/gorca-core/src/browse/present.rs` |
| Focus and change speech for web nodes (switch on/off, new words) | `present/utterance.rs` `present_web`; `present/flow.rs` and `browse/commands.rs` `web_overlay` add the mirror's attributes, description and normalised states to the focus snapshot |
| Description travels with the tree (`Description` property of `Collection.GetTree`); error text by one `error-message` relation hop; READ_ONLY and list-marker rows | `crates/gorca-doc-source-atspi/src/extras.rs` (`WebQuirks`, ADR-0008), `fetch.rs`, `source.rs`, `wire.rs` |
| Pause before a description: `PAUSE` prefix, joined by the verbalizer; `browse.description_pause` setting | `gorca-model` speech.rs, `gorca-speech` verbalize.rs, `gorca-config` |
| Caps on every page-fed string and count | `gorca-doc-source-api/src/limits.rs` |
| Test pages and steps | `tests/web/m8b/` (`widgets.html`, `forms.html`, `hostile.html`, `dump_tree.py` probe), scenarios `M8-BWG`, `M8-BFB`, `M8-BHO` in `m8_steps.rs` (61 + 6 steps) |

Engine facts that drove it (probe `tests/web/m8b/dump_tree.py`, both browsers): both expose `xml-roles`, `level`,
`current`, `haspopup`, `explicit-name`, Description and the `error-message` relation. Differences are quirk rows:
Gecko has no `read-only` state (a focusable, non-editable entry is read only), puts the list marker in a list or tree
item's name (`• Documents`), and answers its first relation query empty (one retry after 150 ms); Blink sets
`read-only` for `aria-readonly` too. Gecko does not expose `aria-readonly` on an `<input>`.

Deliberate NVDA deviations: a combo box with `aria-haspopup=listbox` does not say "opens list"; `selected` is silent
for list, tree and menu items (as NVDA) but a tab says it.

## Security caps (coordinator note)

Names 4 KiB, descriptions and error text 2 KiB, attribute values 4 KiB, 32 relations and 8 targets per relation
decoded (the rest of the reply is skipped unread), one relation hop only (cycles and chains cannot add work), error
text read with a bounded end offset, at most 64 error reads per fetch, all inside the fetch's own deadline, translator
clips names again. Unit tests in `wire.rs` and `limits.rs`; hostile page `M8-BHO` (describedby/labelledby/errormessage
cycle, 2,000 hop chain, 10,000 targets, megabyte aria-label/description/title, 100 invalid fields on a megabyte
error): 6/6 steps pass on both engines, the sentinel button stays reachable.

## Gates (head `f8f17ac`, clean, 3 repeats, host load 7 to 10, 15 GB available, one cargo at a time)

| Gate | Result |
|---|---|
| fmt, clippy `-D warnings` (workspace, all targets), `cargo test --workspace --no-fail-fast`, `cargo xtask deps` | all green (122 test binaries ok) |
| `m8b-final-full` Firefox, whole M8 corpus (`--quick`) plus M8-BWG/BFB/BHO | every scenario fail 0, M8-BWG 46/46, M8-BFB 15/15, M8-BHO 6/6 pass |
| `m8b-final-chromium` (`run_m9.sh m8`) M8-BWG/BFB/BHO | fail 0; BWG 43 pass + 3 gap, BFB 15, BHO 6 |
| `m8b-final-strict` K2 K3 K4 R1, `--impersonate-orca`, strict | K2 p99 0.23 ms, K3 1.05 ms, K4 all 0 |
| `m8b-final-r1` R1 with `GORCA_M4=1` | p99 0.32 ms, lost 0 |
| `m8b-final-shell` A1 N1 R1s (desktop/Shell words) | A1 p99 5.7 ms, a1_wrong 0, lost 0; N1 focus p99 0.36 ms, interrupted 0 |

Earlier runs kept: `m8b-try1`..`m8b-fix5`, `m8b-ho1`, `m8b-chromium1` (first runs had the failures listed below).
Chromium ran only the M8b scenarios, not the whole M8 corpus: main still has the Blink static-leaf duplicate-text
problem (M9 prep), so browse-line steps outside M8b are not meaningful there.

## Failures found and fixed on the way

- Error text empty: Gecko answers the first `GetRelationSet` empty (relations domain activates lazily): one retry.
- `GetText(0, 400)` on a shorter target is refused by Gecko: read `CharacterCount` first, clamp.
- Browse line with a value has no U+FFFC marker: error text added to `enter_items`.
- Step table: unselected tree item on focus says "not selected" (NVDA); Blink lacks that word (gap row, engine reason).

## Open

- `aria-readonly` on a Gecko `<input>` and `aria-disabled` in Gecko are as the engine exposes them (the latter said "unavailable" in the run).
- Chromium full-corpus parity waits for M9.
- Description is read at tree build and on the description event; a changed error-message target text does not refresh the field until its state changes.
- A GTK switch (`ATSPI_ROLE_SWITCH`) still maps to "toggle button" on the desktop (unchanged on purpose).
- Incident: I ran a stale script from the shared scratchpad that edited `crates/gorca-config/src/settings.rs` in another agent's worktree
  (`agent-a5e2c09b05d747ca3`), inserting a duplicate block; I removed exactly that block again and verified one `static DOCS` and one test of each name remain. That agent should re-run its build.

## Review fixes (reviewer report `docs/reports/m8b-review.md`, regression tests from its probes)

| Finding | Fix |
|---|---|
| M-1 error text lost past the 64th invalid field | The AT-SPI connector's focus fetch reads the `error-message` target of a focused invalid control (`Caller::error_text_of`, same-app targets only, clamped end offset); `web_focus_snapshot` declines the mirror shortcut when an invalid control has no error text, and `web_overlay` keeps the fetched text over the mirror's. The source remembers target -> fields (`PathTable::error_targets`, bounded 1024) and re-reads the fields when the target's text, name or children change (r_invalid `RI.focus3` second error passes). |
| L-1 150 ms sleep everywhere | `WebQuirks.warm_relations` (row `gecko-relations-warm-up`, Gecko only): one warm-up `GetRelationSet` per connection (`OnceCell`), wait at most 150 ms and never past the deadline; `error_text` never sleeps; single-node re-reads read at most 16 error texts per batch. |
| L-2 `". "` text prefix | `SegmentKind::Description`; the verbalizer joins it with ". ", `description_pause=false` turns it into `Text`. `PAUSE` is gone; page text ". Dot" is spoken whole (`RM.down3`, verbalizer test). |
| L-3 names | heading with `aria-label` speaks the label; check box and radio named by `aria-label` are named in browse lines (same-line label dedupes); tree item keeps its content (NVDA `nameIsContent`). |
| L-4 | no "not selected" for menu items (role or kind). |
| L-5 | NVDA bit order: `half pressed`, `on/off`, `opens dialog/grid/list/tree` after `required` and `invalid entry`; `submenu` stays before. |
| L-6 | `Step::chars(n)` caps spoken characters; HO.h/fan/big assert it (unit test: a megabyte name fails the step). |

Review scenario fixes: the reviewer's `RD.*`/`RN.*` step ids clashed with the reading scenario (renamed `RVD.*`/`RVN.*`); `RI.focus1` expected the first error text but the page changes it at 3 s (now expects "error", `RI.focus3` the second); `RM.cb.line`/`RM.down*` expected a wrong line order (now the observed order; Blink differences are gaps).

Gates (head after the fix commits, load 8 to 24, disk trimmed to one profile): fmt, clippy `-D warnings`, `cargo xtask deps` ok; unit tests ok (one run showed `gorca-rt sched::this_process_lists_itself_as_other` failing under load, passes on rerun). `m8b-rf-final-full` Firefox whole corpus plus BWG/BFB/BHO and the review scenarios, 3 repeats: all gates pass. `m8b-rf-final-chromium2` M8b and review scenarios, 3 repeats: pass (line-order gaps). `m8b-rf-shell` A1 N1: pass. `m8b-rf-strict` K2 K3 R1 strict `--impersonate-orca`: first run `K3.lost` 1 in one repeat at load 15 (kept, `m8b-rf-strict`), rerun `m8b-rf-strict2`: lost 0, pass. Earlier failing runs kept (`m8b-rf-full`, `m8b-rf-rv`, `m8b-rf-final-chromium`, and the m7 runs refused by a stale reader hash).
Note: `gorca-daemon/build.rs` did not re-embed the commit hash after commits in this worktree; touch it before an m7 run.

## Merge with the punctuation and symbol work

`SegmentKind::Said` and `SegmentKind::Description` coexist. Browse `segments(items, opts)` verbalises Text, Name, Lead and Info (error text) into `Said` and leaves a `Desc` raw with its `Description` kind; `Reader::symbolize` (the single speech sink) now processes `Description` like `Text`, so a description is symbol-processed exactly once and keeps its pause (test `a_description_is_symbol_processed_once_and_still_follows_the_pause`: "C++ 50%" is spoken "Search, edit. C plus plus 50 percent"). Step expectations that quoted raw symbols (`FB.invalid.focus`, `FB.qn.invalid`, `FG.t.2`) now expect the symbol names. Gates: fmt, clippy, `cargo xtask deps`, full `cargo test --workspace` (one failing probe test adapted: its content check compares segments before and after the strip), `m8b-mg-full` (Firefox corpus plus M8b/review, 1 repeat; first run failed only the three symbol-name expectations), `m8b-mg-fix` (M8-FORGE, M8-BFB pass), `m8b-mg-strict` K2 K3 R1 strict pass.
