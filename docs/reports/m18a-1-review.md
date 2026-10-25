# M18a-1 review: `gorca-plugin-host` + `gorca-rt::wait` (r1, commit 3bb38e0d)

Reviewer, 2026-10-25. Spec: ADR-0026 r4 (§2.3.3, §2.4.6, §2.5, §2.7), decisions.md harness-security row.

## Verdict: BLOCK (small fix list; no design change needed)

The structure is good: B' actors behind `WLaneRunner`, MAC-before-deserialize, deny-by-default grants,
pin list, explicit slot table, no `unwrap` on runtime paths, the writer never touches tokio or a channel
receive. Two named security guarantees do not hold as implemented (memory cap, handle unforgeability),
and fuel still does not bound wall time through host imports. All fixes are small.

## Gates run (`. ./env.sh`, `CARGO_TARGET_DIR=/var/tmp/gorca-shared-target CARGO_INCREMENTAL=0`)

- `cargo test -p gorca-plugin-host -p gorca-rt`: all green (29 + 14 runtime + 33 rt unit).
- `cargo clippy -p gorca-plugin-host -p gorca-rt --all-targets -- -D warnings`: clean. `cargo fmt --check`: clean.
- Probes: `/var/tmp/m18a1-probe/probe_m18a1.rs` (copy into `crates/gorca-plugin-host/tests/` to run,
  `--release -- --nocapture --test-threads 1`) and probe guest `/var/tmp/m18a1-probe/guest`
  (test guest + causes 20..23, built to `/var/tmp/gorca-shared-target/m18a1-probe-guest`).

## Findings, ranked

### F-1 (HIGH, blocking): host imports are unmetered; fuel does not bound wall time
`src/host.rs` `text`/`children`/`name`/`attr`; `src/instance.rs:149` (`set_hostcall_fuel` is a per-call
lift cap, not a cumulative budget). A guest loop around a cheap import costs a few fuel units per
iteration while the host copies up to 64 KiB (`text`) or mints 256 SipHash handles (`children`).
Measured (release, probe p1), interactive grant B-2 = 0.2 ms (4.35 M units):
`text(n,0,65535)` loop ran **28-37 ms**, `children(n,256)` loop **67.7 ms** before `OutOfFuel`, i.e.
140-340x the grant. The 67 ms case exceeds `lost_after` = 50 ms, so the supervisor also orphans the
actor and spawns a second thread. The writer is not stalled (it waits <= D), and strikes bound the
total (3 per 60 s, then backoff), so this is not a core stall, but every interactive job of that plugin
misses D and a core is burned per call. `FaultKind::Watchdog` exists and is never produced. This is
ADR-0026 review H-3 still open.
Fix: charge fuel inside each import proportional to work (`store.get_fuel/set_fuel` via a `Caller`-
less accounting field: e.g. 1 unit per byte returned/lifted, ~200 units per minted handle), and a
cumulative per-call byte budget for lifts+lowers; add a probe like p1 asserting wall <= 2x grant + 50 us.

### F-2 (MEDIUM, blocking; scan item 1 CONFIRMED): memory limiter is single-memory
`src/limits.rs:150-163`, `memories() = 4` at `:185`. The limiter ignores `_current` and keeps one
`current`; wasmtime calls `memory_growing` per memory. With 4 memories (allowed), each may reach
16 MiB: probe p4 reached **64 MiB total under a 16 MiB cap**, and the first call grew **16 MiB under a
4 MiB/call cap** (each memory's `desired - start_of_call` is measured against an unrelated baseline;
the same bug can also falsely deny a legitimate grow after another memory changed `current`).
Tables (8 x 10 000 elements) have the same shape. Fix: track `total += desired - current` (use the
argument), compare `total` with `max_bytes` and `total - total_at_call_start` with the per-call cap;
same for tables. Setting `memories() = 1` is also acceptable if the canonical-ABI shims never need a
second memory (verify with the test guest). Add a multi-memory WAT test (enable wasmtime `wat` in
dev-deps or ship a pre-assembled `.wasm`).

### F-3 (MEDIUM, blocking; scan item 2 CONFIRMED): forgery budget resets every call
`src/host.rs:199` (`forged_in_call = 0` in `begin_call`), cap at `:241`. 15 forged handles per call
never fault: probe p2 ran 2000 calls x 15 forgeries = 30 000 failures, all `Done`, same instance
(serial 1), same key. Expected success after ~2^24/15 = 1.1 M calls; at background rates that is hours.
Current impact is bounded (a guest can already walk parent/children of the view), but the tag is the
only thing that will separate SS-3/SS-1 nodes or foreign instances once views are scoped. The Anchor
branch (`:298-303`) increments the counter but never applies the cap: probe p3, 200 forged anchors in
one call = `Done`. Fix: per-instance lifetime budget (e.g. 256 failures -> `Malformed`, strike,
rebuild with a fresh key, which `PluginExec` already does on fault), and apply the cap in the Anchor
arm through the same helper. 64-bit tags (handle tables host-side) are the longer-term option.

### F-4 (MEDIUM): system scopes SS-1..SS-4 are a predicate only
`trust::tier_may_enter` is never called outside its tests; `ModelView` (`src/view.rs`) has no scope
filter and its docs do not require one. A third-party guest given any node can walk `parent`/
`children` into a secret subtree and read `name`/`text`. Not exploitable today (not wired), but the
requirement "never matched by third-party code" has no enforcement point. Fix before wiring
(M18a-2): either the view handed to a non-first-party instance must exclude SS nodes (document as a
`ModelView` contract + test with a view that marks a node SS-3), or `HostState::node()` must refuse
SS nodes for third-party tiers (needs the tier in `HostState`). Recommend doing the host-side check
now: it is ~10 lines and makes the guarantee testable in this crate.

### F-5 (MEDIUM): `Cancelled` reply leaves the slot busy forever
`src/call.rs` `PluginReply::events` maps `Cancelled` to no event; `src/slot.rs` `Ready{busy}` only
clears on `CallOk`/`Fault`. Probe p5: after `Send` + `Cancelled`, state stays `Ready{busy:Some(1)}`
and every later `CallNeeded` returns `Coalesce`: the plugin is silently off for the scope. Reached by
`WLaneRunner::cancel` and by actor-side displacement. Fix: add `Event::Cancelled` (Ready -> busy None,
other states explicit) and emit it.

### F-6 (MEDIUM): package loader blocks on a FIFO
`src/package.rs:224` `read_capped` uses `File::open`, which blocks on a named pipe before the
metadata check. Probe p6: `load_dir` on a sideloaded package whose `plugin.wasm` is a FIFO was still
blocked after 1 s (forever without a writer). Same applies to `manifest.toml` and to `.cwasm` files
(`cache.rs:174`, only the user can plant there). Fix: open with `O_NONBLOCK | O_NOFOLLOW`
(`OpenOptionsExt::custom_flags`), `fstat` must be a regular file, then clear `O_NONBLOCK`. Known
class: add-on security review "loader FIFO".

### F-7 (LOW): actor/supervisor liveness edges (`src/actor.rs`)
- `supervise` (`:417-422`): if `maker` returns `None` or `spawn` fails, the actor has a bumped `inc`
  and no worker; later jobs are queued and never answered (no `ActorLost`). Answer pending and future
  jobs with `ActorLost` and mark the actor dead.
- `Turn.tag` is caller-supplied u16 (`call.rs:144`); a reused tag lets a late reply of an earlier
  turn count down the current turn and end the wait early. Generate the tag inside the runner.
- `threads` Vec (`:400`) keeps every orphaned handle until shutdown; `shutdown` (`:574-580`) joins
  only threads already finished right after the kick, so healthy workers are detached in practice.
  Prune finished handles in `supervise`; in `shutdown`, wait a bounded time (e.g. fuel bound) then
  detach.
- `writer_slack` sets 1 ns slack permanently on whatever (tokio worker) thread calls `interactive`;
  document it or restore after the wait.

### F-8 (LOW): handle index and generation wrap
`handles.rs:68` masks indices to 24 bits, so an index >= 2^24 mints a handle that verifies as a
different node; `generation` is u16 (ABA after 65 536 reuses). Refuse (`None`) indices >= 2^24 in
`HostState::mint` and document the generation-reuse bound for `ModelView`.

### F-9 (LOW): cache path robustness
`cache.rs` checks the final directory only and reopens by path each time; a parent writable by
another user allows a swap between `check_dir` and `open` (no practical attack under `$XDG_CACHE_HOME`).
Prefer an `O_DIRECTORY|O_NOFOLLOW` dir fd and `openat`. A short or corrupt `.secret` (crash during
create) makes `open` fail forever and `load` returns `BadDirectory`/`BadSecret` instead of compiling
uncached; CD-W2 says a cache failure is a miss, so fall back to in-memory compile and log.

## Requested attack areas

1. **Sandbox / handles / scopes / MAC.** MAC: sound. The file is read once into a buffer, the HMAC
   (RFC 2104, verified against RFC 4231 case 2) is checked in constant time over that buffer, the
   cache key binds package sha + compatibility hash and is inside the MAC, so cross-package swaps fail
   (test `artifact_swapped_between_installs_fails_the_mac`); no TOCTOU between check and deserialize.
   Key storage: 0600 file inside the 0700 dir; protects against other users, not same-uid code (which
   is out of scope); guests have no WASI fs. Escape: no WASI, unknown imports trap; caps default none;
   `pointer` T2-only, `act` consent-gated. Open: F-2, F-3, F-4.
2. **`gorca-rt::wait` unsafe: sound.** `FUTEX_WAIT_BITSET` without `FUTEX_CLOCK_REALTIME` takes an
   absolute `CLOCK_MONOTONIC` timeout, same clock as `monotonic_ns`; `tv_nsec < 1e9` always; the
   address is a live aligned `AtomicU32`; `FUTEX_WAKE`'s ignored variadic int args are harmless. EINTR,
   EAGAIN and spurious wakes return `true` and every caller re-checks in a loop bounded by its own
   deadline. Ordering: `Gate::done` (SeqCst CAS then `parked` load) vs writer (`parked` store then
   word load) is a correct Dekker pair; the reply is sent before the count-down. Worker park uses the
   same pattern on `wake`. Timer slack: per-thread, correct `prctl` use.
3. **Stall the core.** Writer: no (bounded by D, measured by existing tests). Plugin-local stall: yes
   via F-1 (30-70 ms per 0.2 ms call), bounded by strikes. Running-job cancel missing: acceptable now
   because D bounds the writer, but only after F-1, since fuel is the only stop. 16 instances is a
   per-store core-instance count, not a stall vector. Trap storm: each fault is a strike; 3/60 s ->
   30 s backoff -> 300 s -> disabled; instance rebuild is off the writer turn (`warm`). Fine.
4. **Gaps.** (1) inline world without `present`: accept until M18a-6; keep a test that a guest built
   against the real `app-module` world instantiates. (2) hostcall-budget by message text: accept, a
   test pins it and a misclassification only relabels a strike as `Trap`. (3) no WASI random/clock:
   accept for M18a-1 (fail-closed), must land (deterministic stubs) before any SDK/third-party
   release. (4) manifest format: accept as provisional but record it as an ADR-0026 delta, not only in
   the implementer report. Gap 6 (running-job cancel) acceptable only together with the F-1 fix.
5. **General.** No `unwrap`/`expect`/`panic` outside `#[cfg(test)]`; slot table is fully explicit;
   `PluginReply`, `TrustTier`, `SystemScope` matches exhaustive; actors never block (except F-6 loader,
   which is not an actor yet).

## Required before merge
F-1, F-2, F-3, F-5, F-6 (each with a regression test; the probes above can be adopted). F-4 before
wiring, preferably now. F-7..F-9 may follow.
