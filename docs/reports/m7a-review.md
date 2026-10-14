# M7a review: Firefox browse mode for static pages

- Branch: `worktree-agent-a1d08cc29c61e0bae`, head `177f39c`, against `main` `0bfe951` (151 files, +15.2k/-0.4k).
- Reviewer run: 2026-10-14. Host load average 4.7 to 6.9 during the runs, governor `performance`. No e2e was run. All probes are unit-level, in-process, and touch no session.
- Spec: `docs/design/browse-mode.md`, ADR-0003 §21, ADR-0007, `docs/plan/checklists/M7a.md`.

## Verdict: MERGE AFTER FIXES

H1 and H2 must be fixed before merge. M1, M3 and M4 should land with this branch or as the first follow-up. The rest can be follow-ups.

## Gates (run in the worktree)

| Gate | Result |
|---|---|
| `cargo fmt --check` | pass |
| `cargo clippy --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass: 1265 passed, 0 failed, 15 ignored |
| `cargo test --release -p gorca-core --lib browse::latency -- --ignored` | pass: 10k and 100k mirrors, W1 p99 1.9 µs, BR1 line p99 1.8 µs, page p99 16.6 µs, J1 p99 75 ns |

## Probes

The probe sources are in `bench/results/e2e/m7a-review/probes/`, in the main repo:

- `probe_identity.rs`, appended as `mod probe_m7a_identity` inside `identity.rs` `mod tests`;
- `probe_translate.rs`, appended as `mod probe_m7a_translate` inside `translate.rs` `mod tests`;
- `probe_browse.rs`, appended as `mod probe_m7a_browse` at the end of `browse/tests.rs`.

They were inserted temporarily and then removed. The worktree is unchanged.

| Probe | Result |
|---|---|
| `probe_deep_fresh_subtree_recurses` (`PROBE_N` env) | **stack overflow, SIGABRT**: release from N=5000 (N=2000 survives), debug from N=2000 |
| `probe_cyclic_fresh_subtree_recurses_forever` | **stack overflow, SIGABRT** with a 3-node cycle |
| `probe_depth_600_fresh_subtree_is_kept_whole` | the source passes 600 levels through, with no cap |
| `probe_moving_existing_subtrees_bypasses_the_depth_cap` | depth 511 becomes **1022** after one move |
| `probe_repeated_moves_grow_depth_without_bound` | depth **51 001** after 100 rounds; `describe` at the bottom takes 0.86 ms |
| `probe_wide_fan_out_has_no_cap` | 300 000 children of one node accepted, `by_src` = 300 001 |
| `probe_deferred_list_size_is_not_capped` | 5 deferred entries hold **5 000 000** ids |
| `probe_cold_field_becomes_browse_mode_after_the_grace` | the focus is on an unmirrored edit field; after 1.1 s the state is `WebMode::Browse` and `Mode::Browse` (browse grabs on) |
| `probe_navigation_loses_the_first_line` | the focus says `["Second document"]`; nothing is spoken on `Changed`; Down then reads the first line |
| `probe_browse_on_a_vanished_document_swallows_keys_silently` | `Mode::Browse` stays; H says nothing |

## Findings (ranked)

### H1 (High): recursion over untrusted trees remains in the web source, and a deep or cyclic subtree aborts gorca

- **Where:**
  - `crates/gorca-doc-source-atspi/src/identity.rs:877-916`: `insert_fresh_subtree`, which calls itself at :909;
  - `identity.rs:696-871`: `rebind_subtree`, which calls itself at :843;
  - both are reached from `apply_children` (:578, :590) and `resync_subtree` (:458);
  - the live source calls them at `source.rs:982` and `source.rs:1182`.
- **Scope:** `identity.rs` is unchanged on this branch, but M7a puts it on the live path for every web page. The translator fix in `translate.rs` is complete for its own walks: `build_spec` is iterative, and `depth_of` and `nested_doc` are bounded. However, the fix stops at the source.
- **Failure:**
  - A page appends a subtree about 5000 levels deep after load, through JS. Children-changed leads to a fetch of the subtree, then `apply_children`, then recursion on a tokio worker (2 MiB stack; no `thread_stack_size` anywhere in the workspace). The result is a stack overflow, and the **whole process aborts**. A panic would be contained by `JoinSet`; an abort is not.
  - A fetched forest that lists one path twice makes `child_index` cyclic and the recursion infinite. The 3-node probe aborts.
  - Firefox's own layout depth limit may stop real web content earlier; that is unverified. Chromium, Electron and any other AT-SPI app are not bounded by it.
- **Fix:**
  - Convert both functions to an explicit work stack.
  - Track visited indices in `child_index` traversal.
  - Apply `MAX_DEPTH` in the source as well (cut the fetched subtree and count it), so the source and the translator agree.
  - Add a 100k-deep regression test and a cyclic one, run on the default 2 MiB test thread.
- **Other recursion in the web path:**
  - The treap in `gorca-doc/src/web/doc.rs`: `merge`/`split` (:575, :606) and `find_next`/`find_prev` (:990, :1010). These are O(log n) expected, with hashed `splitmix` priorities. Acceptable.
  - `wire.rs:148/188`: recursion over D-Bus signatures, which the bus caps at 64. Acceptable.

### H2 (High): an unmirrored focused field turns into browse mode after 1 s, and keys typed into the field are swallowed

- **Where:**
  - `crates/gorca-core/src/browse/commands.rs:913-952` (`web_timer`);
  - `commands.rs:667-670`: `editable` is taken once from `dir.lookup`, and a cold focus is `Lookup::Miss`, so it is `false`;
  - `browse/mod.rs:283-289`: a repeated `FocusPending` for the same focus returns early and never updates `editable`;
  - nothing re-evaluates the mode when the focus snapshot arrives later.
- **Failure:**
  - A web app inserts a login dialog and focuses its field.
  - The focus snapshot is cold (Miss), and the join misses because the source has not refetched yet.
  - `InDocument` arrives. If the mirror needs more than `LOADING_GRACE`, the timer moves the user to `Browse` at the top of the page with the letter grabs on. This happens on a 10k page, under load, or when the field is in a region the mirror cut (depth cap, nested frame).
  - The user's typing becomes quick navigation, and the letters never reach the field.
  - The probe confirms it. The comment in `web_timer` ("a field would have been detected as editable") assumes a Hit that a cold focus does not have.
- **Fix:**
  - Flip to Browse only when the focus snapshot is a `Hit` and `!passes_through`. Otherwise stay in `Loading`, where the grabs are off.
  - Re-run the `FocusPending` classification when the focus presentation reply arrives (`present()`), and when `Changed` makes the join hit.

### M1 (Medium): the depth cap can be bypassed by moving existing nodes (the "resource-bound-bypass" the scanner reported)

- **Where:** `crates/gorca-core/src/doc_actor/translate.rs:705-707`. `ChildSpec::Existing` children skip the cap. The cap is checked only for new nodes, at :709 and :627, and `begin_document` is not involved.
- **Failure:**
  - The page builds two 511-deep chains, then lists the head of one under the tail of the other. The depth becomes 1022.
  - Repeating this costs one batch per +511 levels; the probe reached 51 001.
- **Impact:**
  - The consumers are iterative today, so the effect is CPU and latency, not a crash: `describe` walks every ancestor (`browse/present.rs` ancestor chain) and `WebDoc::depth_of` (`doc.rs:477`) is unbounded.
  - The invariant that "the mirror is never deeper than `MAX_DEPTH`" is false. Any future recursive consumer inherits a crash.
- **Other routes, checked:**
  - Nested documents and iframes do not reset depth: `depth_of` counts from the mirror root, and `nested_doc` is capped at 512 hops.
  - Text and attribute deltas do not change depth.
  - Wide fan-out is not bounded at all; see M2.
- **Fix:** after `SyncChildren`, walk the moved `Existing` subtrees iteratively with their new base depth and cut what exceeds `MAX_DEPTH` (count it as `truncated`). Alternatively, reject the move and request `Rebuild`, which is cheaper but invites a rebuild loop.

### M2 (Medium): page-fed state in the translator has no total bound

- **Where:** `translate.rs`:
  - `MAX_PENDING` (:36) caps the **number of entries** in `orphans` and `deferred` (:396-402, :430-435), not their size;
  - there is no node or width cap: `by_src`, the mirror, and `touched` per batch;
  - `apply_text_edit` (:813-845) converts the whole text to `Vec<char>` on every splice, and node text has no length cap.
- **Measured:**
  - 300 000 children of one node are accepted.
  - 5 deferred lists hold 5 000 000 ids, which survive across batches until a matching parent arrives or a reset happens.
  - The bound here is the source's fetch budget, not the core.
- **Bounded:** `touched` and `break_parents` per batch are bounded by the batch, and `reap` frees detached nodes every batch. These are fine.
- **Fix:**
  - Cap the total ids held in `deferred` plus `orphans`.
  - Set a per-document node budget (for example 500k) with truncation and a counter.
  - Cap per-node text, or splice by byte offset into a `String`.

### M3 (Medium): lost document notices leave browse mode on a dead document, and keys are grabbed and do nothing

- **Where:**
  - `doc_actor/mod.rs:453`: `Gone` uses `try_send`, and the result is ignored.
  - `doc_actor/mod.rs:315-330`: `notify` drops the pending `FirstUseful` when the queue is still full. Its comment says it never loses one.
  - `doc_registry.rs:210-211`: `WebApp`, `NoDocument` and `InDocument` use fire-and-forget `try_send`.
  - The Reader's queue is `DOC_QUEUE = 64` (`reader.rs`), shared by all documents.
  - `browse_ctx` (`commands.rs:980`) does not check the handle, and the cursor and quicknav paths `return` silently when `docs.get(doc)` is `None` (`commands.rs:1113`, `:1176`).
- **Failure:** a session restore of many tabs fills the queue, and `Gone` for the active document is lost. The mode stays `Mode::Browse`, so letters, arrows and Enter are grabbed and produce nothing (probe). A lost `WebApp` disables browse mode for that browser until restart.
- **Fix:**
  - Deliver `Gone` reliably: `send().await` with a deadline in the retire path, or have the registry's `DocEnded` re-notify.
  - In `browse_ctx`, treat a missing handle as `DocGone`: go to `Inactive` and give the grabs back.
  - Keep `FirstUseful` pending instead of dropping it.

### M4 (Medium): the open bug, "M7 Second, document" spoken without its first line

- **Root cause (probe-confirmed route):**
  - The entry line is a one-shot side channel, computed when the focus joins: `web_root_entry`, `commands.rs:690-722`.
  - If the new document's mirror has no line yet, `first_line` returns an `Edge` and `pending_tail` is not set. This happens when the focus on the new root arrives right after the first publish, or the first publish holds only the top of a progressive build.
  - Nothing retries: `web_changed` (`:859-882`) only repairs the cursor, and `FirstUseful` is ignored in Browse or Focus (`:826`).
  - Because the user goes from Browse(A) to Browse(B), no mode word is spoken either, which matches the e2e note exactly.
- **Second route (code-only, not confirmed in e2e):** the tail expires after `WORD_TTL` = 600 ms (`commands.rs:44`, `:504-513`) when the focus presentation of a cold new root waits on a fetch reply from a busy, just-navigated Firefox.
- **Cheap to fix:** about 40 lines plus 2 tests.
  - Replace `pending_tail` with an `entry: Option<(DocId, Instant)>` state.
  - Compute the line when the focus utterance is presented, from the mirror at that moment.
  - If there is still no line, speak "title, first line" on the first `Changed` that has one, within 2 s, provided no key was pressed since.
  - Clear the state on any key or focus change.
  - The cold-load fix from this branch (`web_first_useful` → `web_speak_entry`) has the same gap when its first publish is empty.

### M5 (Medium): RV-1 corrections of web documents are dropped and counted as agreement

- **Where:** `crates/gorca-core/src/present/flow.rs:436-441`. Every non-defunct revalidation of a `DocumentWeb` returns early and increments `revalidations_agreed`.
- **Failure:** the cache served an old title or `busy` state, for example the previous page after navigation. The correction is never spoken, and the B-1 counter M-6 reports agreement that never happened.
- **Fix:** refine only when the name changed (that is what the user needs), or at least count the drop as `revalidations_suppressed`.

### L1 (Low): the "unavailable" fix is role-specific

- **Where:** `present/utterance.rs:50-55` exempts only `DocumentWeb`.
- **Root cause:** the connector maps "no SENSITIVE" to `DISABLED` (implementer memory). The browse presenter correctly limits "unavailable" to control roles (`browse/present.rs:110-126`), but the focus presenter does not.
- **Exposure:** a `div tabindex=0`, a focusable heading, or a section may also lack SENSITIVE/ENABLED in Firefox. Unverified on live Firefox.
- **Fix:** apply the same control-role rule in `present_with`, or stop deriving `DISABLED` from a missing SENSITIVE for Gecko.

### L2 (Low): Shift+digit grabs hard-code US symbols

- **Where:** `commands/compile.rs:292-297`.
- **Effect:** on a non-US layout (ru: Shift+2 is `"`, Shift+3 is `№`), Shift+1..6 are not grabbed, so "previous heading of level N" does nothing. Letters are keysym-grabbed too (pre-existing), so a Cyrillic layout disables quick navigation.
- Nothing is swallowed. It is a functional gap for a user who switches layouts. Derive the symbols from the live keymap.

### L3 (Low, by design): keys race the grab change after a mode switch

- **Where:** `crates/gorca-bench/src/e2e/web.rs:516-536` hides the race with `settle_grabs`.
- **Effect:** M7a grabs letters, so the first letter typed right after Tab into a field can be classified `Ambiguous` and answered with "again" instead of being typed. No gate measures this. Add an FM-style scenario that types immediately after Tab and gates on Ambiguous == 0, or replay ambiguous printable keys.

### L4 (Low): smaller gaps

- `Loading { doc: None }` never times out. A dropped `Ensure` (`app_actor/doc_route.rs:58`, `try_send`) leaves Loading for good. This is safe because the grabs are off.
- `spawn_action` awaits `req.send()` without a deadline (`doc_actor/mod.rs:481-488`). With 8 hung actions, further Enter presses are dropped silently (`mod.rs:438`).
- `KP_Enter` activates only with Num Lock off (operator branch, `compile.rs:268-276`). This is safe.
- The doc comment at `fetch.rs:17` names a `fallback_walk` that does not exist.
- `Reader.web_extra` is never cleared. This is harmless because it is keyed by `UttId`.

## Mode machine and keys: checked and correct

- **Browse and focus switching:** the switching is correct.
  - Focus on a link leads to Browse; focus on an edit leads to `Focus{Auto}`. Escape is grabbed only in `Focus{Auto}` (`escape_grabbed`, `current_grabs`).
  - `Manual` and `Application` focus modes survive focus moves inside the same document. Only the toggle leaves them, and Escape reaches the page.
  - Activate on a field leads to `Focus{Auto}` plus `Action::Focus`.
  - The `next()` match is exhaustive.
- **Keys in focus mode and in native apps:**
  - `Loading`, `Focus` and `Inactive` all map to `Mode::Focus`, so no browse grabs apply (`web_sync`).
  - Native apps get `FocusAway`, then `Inactive`.
  - Browser chrome gets `Loading`, then `NoDocument`, then `Inactive`, with the grabs off throughout.
  - Only implemented commands are grabbed (`compile.rs:141`), and every implemented `Browse*` or `Quicknav*` command has a handler (script check, none unhandled).
- **Num Lock:** keypad digits are never grabbed with Mod2 (`compile.rs:277-286`). Quick navigation 1-6 uses the main row.
- **Quick navigation:** it does not wrap and says "no next/previous X"; Prev steps out of the current match (`outermost_match`). Edge messages are "top" and "bottom", and the cursor stays. PgUp/PgDn move 25 lines and land on the last line before the edge.

## Secrets

- The browse utterances are not written to the presentation log, which is snapshot-based and redacts `PasswordEntry` (`present/log.rs`).
- The new `tracing` calls log ids, errors and keys, never text.
- The mirror holds whatever text Firefox exposes for a password field (bullets). That is unverified for Chromium. No finding.

## The three speech fixes

1. **The refine lost the mode word and line:** fixed. `apply_web_extra` is re-applied on the Replace path (`flow.rs:459-462`, `:461`). M5 is a side effect of the related document special case.
2. **"Unavailable" on documents:** fixed for `DocumentWeb` only; see L1.
3. **The cold load skipped its first line:** fixed when the first publish has content. Same gap as M4 otherwise.

## W2b: proposed relaxation from 150 ms to 400 ms

- **The data does not support the claimed split.**
  - `bench/results/e2e/m7a-final2/20261014T053847Z.*` records only end-to-end wall time: `w2b_ms` p50 275.5 ms, max 286.0 ms, n=6 per repeat, so "p99" is the maximum. It records no phase timings.
  - The "about 257 ms waited, about 25 ms gorca" split comes from `WarmReport.waited`. It was observed in logs that were not kept, and appears only in `docs/decisions.md` and the implementer's memory.
- **`waited` is not Firefox-only time.**
  - It times gorca's own warm-up protocol: `warm_domains`, `crates/gorca-doc-source-atspi/src/warm.rs:40-129`.
  - The protocol makes 5 serial throwaway probes.
  - It then needs at least 2 sampling rounds that must agree. Each round is a `GetChildren` plus up to 3 `GetTree` calls on the first, middle and last top-level children; on a page with one wrapper element, that is roughly the whole 10k-node document, twice.
  - It sleeps 25 ms between rounds.
  - Part of the 257 ms is therefore cost that gorca's design causes. The real build then follows, and with `BUILD_HOLD` a build that started during the warm-up is rebuilt.
- **The warm-up is on the focus path only because of the rig.** The attach-time warm-up reports `NoDocument` because the headless window is never active. On a real desktop the warm-up would have run at attach.
- **Verdict:** do not relax the product gate to 400 ms on wall time.
  - Gate W2b on gorca-side time: focus arrival → speech, minus only the time spent blocked on Firefox answering, with the warm-up's own sleeps and sampling rounds counted as gorca's.
  - Keep `W2b.wall` as information.
  - Instrument the phases first: export `WarmReport.{waited, probes, sampled_nodes}`, the fetch timings and the publish time into the e2e JSON.
  - Until then, record W2b as "rig-blocked" in `docs/decisions.md`, rather than as a passed 400 ms gate.
  - Also run a variant where the window is active before attach, which is the realistic path.

## Safety of the rig (checked)

- The harness runs private buses (`run-isolated.sh`), a private headless gnome-shell on a unique Wayland socket and a unique runtime dir, and a fresh Firefox profile per launch (`--no-remote --new-instance --profile`).
- Teardown signals recorded PIDs and the Firefox process group only.
- `--impersonate-orca` is used only inside the private session.
- Failing results under `bench/results/e2e/m7-w2b/` are kept. No violation found.
