# M22b3 cut-over: review and object navigation (rows P8, P9)

Date: 2026-10-25. Branches: `worktree-agent-ac6f998f39aeec31a` (gate-ready base, legacy path live; its tip is the
candidate for the tag `legacy/review-last`) and `cutover-review` (the single switch commit on top). Not applied to main.

## What moved

* `gorca-present::zone`: `compose(Reason::Review)` (row 6). Name, role word (the native table, plain-text roles say none),
  the **positive** state words, value. Same words as the legacy flat review, so the cut-over changes the model's inputs, not
  the user's wording; the NVDA web words stay with focus and browse.
* `gorca-core::review::zone`: `object_tokens` / `zone_tokens` / `flat_text` / `role_word`, all over `entry_of(snapshot)`
  (attached entry, else the provisional one of the source record). Used by the zones of the line model, the object text,
  the status bar and `InfoNoName`.
* `objnav::class_entry`: the navigation class from the entry (role via the exhaustive `role_class`, name and description
  from the entry) and `StructState`. `collect`, `cluster`, `validity` (the hash is now of the spoken groups), `window`
  dialog content read it. `cluster` also reads roles and names from the entry.
* `StructState` (open item of the r4 inventory): `gorca_model::StructState` / `StructFlags` and
  `NodeSnapshot::struct_state()`; review reads no other state bit (rule 12 ban `snapshot-states`, zero rows).
* Deleted: `flat_zone_text` and its tables, `class` / `class_in`, the legacy zone code, the replay, the 3 `role-spoken`
  rows of M22b3.

## Model gaps fixed (not rows)

* `SemFlags::TEXT_LABEL` (131072, `codes.snapshot` edited): raw role 29 (label) with role StaticText. The model merged label and
  static text; object review keeps labels out of its stops and a web text run is a stop. A `label-is-content` row would have
  covered 512 records and changed navigation in every GTK dialog.
* Inline phrase roles (`SemRole::is_inline_text`) are text runs for class and for the nameless word ("text").
* A form is a landmark word and a stop only with a name; the legacy `Role::Application` identity check of `parent()` stays
  (the golden-vs-gate comparison caught the first attempt, which treated an ARIA `role=application` widget as the tree
  root; regression test `an_aria_application_widget_is_not_the_application_root`).

## Gate (`cargo xtask cutover review`, `bench/results/cutover/review-2026-10-25.md`)

en and ru, 8 of 8 families, 35270 compared records per language (flat, zone, noname, class, class-web, walk of 6969 nodes),
0 unclassified, 120 dropped (all explained: 105 native chrome in the electron web family, 15 unreadable Firefox nodes),
deterministic under a seeded arrival shuffle. The `walk` case replays the six object moves from every node with the legacy and
the new class; a differing move is intended only if replaying it with the new class on exactly the nodes whose class row applies
reproduces the new result (`walk-follows-class`). Row hardening: a row whose fused fact the source record contradicts is
UNCLASSIFIED (`unavailable-static`: source role can be disabled; `toggle-state-word` / `pressed-without-toggle`: engine role;
`content-derived-name`: the engine gave a name; role rows: fusion changed the connector's role), witness tests in the replay.

Intended rows (all `m22b3`, until 2026-11-28; removed from `deviations.toml` by the deletion commit because nothing consumes them after it, surfaces `review` and `review-web`): content-derived-name, marker-stripped-name,
name-whitespace, name-cap-differs, blank-name-not-spoken, role-from-model, unavailable-static (2006 records: static Shell
actors no longer say "unavailable"), role-word-improved, toggle-state-word, pressed-without-toggle, value-text-spoken,
value-from-attrs, wrapper-word-dropped, unworded-role-is-object, static-text-word, class-role-from-model, layout-table-class,
walk-follows-class. Before the label flag and the role fixes the same replay showed 1600 unclassified differences.

Goldens equal the gate's new column record for record (35270 of 35270, checked against the last gate dump).

## E2E and flat review (`tests/shell/run_p3.sh`, private Shell world, real GTK4 window, Firefox, GNOME Shell quick settings)

62 steps per build, legacy base (`bench/results/e2e/m22b3-p3-legacy`) and cut-over (`m22b3-p3-new`): object moves and
flat review by line, word and character, backwards and forwards. All pass; per-step comparison in
`bench/results/e2e/m22b3-p3-compare.md`: spoken text identical except the intended Shell `panel unavailable` -> `panel`,
a Shell toast and a focus-path utterance split in Firefox (not review). Line-building rules (label-like text, named
containers, entries) are also checked on a page of mixed roles, line model of both trees diffed:
`bench/m6/review-lines.sh`, result `bench/results/m6/review-lines-2026-10-25.txt` (IDENTICAL). The corpora carry no
geometry, so the gate does not replay `build_line_model`; these two checks stand in for it.
Known failures, identical on both and recorded as such by the script (backlog row of 2026-10-25 in docs/decisions.md):
"not responding" on GTK4 `next` at the end and Firefox `next`/`previous` from the check box; Shell "layout unavailable".
One earlier cut-over run under load 17 failed 3 flat-review steps whose review cursor started elsewhere (the first flat key
raced the model build); kept in `m22b3-p3-new-run1-load17-flat-review-start-flake`, the rerun passed.

## Latency (`bench/m6/review-latency.sh`, `bench/results/m6/review-latency-2026-10-25.txt`)

Median of 5 interleaved rounds, performance governor, load 12 to 16: object moves +42 to +126 ns p99, validity hash +7 to
+18 ns per node (gate 100 us), line-model build +0.15 ms (1k nodes) and +1.4 ms (10k nodes) (gate +2 ms). The first flat-review key after a change waits for the build, so this is on the path of that key, not off it.

## Residual (b4)

`Role` identity reads that are not words remain in `review/` (surface roles, menu bar, status bar, `wants_lines`): they go
with the legacy `Role` type. Web-document entries are not consulted by review (it reads the application mirror's entry).
