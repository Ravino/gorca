# CHR-0 review, round 1

Date 2026-10-19, reviewer. Branch `worktree-agent-a508eca24e2626204` at `de1a1aee` (base `d87409fb`), against
main `b3e40f16`. Spec: `chromium-base-design-review.md`, decision rows of 2026-10-19 (CHR-0 scope, DP-3).

**Verdict: MERGE AFTER FIXES** (F-1..F-3 before merge; F-4..F-8 may follow).

## What was verified

| Check | Result |
|---|---|
| `git merge-tree main HEAD` | one textual conflict, `docs/decisions.md` (append-only rows, union resolves it); `gorca-config/src/lib.rs` and `gorca-testkit/src/fake_source.rs` auto-merge |
| Merged tree (main `b0c94853` + branch), `cargo fmt --check` | clean |
| Merged tree, `cargo clippy --workspace --lib --bins --tests -D warnings` | clean (the `--all-targets` failure in `gorca-bench/benches/fuse.rs` was main's, fixed by `b3e40f16`) |
| Merged tree, `cargo test -p gorca-core -p gorca-doc-source-atspi -p gorca-config -p gorca-doc -p gorca-testkit` | all pass (core 857, config 137, doc source 112 + fake_peer) |
| ADR-0016 alignment of `app_modes.rs` | §2.9 maps every field; "CHR-0 needs no change before it merges" holds. P6 parity will need care with the rank order (later layer beats host row in CHR-0; PM-1 is Site > App) |
| Key path (p99 < 15 ms) | No Reader path awaits a repair or a refetch: repairs are `try_send` from the router/hub, the gate runs in the doc actor, the refetch is a spawned task. The pre-click read (≤ 150 ms) is inside the source's request loop, after Enter's speech decision; it delays the click, not speech |
| Stale results vs cursor | The refetch is re-matched with the identity ladder (ids kept); the cursor moves only by the pre-existing position restore and the B-2 rejoin, which is cleared by any focus change or browse command and checks the cursor is still at the fallback root |
| Pre-click check vs main's displaced-cursor rule | No duplication: the source guard is a second, source-side check; the notice clears `activated_node`, so the refresh is treated as displacement by main's rule. Interaction defect in F-3 |
| `.cw.sh` | untracked in the worktree root; must not be committed |

Probes: `docs/reports/chr0-review-probe.patch` (applies to the branch; run
`cargo test -p gorca-doc-source-atspi probe_review -- --nocapture` and
`cargo test -p gorca-core --lib probe_review -- --nocapture --test-threads 1`; revert afterwards).

## Findings

### F-1 (High) A repair freezes every incremental update of the document for its whole length

`source.rs` select loop: `() = sleep, if live.activity == Activity::Awake && flight.is_none()` and
`start_flight` takes `live.dirty`. While a refetch runs, no event-driven update reaches the mirror.
Probe `probe_review_incremental_update_*`: a paragraph added during a 600 ms repair becomes visible after
483/498 ms, against 17/28 ms without a repair. The branch's own M9 Chrome A/B log
(`bench/results/e2e/chr0-f1-ab-new-m9-chrome/reader-*.log`) has repairs of 4.4–4.95 s (178,589 nodes)
and 0.66 s (25,033 nodes); 92 repairs in one corpus run, most `changed=false`. Before CHR-0 a refetch
happened only on overflow or wake; now it happens on focus misses, orphans, root-state flips and young
checks. Firefox is affected too: Gecko keeps the focus-miss trigger, and the miss fires right when the
focused node's incremental add is pending, so that add (the one that would join the focus) waits for a
whole-document refetch.
**Fix:** Gecko policy `NONE` (Gecko sends the events; the owner's rule is that nothing may break), or have
the focus miss wait one source settle and re-check the join before it repairs. For Blink, keep processing
dirty nodes during a flight and re-apply them after `apply_resync` (they are already re-marked), or cap
whole-document repairs by node count and fall back to subtree revalidation above it.

### F-2 (Medium) `PathTable::absorb` drops the error-message targets the refetch learned

`fetch.rs` `absorb` merges only `other`/`next`; `fetch_document` writes `note_error_target` into the forked
table (`fetch.rs:464`). Probe `probe_review_absorb_keeps_error_targets_and_keys`: after absorb
`error_fields(target) = []`. Every refetch now runs on a fork (repairs, overload resync, wake from
dormancy), so an invalid field first seen by a refetch no longer re-reads its error text when the
`aria-errormessage` target changes (M8b regression), and the stale-mirror case is exactly where fields
first appear in a refetch. Same probe: a non-numeric path interned by the live table during the flight keeps
the live key, but the fetched nodes carry the fork key, and `path_of(fork key) = None`.
**Fix:** merge `error_targets` in `absorb`; return a fork-key → live-key remap and apply it to the
fetched document before `apply_resync`.

### F-3 (Medium) After "that item changed" a name-only change is clicked on the next Enter without being said

`DocNotice::ActionDisplaced` speaks a fixed message. Main's displacement rule fires only when the cursor
node's `content_sig` changes. Probe `probe_review_displaced_refusal_then_name_only_refresh`: a button with
visible text "Delete" whose name changes from "Delete message from Bob" to "... Alice" (row re-use, the case
the guard exists for) → Enter refused with "that item changed", the refresh lands, the 2nd Enter **clicks**,
and nothing about the new item was ever spoken. When the visible text changes
(`..._then_text_refresh`) the 2nd Enter speaks the new item and the 3rd clicks: three Enters, the middle one
needed only because the notice said nothing useful.
**Fix:** carry the fresh role/name in `ActionDisplaced` and speak it ("changed: Delete message from
Alice, button"), and set `b.displaced = None`, so the next Enter clicks what was just said; or set
`b.displaced = Some(cur.node)` and speak the line when the refresh lands.

### F-4 (Medium) Repairs ignore dormancy and orphans go to an arbitrary document of the app

`repair_request` / `repair_tick` never check `a.awake`; the source runs `DocRequest::Repair` in any
`Activity`. Background tabs get load-complete, young and orphan repairs although the wake rebuilds them anyway
(DM-I5: dormant documents track nothing). The router counts any unknown-node event (Chrome's own toolbar and
omnibox nodes included) as an orphan of `DocDirectory::of_app`, which is the first match in a `HashMap`, so
it can be a background tab.
**Fix:** refuse repairs while dormant (keep a flag; the wake rebuild covers it); count orphans only on awake
documents.

### F-5 (Medium) The fruitless back-off is shared by all triggers, so cheap orphan guesses delay a real focus miss

`RepairGate::gap`: `FocusMiss` and `RootState` pay `400 ms × 2^min(quiet, 6)`, and `quiet` grows with any
fruitless repair, mostly orphans (fruitless by construction for Chrome UI nodes, and on Electron before the
load ends). After six fruitless orphan repairs a focus miss on a really stale mirror waits 25.6 s.
**Fix:** keep `quiet` per reason (or back off orphans only).

### F-6 (Low) Root-state and load events are not coalesced before the control queue

`doc_route.rs` `root_state`/`repair_doc_of`: one `DocCtl::Repair` per event into the 64-slot control queue
that also carries `Act`, `Select` and `Activity`. Probe `probe_review_root_state_storm_fills_ctl_and_drops_a_click`:
16 root-state batches fill a 16-slot queue and the next click `try_send` fails (counted in `controls_dropped`,
not spoken). Needs a busy doc actor plus a page that flips its root states fast, so Low.
**Fix:** an `AtomicU8` "repair pending (max reason)" on `DocHandle`, set by the router; one ctl message per
transition from empty.

### F-7 (Low) The B-2 rejoin has no time limit

`grace_fallback` is cleared only by focus changes and browse commands. A repair minutes later (orphans,
60 s cap) still moves the cursor and speaks while the user is listening to something else (a non-browse
command such as reading the title does not clear it). **Fix:** expire it with `YOUNG_WINDOW` (30 s).

### F-8 (Low) Evidence not in git

The e2e results cited in `chr0-implementation.md` (`bench/results/e2e/chr0-*`) are untracked. Commit them
(failing runs included, per the repo rules); do not commit `.cw.sh`.

## Not found

No `unwrap()` on runtime paths in the diff; `Flight` aborts its task on drop; a lost source answers the
repair's waiter with an error that counts as fruitless; the young checks and the load-end follow-ups are bounded
(3 and 2); the gate caps the gap at 60 s and keeps one repair in flight per document.

---

# Delta review, round 2 (head `f46afdbe`)

Date 2026-10-19. **Verdict: MERGE AFTER FIXES** (D-1 and D-2 before merge).

| Check | Result |
|---|---|
| `git merge-tree main(689dd995) HEAD` | **conflict in `crates/gorca-bench/benches/fuse.rs`**: the branch carries a `cargo fmt` reflow of main's line 624 and main rewrote the file since. Take main's version (the branch has no real change there) |
| HEAD `cargo clippy --workspace --all-targets -D warnings` | clean |
| HEAD `cargo test -p gorca-core -p gorca-doc-source-atspi -p gorca-config` | core 866, config 137 pass; **fake_peer 29/30**: `incremental_updates_are_applied_while_a_repair_runs_and_survive_it` fails at `fake_peer.rs:2040` ("a later list dropped the node"), 1 of 3 standalone runs and in the suite run (load ~8) |
| F-2, F-4, F-5, F-6 | fixed as described; tests present |
| Gecko | `RepairPolicy::for_engine(Gecko) = NONE` |
| F-3 (security change) | matches the stated rule: bypass only if the mirror shows exactly the spoken name on the same node, with the role the mirror had at refusal, within `ANNOUNCE_WINDOW` (2 s); an empty name stores nothing; a mismatch speaks the line instead of clicking; the source guard still re-checks on the next click. The role is the mirror's *old* role (the notice carries only the name), so a role change fails closed (extra announcement, never a blind click) |

## D-1 (High) A repair still reverts an update made during it; only the re-read puts it back

`finish_flight` applies the older snapshot with `apply_resync` first and only then marks the touched nodes
dirty, so the revert goes out to the sink and the restore comes one settle later. Delta trace of the
branch's own test (prints added): after the repair, `children of doc: n=1` + `Removed 1` (the paragraph the
update added), `Settled`, then `Upsert 3` + `children of doc: n=2`, `Settled`. The Reader therefore sees the
new paragraph vanish and come back in two publishes (a cursor on it is lost and goes through position restore,
and main's displacement mark can cost the next Enter). The test's last-list check is order-dependent, which is
why it flakes rather than failing every time.
**Fix:** in `finish_flight`, re-read the touched nodes (bounded, `MAX_TOUCHED`) *before* emitting, and push
the resync difference and the re-read as one batch ending in a single `Settled`. Alternatively, leave the
child lists and removals of touched parents out of the resync. Then make the test assert that no `Removed` or
child list without the node is ever emitted, not just the last one.

## D-2 (Medium) A fruitless cheap check never escalates

`top_unchanged` (48 BFS nodes, 150 ms) answers `Orphans`/`RootState` repairs with "nothing changed". When the
staleness sits below those 48 nodes, every later orphan request gets the same answer, `quiet[Orphans]` climbs
to the 60 s cap, and no whole-document refetch runs, although the actor can see that the orphan keys are still
unknown after the "repair". Exact signals (load end, focus miss, young) still refetch in full, so this is
bounded to staleness that only orphans reveal (dropped adds after the load).
**Fix:** when a cheap pass returns and the sampled orphan keys are still not in the join, send the next
orphan repair with `cheap_check: false`.

## D-3 (Low)

- `DocHandle::request_repair`: if `try_send` fails it stores 0, which can wipe a reason a concurrent caller
  (hub vs. router) just raised. The next event re-raises it.
- `top_unchanged` runs inline in `handle_request`, so a click or selection queued behind it waits up to
  150 ms. No key-to-speech path: the core sends the source only Action/Select/Activity/Rebuild/Repair.
- Open as stated: orphans still go to `DocDirectory::of_app` (the first document of the app); dormant
  documents now drop them, so the cost is a wrong-document guess on an awake tab.
