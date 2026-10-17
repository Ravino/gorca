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
