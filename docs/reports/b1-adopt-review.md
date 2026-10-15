# Review: B-1 adoption (focus-window prefetch as default fill policy)

Date: 2026-10-15. Branch `worktree-agent-a259b4477ef251616`, head `df20f8c`, against `main` 932d611.
Inputs: `docs/reports/2026-10-14-b1-judgement.md` (main), `docs/reports/b1-adopt.md` (branch).

**Verdict: MERGE-AFTER-FIXES.** The policy is well bounded and its numbers check out. The
eviction-protection fix (`on_revalidated`) is correct but incomplete. With prefetch, about 85 % of
focuses are presented from the cache, so the remaining paths where a focus never earns protection
are now the common path. F-1 needs a small fix before this becomes the default. The rest are
follow-ups.

## Gates (run by the reviewer on df20f8c)

| Gate | Result | Load (1-min) |
|---|---|---|
| `cargo fmt --all -- --check` | clean | 5.08 |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | 5.72 |
| `cargo test --workspace` | 1437 passed, 0 failed, 15 ignored | 6.21 at end |

Log: `bench/results/b1-adopt-review/gates.log`. The pass count matches the implementer's 1437.

Probes: `bench/results/b1-adopt-review/probes.diff`. Apply it to a clean checkout of df20f8c.
Every probe passes, and each pass confirms a finding:
* `gorca-core` lib: `probe_timeout_fallback_on_a_prefetched_focus_never_earns_protection`, `probe_same_app_cached_move_demotes_and_rv1_timeout_never_restores`;
* `gorca-mirror --test prefetch_behaviour`: `probe_backward_walk_gets_no_lookahead`, `probe_children_changed_during_inflight_window_is_lost`.

## Findings (ranked)

### F-1 High: the protection fix covers only the RV-1 success path; three sibling paths still never earn protection
`crates/gorca-core/src/present/flow.rs:362-371` (demote at presentation), `:446` (Timeout fallback
with `revalidate=false`), `:478-485` (fix), `:542-549` (RV-1 error arm does nothing).
Protection (`FocusedApp::protects`) controls three things:
* text events (`app_actor.rs:261`, `wants_text_events`);
* mirror shedding (`app_actor.rs:292`, `set_protected`);
* registration eviction (`events/router.rs:312`, `:722`).

Three paths still leave the focused app unprotected:
1. **Focus fetch times out after a prefetch filled the node.** The focus is a miss, so a Focus
   fetch goes out. The Siblings reply of a neighbouring window lands first, and then the fetch
   times out (a slow app, which is exactly when timeouts happen). The fallback
   `present_cached(.., revalidate=false)` presents a node without `FOCUSED`, demotes, and asks for
   no RV-1. Protection is never earned until the next focus move. The probe shows effects
   `["speak","settled"]` and `published=None`.
2. **RV-1 returns Timeout, Busy or Protocol.** The `Err(..) => {}` arm leaves the app demoted
   until the next focus. The probe confirms it.
3. **Every cached move inside the protected app demotes it until RV-1 answers.** The probe shows
   the cold-focused app 1 losing protection when the focus moves to its prefetched sibling. In
   that window (one RV-1 round trip, up to 120 ms on a busy app) the app's caret events are
   dropped silently (`wants_text_events` false, not counted). Its mirror may be shed, and its
   connection may be evicted under the registration cap.

The `confirmed_after_trigger` skip is a fourth path, which the implementer acknowledged. On
`main` the same holes existed for revisits only (43 % hit), and `main` never earned on RV-1 at
all. The branch improves on `main`, but it makes cached presentation the dominant path.

Failure scenario: Firefox is busy loading. The user Tabs into a prefetched text field, RV-1 times
out, and the user's typing and caret movement in that field are silent until they leave it.

Fix, small:
* (a) do not demote when the cached node's app is the app already published (cross-app fail-closed is kept);
* (b) on an RV-1 or Focus error with a cached presentation, issue one more `FetchKind::Revalidate` with a fresh deadline (as `focus_retried` does), or earn from a later `StateChanged` that carries `FOCUSED`.

Add the probes above as regression tests.

### F-2 Medium: a GetChildren of the parent on every window and look-ahead; a hostile or huge list can amplify
`crates/gorca-connector-atspi/src/fetch/strategy.rs` (`siblings`) together with
`fetch/caller.rs:261-285` (`children_of`). Each `Siblings` op, including the per-step look-ahead,
issues one `GetChildren` of the whole parent. `children_of` deserialises the full reply, up to
16 MiB (`MAX_DUMP_BYTES`), before `take(4096)`.

Before this branch only `Subtree` (Background, off the keypress path) called it. Now it runs about
once per focus step, and `can()` (`prefetch.rs:259-269`) still admits Prefetch when the app is
`Flooded`.

Failure scenario: a hostile app floods `children-changed` on the focus parent, with a child list
of about 300 k refs. The policy refetches the window every `MIN_REFETCH` (300 ms), with 3
concurrent Prefetch ops. That is about 3 x 16 MiB of zbus deserialisation per 300 ms in the
reader process: transient tens of MB and CPU per op. On a benign 10 k-row GTK3 tree, every arrow
key serialises the whole list on the app's main loop ahead of the next Interactive call.

The implementer flagged this (report section 7). It is unmeasured. Fix:
* when `ChildCount > 512`, use `GetIndexInParent` plus indexed `GetChildAtIndex` (A's `child_refs` path);
* lower the byte cap for this op (4096 refs is about 300 KiB);
* do not refetch dirty runs while `Flooded`.

### F-3 Medium: the look-ahead covers only the forward direction
`prefetch.rs:525` iterates `idx+1..=idx+LOOK`. With `EDGE_BEHIND = 1` and `BEFORE = 1`, a walk
upwards gets no re-read of the rows it reaches next. The probe confirms it: row 21 is re-read,
the silently renamed row 19 is not.

The harness walk (`gorca-testkit/src/b1/walk.rs`, sidebar, rows and switches) only ever increases
the index. So M6 = 0.019 is a forward-only number, and an Up-arrow walk through recycled GTK4 rows
falls back to R0-like behaviour (spoken, then corrected by RV-1). Fix:
* keep the last step's direction and look ahead in that direction;
* add a reverse leg to W-1.

### F-4 Low: a ChildrenChanged while the first window is in flight is lost by the policy
`prefetch.rs:330-333` (`note_dirty` ignores a parent without a run) and `:665-676`
(`install_run` sets `dirty: false`). A reply computed before the change installs a clean run with
the wrong key order, and the probe shows no refetch afterwards. The mirror stays correct (the
LR-1..3 issued_at rules apply to Siblings replies too). The cost is a stale run: misses and a
wasted window when the inserted row is reached. Fix: remember the dirty parent for any `Siblings`
op in `out` and install that run dirty.

### F-5 Low: `memory_bytes` leaves out `Run.stamps`
`prefetch.rs:983-996` counts `keys` capacity but not `stamps` (16 B per entry, larger than the
keys). The state is still hard-bounded (48 x 96), so this is accounting only.

### F-6 Low: the policy's own bookkeeping depends on the driver echoing the exact op
`out.retain(|o| &o.op != op)` (`prefetch.rs:763`) and `after == EXTEND` (`:839`) both assume the
driver echoes the op unclamped. The driver clamp (`driver/mod.rs:587-588`) is a no-op at
`MAX_CHILD_RANGE = 1024`. A smaller configured range would leak `out` entries until the TTL and
misclassify extensions. Use a request id instead.

## Verified clean
* **Bounded state.** `runs` <= 48, `member` <= 48 x 96 (every insert is paired with a removal in
  `drop_run` and `grow_run`), `out` <= 8, rings <= 8, `chain` <= 24, `dirty_parents` <= 4. No
  recursion anywhere in the new code: `ancestry` and `subtree` are iterative and have `seen` cycle
  guards. No `unwrap`, `expect` or `panic` in non-test code.
* **Concurrency.** `map_concurrent` keeps at most `conc` tasks in a `JoinSet` owned by the op
  future, so dropping it aborts them. Every call carries the op's `Deadline`. The per-app
  semaphore is 8, with Prefetch leaving 2 for Interactive and the policy capping itself at 3
  Prefetch. Effective worst case: 3 ops x 3 children x ~6 pipelined calls on one app's serial
  loop. The bound is real, but it is up to about 54 calls queued ahead of an Interactive call on
  real GTK (unmeasured; the fake app shows L1c at 1.07-1.08x R0).
* **Health gating.** No requests to `Hung` or `Gone` apps, only Interactive when `Slow`, and
  backoff after Timeout or Failed. Shell dump: off unless `--shell-cache-dump`; refuted by
  `!dump_complete`.
* **Numbers.** `bench/results/b1-mirror-fill/adopt-final{,2}/*.txt` match the report: HIT.all 0.853
  vs 0.432, L1c p99 3.62 vs 3.35, M6 0.019, STALE, LOST and L3.lost 0, MSG.idle 0. The counting
  metrics are identical across the two sessions because the walk is deterministic; the run files
  differ (checked by md5). `adopt-final2` started at load 9.5, and its slots ran at load 0.9-9.3.
  Fidelity passed.

## Re-soak
The soak (`adopt-soak-p`, RSS +0.52 MiB, STALE 0) predates the Reader fix. The fix changes only
which app is protected, which changes shedding. Policy memory is capped at about 80 KB, so the RSS
gate is not at risk. **The re-soak can follow merge**, but run it once after F-1 is fixed, because
F-1 changes protection again.
