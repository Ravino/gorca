# M22a-C3 review (readers on the fused semantic model)

## Round 1 (2026-10-20), commits b264e4b1, 80318b76, 3bfc64cd on d018080c

Verdict: **MERGE AFTER FIXES** (one Medium fix before merge, the rest are follow-ups). Nothing here
blocks on safety; the one user-visible regression is narrow and has a ~15-line fix.

### What was run

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets -D warnings` (branch) | clean |
| `cargo test` gorca-fuse, gorca-core, gorca-model, gorca-sem-atspi, connector, doc-source, mirror (branch) | all pass |
| Merge into main 8fdab360 (`git merge --no-commit` in a detached worktree under /var/tmp) | one textual conflict, `docs/decisions.md` (both sides appended rows; keep both). `app_actor.rs` auto-merges. |
| Merged tree (main 8fdab360 + branch, decisions.md resolved by keeping both rows): clippy `-D warnings`, `cargo test --workspace --exclude gorca-bench` | clippy clean; tests rc=0 (140 test binaries ok, 0 failed). The temporary worktree was removed afterwards; nothing was committed. |
| Storm probe (release, `crates/gorca-fuse/tests/audit_c3_storm.rs`, removed after the run; copy in the reviewer scratchpad) | numbers in finding M-3 |
| xtask rule 11 (`entry_unmasked`) | no new use outside gorca-fuse; the overlay reads `NodeMeta.sem`, which the doc actor fills through `entry_view` |
| unwrap/expect in non-test code of the diff | none (decode.rs:292 is in `mod tests`) |
| Exhaustive matches | `rare_write`, `rebuild_slot`, `default_slot`, `write_default`, `Val::group` list every group, no `_` arm |
| M8 A/B (implementer's run, bench/results/e2e/c3-m8-*) | 52 PASS on both; the one FAIL (M8-LV live 106.8 vs 108.8 ms, gate 100) is on base too |

### Findings, by severity

**M-1 (Medium, fix before merge). A node whose entry is not fused yet loses everything the overlay used to add.**
`browse/commands.rs overlay_from_meta`: with `meta.sem == None` it returns `None` (no change). The old
overlay needed no fusion: the marker strip used the mirror node's name, description and
`READ_ONLY|HAS_POPUP` came from `NodeMeta.description/states`, and `subtree_label` read the
document text. Entries are missing for real stretches: the doc engine fuses `BATCH_BUDGET` = 2 ms
per tick with a 1 ms sleep between ticks (`doc_actor/fuse.rs:38`, `mod.rs:767`); a 21k-node first block
at 1.2-1.5 us/node (C2 numbers) is 25-32 ms of CPU, so roughly 40-50 ms wall (more at load 20).
The doc engine has **no focus chain** in that window: `set_focus_chain` is called only from
`FuseStage::fact`, and nothing sends `DocCtl::Fact`. `fuse_tick` patches the mirror without telling
the Reader, so an utterance spoken in the window is never corrected.
Failure scenario: Firefox, a long page loads with autofocus on (or the user tabs to) a list item or
tree item named "• Alpha" (the corpus has such items: the parity test asserts `marked > 0`):
gorca says the marker (old: "Alpha"). A control the engine left nameless is spoken as a bare role.
Description and `READ_ONLY|HAS_POPUP` are lost only where the mirror had them and the focus fetch
did not (not observed on the corpus; by reading only).
Fix (minimal, deterministic): in `overlay_from_meta`, when `meta.sem.is_none()`, take the legacy
values that are still in `NodeMeta`: name of a `ListItem|TreeItem` from the doc node's name when
the snapshot's ends with it, `description` from `meta.description`, `meta.states & (READ_ONLY|HAS_POPUP)`;
keep `subtree_label` as the nameless-control fallback for that case only (it is cheap and was the
behaviour on main). Wiring `DocCtl::Fact` is still worth doing (it puts the focused node first in the
doc engine), but it does **not** fix this: it is asynchronous and the Reader reads the overlay before
the fact is fused. Add a test: `overlay_from_meta` with `sem: None` and legacy meta keeps main's output.

**M-2 (Medium, follow-up before M22b). The engine and the readers use different `content-over-engine` predicates.**
`strata.rs` (fuse_node_with, S3 block): the engine replaces an engine content name whenever the
derived name is *longer* (`c.name.len() > current_name_len`), whatever its text. The overlay
(`fused_name`) and both agreement checkers accept the replacement only when
`gorca_fuse::name_extends` holds. Today that only means the entry carries a name that speech
ignores and the checker logs as a disagreement (so the deviation does **not** hide unrelated
names). From M22b, when the presenter reads the entry, any longer derived name wins.
Example: a Blink grid row with a text field whose value is "42" and label "Quantity": accname 2E
puts the value in the row name, S3 puts the child's *name*, the longer derived string replaces the
engine's correct one.
Also `name_extends` is a substring test on space-squeezed strings: a one- or two-character engine
name ("5", "OK") is "extended" by almost any longer row text.
Fix: use `name_extends` as the engine's gate too (one predicate for engine, checker and reader), and
require a word-boundary prefix/suffix match or a minimum length of the engine name.

**M-3 (Medium-Low, measure before M22b, not merge-blocking). Re-fusion cost on text changes is bounded but now cascades through list items.**
Probe (release, Rules::aria, load ~11):

| Tree | fusions per leaf rename | time per rename |
|---|---|---|
| chat log: 500 list items, 3 nameless wrappers, paragraph, 20 leaves; last leaf streams | 3.0 | 1.6 us |
| one list item over 8 nameless levels, fan-out 2, 256 leaves | 3.0 | 4.7 us |
| 64 list items nested through nameless lists | 130 | 47-50 us |

Bounded: `views` visits at most ~`content_depth x max_children` nodes (~2.3k) per fusion, the
dirty set dedupes within a run and the doc run is 2 ms-budgeted. The new cost is that `ListItem`
and `Article` are now content-named, so every token streamed into a nested list item re-fuses
(and re-publishes, `patch_sem` + mirror `SetProps`) every ancestor list item: the 64-deep case is
130 fusions and 64 mirror patches per token. That is not a merge blocker (the doc actor is
budgeted, nothing blocks the Reader), but ADR-0024's 10k events/s target is not shown for it. A
storm bench row (nested list items + streaming text, doc actor end to end) should land with M22b,
when these entries become speech. Note too that ARIA 1.2 gives `listitem` and `article` *name from
author*, not content: naming them from content is a gorca choice (it replaces `subtree_label`'s
list-item fill) and should be written down as such in the profile comment.

**L-1 (Low, latent until M22b). App writer child order is arrival order.**
`app_actor/fuse.rs apply_one/link`: children are appended by `Ivm::reparent` in the order replies
arrive; the snapshot's `ChildList` order is never applied (`set_children` is never called by the
app writer). A content name fused on the app side can therefore read "all Save" for "Save all".
Not spoken today (dual-carry), but it will be after M22b. Fix: when a parent's snapshot has
`ChildList::Known`, call `set_children` with that order for the children the engine holds.

**L-2 (Low). A node whose new parent is not known yet stays under its old parent.**
`apply_one`: `(Some(pk), None) => remember_orphan` but no `reparent(idx, None)`; the old parent keeps
reading the moved child's name until the new parent arrives (or forever if the orphan list is
dropped at `ORPHAN_CAP`). Fix: detach (`reparent(idx, None)`) before remembering the orphan.

**L-3 (Low). `TextChanged` still bypasses fusion invalidation.**
`app_actor.rs` `invalidate_for`, `NodeEvent::TextChanged`: calls `self.writer.invalidate` directly
with `NAME|VALUE` (both in `FUSED_FIELDS`), not `invalidate_node`, so the app entry of a text node
reads fresh next to a stale snapshot. If this is deliberate (storm cost), say so in a comment;
otherwise route it through `invalidate_node`.

**L-4 (Low). Entry staleness is not checked by the overlay.**
`fused_name` uses `e.name` without looking at `e.stale`; for the nameless-control path a stale
content name is spoken as current. Acceptable while the doc side rewrites every node per batch, but
it should check the mask once the app-side entry is read.

**N-1 (Nit).** `INVAL_CAP` doc says "the oldest are dropped"; the code drops an arbitrary
`HashMap` key. **N-2.** The Signal row evidence is a hand-modelled tree; `tests/web/chr0/pages/signal-like.html`
can be recorded with the corpus recorder in Chromium, which would turn the deviation's only evidence
into a recorded case (and show whether real Blink includes hover-only action buttons in the row,
i.e. whether the derived name repeats "React to message, Reply, More actions" on every row).

### Answers to the focus questions

1. Spoken output: after full fusion the corpus parity holds (0 lost, 0 different, every marker
   stripped) and the synthetic cases match `subtree_label`. The regression is the pending-fusion
   window (M-1): user-visible, narrow, with a cheap fallback fix. Headings, links and buttons are
   named by the engines themselves on the corpus, so they are not affected once the entry exists.
2. content-over-engine: sound as gated **in speech** (Derived prov plus `name_extends`, 160-char
   cap), and the checker logs anything else, so it is not hiding disagreements. The engine gate is
   looser than the reader gate (M-2), and rows will be verbose where Blink keeps action buttons in
   the row (N-2).
3. Reparent: atomic, cycle-checked (by reading: a hostile A<->B parent loop is refused through `link`; the Ivm tests cover direct cycles), depths
   re-keyed. Gaps are order (L-1) and detach (L-2), both latent.
4. Storm: bounded (M-3 numbers); a storm bench is a pre-M22b requirement, not a pre-merge one.
5. Rules: clean.
