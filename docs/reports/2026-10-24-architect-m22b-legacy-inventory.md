# M22b legacy inventory (2026-10-24)

Input to ADR-0022 section 8 (cut-over and no-legacy rule). Method: grep of non-test sources for
every read of a presentation fact outside the fused `SemEntry` and `PresIr`, then reading each
function. The machine-checked part is `xtask/legacy.allow` (82 rows, 224 uses on 2026-10-24;
`cargo run -p xtask -- legacy --print` regenerates the counts). This report adds what a symbol
grep cannot see. Slice names: `b1-5` is M22b1 slice 5 (focus and change), `b2` browse, quick-nav
and say-all, `b3` review and object navigation, `b4` window, notification, info, explain and every
extension path, plus the carrier deletion.

Allowlist rows by deleting slice: b1-5 24 rows / 56 uses, b2 15 / 68, b3 2 / 2, b4 41 / 98.

## Focus and change (P1, P4)

| Where | What it does | Replacement | Slice |
|---|---|---|---|
| `present/utterance.rs` `present_focus`, `present_selected_item`, `present_web`, `present_with`, `spoken_states`, `log_states` | legacy focus presenter; reads `Role::spoken`, `aria::*` words, `NodeSnapshot.attrs` | `gorca_present::compose` + `render` on the entry | b1-5 |
| `present/utterance.rs` `present_change`, `changed` | state-change presenter (ST-1..6 words from `StateSet` bits) | `compose` with `Reason::Change` | b1-5 |
| `present/flow.rs` `spoken_snapshot`, `web_overlay` call | Reader-side overlay of mirror facts onto the snapshot | none: the entry is attached by the writer before publish (section 8.3) | b1-5 |
| `browse/commands.rs` `web_overlay`, `overlay_from_meta`, `legacy_name`, `Legacy`, `subtree_label`, `fallback_name` | C3 pre-fusion fallback: legacy name, description, READ_ONLY/HAS_POPUP from `NodeMeta` while no entry exists | provisional S5 facts written by the document actor (8.3) | slice 4 moves, b1-5 deletes |
| `browse/commands.rs` `editable_section` | extension-free role fix: panel + EDITABLE + FOCUSABLE without role token is a text entry | S-1 quirk row `contenteditable-host` -> `SemRole::TextEntry` in `gorca-fuse` | slice 4, deleted b1-5 |
| `browse/commands.rs` `web_focus_snapshot`, `web_focus_fallback` | cold focus snapshot built from `NodeMeta` (states, value, attrs clone) | cold focus presents from the provisional entry | b1-5 |
| `present/flow.rs` late relabel (C3b inferred label, `relabel`, `speakable_description` merge) | inferred label kept outside the entry, merged into a cloned snapshot | S4 `inferred` fact with taint and layout revision (slice 4) | slice 4 moves, b1-5 deletes |
| `present/flow.rs` shadow hook (`aria::Node` for `is_web`), `GORCA_PRESENT_SHADOW` | shadow sampling of the legacy utterance | deleted: after the switch there is nothing to compare; the replay table stays as golden tests | b1-5 |
| `lib.rs` re-exports of the above | public surface | removed | b1-5 |

## Browse, quick-nav, say-all (P2, P3)

| Where | What it does | Replacement | Slice |
|---|---|---|---|
| `aria.rs` (whole module: `role_word`, `state_words`, `current_word`, `level_phrase`, `error_text`, `speakable_description`, `strip_pause`, `Node`, `Kind`) | NVDA word tables over `Role` + `StateSet` + attribute strings | `gorca-present` tables over `SemEntry` groups (`role`, `check`, `press`, `expand`, `current`, `popup`, `level`, `error`) | b2 (focus users gone at b1-5) |
| `browse/present.rs` `node_of`, `role_phrase`, `push_states`, `landmark_kind`, `container_items`, `marker_items`, `authored_name_items`, `describe_node_from` | browse line, quick-nav and landmark text from `NodeView.meta` states/attrs/description | `compose(Reason::Browse / QuickNav)` + `render` | b2 |
| `browse/commands.rs` `is_group` (TAG, XML_ROLES), `states_of`, `modal_scope` (MODAL via `meta.states`), `strip_pause` call sites | structural predicates reading attrs and states | `Semantics` structure/modal reads (X2/X3 of section 2.11) | b2 |
| `gorca-doc/src/web/cats.rs` `cats_of`, `NodeMeta` | quick-nav categories computed from role + meta | `cats_of(&SemEntry)` computed by the document actor at translate | b2 (type deleted b4) |
| `browse/forms.rs`, `tablenav.rs`, `selmirror.rs` | read `NodeView.role` / `states` for structure, not words | structure reads of the entry; no word tables here | b2 |

## Review and object navigation (P8, P9)

| Where | What it does | Replacement | Slice |
|---|---|---|---|
| `review/cluster.rs` zone text `s.role.spoken()` | object zone words | `role_word(SemRole)` from the entry | b3 |
| `review/commands.rs` `InfoNoName` | role word of a nameless object | same | b3 |
| `present/utterance.rs` `flat_zone_text` (used by `review/collect.rs`) | zone text of a snapshot | `compose` field text | b3 (moves out of the legacy file first) |
| `review/objnav.rs` `class_in` | presentation class from `snap.name`, `description`, `states` | entry name/description known mask + structural `Existence` (see open gaps) | b3 |
| Direct `AttrMap` read in review | none today; review reaches attrs only through `flat_zone_text`. The owner's scenario is prevented by the `attr-map` / `attrs-field` bans, which cover the future code. | | |

## Info, explain, window, notification (P5-P7)

| Where | What it does | Replacement | Slice |
|---|---|---|---|
| `commands/info.rs` `where_am_i`, `node_info`, `state_words` (own state table), `window_title` | `role.spoken()` per ancestor, its own state words | `compose(Reason::Query)` over the ancestor entries | b4 |
| `commands/diag.rs`, `present/log.rs` explain-last | explains a legacy `Utterance` record | explains `PresIr` `origin` + `ProvTag` | b4 |
| `present/window.rs` `lead_segments`, `apply_policy` window path | role word for dialog and alert | `compose` window lead-in as IR edit | b4 |
| `present/notification.rs` `notice_segments` | role word + name + text segments | `compose(Reason::Notice)` | b4 |

## Extensions and plugins (ADR-0026)

| Where | What it does | Replacement | Slice |
|---|---|---|---|
| `builtin_ext.rs` `PolicyFacts` (built in `present/flow.rs`, `present/window.rs` from `NodeSnapshot`) | role, states, name, description, range handed to the policy | facts from `SemEntry` accessors (WIT 0.9) | b4 |
| `gorca-ext-api` `api.rs`, `guest.rs`; `gorca-ext-host` `host.rs`, `convert.rs`, `instance.rs`, `native.rs`, `wasm.rs`; `extensions/ext-gnome-shell`, `ext-test`; `gorca-daemon/src/shell_policy.rs` | policy API; `convert.rs` maps the legacy `Role` enum to WIT role numbers and reads `NodeSnapshot` | `SemRole` codes and entry accessors | b4 |
| Extension-side role fixes | none exist today (the shell extension decides silence, value, say, notice only). A role fix is an ADR-0019 S1 patch, seen by fusion before any presenter | rule stated in section 8.6 | n/a |
| `Verdict::Say(segments)` | replaces a whole utterance | IR edit hook with origin tag | b4 |

## Carriers and shadows (deleted last)

| Where | Replacement | Slice |
|---|---|---|
| `NodeMeta` presentation fields (`states`, `attrs`, `description`, `value`, `sem: Option`) in `gorca-doc`; `meta_of` in `doc_actor/translate.rs`; `NodeView.meta` | entry only; mirror node keeps structure | b4 |
| `doc_actor/live.rs` (LIVE, XML_ROLES attrs), `doc_actor/mod.rs` label-inference skip on XML_ROLES | `live` group, `role_source == Token` of the entry | b4 |
| `NodeSnapshot.attrs`, `.description` read by consumers; `statemodel/world.rs` | connector-private `SrcRecord`; consumers get the fused node | b4 |
| `app_actor/agree.rs`, `doc_actor/agree.rs` (legacy-vs-fused comparers, MARKERS oracle for `gecko-list-marker`) | deleted with the legacy values they compare | b4 |
| `SemRole::legacy_family`, `Role::spoken` (model) | deleted; `gorca_present::roles` is the only word table | b4 |

Connector-specific shortcuts found: the Gecko list-marker branch in `legacy_name` (b1-5; the S-1
quirk row already does it in the model) and its oracle copy in `app_actor/agree.rs` (b4);
"Orca does not infer for an author role" in `doc_actor/mod.rs` (b4). Not presentation, excluded:
`directory.rs` `gecko-insert-offset` and UTF-16 text units (text layer, ADR-0007).

Braille plan: no braille presenter exists in `gorca-core` (only sinks in `gorca-output-api` and
`gorca-braille-brlapi`), so there is nothing to migrate: the first one is built on `PresIr`
`FlatSpan` only. The bans cover the braille crates, so it cannot start from legacy.

## No clear replacement yet

1. **Structure vs presentation split of `StateSet`.** `SHOWING`, `VISIBLE`, `DEFUNCT`, `FOCUSED`,
   `FOCUSABLE` are structural (review `class_in`, focus tracking); `existence` is "not fused" in
   ADR-0022 r3. They must stay readable from the node record after the cut-over. Proposed in 8.7:
   the node record keeps an explicit `StructState`; everything spoken comes from the entry.
2. **Entries for lazily cached ancestors** (where-am-I walks the chain; native mirror hidden-cache
   skips, `native-mirror-facts`): needs the app actor to fuse ancestors it caches, or a bounded
   on-demand fuse. Not designed in M22a.
3. **Cost of the provisional content label** (8.3) on a 100k-node page: B-FUSE has no row for it.
4. **WIT 0.9 accessor set for policy facts**: the design exists (ADR-0019, ADR-0026) but the
   exact facts the shell policy needs (range triple) are not in it yet.
