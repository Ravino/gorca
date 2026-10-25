# M22b3 cut-over judgement: review and object navigation

## r1 (2026-10-25): APPROVE AFTER

Base `worktree-agent-ac6f998f39aeec31a` 33e7c253 (candidate `legacy/review-last`), deletion `cutover-review` 9a134bd9.
Standards: m22b1 / m22b2 cut-over judgements. Judged from reports and code. The only command run was the xtask check.

### Commands

- `cargo run -p xtask -- cutover review --check` on 9a134bd9: `xtask cutover: unknown option '--check'`. The parser
  accepts `<surface> [--seed N]` or a bare `--check` (`xtask/src/cutover.rs:1101-1121`), so this command cannot be run as
  written.
- `cargo run -p xtask -- cutover --check` on 9a134bd9: `cutover check: ok (no cut-over surface lacks a passing gate report)`.
- No cargo test, gate replay or e2e was rerun, so the gate numbers below are the agent's.

### Claims

| claim | verdict |
|---|---|
| Gate PASS en+ru, 35270 records, 8/8 families, 0 unclassified, deterministic | Report `bench/results/cutover/review-2026-10-25-2.md` at rev 4313fd19, `dirty: no`. Consistent; not replayed. |
| Goldens match | `review.{en,ru}.tsv` has 35270 lines each, the same as the compared count (6969 nodes x flat/zone/class/walk + 4499 class-web + 2895 noname). en and ru keys are identical. This is stronger than m22b1 r2. The record-by-record equality with the gate dump is not re-verified. |
| StructState is the only state review reads, ban enforced | Holds. No production `.states` read is left in `review/` (only test modules and the `states_all` match rule in build.rs). The ban needle `.states` would miss a destructuring pattern; none exists. |
| TEXT_LABEL instead of a 512-record row | Sound (Q1). |
| Latency at most +126 ns p99 | Object moves +42 to +126 ns, validity +7 to +18 ns. Line-model build is +43 % at 10k nodes (3.36 -> 4.81 ms), still within the +2 ms gate. See item 6. |
| e2e 29 steps, speech identical except "panel unavailable" -> "panel" | The p3.md diff confirms only that change. Latency columns are not comparable: legacy ran at load 13 with the performance governor, the new build at load 26 with powersave. |
| "not responding" is pre-existing | Present in both runs. It is pinned as the expected text (item 3). |

### Q1: is TEXT_LABEL a sound fact or a hack?

It is sound, and it gives legacy parity. Legacy `class_in` made `Role::Label` (raw LABEL or CAPTION) Layout and
`Role::StaticText` Content (`git show 33e7c253:crates/gorca-core/src/review/objnav.rs:72-116`). The model merged raw roles
29 and 116 into `StaticText`. The flag restores an engine fact: it is set only for raw 29 with no author token
(`gorca-sem-atspi/src/record.rs:268`). CAPTION maps to `SemRole::Caption`, which is already Layout.

Layout splices children, so nothing reachable is suppressed:
- links inside labels stay stops: Firefox has 20 labels with children and Chromium 63;
- an `<input>` wrapped in a `<label>` stays a stop;
- GTK3 hypertext links are not children in either path.

The one residual is a focusable or selectable label (1 GTK4 node in the corpus), which is not a stop. Legacy behaved the
same, so this is a low follow-up: a focusable or actionable TEXT_LABEL could be Content. Only `gorca-sem-atspi` sets the
flag, so a Newton or AccessKit source must map its own `Label` role to it.

### Q2: is "panel unavailable" -> "panel" consistent with the focus-surface decision?

Yes. The classifier (`gorca-present/src/diff.rs:643-653`) is limited to Generic, StaticText, Caption, Img and
Presentation with `avail_na`. A disableable source role becomes UNCLASSIFIED. This is the same rule that m22b1 r2 accepted
on clutter `set_reactive`.

The control case comes out right. Shell's `SystemItem` is an `St.Button` with `reactive: false`
(`refs/gnome-shell/js/ui/status/system.js:263-267`), and it still says "button, unavailable" on both builds (e2e
"object: parent").

### Q3: do the deviation rows hide lost information?

I found no hidden loss in the classifier bodies:
- wrapper-word-dropped needs the new side to have no role word and the role to have no word;
- unavailable-static is narrowed by role;
- class rows require `legacy_coarse`;
- walk-follows-class is checked by replay.

One small point: static-text-word makes a nameless label say "text" although TEXT_LABEL could keep "label"
(64 review-web records). This is a low item.

### Q4: is the corpus adequate?

At node level, yes: 8 families, native and web, tables (`tables`, `m8-tables`), and all six object moves from every node.

Flat review lines, word and char are not gated. The corpora carry no geometry, so `build_line_model` is never replayed.
Its rules did change in the cut-over (`review/cluster.rs`):
- `label_like` now also covers inline phrase roles and Caption;
- `name_is_content` adds Option, Term, Definition and GridCell;
- `is_entry` adds SearchBox.

Only 6 e2e flat steps cover this, with no word or char steps. Shell flat review says "layout unavailable" on both builds.

### Q5: does the deletion remove exactly the M22b3 rows and paths?

Yes:
- the 3 M22b3 `role-spoken` rows in legacy.allow;
- `review_gate_tests.rs`, `review/gate.rs`, `flat_zone_text`/`flat_zone_tokens`, `class`/`class_in` and `legacy_zone_tokens`;
- the xtask surface is emptied.

A grep finds no leftovers. The deviations.toml m22b3 rows stay, which follows the m22b1 and m22b2 convention. The commit
also carries goldens, the ban, the e2e and the latency series.

### Items (before tagging and merging)

1. **Medium, base tree red (inferred, not run).** 33e7c253 already sets TEXT_LABEL in `record.rs`, but the matching
   `crates/gorca-sem-atspi/tests/sweep.rs` hunk exists only in 9a134bd9.
   - Failing test on the base: `unknown_vs_absent_follows_the_fetched_mask_on_real_nodes` expects the Flags group unknown
     for a ROLE-only fetch unless ROLE_AUTHORED is set.
   - Evidence: I replayed its `step_by(7)` sampling in Python. It hits 47 raw-29 nodes with no `xml-roles`.
   - Effect: `legacy/review-last`, and main between the base merge and the deletion, would fail `cargo test`.
   - Fix: move the hunk to the base, then run `cargo test -p gorca-sem-atspi --test sweep` there.
2. **Medium, line model ungated.** Before the legacy path is deleted:
   - dump the line texts of `build_line_model` from both trees on the M-6 synthetic pages (`bench/m6/review-latency.sh`
     already builds them) and diff them, or
   - add e2e steps for word, char and multi-line flat review on GTK4 and Firefox.
3. **Low.** The e2e pins pre-existing bugs as expected text:
   - "not responding" on GTK4 next-at-end and on Firefox next/previous from a check box;
   - Shell flat review "layout unavailable".

   Record them as owner items, not as parity.
4. **Low.** The report says the line-model build is "off the key path", but the first flat key waits for it (the load-17
   flake). Correct the wording.
5. **Low.** The m22b3 rows expire on 2026-11-28 and have no consumer after the deletion. `no_row_of_the_checked_in_list_is_expired`
   will then fail. The same applies to the m22b1 rows on 2026-11-07. Decide a policy: delete the rows of cut-over surfaces
   or exempt them.
