# M22a-C3: W-6 prerequisites, web overlay on fused entries, name from content through containers

## What changed

- **W-6 registers**: groups 18..22 `roledescription`, `placeholder`, `keyshortcuts`, `valuetext`, `namefrom` (primary-only: no fact
  shape, `Group::is_primary_only`). One writer helper `gorca_fuse::rare_write` used by both writers. The oracle fixture stays at 18
  groups (appended groups are listed in `fuse_fixture_registry.rs`); `tools/gen-sem-codes.py` appends them to `codes.snapshot`.
- **`Ivm::reparent`** (atomic, cycle-checked) and parent adoption in the app writer (children that arrive before their parent).
- **Property-event invalidation**: `FuseWriter::invalidate/invalidate_all` (mirror sequence; whole-app sweep in 512-node chunks) and
  `MirrorWriter::refresh_sem`, so an entry on a stale snapshot says `stale` and a content-named parent follows its children.
- **Web overlay**: `browse/commands.rs overlay_from_meta` reads the document node's `NodeMeta.sem`: nameless control -> one-line
  label (cap 160), quirk-stripped name (entry name is a proper suffix), derived name that contains the engine's shallower one,
  description, `READ_ONLY`/`HAS_POPUP`. `subtree_label` and the role-gated list-marker branch are deleted. Attributes are still
  carried from the mirror, only for a snapshot fetched without `ATTRS` (the presenter reads them until M22b).
- **One flat state table**: `gorca_sem_atspi::states::state_set(bits, StateMode::{Native,Web})`; the connector's and the document
  source's `map_states` are gone.
- **S3 through nameless containers** (coordinator request, Signal study G1): `Rules.content_depth` (8; the oracle profile keeps 0),
  `views()` replaces a nameless alive child by its own children, dirtiness climbs through nameless ancestors,
  `Rules.content_over_engine` (row, cell, gridcell, listitem): an engine name with `namefrom = content` gives way to the derived
  name when it is longer. `listitem` and `article` are content-named now. Caps: 256 children, 1 KiB, 8 levels.

## Documented deviation `content-over-engine` (agreement checker)

Evidence: signal-like message row ("React to message" -> "Contact 0 Message number 398 body text 10:38 React to message",
test `the_signal_message_row_...`), and 6 Chromium `m8-tables` rows in the recorded corpus ("SeatLeftRight" -> "Seat Left Right").
Predicate: derived name, `gorca_fuse::name_extends(engine, fused)` (same words plus more, spaces ignored). Corpus: 0 other
disagreements (2795 doc nodes), app-side corpus 22965 checks 0 mismatches.

## Known limits

- The recorded corpus has no case where the legacy `subtree_label` fill applied (engines name controls themselves); the synthetic
  cases in `agree_tests.rs` are the parity evidence (runs, blanks, glyphs, leaves, wrappers, 160 cap).
- A node without an entry yet (fusion pending after a big page load) is left to the snapshot: no text fallback exists any more.
- `DocCtl::Fact` sender (SM-8 focus-fetch record into the document) is still not wired.
