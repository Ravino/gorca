# M22a-C1b: app actor writer, dual-carried semantics

## What changed

- `SrcRecord`, `Notes`, `TagName`, `RoleSource` moved from `gorca-sem-atspi` to `gorca_model::sem`
  (the core may not depend on the normaliser, xtask rule 10). `NodeSnapshot.sem: NodeSem { rec, entry }`
  (+16 B). C2 should use `gorca_model::sem::SrcRecord` for `SrcNode.rec`.
- Connector: `decode::snapshot` runs `gorca-sem-atspi` (raw attribute pairs kept by
  `keep_sem_attrs`, `WANTED_ATTRS`), `sem_profile.rs` picks the table bundle and hands it to the core in
  `AppInfo::sem_tables` (text; the core parses it with `gorca-fuse`).
- Core: `app_actor/fuse.rs` `FuseWriter`. Reply, push, review and reader-fetch paths only enqueue the snapshots
  (Arc clone). `step()` runs at the end of `pump`, after routing, under 200 us: registers, `Ivm::run` (focus chain
  first, at least 4 fusions), then `MirrorWriter::attach_sem` (new; pointer identity on the record, no revision
  bump). Entries are read through `entry_view`; xtask rule 11 bans `entry_unmasked` outside `gorca-fuse`.
  Engine is rebuilt past mirror cap + 4096 nodes. A wake of 1 ms is armed only while work is left.
- Legacy agreement: `app_actor/agree.rs` (debug builds or feature `legacy-agree`), counters
  `agreement_counters()`, warnings capped at 64 per actor, `set_strict_agreement` for tests. Corpus test
  `gorca-connector-atspi/src/legacy_agreement_tests.rs`: 22,965 checks over the A2 corpus
  (Firefox, Chromium, GTK4), 0 mismatches, 8 documented deviations (each with a witness):
  gecko-list-marker 45, name-from-content 68, aria-role-over-engine 77, weak-role-refined 29,
  layout-table-presentation 1, select-not-applicable 10, pressed-without-toggle 5, empty-state-set 1.
- `Segment::new`, feature `seg-origin` (`MsgId`, `OriginId`, `Segment::tagged`); all literals converted;
  verified with `--features gorca-core/seg-origin`.
- `ReaderCounters::revalidations_list_row` for C5.

## Measurements (K1/K2, e2e, interleaved A/B, release, same e2e binary, `--allow-stale-reader`)

Host NOT quiet: load 12-28 (other agents). K1 p99 ms, base vs C1b (8 runs each, interleaved):
base 0.91 3.51 3.04 0.78 0.96 3.02 2.08 0.89; C1b 0.88 0.90 349.6* 3.24 0.89 0.69 0.94 0.94.
K2 (3 runs, `--reader-env GORCA_M1=1`): p99 base 1.95 0.89 0.83, C1b 1.31 0.89 0.82; lost 0, not_consumed 0.
*One C1b run (c1b-c1b-3: p99 349.6 ms, lost 1) coincided with load 16.8 -> 28; not reproduced in 7 other
C1b runs and kept (`bench/results/e2e/c1b-c1b-3`). It needs a rerun on a quiet host before the C5 gate.
Result dirs: `bench/results/e2e/c1b-{base,c1b}-N`.

## Open items

- Fused entries carry only `description` of the rare text (placeholder, key shortcuts, value text,
  role description) and no `name_from`: no register exists for them.
- Engine tree: a node is linked under its parent only if the parent is already known (no reparent in `Ivm`);
  C3's S3 content names need parents first or a reparent API.
- Property events invalidate the mirror snapshot but not the engine; the entry on a Stale snapshot must be read
  with `NodeView.stale`.
- Empty state set reads as Disabled in the normaliser (deviation); GTK tab PRESSED is `selected` in the model.
