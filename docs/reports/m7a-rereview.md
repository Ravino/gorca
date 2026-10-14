# M7a re-review: Firefox browse mode, round 2

- Branch `worktree-agent-a1d08cc29c61e0bae`, head `cb93527`, which includes main `4f4a479` (M3) through merge `c87e576`. Round 1 was `docs/reports/m7a-review.md` at `177f39c`.
- Reviewer run: 2026-10-14, governor `performance`. Host load average was 5.4 at the start, 7.1 after the test run, and 5.7 to 9.5 during the e2e loop.
- The implementer's worktree was not modified. Probes and the e2e loop ran in a `git archive cb93527` copy with its own target directory (`$SCRATCH/src`, `$SCRATCH/target`).
- The e2e loop used `tests/web/m7/run_m7.sh` unchanged except for one added reader flag (`--bench-present-log`). Everything ran in a private session: private dbus-daemon and a11y bus, a headless gnome-shell on a unique socket, and a fresh Firefox profile per launch. Teardown signalled recorded PIDs and process groups only. Nothing touched the live session.

## Verdict: MERGE AFTER FIXES

- All round-1 findings except L2 are fixed, and every round-1 probe now fails as it should (table below).
- **N1 must be fixed before merge.** It is the one new defect that silences a focus, and the minimal patch is small.
- W3 (the silent tab switch) could not be reproduced: 0 lost in 300 trials. N1 and N2 are the probe-confirmed ways a focus goes unspoken. Instrument W3 (see section 3) before calling it closed.
- N3 to N8 can be follow-ups.

## Gates (run in the worktree, unmodified)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass: 1397 passed, 0 failed, 15 ignored |
| `cargo test --release -p gorca-core --lib browse::latency -- --ignored` (copy) | pass. 10k nodes: W1 heading p99 1.7 µs, BR1 line p99 1.9 µs, page p99 17.7 µs. 100k nodes: page p99 15.7 µs. J1 p99 227 ns. Same as round 1. |

## 1. Round-1 findings: re-verified

Round-1 probes are in `bench/results/e2e/m7a-review/probes/`. They were re-inserted into the copy unchanged; only `#[ignore]` was removed from the two abort probes. Each probe asserted the bug, so a failing probe now means the bug is fixed.

| Finding | Probe result at `cb93527` | Verdict |
|---|---|---|
| H1 recursion and abort | `probe_deep_fresh_subtree_recurses` (100k chain, debug, 2 MiB thread) and `probe_cyclic_fresh_subtree_recurses_forever` pass with no abort. `probe_depth_600_fresh_subtree_is_kept_whole` gets 511 new ids, not 600: the source cut the chain at the cap. | fixed |
| H2 cold field becomes browse mode | After 1.1 s the mode is `Loading{known:false}` and the reader mode is `Focus` (grabs off). | fixed |
| M1 moves bypass the depth cap | One move gives depth 512, which equals `MAX_DEPTH`. After 100 rounds of moves the depth is still 512 and `describe` takes 42 µs. | fixed |
| M2 total caps | Fan-out is cut to 100 001 nodes (was 300 001). Deferred lists hold 2 entries (was 5 holding 5M ids). Bypasses remain: see N4. | mostly fixed |
| M3 lost notices | A key on a vanished document now gives the keyboard back (`Mode::Focus`). Residue: see N6. | fixed |
| M4 first line after navigation | The first line is spoken on `Changed` (`["Second document", "First line of the second page"]`). | fixed |
| M5 web refinements dropped | Code reviewed: a changed title is refined, and other differences go to `revalidations_suppressed` (`present/flow.rs:436-457`). | fixed |
| L1 "unavailable" | Fixed for web content, but the exemption is global; see N7. | fixed, with a side effect |
| L2 Shift+digit grabs | Deferred to M7b (keysym-based grabs). | open, accepted |
| L3 keys after a mode switch | Now measured: FM1 `lost_after_switch` is gated, and the at-once race is reported. | fixed |

## 2. The M3 integration (merge `c87e576`)

- **Say-all and say-selection in browse mode:** correct.
  - `text_mode_allows` (`text/mod.rs:586-592`) admits only `Mode::Focus` and `Mode::PassThrough`. Browse mode says "not available in this mode" and starts no `TextFetch` and no `SayAllChunk`.
  - The merged test `say_all_and_say_selection_refuse_cleanly_in_browse_mode` checks exactly this.
- **Typing echo in Firefox focus mode:** correct, but the merged test does not test the Firefox path (N5).
  - In a real Firefox the app is non-scalar (`toolkit.rs:47` `Utf16Bom`, through `app_actor.rs:689`). `text_track` therefore untracks it (`text/mod.rs:394`), `text_key` returns false, and the generic key echo speaks the letter once.
  - My probe `probe_firefox_non_scalar_field_echo_is_generic_and_once` confirms this: `tracked=false`, spoken `["a"]`.
- **`Role::DocumentWeb` in `trackable`:** right.
  - The role is tracked only when it is `EDITABLE` (designMode or a contenteditable root) and only in focus or pass-through mode.
  - Non-scalar apps are refused before `trackable` runs (`text/mod.rs:394`), so a Firefox document is never tracked, which is what M3 requires.
  - Chromium is `Scalar` (it converts UTF-16 offsets itself), so an editable Chromium document is tracked in focus mode. That is the intended M3 behaviour.
  - Remaining risk: toolkit detection that misses Firefox (`toolkit.rs:23` matches "firefox", "thunderbird" or "gecko") falls back to `Scalar`. Any other Gecko embedder would then be tracked with UTF-16 offsets.
- **Merge resolution error (N8):** `Scn::all()` in `crates/gorca-bench/src/e2e/scenarios.rs:169-172` now lists `K4 K4r R1 R1s RS` twice. A full run executes them twice and pools their samples.

## 3. The open bug: one W3 trial lost its speech

**What the lost trial shows.** The run is `bench/results/e2e/m7a-merged/20261014T083216Z.txt`, note 12. It came from the full suite at load 9.5, in repeat 1, around trial 2 of 10. The A=66 and B=84 ids below come from that run; the link in A was 83.

- The tail of focus events is `84,84,83,83,84,84,83,83`: each switch emits two `focused=1` events, and the A-return lands on 83, A's link.
- The return focus did reach the bus. Nothing was spoken in 3 s.

**What a normal trial looks like.** The presentation log from my loop shows it:
- the A-return is always presented as `hit` (link, `may_refine`);
- B is presented as `hit` (`document_web`) about 0.8 s earlier;
- each run has 23 presentations, identical across all 30 runs.

**Reproduction attempt.**
- W3 alone, 30 invocations × 10 trials = 300 trials, at load 7 to 9.5, with `--bench-present-log`.
- Result: **0 lost** (`bench/results/e2e/m7a-rereview/w3-loop/`).
- Together with the implementer's 6 × 10 rerun, that makes 1 loss in 390 trials. It is too rare to bisect from bench output, and no counter was recorded when it happened.

**Root cause.** Not proven. Below are the two silent-focus routes that the probes confirm, then the third route that W3's data points to.

1. **N1 (confirmed silent path): the single timer slot loses deadlines.**
   - The runtime keeps one slot: `runtime.rs:1043` sets `timer = min(timer, at)` and `:984` clears it when it fires.
   - The Reader does not re-arm a deadline that is not yet due:
     - `focus.rs` `settling()` returns `(same, None)` on an early `TimerElapsed`;
     - `browse/commands.rs:1004-1047` `web_timer` does not re-arm `LOADING_GRACE`.
   - Probe `probe_an_early_timer_drops_the_focus_settle_deadline` shows a menu-item focus armed for +30 ms. A `Timer` at +5 ms leaves it in `Settling` with no `ArmTimer`. In the daemon nothing ever presents it.
   - The A-return in W3 is a `hit` with no settle delay, unless the previous focus was under 20 ms earlier (`STORM_HOLD`, `present/flow.rs:136`). So this explains W3 only if an unseen focus came first. It does explain every silent flapping-role focus that follows a browse key within the 150 ms scroll debounce.
2. **N2 (confirmed silent path): failed presentations are silent.**
   - `present/flow.rs:403-433`: a focus fetch that answers `Gone` or `defunct` goes to `Orphaned` and asks for `QueryActiveWindowFocus`, which is not implemented (`runtime.rs:1044-1046`, a debug log).
   - A `Timeout`, `Busy` or `Unresolved` with no cached snapshot speaks nothing.
   - The A-return would take this path only if the B-1 snapshot of the link was a Miss or Gone. A wake rebuild (`source.rs:1124`, AM-13) runs on the same Firefox at the same moment, which makes a slow or failed answer more likely under load.
3. **Most likely for W3, given its data:** route 2 with a cold or invalidated link snapshot. The loss came early in the run (trial about 2), when the doc actor of A had just been woken for the first time. Under load the focus fetch and the rebuild compete for the same Firefox main thread, and a failed fetch leaves nothing to speak.

**What to do.**
- Fix N1.
- Implement a fallback for an unspeakable focus. At minimum, after `Orphaned` or a failed fetch, speak from the doc mirror when the node is joined, since the browse mirror has the link's name and role. In general, re-fetch once.
- Make the W3 harness dump these on a loss: the presentation log (`--bench-present-log`) and the Reader counters `stale_replies_dropped` and `duplicate_focus_dropped`, the lane's `stale_dropped` and `inbox_dropped`, the runtime's `fetch_refused`, the focus state, and the last 10 present records. The next loss will then name its route.
- Keep W3 at least Medium until a loss is caught with these counters.

## 4. Stale binary in `bench/e2e/run.sh`

`bench/e2e/run.sh:18-19` builds `gorca-bench` only. A `--reader daemon:target/release/gorca` run then uses whatever binary is there. The `m7a-merge-m3` run (08:10:41Z) used a binary built before the merge: the binary's mtime is 08:11:19Z, while `meta.git_rev` says `c87e576`. The result was 150 C1 losses and 27 E2 missing.

Proposed fix:
1. Build the reader too: `cargo build --release -p gorca-bench --bins -p gorca-daemon --offline`. `run_m7.sh:31` already does this.
2. Stamp the binary. Have `gorca-daemon` embed the git rev and dirty flag at build time (a `build.rs` that sets `GORCA_GIT_REV`, printed by `--version`). The harness records it as `meta.reader_rev`, and **refuses to run** when it differs from `meta.git_rev`. Recording the binary's sha256 and mtime as well catches a dirty tree.
3. Reject a `daemon:` path outside the worktree's `target/release` unless `--allow-foreign-reader` is given.

## 5. New findings (ranked)

### N1 (High): one timer slot, and deadlines that are not due are never re-armed, so a focus can stay silent

- **Where:**
  - `crates/gorca-core/src/runtime.rs:1043` (`timer = min`) and `:984` (cleared on fire);
  - `crates/gorca-core/src/focus.rs` `settling()` returns `TimerElapsed{now<until}` → `(same, None)`;
  - `crates/gorca-core/src/browse/commands.rs:1004-1047` (`LOADING_GRACE` not re-armed).
- **Failure:**
  1. In browse mode the user presses Down. The scroll debounce arms +150 ms (`commands.rs:1206/1249`).
  2. Within 150 ms the user opens a menu. Focus on a `MenuItem` settles for 30 ms (`FOCUS_FLAP_DELAY`), so the slot keeps the earlier scroll deadline.
  3. The scroll timer fires. The focus is not due and is not re-armed, and the slot is cleared.
  4. The menu item is never spoken, until some later timer happens to fire.
  - The same applies to the 4 ms `STORM_HOLD` of a cached focus in a rapid sequence, and to M3's caret and say-all timers.
- **Origin:** pre-existing (`b58f690`). M7a adds two timer sources and M3 adds more, so collisions become routine.
- **Probes:** `probe_an_early_timer_drops_the_focus_settle_deadline` and `probe_an_early_timer_drops_the_loading_grace` (no re-arm; `Settling` / `Loading` kept).
- **Fix:** have `Reader::handle` end by pushing one `ArmTimer(next_deadline())`, computed from every subsystem (focus `Settling.until`, the web grace and scroll, pass, review and text). The runtime then replaces the slot (`timer = Some(at)`) instead of taking the minimum. The minimal patch re-arms `until` in `settling()` and the grace in `web_timer`. Add a Reader test with an early `Timer`.

### N2 (Medium): an unspeakable focus is silent

- **Where:**
  - `present/flow.rs:403-433`;
  - `focus.rs` `NodeRemoved` → `Orphaned` + `QueryActiveWindowFocus`;
  - `runtime.rs:1044-1046` (not implemented).
- **Failure:** a focus that misses the B-1 mirror and whose fetch times out, is refused (`fetch_tx` full → `Busy`, `runtime.rs:1021-1030`), or answers `Gone` produces no speech and no retry. This is the most likely route for the W3 loss (section 3).
- **Fix:**
  - retry once with a fresh deadline;
  - for a web focus, speak from the browse mirror through the join (name and role are there);
  - implement the active-window query, or at least say "unknown" rather than nothing.

### N3 (Medium): the warm-up budget is not enforced inside a round, and the new descent adds up to 72 serial calls per round

- **Where:** `crates/gorca-doc-source-atspi/src/warm.rs:48-86` (`descend`) and `:156-215` (the loop checks `end` only between rounds).
- **Cost:** each call has its own 800 ms deadline, and `GetTree` has 2 s.
  - One round costs at most 5 × 0.8 + 0.8 + 3 × (24 × 0.8 + 2.0) ≈ **68 s** against a slow-answering app; the old protocol's worst case was about 10.8 s.
  - At least 2 rounds are needed. The `budget` of 2 s (`fetch.rs:212`, `factory.rs:283`) bounds nothing inside a round.
  - In the rig (Firefox fast, depth about 6) the cost is 17 ms of D-Bus time. A busy, just-loaded page with 20 levels at 10 to 30 ms per call costs 0.6 to 1.8 s per round. The first browse line waits for that (W2b path).
- **Correctness:**
  - `descend` is not a bounded window. It returns `kids[pick]` of the first node with more than 64 children (`:81`), or the node at level 23 (`:85`). Either can root an arbitrarily large `GetTree`, for example the first of 100 sections each holding a 10k-row table.
  - With n == 2, `picks` samples the last child twice.
  - All 43 warm-ups in the merged and fix logs finished in exactly 2 rounds with 17 sampled nodes. "Stable across two rounds" on 17 nodes now always holds. It no longer measures whether the attribute domain has reached the whole page, and no e2e gate checks attributes (tag, level, xml-roles) on a 10k page after warm-up.
  - `rep.probes` does not count the `GetChildren` calls made in `descend`.
- **Fix:**
  - one `Deadline(end)` for the whole warm-up, passed as `min(800 ms, end)` to every call;
  - run the three descents with `join`;
  - make `descend` return the parent of the over-wide node and cap the sample with a node budget (a `GetTree` limit, or fall back to `GetChildren` only);
  - add a W2b-style gate that checks heading levels and landmark roles on the 10k page's first block.

### N4 (Low): the node and pending caps can still be passed within one batch

- **Probes** (`bench/results/e2e/m7a-rereview/probes/probe_rereview_translate.rs`, release):
  - `probe_one_list_of_new_subtrees_passes_the_node_cap`: one child list of 60 000 new subtrees of 10 nodes on an existing parent gives **600 002** mirror nodes (cap 500 000) and `truncated 0`. The cause:
    - `sync_children` checks `by_src.len()` once per child before the `SyncChildren` (`translate.rs:759`);
    - `build_spec` counts only its own subtree (`:668`);
    - `by_src` grows only after `register_created`.
  - `probe_deferred_plus_orphans_exceed_the_pending_cap`: **400 000** ids parked (cap 200 000). The two pools are not counted together:
    - orphans are drained at batch start (`:397`);
    - lists are parked against `deferred + orphans` while orphans are empty (`:428-443`);
    - orphans are re-parked against their own count (`:473`).
- **Bound:** each overshoot is bounded by one batch, and a batch is bounded by the source's fetch.
- **Other gaps:**
  - The source's `IdentityMap::load_parts` (`identity.rs:374`) has no cap, so the initial fetch is unbounded except by D-Bus message size.
  - Orphan `SrcNode` text is unclipped until `spec_of`.
- **Fix:**
  - pass a running node budget into `build_spec` across one `sync_children`;
  - park lists after orphans are re-parked, against one shared counter;
  - cap `load_parts` at `MAX_DOC_NODES`.

### N5 (Low): the merged typing-echo test does not exercise Firefox

- **Where:** `browse/tests.rs:1186-1212`. The rig never calls `set_text_scalar(app, false)`, so the M3 caret layer tracks field 1014 and owns the echo (probe: `tracked=true`, `keys_seen=1`).
- **Effect:** the test passes for the wrong layer. A regression in the non-scalar gate, double echo included, would not be caught.
- **Fix:** mark the rig app non-scalar in that test and assert that the caret layer is off.

### N6 (Low): the remaining fire-and-forget notices and controls

- `doc_registry.rs:216`: `WebApp`, `InDocument` and `NoDocument` still use `try_send`. A lost `WebApp` is sent once per app, so the loss is permanent: every join miss in that browser is treated as `FocusAway`.
- `runtime.rs:1090/1095`: `Activity` and `Act` use `try_send` into a 64-slot control queue, then `doc_actor/mod.rs:457` `try_send`s again into a 32-slot request queue. A lost `Awake` leaves the document dormant with a stale mirror.
- `GONE_DELIVERY` waits 2 s per pending notice (up to 9 × 2 s) during a retire.

### N7 (Low): the "unavailable" exemption is global

- **Where:** `present/utterance.rs:42-79`. `ListItem`, `Row`, `Cell`, `Table`, `Image`, ... are never called unavailable in any toolkit.
- **Effect:** a genuinely insensitive GTK list row or cell (GNOME Settings) loses its "unavailable".
- **Fix:** scope the exemption to non-scalar or Gecko apps, or to the connector mapping (do not derive `DISABLED` from a missing `SENSITIVE` for Gecko).

### N8 (Low): duplicated scenarios after the merge

- **Where:** `crates/gorca-bench/src/e2e/scenarios.rs:169-172`. `K4 K4r R1 R1s RS` appear twice in `Scn::all()`.
- **Fix:** delete the second copy, and add a test that `all()` has no duplicates.

## Evidence

- New probes, in the main repo: `bench/results/e2e/m7a-rereview/probes/`.
  - `probe_rereview_translate.rs`: N4, both pass, which confirms the bypasses.
  - `probe_rereview_browse.rs`: N1 ×2 and N5 ×2.
- Probe insertion: `mod <name> { <file> }` inside `translate.rs` `mod tests`, or at the end of `browse/tests.rs`.
- W3 loop: 30 runs, 0 lost, present logs included: `bench/results/e2e/m7a-rereview/w3-loop/`.
- Round-1 probe output is summarised in the section 1 table.
