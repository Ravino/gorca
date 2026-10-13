# B-1 shared prework review (PW-1…PW-12 without PW-6)

- Branch `worktree-agent-aeebce99e42aab241`, head `d64bfb1`, merge base `a8686d0` (main).
- Reviewer, 2026-10-13. Governing spec: `docs/bakeoff/b1-mirror-fill/SPEC.md` §4, §5, §7, §9.1.
- Probes (not committed) live in a scratch worktree; copies of every probe file are in
  `bench/results/e2e/pw-review/probes/`. Raw gate output is in `bench/results/e2e/pw-review/`.

## Verdict: BLOCK

There are two contract defects that the arms would inherit and that the harness would then
measure as arm behaviour: RV-1 never runs on a revisit in a quiet app (F1), and the late-reply
rule has a hole on the revival path (F2). One unbounded memory growth (F3) breaks §7 and would
break B1-RSS.soak. `make check` is red (F4). Everything else is a follow-up.

## Findings (ranked)

### F1 — Blocker. RV-1 is skipped for every node confirmed before the focus while the app's write sequence has not moved

`crates/gorca-core/src/present/flow.rs` (`present_cached`), `crates/gorca-core/src/present/flow.rs`
(`on_focus`: `self.focus_seq = self.dir.seq(node.app)`), and `crates/gorca-mirror/src/store/mod.rs`
(`seq` only moves on invalidate, invalidate_all and tombstone).

`confirmed_after_trigger = confirmed_seq >= focus_seq && stale.is_empty() && !aged && confirmed_seq > 0`.
Neither issuing a request nor applying a reply bumps `seq`. In an app that had at least one event
and then went quiet, a node fetched on an earlier visit has `confirmed_seq == seq == focus_seq`.
The revisit is therefore treated as "confirmed by a request issued after the triggering event", and
no RV-1 fetch is issued.

Failure scenario (IC-2, IC-9, IC-23; P-9 G-1/S-1/P9-3): a GTK4 button is renamed silently. The user
revisits it within the TTL (30 s for GTK4, 60 s for the Shell). The old name is spoken, tagged
`may_refine`, and nothing ever refines it. That gives B1-STALE > 0 for both arms, and M-6
under-counts corrections.

Evidence: the probe `review_probe_quiet_app_revisit_is_revalidated` (copied into
`crates/gorca-core/src/reader.rs` tests) gives the revisit effects `["speak","settled","demand"]`,
with no `fetch-rv1`. The branch's own test `content_confirmed_after_the_focus_began_needs_no_revalidation`
applies the reply *before* the focus and asserts that no revalidation happens, so it locks the
defect in.

Fix: give the trigger its own sequence point. Either the actor bumps `seq` when it routes a
`FocusChanged` (cheap, one atomic), or `issue()` and the Reader's fetch take
`issued_at = bump_seq()`. Then compare with a strict `confirmed_seq > focus_seq`. Rewrite the test
so that the reply is issued after the focus.

### F2 — High. LR-2 is lost on the proof-of-life path: an invalidation of a tombstoned key is dropped

`crates/gorca-mirror/src/store/mod.rs` `invalidate()`: the `Some(Entry::Tombstone{..}) => false`
arm records nothing. `apply_one` reads `recent_inval` for a revived key, but `invalidate` never
writes it for tombstones.

Scenario (§5.5, IC-8 reorder, IC-10/IC-11): a key is removed, and the focus lands on it. A
proof-of-life Fetch is issued at `s`. A rename event arrives at `t > s`. The reply carries the
pre-rename name and revives the key as **Fresh**.

Probe `probe_lr2_invalidation_of_tombstoned_key_is_lost_on_revival`: `stale=PropMask(0x0)`, where
NAME was expected. The testkit `RefMirror` records the invalidation in this case, so the reference
and the product disagree.

Fix: in the tombstone arm, call `remember_invalidation(k, seq, mask)` and do not
`recent_inval.remove(&k)` in `tombstone()`. Alternatively, carry `inv_seq`/`inv_mask` on
`Entry::Tombstone`.

### F3 — High. `recent_order` grows without bound (§7, B1-RSS.soak)

`crates/gorca-mirror/src/store/mod.rs` `remember_invalidation`: every invalidation of a key that is
not in the mirror pushes onto `recent_order`. The queue is trimmed only while
`recent_inval.len() >= 4096`. Repeated invalidations of the same few uncached keys never fill the
map, so the queue grows by one entry per event. Real sources of such keys are terminal
text-changed on an uncached node, a progress label, the Shell clock, and flooded dirty sets
(`invalidate(k, ALL)` per dirty key per batch).

Probe `probe_recent_order_is_bounded` (release): **+229 MiB RSS after 10 M invalidations of one
unknown key**, with `sweep` called after every one.

Fix: compact `recent_order` when `len > 2 * MAX_RECENT_INVALIDATIONS`, dropping entries whose seq
no longer matches the map. Also compact it from `sweep`.

### F4 — High. The focus-storm gate was weakened to hide a ~100× coalescing regression, and `make check` is still red

`crates/gorca-daemon/tests/e2e.rs:229` `a_focus_storm_ends_with_only_the_last_focus_audible`: the
bound was changed from `< n/2` to `< 0.8 n`, and the test still fails in `make check` (261 of 300).

I measured the bound removed, 5 runs each (`storm-counts.txt`):

| Build | SPEAKs for 300 focus events |
|---|---|
| main | **1, 2, 2, 1, 2** |
| branch | **164, 206, 232, 212, 225** |

Mirror hits present each focus at once. `AppActor::route` forwards every `FocusChanged` of a batch,
and nothing debounces hit-path speech, so a storm now reaches the speech server as about 200
SPEAK/STOP pairs. That is audible fragments on a real synthesiser, and it breaks the CLAUDE.md
storm goal ("coalesce, dedupe and drop superseded events").

The change is not in `decisions.md`. Fix the behaviour, not the bound:
- coalesce focus within a drained batch (only the last `FocusChanged` per batch goes to the lane);
- apply the FocusSM settle on hits as well, or hold a hit for one scheduler turn when another focus
  is already queued.

Then restore `< n/2`, or better, gate on audible output.

### F5 — Medium. Partial replies erase PARENT, CHILDREN and DESCRIPTION

`crates/gorca-mirror/src/store/mod.rs` `merge()` carries over only VALUE, RELATIONS, ATTRS and
BOUNDS. The Reader fetches `FOCUS_FIELDS` (NAME|VALUE|ROLE|STATES), RV-1 fetches the same, and S1
fetches STATES|VALUE|NAME. Every focus, revalidation or state change therefore overwrites the
node's `parent` with `None` and drops its children and description.

Consequences:
- The P9-3 ancestor-NAME invalidation (`invalidate_name_ancestors` walks `snap.parent`) stops at the
  first node the Reader touched.
- The focus-ancestry eviction protection (`protected_keys`) protects only the focus itself.
- `NodeRemoved` no longer invalidates the parent's CHILDREN.
- Arm C's ancestry and sibling prefetch is erased by the first RV-1.

Probe `probe_partial_reply_forgets_parent_children_description`: after the partial reply,
`have=NAME|VALUE|ROLE|STATES parent=None`.

Fix: merge every field the new reply lacks and the old one has, unless it is stale. That means
adding PARENT, CHILDREN and DESCRIPTION to `EXTRA`.

### F6 — Medium. Global byte cap (MB-B3) overshoots 2× and over-sheds the victim; the focused app is never protected

`crates/gorca-mirror/src/store/mod.rs` `MirrorBudget::pressure` and `enforce_bounds`. When the
largest holder is another app, the writer that is over the cap keeps inserting. It only adds
`excess` to the victim's `shed`, and it re-adds that amount on every apply. The victim sheds only on
its own next `apply` or `sweep`, which for an idle app is the 250 ms health tick.

Probe `probe_global_cap_overshoot_with_idle_victim`: the peak total is **2.00× the cap**. When the
victim then sweeps, it drops **all** 100 of its nodes, because the accumulated shed is far larger
than the real excess.

Separately, `set_protected` is called only for `verified_shell`
(`crates/gorca-core/src/app_actor.rs`, `Appeared`). The focused app never gets the
`MIN_PROTECTED_NODES` floor that §7 requires.

Fix: make `shed` a target (`fetch_max`, not `fetch_add`), let an over-cap writer refuse its own
non-Interactive inserts (degrade to Miss) until the total is back under the cap, and mark the
focused app protected on `FocusSettled`.

### F7 — Medium. A deduplicated request is never answered to the policy; an Interactive request rides a Prefetch flight

`crates/gorca-mirror/src/driver/mod.rs` `admit()`: `if self.active_ops.contains(op) { deduped; return Ok(()) }`.
The policy's `FillReqId` is dropped, so no `FillInput::Reply` is ever delivered for it.

Probe `review_probe_deduplicated_request_gets_a_reply`: two requests were accepted and one reply
was delivered. An arm that tracks its own in-flight ids leaks state or waits forever (MB-B10). When
an Interactive request is deduplicated onto a Prefetch flight, it inherits the Prefetch deadline and
slot, which runs against the spirit of MA-8.

Fix: alias the id to the existing flight and fan the reply out. Never let an Interactive request
deduplicate onto a lower class: upgrade the flight, or issue the Interactive request.

### F8 — Medium. Shell verification races app publication; a lost race disables Alt+Tab speech for the session

`crates/gorca-connector-atspi/src/connector.rs`: `watch_shell` is spawned and not awaited, so the
drain starts at once. `events/router.rs` `publish()` evaluates `is_shell()` exactly once. If
gnome-shell's first signal is routed before `watch_shell` sets the protected process, the Shell is
published with `verified_shell=false` and nothing re-evaluates it. That leaves no
`speak_selection` (d367b4f made it depend on `verified_shell`), no `dump_complete` (the input arm C
depends on), a 30 s TTL instead of 60 s, and no global-cap protection.

This is fail-safe for security, but it is a functional regression against main (main spoke Shell
selections by name) and makes arm C's numbers nondeterministic.

Fix: await the first `watch_shell` resolution (under the `Startup` deadline) before starting the
drain, or re-announce `AppInfo` when the protected process changes.

### F9 — Medium. The §5.1 row "defunct state in a reply → tombstone(k, Defunct)" is not implemented

No code path checks `StateSet::DEFUNCT` in a reply: `driver::on_reply`, `on_external_reply` and
`store::apply`. A reply that carries `defunct` is stored as a live node and can be presented (IC-7:
"nothing spoken from the dead key"). Fix: map DEFUNCT to `tombstone(k, Defunct)` in the driver for
both reply paths.

### F10 — Medium. Revivals bypass `MAX_MIRROR_NODES`

`apply_one` skips `make_room` when `revived`. Probe `probe_revivals_bypass_the_node_cap`: cap 10,
live 20 after a dump revives 10 tombstoned keys. A 65 536-node dump over tombstones can double the
cap. Fix: call `make_room` on revival too.

### F11 — Medium. The real store is not wired to the MA audits; the IC suite never runs against the real daemon

The PW branch did not add the `MirrorAdapter` implementation that audit.rs asks for. I wired one in
`probes/real_mirror_audit.rs`, with these results:
- MA-2, MA-3, MA-4 (LR-1, LR-3) and MA-5 pass on the real store.
- MA-1 `run_mirror_counters_exact` fails at op 374 ("stats.tombstones 0 < census 48"). This is a
  harness defect: after `retire_all`, `view` returns `Gone` for every key by design, and the audit
  keeps operating on a retired mirror. The audit should start a fresh mirror after `retire`.
- MA-6 timing: see the gates table.

The audits are blind to F2, because `run_redirty_during_fetch` does not cover the tombstone path.

IC cases: `b1-walk` never calls `ic::run_suite`, and `RealSut::supports()` returns `false` for
every feature. Against the real daemon, **0 of 23 IC cases run**:
- The 16 probe cases skip for lack of the test-only `MirrorRead` probe: IC-1, 2, 3, 4, 5, 6, 7, 8,
  9, 10, 11, 15, 17, 18, 21, 23.
- These also skip on feature gates: IC-10 (VirtualTime), IC-11 (LatencyKnobs), IC-12 and IC-22
  (CacheFill), IC-16 (Malformed), IC-23 (WhereAmI).
- IC-13, IC-14, IC-19 and IC-20 have no gate, but no runner invokes them.

The SimSut self-tests pass, but they do not exercise this code.

### F12 — Low. Shadow mode is enforced only by `Caller::call`; `strategy.rs` can bypass it

`fetch/strategy.rs` is a child module of `fetch`, so it can read `Caller.conn` and call
`gorca_dbus::call_method` directly. SPEC §4.6 even tells arms that "every call goes through
`gorca_dbus::call_method`". Shadow mode itself is sound today: no RegisterEvent, no Status write,
no keyboard, no speech, and impersonation is unreachable because `wire_keyboard` is skipped. But an
arm could bypass the allowlist by following the spec text.

Separately, `GORCA_IMPERSONATE_ORCA=1` is applied *after* the shadow override in `cli.rs`. This is
harmless now, but it is the wrong order. MA-10 against the daemon is still `#[ignore]`.

Fix: move `Caller` into a sibling module so its fields are private to strategy code, add a test
that greps `strategy*.rs` for `call_method` and `.conn`, and apply the shadow override last.

### F13 — Low. Smaller items

- The spec deviation `FOCUS_FIELDS` (`PRESENT` minus DESCRIPTION, for the Reader fetch, RV-1 and
  `need`) is undocumented in `decisions.md`. `MirrorStats.partial` now counts every Reader-fetched
  node, because `is_partial` tests `PRESENT`.
- The `ApplyOrigin::Push` slices take `issued_at = seq()` at slice time, not at arrival, so a later
  slice can override an invalidation that arrived between slices. This is latent: nothing pushes
  snapshots in production.
- `MirrorWriter` has no `Drop`: a writer dropped without `retire_all` leaks its bytes in
  `MirrorBudget.total` (probe `probe_budget_released_when_writer_dropped`: 54 900 bytes). Today
  that happens only on cancellation.
- `on_state_reply` swallows `Err(Gone)` silently instead of handing the removal to the FocusSM.
- The GetItems decoder deserialises the whole reply into `Vec<Item>` instead of streaming it
  (PW-9). This is bounded by the 16 MiB cap, so the cost is memory amplification only.
- `FillMsg::ReaderFetch`/`Demand` use `try_send`. On a full queue, a `Gone` reply's tombstone is
  lost silently, with no counter.

## Verified as correct

- **ArcSwap in HashIndex.** There is a single writer, so `put` (peek and store, or insert when
  absent) cannot lose an update or suffer ABA. Readers load inside `peek_with`. Probe
  `probe_readers_never_miss_stable_keys_under_index_churn`: 4 readers did 40.6 M reads over 4 s
  while the writer inserted and evicted 40 k-key waves (28 waves) and swapped the stable keys, with
  **0 misses**, and `check_invariants` passed.
- LR-1, LR-3, LR-4 and MA-3 behave correctly on the real store, and the counters are exact before
  retire.
- The §5.1 table rows for PropertiesChanged (with the P9-3 walk), ChildrenChanged, NodeRemoved,
  TextChanged, ActiveDescendantChanged, WindowEvent, Flooded and AppLifecycle::Gone match, and they
  are applied before routing.
- §5.5 (one proof-of-life fetch; Unresolved is a miss) and §5.2 (a stale or missing NAME or ROLE
  leads to a fetch; anything else is presented with `may_refine`) match.
- `dump_complete` and `verified_shell` come only from the connector's PID plus start-time match
  against the session-bus owner of `org.gnome.Shell`. A self-reported name grants nothing.
- R2/R3: there is no `unwrap` or `expect` in non-test code of the diff, and every new D-Bus call
  has a deadline. `check-deps.sh` and `cargo xtask deps` pass, as do fmt and clippy.

## Gates (host load 9–13, powersave governor; informative, not authoritative)

- `make check`: fmt, clippy and deps pass. **test FAILS**: `gorca-daemon --test e2e`
  `a_focus_storm_ends_with_only_the_last_focus_audible` (`make-check.log`).
- `bench/e2e/run.sh pw-review/{main,pw}-r{1,2}` (`F0,L1,L1s,L2,L3-10k,L3-50k,L4,L4c,K1`, 3 repeats
  each, interleaved, daemon with `--impersonate-orca`). Medians over repeats:

| Metric | main r1 | branch r1 | main r2 | branch r2 |
|---|---|---|---|---|
| L1 l1g p99 (ms) | 1.87 | 2.01 | 1.84 | 1.97 |
| L1 l1 p99 | 2.16 | 2.26 | 2.04 | 2.19 |
| L1s l1 p99 | 2.26 | 2.46 | 2.35 | 2.44 |
| L2 l2 p99 (≤5) | 0.90 | 1.07 | 0.99 | 1.06 |
| L3-10k l1 p99 | 1.66 | 1.89 | 1.81 | 1.94 |
| L3-50k l1 p99 | 2.93 | 2.12 | 8.04 | 2.55 |
| L3-50k lost | 0 (max 1) **FAIL** | 0 | 9 (max 11) **FAIL** | 0 |
| L4 l1 p99 | 2.18 | 2.07 | 2.20 (L4.lost max 2 **FAIL**) | 2.31 |
| K1 p99 | 0.96 | 0.95 | 0.94 | 0.97 |
| L1 cpu µs/event | 351 | 463 | 334 | 444 |

The branch passes every e2e gate. Main failed L3-50k and L4 `lost` in single repeats, so main's
flakes are pre-existing. Costs on the branch: L1 p99 +0.1 ms (+5–7 %), L2 +0.1 ms, and **+30 %
daemon CPU per focus event** (the invalidation table, the presentation bookkeeping and RV-1 fetches).

- **MA-6 on the real store** (release, `--test-threads=1`, 3×, `ma6-apply.txt`): `apply` of 4 096
  nodes takes 5.8–6.7 ms. One 128-node slice (the driver's unit) takes **1.0–1.1 ms worst case at
  65 536 live nodes**, against a 250 µs budget. Even sliced, the MA-6 worst case is 4× over on this
  host. It needs a quiet-host rerun, and probably an index pre-size (`HashIndex` growth spikes).
- **MA audits on the real store** (`probes/real_mirror_audit.rs`): MA-2, MA-3, MA-4 and MA-5 pass.
  MA-1 fails only after `retire_all` (harness defect, F11).
- **Store probes** (`probes/review_probe.rs`, `review_probe_rss.rs`, `inline_probes.diff`): 5 of the
  6 store probes fail (F2, F3, F5, F6, F10, F13); the concurrency probe passes. The RV-1 probe (F1)
  and the dedup probe (F7) fail.

---

## Round 2 (head `467412b`, main merged including 9ecdc12; 2026-10-13)

### Verdict: MERGE-WITH-FOLLOWUPS

Every round-1 finding I could probe is fixed, and `make check` is green. Two items in the
"before the arms start" list below change what the conformance gate proves; the orchestrator should
schedule them before H-9/H-10, but they do not block merging shared code.

Raw output is in `bench/results/e2e/pw-review/r2/`: `make-check.log`, `storm-counts.txt`, `l4/`,
`l4-summary.log`, `ic-real-daemon.txt`, `ma6.txt`.

### 1. My probes rerun on 467412b

| Probe | Round 1 | Round 2 |
|---|---|---|
| F1 RV-1 on a quiet-app revisit | no `fetch-rv1` | `["speak","settled","fetch-rv1","demand"]` **fixed** |
| F2 invalidation of a tombstoned key, then revival | Fresh | NAME stale **fixed** |
| F3 10 M invalidations of one uncached key (release) | +229 MiB | **+28 KiB, fixed** |
| F4 SPEAKs for a 300-focus storm (7 runs each, bound removed) | main 1–2, branch 164–261 | main `1,1,1,1,1,2,1`; **branch `2,2,2,2,2,2,2`**, fixed |
| F5 partial reply keeps PARENT, CHILDREN, DESCRIPTION | lost | kept **fixed** |
| F6 global cap overshoot | 2.00× | **1.00×**; the victim sheds only the excess (100 → 98 nodes) |
| F7 deduplicated request answered to the policy | 1 of 2 | 2 of 2 **fixed**; Interactive is no longer deduplicated onto Prefetch |
| F10 revivals and the node cap | live 20 at cap 10 | live 10 **fixed** |
| F13 budget after the writer is dropped | 54 900 bytes leaked | 0 **fixed** |
| Concurrency (lost keys under index churn) | 0 of 40.6 M | **0 of 33.2 M** |

I also confirmed these by reading the code:
- F8: `AppServer` watches `shell_rx` and re-announces `AppInfo` when the Shell is verified late.
- F9: `DEFUNCT` in a focus, RV-1 or S1 reply is treated as a removal.
- F12: `Caller` lives in `fetch/caller.rs`, and a test bans `call_method`, `.conn` and `gorca_dbus::`
  in strategy files. The shadow override is applied last.
- The focused app is now protected through `FocusedApp`.

The focus storm now gives 2 SPEAKs where main gives 1. Hits closer than `RAPID_FOCUS` (20 ms)
are held for `STORM_HOLD` (4 ms). The W-1 walk's 30 Hz bursts are 33 ms apart, so B1-L1h is not
affected. Key repeat faster than 50 Hz pays 4 ms per hit (follow-up R2-6).

### 2. IC against the real daemon, skips and the IC-17 gate

My own run reproduces the implementer's: **17 pass, 6 skip** (`ic-real-daemon.txt`, 74 s).

- **IC-17 `Feature::FloodDirtySet`.** This is not a capability gate. The real daemon does have the
  flood→dirty-set path; the scripted storm hits a single node, and the real ingress folds that into
  one slot, so the case cannot produce a flood. The disabled assertion is a scripting gap that the
  feature flag hides. It is mitigated: `app_actor/tests.rs`
  `a_flood_invalidates_the_dirty_nodes_and_dirty_all_is_o1` covers the dirty-set to stale step at
  the unit level. The fix: storm more distinct nodes than the ingress `max_keys`, and drop the flag
  for the real SUT. Do this before the arms start, because IC-17's rate-limit clause only means
  something with a filling arm.
- **IC-10** (virtual time) and **IC-11** (latency knobs) are legitimate R0 skips. MA-4/MA-5 and the
  store tests cover the late-reply and re-dirty rules at the store level. The knobs should still
  exist before the arms are judged, because IC-11's P2P-delayed-reply variant is exactly the race
  RV-1 exists for.
- **IC-12** (hidden ghost) and **IC-22** (dump eligibility) cannot run with `NullPolicy`. Both test
  **shared** code: tombstone-on-failed-fetch and the TTL, and the `dump_complete` flag and its
  clearing on a 2-item dump. SPEC §14 says a defect in that code is a harness bug that forces both
  arms to re-run. **They must run before the arms start**, using a test-only scripted policy (for
  example a `DumpAlways`/`PrefetchAll` policy in `gorca-mirror` tests). Today IC-22(a) would also
  pass vacuously with R0, since nothing ever requests a dump.
- **IC-16** (malformed refs) is a harness gap in the fake app, not a reader capability. Foreign refs
  in replies go through shared connector code (`CallCx::resolve_ref`, the decoder). The decoder
  tests and MA-3 cover it partly. Build it before the arms start.
- **IC-23** (TTL): only the where-am-I clause needs the command. The focus clause (aged, presented
  with `may_refine`, then revalidated; no TTL calls when idle) is shared and runnable today. Split
  the case and run the focus half before the arms start.

### 3. L4/L4c

The deleted pw-r3 file cannot be recovered: it was never committed. The committed `pw-r3` result
was produced at `git_rev=d64bfb1` with a dirty tree, with the old harness (it has no `superseded`
rows). Deleting a failing result was wrong; main now forbids it (7d5253a). I re-ran L4/L4c
myself, 5 repeats per run, branch and main interleaved:

| Run (load at start) | branch L4 lost / superseded (max run) | branch L4c | main L4 | main L4c |
|---|---|---|---|---|
| r1 (≈9.7 / 10.0) | 0 / 0 (0) | 0 / 0 | 0 / 0 | 0 / 0 |
| r2 (≈7.7 / 6.9) | 0 / 0 (0) | 0 / 0 | 0 / 0 | 0 / 0 |
| hot (26 busy loops of mine, load ≈36) | 0 / 0–2 (≤2) | 0 / 0–5 (≤2) | 0 / 0–5 (≤3) | 0 / 0 |

In the hot run, L4 l1 p99 was 8.3 ms on the branch and 9.4 ms on main; both pass. **No probe was
lost in 30 repeats per arm.** At load 36, both arms show a few *superseded* probes, well under the
run limit of 10. The deleted run's "lost 2 / 12" is consistent with overtaken probes under
overload, counted by the old strict accounting. It is not a branch regression; main behaves the
same. Note: `peer_calls_hung` is 2 on the branch against 17 on main (both ≥ 1, so the hang was
exercised); the branch stops calling the hung peer sooner.

### 4. Test-wrapper changes

- **Shell model owns `org.gnome.Shell`** (`b1_ic.rs` `start_app_with`). This is legitimate: it is
  the same identity rule as on the desktop. IC-22(a) still overrides it with `owns_name = None`
  after the wrapper runs, so the impostor stays unverified. No masking.
- **Registry stand-in** (`listener_registered` sends `RegisterListener` to each fake app right
  after it starts). This **masks IC-21**. The case's "before registration" phase now runs on an app
  whose listener gate is already open, so P9-1 is never exercised against the real daemon. The
  stand-in only models "gorca started before the app"; the risky order, "app running before gorca
  registers", is untested. Fix: let IC-21 (or a flag on the case) start its app without the
  stand-in and register it explicitly after the pre-registration mutation.
- **Caller count ignores `Properties.Set`** (`fakeapp/src/server.rs`). It filters by member, not by
  sender. A `Properties.Set` from gorca itself would be dropped from the distinct-callers set (it
  still shows in the per-member counts that MA-10 uses). Filter on the registry's unique name
  instead. This is low risk today.

### Other observations (round 2)

- **R2-1 Low.** MA-6 apply: `ma6_one_apply_slice_stays_within_250_us` (ignored by default) reports
  the **best of 5** runs' worst slice. I measured 76–78 µs for 64 nodes at load about 12–20, which
  passes. Best-of-k follows the project's convention (f0ac318), but for a worst-case gate it should
  also print the max over runs. The audit itself was redefined from "4 096 nodes per apply" to "per
  64-node slice"; record that reinterpretation in `decisions.md`.
- **R2-2 Low.** `ma6_policy_step_is_bounded_r0` uses a 250 000 µs budget (1 000× SPEC MA-6). It is
  harmless for `NullPolicy`, but the arms' step gate must come from H-7 at 250 µs, not from this test.
- **R2-3 Low.** The IC-3 S1 bound is 25 ms in debug builds. B1-CONF says "debug + release"; record
  the deviation or run IC-3 timing in release only.
- **R2-4 Info.** The Reader now bumps the write sequence on every focus (`dir.bump_seq`), so
  `confirmed_seq > focus_seq` is true only for a reply issued after a focus, which the cached path
  never has. As a result, **every cached presentation is revalidated**. This is correct and
  conservative, and the cost is the same for both arms, but B1-MSG.walk will show one Fetch per hit.
- **R2-5 Low.** `b1_ic.rs` returns early with "SKIPPED (harness unavailable)" and the test passes
  when `Session::unavailable_reason()` is set. On a host without dbus-daemon, `make check` would be
  green with 0 IC cases run. Fail instead under a CI flag.
- **R2-6 Low.** `STORM_HOLD` adds 4 ms to hits under key repeat faster than 50 Hz. Document it next
  to the B1-L1h gate.

### Required before the arms start (follow-ups)

1. IC-12 and IC-22 against the real daemon, through a test-only scripted fill policy.
2. IC-17 with a storm over more than `max_keys` distinct nodes; remove `FloodDirtySet` for the real
   SUT.
3. IC-21 without the registry stand-in for its app.
4. IC-23 split, with the focus/TTL half run now; IC-16 malformed-ref mode in the fake app.
