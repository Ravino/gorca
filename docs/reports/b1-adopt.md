# B-1 adoption: the focus-window prefetch becomes the default fill policy

Date: 2026-10-14/15. Decision: `docs/decisions.md` (B-1 entry of 2026-10-14), judgement
`docs/reports/2026-10-14-b1-judgement.md` (arm C won 65-62; adopt C's focus-window prefetch with
A's `child_refs`, bounded concurrency and one-round ancestry; drop the 2,048-node crawl and the
8 s refresh; take the child index from the `GetChildren` reply; Shell dump off by default).
Code was ported, not merged; arm branches were only read.

## 1. What shipped

| Piece | Where |
|---|---|
| Product policy `FocusPrefetch` (+ `PrefetchConfig { shell_dump }`) | `crates/gorca-mirror/src/fill/prefetch.rs` |
| `Candidate` is now an alias of it (harness hook unchanged) | `fill/candidate.rs` |
| New op `FillOp::Siblings` / `BackendOp::Siblings` (index from the `GetChildren` reply) | `fill/mod.rs`, `gorca-connector-api`, `runtime.rs`, driver clamp + cap + `target()` = parent |
| Connector strategy: A's `map_concurrent` (JoinSet owned by the op future), `child_refs`, one-round `ancestry`, plus C's no-`Parent`-call children walk, `siblings`, subtree cycle guard | `gorca-connector-atspi/src/fetch/strategy.rs` |
| Daemon default = prefetch; `--bench-fill-policy=r0|prefetch|candidate`; `--shell-cache-dump` (off) | `gorca-daemon/src/{cli,lib}.rs` |
| Reader fix found by M3 (section 4) | `gorca-core/src/present/flow.rs` |
| Harness: soak of fixed duration (`execute_for`), `B1_SOAK_S` passed through, `b1-fakeapp` built by `run-all.sh`, IC-17 policy-neutral | `gorca-testkit`, `bench/b1/` |

### Policy in one paragraph
On `FocusSettled{n}`: if the chain to the window is not mirrored, one `Ancestry` from its first
unmirrored level (once per level, 3 s gap). Then a *run*: `Siblings{parent, n, before 1, after 6}`;
the connector finds `n` in the one `GetChildren` reply (no probing) and fetches the window with a
bounded 3-wide concurrency and no `Parent` call per child. Near a run's open edge (2 / 3 entries)
the run grows by 6. The 3 siblings ahead of the focus are re-read when older than 500 ms or when
their name/role went stale (look-ahead). A `ChildrenChanged` on a parent with a run dirties it:
the focus run is refetched after 60 ms (>= 300 ms apart), another parent's run is dropped and the
parent re-read once. Window activation refreshes the focus run if older than 2 s. No Subtree, no
crawl of other apps, no age-based window refresh, no timers when idle, own requests bounded (3
Prefetch outstanding, TTL = 2x the class deadline instead of 3 s). State: <= 48 runs of <= 96 keys.

## 2. Method
Private bus per slot (`tests/harness/run-isolated.sh`), arms interleaved and rotated by the
runner. R0 = `main` 9d44caa built unchanged in its own tree (a copy in the scratchpad), policy
`r0`, arm label `r0`; the new tree is arm `p`. `taskset -c 2-9`, governor `performance`, C2/C3
disabled (`quiet-host.sh status`). Strict fidelity (H-10(a)) **passed** in every session of the
final runs. Load average per slot is in each `*.log` (`[k/N] arm kind #r (load ...)`); the final
sessions ran at load 0.9-9.5 (the other agents were mostly idle; session starts: 4.7 and 9.5).
`B1-L4.announce` has no value for either arm (known harness gap, fails R0 identically).

## 3. Results

### Final (policy v6 + Reader fix), median of repeats, R0 beside it

| Gate / metric | limit / target | R0 | new | note |
|---|---|---|---|---|
| HIT.all | close to C's 0.896 | 0.432 | **0.853** (0.852-0.854) | C: 0.896 |
| calls per focus (all apps, walk) | <= 25 (goal) | 6.44 | **26.4** (26.38-26.48) | C: 103; tradeoff in section 5 |
| L1c p99 ms | <= 15 and <= 1.2x R0 | 3.35 (3.27-3.42) | 3.62 (3.57-3.64) = 1.08x | `adopt-final`; `adopt-final2`: 3.49 vs 3.73 = 1.07x |
| M6 / M6.audible | low / < 1 % | 0.266 / 0 | **0.019** / 0 | C: 0.013 |
| STALE@walk, LOST@walk, L3.lost | 0 | 0 | 0 | worst of 3 repeats |
| HIT.ancestry | 1 | 1 | 1 | |
| HIT.revisit | >= 0.95 | 0.996 | 1.000 | |
| L1h p99 ms | <= 2 | 0.38 | 0.40 | |
| S1 p99 ms | <= 5 | 2.31 | 2.67 | |
| L4 p99 ms | <= 15 | 3.01 | 2.66 | |
| FF p99 worst model (shell) ms | <= 15 | 0.97 | 0.96 | no tail (C: 3.69 pooled) |
| MSG.walk msgs/s | info | 31.3 | 128.5 | C: 505 |
| MSG.idle, MSG.l3 | gates | 0, 0.013 | 0, 0.013 | |
| CPU per focus us | info | 7 080 | 7 741 | +9 % (C: +33 %) |
| RSS.big MiB | <= 128 | 1.81 | 3.21 | C: 8.29 |
| STEP.max us (median of 7) | <= 250 | 14.5 | 26.5 | |

Raw: `bench/results/b1-mirror-fill/adopt-final/` (walk 3, ff 5, idle 3, big 3, step 7; 42 slots,
2 h 10 min) and `adopt-final2/` (walk 3, ff 3, idle 1, big 1, step 3; repeated after the Reader
fix of section 4). Logs: `bench/results/b1-adopt/adopt-final*.log`.

### Soak (valid, fixed duration)
`B1_SOAK_S=3900`, both arms concurrently (p on cpus 2-5, R0 on 6-9), `adopt-soak-p/`,
`adopt-soak-r0/`: 4 035 s and 4 033 s of walk loops (the harness loops until the deadline).

| | R0 | new | limit |
|---|---|---|---|
| RSS.soak (minute 10 -> end) | +0.58 MiB | **+0.52 MiB** | <= 8 |
| RSS peak | 12.2 MiB | 14.0 MiB | |
| STALE@soak | 0 | 0 | 0 |

The soak ran on the code before the Reader fix of section 4 (policy unchanged since); it was not
repeated for one hour for a change in the revalidation reply path.

### Variants tried (quick sessions: walk 1, ff 1, idle 1, big 1, step 3; R0 beside)

| v | change | calls/focus | HIT.all | L1c p99 (R0) | M6 | HIT.anc | load |
|---|---|---|---|---|---|---|---|
| v1 | first port: window per focus, re-read when any near sibling is not LIGHT-fresh | 44.6 | 0.854 | 3.95 (5.21) | 0.017 | 1 | 5-24 |
| v2 | near check = NAME\|ROLE only (STATES churn of the node a focus leaves was refetching the window per step) | 22.7 | 0.856 | 3.11 (3.48) | 0.261 | 1 | 5-7 |
| v3 | + look-ahead re-read of the 3 next siblings (500 ms) | 26.6 | 0.856 | 11.2 (9.95, host loaded) | 0.017 | 1 | ~20 |
| v4 | windows 1/6/6 instead of 2/8/8 | 26.5 | 0.852 | 3.59 (5.91) | 0.019 | 1 | 5-7 |
| v5 | no ancestry prefetch (parent link by Fetch) | 15.1 | 0.852 | 3.09 (4.62) | 0.019 | **0.80** | 5-7 |
| v6 | ancestry from the first unmirrored level, once per level | 26.5 | 0.852 | 3.52 (3.50) | 0.019 | 1 | 1-3 |

v1-v5 JSON/text: `bench/results/b1-mirror-fill/adopt-{s1,v2-quick,...,v5-quick}`. (`adopt-s1`
also contains the earlier aborted attempt log: the first run failed before any slot because
`run-all.sh` did not build `b1-fakeapp`; its log was overwritten by the rerun, no data existed.)
v6 is the shipped variant: the best HIT.all/M6 with HIT.ancestry 1 within the ~26 calls the
orchestrator accepted. v5 is the cheap alternative (15 calls) if where-am-I misses (0.8) are
acceptable.

## 4. Defect found by the M3 sanity run: cached focus never earned protection
A prefetched node has the states of its fetch (no `FOCUSED`). The Reader presents it from the
cache and, fail-closed, *demotes* the eviction protection (`flow.rs`); RV-1 re-reads the node but
`on_revalidated` never published protection, so the app actor's `wants_text_events()` stayed false
and every caret event of the text view was dropped (`C1`, `C1h`, `S2`: lost 150/150 with the new
policy, 0 with `--bench-fill-policy=r0`). The R0 path only worked because a cold fetch carries
`FOCUSED`. Fix: `on_revalidated` publishes the protection when the re-read shows `FOCUSED` (the
proof the presentation lacked); a re-read without it still earns nothing. Test:
`reader::tests::the_revalidation_of_a_cached_focus_earns_the_protection`. After the fix M3 passes.

IC-5 (a cached parent whose child list went stale had to be re-read or marked stale) and IC-17
(a post-flood node had to stay stale) assumed no policy touches them: the policy now re-reads the
parent after a `ChildrenChanged` and after a resync, so IC-5 passes by the policy's own refresh and
IC-17 became policy-neutral (stale or re-read, call bound unchanged).

## 5. Tradeoff: 26 calls/focus, not 25
Calls per focus are 4.1x R0 and a quarter of C's (103). The remaining cost is the sibling windows
(about 11 nodes x 4 calls each) and the 500 ms look-ahead. The look-ahead is what brings M6 from
R0's 0.27 to 0.02; without it v2 costs 22.7 calls but corrects silent GTK4 row renames only after
speaking them. The fake app prices a call at its service time; on real GTK4 (0.12-0.15 ms per
call, P-9) 26 calls are ~3-4 ms of the focused app's loop spread over the walk, not on the
keypress path (L1c 1.07-1.08x R0). A 10 000-row GTK4 list makes `GetChildren` (one per window and
per look-ahead) far more expensive than the fake app models: the index is found in a full child
list (clipped at 4 096). **Not measured**, see risks.

## 6. Gates run
* `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask deps`
  (dependency and environment rules): clean (`workspace-tests-final.log` header).
* `cargo test --workspace`: 1437 passed, 0 failed (`bench/results/b1-adopt/workspace-tests-final.log`; first run
  `workspace-tests.log`: 755 passed, 1 failed = IC-5 under the new policy, fixed as in section 4).
* IC suite against the real daemon, default policy and `candidate`: `ic-candidate-run4.log`
  (3 passed).
* e2e (`bench/results/e2e/adopt-k2`, strict `GORCA_M1_STRICT=1 GORCA_M4_STRICT=1`, 3 repeats):
  K2 p99 0.30 ms (<= 5), K3 1.13 ms (<= 15), K4 10 000 switches with `app_lost`, `app_doubled`,
  `chord_leaked` 0, R1 p99 0.41 ms (<= 3), all `lost` 0. Before the Reader fix: `adopt-k`, same.
* M3 quick (`adopt-m3b`: C1 C1h C1hs E1 E1s E2 S2 SA1 SA2 SA3 SA3a PW, strict): all gates pass.
  The first run (`adopt-m3`) failed C1/C1h/S2 with 150 lost: section 4. Kept.
* M7a quick (`adopt-m7`, private Firefox, strict): B0, W1, BR1, BR2, W2b, W3, FM1, FM2 all pass.

## 7. Risks and open items
* The Shell `Cache.GetItems` dump is implemented, tested and **off**; the harness Shell model
  still lacks `owns_name`, so no authoritative run exercises it. Both findings of the judgement
  (keyboard-idle gating, a live frame-time measurement) remain open.
* Huge lists: `Siblings` needs the full `GetChildren` of the parent (4 096 refs max). A real GTK4
  list with thousands of rows will make every window and look-ahead on it expensive for the app's
  main loop. A possible fix is `GetIndexInParent` plus `GetChildAtIndex` ranges for parents with
  `Count > 512`. Not measured on a real GTK4 list.
* The 26 calls/focus are measured on the fake apps, not on a live desktop.
* The final soak predates the Reader fix (section 4).
* `FocusChanged` still does not invalidate the node's states; the revalidation proof is what
  earns protection. A focus whose RV-1 re-read is skipped (confirmed after the trigger) with
  stale states keeps R0's behaviour.
