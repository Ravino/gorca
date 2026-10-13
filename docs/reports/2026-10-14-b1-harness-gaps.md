# B-1 harness gaps: what was wrong, what is fixed, what needs a decision

Branch `b1/harness-gaps` (from `main` at `b103c36`). Inputs: arm A `RESULTS.md`, arm C `RESULTS.md`,
SPEC 6, 8, 9, 14. Host for every number below: Core Ultra 9 285H, 16 cpus, `powersave`, deep idle
on, load 2.5-4 (other agents running), so **informative, not authoritative**. Nothing was deleted;
raw runs of this report are in `bench/results/b1-mirror-fill/smoke1/` (runner smoke) and were
analysed with `b1 analyze walk`.

## 0. Summary

| # | Gap | Root cause | Status |
|---|---|---|---|
| 1 | every walk INVALID (`ancestry_demands 0`) | `RealSut::where_am_i` unsupported | fixed: presses Insert+Tab through the emulated Mutter (K3 recipe) |
| 2 | `S1.p99.n = 0` | **two bugs**: GTK4 fake apps swallow every object event (listener gate P9-1, nobody told them), and the classifier would have rejected the daemon's `s1` source anyway | fixed; S1 now n = 24 per walk |
| 3 | step bench measures noop; arms edit wiring | no `Candidate` hook | fixed: stand-in + `--bench-fill-policy` + generic adapter + IC test for the candidate |
| 4 | M-6 28 % for R0 and A | not a Reader bug: silent GTK4 row recycling, corrected 2-5 ms later; the gate counts inaudible corrections | metric split `B1-M6.audible` (needs sign-off); no shared-code change needed for M-6 |
| 5 | strict fidelity fails | C2/C3 idle exit latency and placement, not load | not fixable without root; script + recipe provided |

Two further shared-code findings (not edited): the SelectedItem log/verdict mismatch (section 5),
and `DemandWhy::WhereAmI` is never emitted by the daemon (section 1).

## 1. Where-am-I in the walk

* `crates/gorca-testkit/src/b1/keys.rs`: client of `tests/harness/fake_mutter.py`; `RealSut::with_keyboard`;
  `where_am_i` presses INSERT+TAB with virtual event times (700 ms apart, Mutter first-click rule)
  and fails the step if Mutter did not consume the chord.
* `b1-walk` starts the emulated Mutter before the daemon for `walk|big|soak`, passes
  `--impersonate-orca` only when the session bus path contains `/gorca-h.`, GORCA_REPEAT_DELAY_MS /
  XKB / clock facts as the e2e harness does, and waits (15 s) until the daemon's modifier grabs are
  applied.
* The daemon's answer is one sentence; it writes **no presentation line**, so the step returns no
  per-node stimulus (it would be counted as lost focus). `B1-HIT.ancestry` = demands during which
  the focused app served no call (80 ms window): 15 / 15 on R0 and on arm C.
* Also fixed on the way: `duration_s` was 539 526 s (RealSut clock started at the first app);
  `ancestry_demands` / `ancestry_no_ipc` are now written at the top level where `rundir.rs` reads
  them (they were only inside `actual`).
* **Shared-code finding (not edited).** The M1 command answers from `Reader::ask_node` with an
  `Info` fetch and `info::ancestry(&self.dir, ..)` over the mirror; it never emits
  `FillInput::Demand { why: DemandWhy::WhereAmI }`. Arm A's `Ancestry` request and arm C's where-am-I
  path are therefore reachable only from unit tests and from `FocusSettled`. If the arms are meant to
  be judged on where-am-I, `ask_node` must push `ReaderEffect::Demand { node, why: WhereAmI, outcome }`
  on a hit, miss and stale ancestor (same shape as `flow.rs` `begin_present`).

## 2. S1

Root cause chain, found by reading `present.jsonl` of the arm A run (no `s1` line at all) and then the
fake app:

1. `listener_gate` (P9-1) in the Settings/Files models: a GTK4 fake emits no `PropertyChange` /
   `StateChanged` / `ChildrenChanged` until it sees the registry's `EventListenerRegistered`
   broadcast. `at-spi2-registryd` sends it only when an AT registers; the daemon registers before
   the walk starts any app. The conformance fixture (`b1_ic.rs`) sends `Cmd::RegisterListener` itself;
   `RealSut` in the walk did not. **In every earlier walk, all "evented" mutations of Settings and
   Files were silent**, which also inflated M-6 and weakened B1-STALE (events never invalidated).
   Fix: `RealConfig::register_listeners` (on in `b1-walk`, off in the in-process fixture that tests
   the deferred case), plus `defer_listener_registration` / `register_listener` for `RealSut`.
2. The daemon logs the focused-node change with source `s1`; `verdict::Source::parse` bailed on it
   ("unknown source"), which would have invalidated the whole run. `Source::S1` added: judged as a
   first utterance (kind `StateChange`, latency fake-app emit to SPEAK), never counted as answering a
   focus stimulus. Test: `an_s1_line_is_a_state_change_...`.

Result (R0, full walk, load ~3): `B1-S1.p99` **6.0 ms** (p50 3.7, n = 24); arm C 6.3 ms (p50 3.8).
That exceeds the 5 ms gate for **both**. The S1 reply is one Interactive fetch of STATES|VALUE|NAME
(`L1c.p50` is 2.6 ms on this host, p99 5.2), i.e. three serial calls at ~0.9 ms each against a fake
that serves one request at a time. It is a shared-path cost (arm A finding 7: no `GetAll`), not an arm
effect, and it is unverified on a quiet host; the reviewer should expect to see this row near its
limit, and the connector fix (one `GetAll`, or the three reads issued concurrently) would move it by
~2x.

## 3. Candidate hook, step bench

* `crates/gorca-mirror/src/fill/candidate.rs`: `Candidate` stand-in = R0 (`NullPolicy` inside).
  An arm replaces this file (and `candidate/**`, `fetch/strategy.rs`) and edits nothing else.
  Contract: `FillPolicy + Default + Send + 'static`. `fill/mod.rs` got the two lines; this is the
  only edit outside harness paths and it is the SPEC 10 hook.
* Daemon: `--bench-fill-policy=r0|candidate` (CLI flag only, never env/config; default R0, test
  in `cli.rs`). Runtime selection replaces SPEC 10's "at build time": one binary serves R0 and the
  arm, and a stand-in build can be diffed against R0 as a self-check. `b1-walk` uses `candidate` for
  every arm not labelled `r0` (`B1_FILL_POLICY` overrides). Neither arm needs to edit
  `gorca-core/src/runtime.rs` any more.
* `benches/step.rs`: arm A's adapter, generalised (adds `FocusSettled` and `WindowActivated`
  inputs), default subject `candidate`; `driver.sh` no longer forces `noop` and makes `B1_RUN_DIR`
  absolute (arm A finding 5); `run-all.sh` builds `b1-walk` + `b1-fakeapp` and exports
  `B1_WALK_DRIVER`. **Arms must merge this commit** (the bench lives in their tree).
* `cargo test -p gorca-daemon --test b1_ic candidate` runs IC-1..23 against the tree's `Candidate`.
  Passing on the stand-in; with arm C's `candidate.rs` + `strategy.rs` copied in (temporary, reverted)
  it passes everything except IC-14 (section 5).
* The binary is `target/release/gorca`, not `gorca-daemon` (docs fixed where I touched them).

## 4. M-6: diagnosis

Question: are the presentations spoken from cache and corrected afterwards a UX bug or an artifact?

**Evidence (R0, full walk after the listener fix, 1663 focus presentations, 442 refines = 26.6 %):**

| cause (truth log, every refine traced to its mutation) | refines | evented? |
|---|---|---|
| Files list row recycled by `Scroll` (name `File 00000.txt` -> `File 00060.txt`, same path, no event) | 426 | silent |
| Settings control, computed name changed silently (G-1) | 5 | silent |
| other silent renames at revisit | 11 | silent |
| evented mutation raced the event | 0 | - |

* Before the listener fix the same walk gave 28.2 % (469): with events delivered, evented mutations
  now invalidate the mirror, so the 28 % was partly the harness (evented mutations behaving as silent).
* Arm C on the fixed harness: 1.3 % (22 refines: 18 recycled rows, 4 silent renames), 0 evented.
  Arm C's own 1.98 % measured with the gate closed. C removes 95 % of R0's refines by re-reading
  pages near the focus (>8 s, dirty) but cannot see a recycled row, as it said.
* **Not audible.** `Judged.fix_delay` (new): the refine replaces the utterance 1.8 ms (p50), 3.7 ms
  (p99), 4.9 ms (max) after it was handed to speech (R0), 2.4 / 4.1 / 4.1 ms (C); the Reader's own
  replace window is 150 ms (`REFINE_WINDOW`). All 442 are `REFINED`, none `LATE`, none `STALE`. A
  speech server cannot start audio in 2-5 ms (voicego time-to-first-audio is not measured here;
  **unverified live**, the mock cannot tell), so the user hears the corrected name only.
* **Model fidelity.** `gtkatspicontext.c` emits `accessible-name` only when `GTK_ACCESSIBLE_PROPERTY_CHANGE_LABEL`
  is updated on that context (`refs/gtk/gtk/a11y/gtkatspicontext.c:1232`). A recycled `GtkListItemWidget` keeps
  its accessible and its content-derived name changes through its bound children; P-9 observed such
  names going stale silently (`docs/reports/2026-10-13-p9-cache-p2p.md`, GTK4 row). So the mechanism
  is real. The *rate* is the model's pessimism: SPEC 16 says G-6 / S-1 / H4 are still hypotheses with
  pessimistic flags, and the walk scrolls and then refocuses exactly the rows it cached (200 arrows
  over ~60 realised rows). A real Nautilus user scrolling a long list recycles the same way, so the
  order of magnitude is plausible for list scrolling and wrong as a per-focus average.
* **Conclusion: harness metric artifact plus an honest model, not a shared-code bug.** Nothing in
  `gorca-core` needs to change for M-6. Any policy that serves a revisited row from the mirror pays
  one inaudible refine; a policy that never serves it (R0 with `Revalidate` first) pays latency.
  Gating `< 1 %` on corrections that are replaced before audio penalises caching itself.
* **Fix (harness, needs sign-off, SPEC 9 / 15):** `B1-M6` becomes a ranking row; the gate is
  `B1-M6.audible` = LATE plus REFINED whose replacement came later than 10 ms
  (`limits::AUDIBLE_AFTER_NS`), `< 1 %`. Info rows `refine_delay_p99_ms`, `refine_delay_max_ms`,
  `m6_audible`. R0 and C both read 0 audible. If the orchestrator prefers to keep the old gate, flip
  `B1-M6` back to `Cmp::MedianAtMost` in `limits.rs` (one line) and every arm fails it.
* Possible product-side improvement, no harness gain: when the focus lands in a virtualised list
  (`MANAGES_DESCENDANTS` parent, GTK4 `ListItem`/`Row`), skip the cached presentation and take the
  fetch (cold path 2.6 ms p50) instead of presenting then refining. Costs L1 for revisited rows; not
  recommended while refines are inaudible.

## 5. Shared Reader bug: SelectedItem context (confirmed)

Arm C's finding is real, and it is a **logging/verdict** defect, not wrong speech:

* `present_with` (`crates/gorca-core/src/present/flow.rs`) logs `log_presentation(&u, snap, ..)`, i.e.
  the cached snapshot's states. For a switcher item presented in `SelectedItem` context the spoken
  text omits `selected` (it is "because it is selected", `skip_states`), and RV-1 compares with
  `skip_states(SelectedItem) = SELECTED`, so a snapshot cached **before** the `selected` event
  (prefetched as a sibling) is neither corrected nor logged as selected. The classifier compares the
  logged states with truth, which says `selected`: verdict STALE.
* Reproduced: arm C's policy on this tree, `IC-14` fails with a `Stale` presentation of "Window 1".
* **Verified fix** (temporary local patch, reverted, `gorca-core` untouched in the branch): in
  `present_with`, when `ctx == PresentContext::SelectedItem`, pass `log_presentation` a snapshot with
  `StateSet::SELECTED` set (the context asserts it):
  ```rust
  let logged = if ctx == PresentContext::SelectedItem && !snap.states.contains(StateSet::SELECTED) {
      let mut c = (**snap).clone(); c.states |= StateSet::SELECTED; Arc::new(c)
  } else { Arc::clone(snap) };
  self.log_presentation(&u, &logged, source, None);
  ```
  With it, `IC-14` passes under arm C. Spoken text is unchanged. Cleaner: give `PresentRecord`
  an `assert_states: StateSet` instead of cloning. Assign to rust-engineer; both arms must be re-run
  (SPEC 14.6: a defect in shared rules is fixed on `main` and re-run for both).

## 6. IC cases C reports as failing: policy-neutral or R0-shaped?

All five encoded R0's (or PrefetchAll's) behaviour; fixed in `crates/gorca-testkit/src/b1/ic.rs`
and `crates/gorca-daemon/src/probe.rs`:

| Case | Verdict | Change |
|---|---|---|
| IC-5 | R0-shaped: asserted the panel stale **after** focusing a new node; a policy may legitimately re-read the panel on the event or on the next focus | read the probe before anything is focused; accept `stale` **or** "a call was served after the replace" |
| IC-12 | harness defect: SPEC says "aged by TTL" but `DaemonProbe.stale` ignored `aged` | `stale = stale \|\| aged`; no polling policy can or should pass otherwise |
| IC-14 | R0-shaped: required **zero** calls to the shell for 2 s after a rebuild; SPEC says "dead switchers are not prefetched" | keep `calls_unknown_object == 0` (nothing sent to dead paths); drop the total-call equality. The remaining C failure is section 5 |
| IC-22 | R0/PrefetchAll-shaped: demanded a dump from one event with no focus; the "VACUOUS" guard failed lazy policies | focus first, then look; a policy that never dumps gets SKIP "(b) not applicable", the eligibility property is unchanged when a dump is sent |
| IC-23 | R0-shaped: baseline call count taken right after the focus, so a post-focus prefetch counted as "TTL traffic" | 3 s settle before the baseline; the idle claim (no TTL-driven calls over 31 s) is unchanged |

Checked: R0 and PrefetchAll still pass (`b1_ic`, 197 s), the stand-in passes, arm C (temporary copy)
passes IC-5/12/22/23 and fails only IC-14 on the Reader bug of section 5. `selftest` (sim faults)
still passes.

## 7. Strict fidelity at load ~2.5 on powersave

Measured: `fidelity` test, `B1_STRICT=1`, one measurement per run, this host, load 2.4-3.5.

| Condition | runs inside +-20 % of 0.5 ms (0.4-0.6) | delta p50 (ms) | base RTT p50 (ms) |
|---|---|---|---|
| unpinned | 2 / 6 | 0.07, 0.44, 0.50, 0.94, 1.21, 1.27 | 0.84-1.64 |
| `taskset -c 4-7` | 0 / 6 | 0.71-1.03 | 0.75-0.96 |
| `taskset -c 4,5` | 0 / 6 | 0.70-1.15 | 0.24 |
| `-c 4-7` + nice-19 spinners on each core | 2 / 6 (by coincidence: the spinners inflate the base) | 0.25-0.63 | 0.39-0.73 |

Mechanism: the delta is **not additive**. `/sys/.../cpuidle`: POLL 0 us, C1 1 us, **C2 127 us, C3 1 048 us**
exit latency; during one 5 s pinned run each of cpu4/cpu5 entered C2 ~2 900 times and C3 ~1 300 times.
While the fake spins for 0.5 ms the client's and the bus's cores sleep and pay the exit latency, which
the near-zero baseline never pays; unpinned, thread placement also moves the base from 0.24 to 1.6 ms.
`intel_pstate` powersave (min 400 MHz) adds frequency ramps on top. So +-20 % is **not achievable** on this
host as configured, with or without pinning; load is not the cause. I cannot test the cure (needs root);
the mechanism is supported by the counters above.

**What authoritative runs need** (user runs it, sudo asks for a password):

```bash
sudo bench/b1/quiet-host.sh on        # governor performance + C2/C3 disabled on every cpu
bench/b1/quiet-host.sh status
taskset -c 2-9 bench/b1/run-all.sh <label> --arm ...
sudo bench/b1/quiet-host.sh off       # restores powersave and all idle states
```

By hand, no script: `echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor` and
`for s in /sys/devices/system/cpu/cpu*/cpuidle/state[23]; do echo 1 | sudo tee $s/disable; done`
(`cpupower` is not installed; `cpupower idle-set -D 1` would do the second). Plus load < 1.
Harness side: `host::deep_idle_states()` records enabled states with exit latency > 20 us in the result meta
and makes `meta.quiet_host` say "no" while they are on. **Unverified**: that +-20 % then holds; if it still
fails on the quiet host, the check should compare in-app service time (the fake can report it) instead of the
round-trip delta, which is a SPEC H-10(a) amendment.

## 8. Files changed (all committed on the branch)

Harness: `bench/b1/{driver,run-all}.sh`, `bench/b1/quiet-host.sh`, `bench/b1/README.md`,
`crates/gorca-bench-b1/{Cargo.toml,benches/step.rs,src/{host,limits,rundir,verdict}.rs}`,
`crates/gorca-testkit/src/{b1/{keys,real,walk,ic,mod}.rs,bin/b1_walk.rs}`, `crates/gorca-daemon/tests/b1_ic.rs`,
`tests/harness/b1/fakeapp/tests/real_sut.rs`. Hook: `crates/gorca-mirror/src/fill/{candidate,mod}.rs`,
`crates/gorca-daemon/src/{cli,lib,probe}.rs`. Docs: SPEC 9 / 15, `docs/decisions.md`, this report.
No edit in `gorca-core`, `gorca-connector-atspi` or the policy logic of `gorca-mirror`.

## 9. Open

* Smoke of the whole runner (`bench/b1/run-all.sh smoke1 --quick`, R0 vs stand-in): see section 10.
* S1 p99 6 ms vs the 5 ms gate on this host, both arms.
* Voicego time-to-first-audio vs the 10 ms inaudible threshold: unmeasured.
* The stand-in is not compared against R0 numerically beyond the smoke run.

## 10. Runner smoke

`bench/b1/run-all.sh smoke1 --quick --no-fail --arm r0:<gorca>:standin --arm candidate:<gorca>:standin`
(`bench/results/b1-mirror-fill/smoke1/`; plan walk=1 quick, ff=1, idle=1, big=1, step=3; load 7-11 from
other agents). The run is still `INVALID RUN` because strict fidelity failed (delta -0.21 ms), as
expected on this host. What it proves:

* no walk or big run is invalid any more (`B1-RUNS.invalid` 0 for both arms; `ancestry_demands` 15);
* the stand-in equals R0 (`HIT.all` 0.432 / 0.432, `M6` 0.266 / 0.266, `LOST` 0, `STALE` 0), so the
  runtime selector and the `candidate` default do not change behaviour;
* the step row measures the tree's `Candidate` through the adapter (8.4 / 8.1 us, was 0.015 = noop);
* `B1-M6` info 0.266, `B1-M6.audible` 0; `B1-S1.p99` 5.3 / 5.8 ms (n = 24, FAIL for both);
* **new gap, not fixed:** `B1-L4.announce` has no value for any arm (bold row: FAIL). The `hung` model
  scenario is scripted but the "application not responding" announcement is not a node presentation, so
  nothing in the run directory carries its time. It needs the speech-mock timestamps of the SPEAK that
  contains "not responding" joined with the `hung-start` mark; I did not build it.

Full-length evidence (not part of the runner): `bench/results/b1-mirror-fill/harness-gaps/r0-full-walk`
(R0, 1663 focus, M-6 26.6 %, S1 6.0 ms) and `armc-full-walk` (arm C's `candidate.rs` built into a binary,
M-6 1.3 %, HIT.all 0.894, 103 calls per focus, S1 6.3 ms).
