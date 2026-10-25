# M22c plan: legacy Role and StateSet off the node record (2026-10-25)

Goal (ADR-0022 sections 8.1, 8.5, 8.7): no consumer reads `NodeSnapshot.role`, `.states`, `.attrs`,
`.interfaces` or the legacy `Role` / `StateSet` / `Ifaces` types; the snapshot fields are private;
the legacy fields leave the record. Connectors (`gorca-connector-atspi`, Newton) keep their own role
and state types to *produce* the fused entry.

## Mechanism: a ratchet, not an allowlist

`PORTED` in `xtask/src/legacyrule.rs` lists the module paths already moved. For those paths rule 12
bans `Role` (the legacy type, not `SegmentKind::Role`), `StateSet`, `Ifaces`/`Iface`, `.interfaces`,
`.states` and `.role` (not `.sem.role`). A module is added to `PORTED` in the commit that ports it and
never leaves. There is no per-file count and no expiry. Part 1 put `crates/gorca-core/src/review/`
in it. Test code is not scanned (a ported module's tests may still build snapshots from a legacy
role through `with_test_record`; that goes away when the record fields do).

## Replacements (what each legacy read becomes)

| Legacy read | Replacement | Status |
|---|---|---|
| `snap.role == Role::X`, `match snap.role` | `entry_of(snap).sem.role` (`SemRole`), exhaustive `match` where the old code was exhaustive | done in review |
| `snap.states` structural bits (showing, visible, defunct, focusable, focused, multiline) | `NodeSnapshot::struct_state()` (`StructState`) | done in review |
| `snap.states` spoken bits (checked, expanded, ...) | groups of the entry (`check`, `expand`, `select`, `avail`, ...), words through `gorca-present` | done on every speaking surface |
| `snap.interfaces.contains(TEXT / ACTION)` | `NodeSnapshot::caps()` (`NodeCaps::is_known/has_text/has_action`), new in part 1 | done in review |
| `MatchRule { roles: vec![Role::StatusBar], states_all: StateSet::SHOWING .. }` | `MatchRule::showing_status_bar(root)`; the connector API owns its vocabulary | done |
| application root vs ARIA `application` vs `<object>` | `objnav::is_app_root(snap)`: model role `Application`, no `ROLE_AUTHORED`, parent fetched and absent | done, see risks |
| `.attrs` (tag, level, xml-roles, live) | typed groups (`level`, `live`, `SemText`), `SrcRecord` inside the connector | speaking surfaces done; remaining reads in doc-source-atspi and mirror |
| `snap.states` / `Ifaces` in `statemodel`, `testkit`, `fact-api` | a typed `StructState`/`NodeCaps` projection, or the entry | part 3 |

## Inventory (production code, tests excluded, approximate; `python` scan of `Role::`, `StateSet`,
`.states`, `.attrs`, `.interfaces`, `Ifaces`, `.role` not preceded by `sem`)

Counts include some `.role` reads that are not the legacy field (other structs); the true list is
what the rule reports once a module is added to `PORTED`.

| Module | Hits | Part |
|---|---|---|
| gorca-core/review | 0 (ported) | 1 |
| gorca-core/browse (commands 117, forms 106, present, ...) | ~237 | 2 |
| gorca-core/present (flow 70, utterance 21, window 58, node, ...) | ~190 | 2 |
| gorca-core/text (mod.rs 61) | ~61 | 2 |
| gorca-core/commands (info 40, mouse, ...) | ~43 | 2 |
| gorca-core/doc_actor (flags_sem 44, translate 28) | ~76 | 2 |
| gorca-core/statemodel, app_actor, terminal, reader glue | ~25 | 2 |
| gorca-doc (own `Role` use) | ~111 | 2 (check: may be the doc crate's own role type) |
| gorca-doc-source-atspi | ~87 | 3 (connector side: produces the entry; may keep its types) |
| gorca-mirror (prefetch, fill) | ~15 | 3 |
| gorca-fuse, gorca-present, ext-api, ext-host, daemon, fact-api, plugin-host, testkit | ~110 | 2/3 (mostly `.role` of other structs and test support) |
| gorca-model (the definitions), gorca-connector-atspi, gorca-sem-atspi, connector-api | ~460 | 3 (they produce; the fields go private last) |

## Fields the entry lacks

Nothing had to be added to `SemEntry` or `SemFlags` for review. What was missing was outside it:

* **Capabilities** (`Ifaces`: text, action). Not a presented fact (ADR-0022 8.1 "structure"), so it
  stays beside `bounds`, `children` and `parent` in the record, behind `NodeCaps`. Decision for
  the owner: keep `interfaces` private on the record as structure (recommended), or move TEXT/ACTION
  into `SemFlags` (costs two flag codes, the codes snapshot and the normaliser needing the interface
  list, which it does not see today).
* **Raw role detail.** The fused role folds raw `application` (75) and `embedded` (78); the record's
  `raw_role` is dropped at fusion. Part 1 uses "no author token and no parent" to find the root.
  If browse or present need more (a native `status bar` vs an ARIA `status`, a GTK `label` vs
  `caption`), prefer a flag set by the normaliser (like `TEXT_LABEL`) over keeping `raw_role`.
* **A bit for "any role token present"** (known gap from 8.7): label inference in the document
  actor. Part 2.

## Behaviour risks (Orca equivalence)

* Roles with no `SemRole` fall to `Unknown` (calendar, desktop icon, ruler, extended, invalid).
  Nothing in review reads them. Browse and present must be audited against the same list.
* `Label`/`Caption`: the legacy `Label` is raw label and caption; the fused model has
  `StaticText` + `TEXT_LABEL` for the first and `Caption` for the second. The testing record maps
  `Role::Label` to `Caption`; tests that need the real shape set `TEXT_LABEL` explicitly.
* `StatusBar` vs ARIA `status`: both are `SemRole::Status`. Gecko and Blink export `role=status`
  as a status bar too, so the legacy walk also matched them; kept as is.
* Differences measured in part 1 are in ADR-0022 8.7 (2026-10-25 rows).

## Parts

1. (done) plan; `NodeCaps`, `MatchRule::showing_status_bar`; review/ and object nav; ratchet; golden
   rows `walk-build` (`wants_lines`, surface role) over every recorded node, no existing row changed.
2. browse/ (with its goldens), present/ (flow, utterance, window), text/, commands/, doc_actor/;
   each module joins `PORTED` in its own commit. Order by independence: text, commands, present,
   doc_actor, browse (largest, most behaviour: forms.rs alone has ~100 `Role::`).
3. Connectors and the rest (`gorca-doc-source-atspi`, mirror, testkit, statemodel), then make the
   `NodeSnapshot` fields private (constructor + accessors; `gorca-connector-atspi` and
   `gorca-mirror` build through `NodeSnapshot::new`), delete `role`, `states`, `attrs` from the
   record and the legacy `Role` and `StateSet` from `gorca-model` outside the connector crates,
   update the ADR to "closed".
