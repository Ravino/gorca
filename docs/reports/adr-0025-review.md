# ADR-0025 mouse emulation - adversarial design review

## r1 (2026-10-19, reviewer, ADR at 5c517151, Proposed)

Verdict: **ACCEPT WITH CHANGES** (C-1..C-9 below are required before MO-1/MO-2 start; the rest can land in the slices).

Read-only review. Nothing was run against the live session or a private shell. Evidence is from `refs/`, the current
tree and one script that loads the four preset TOMLs.

### Verified claims (hold)

- **Num Lock off only (RG-3).** Mutter matches `keysym == ks && modifiers == state` exactly
  (`refs/mutter/src/backends/meta-a11y-manager.c:563`). The current compiler already emits keypad operators
  0xffaa..0xffaf (KP_Multiply..KP_Divide) as `exact` with `{0, LOCK}` only (`crates/gorca-core/src/commands/compile.rs:437-445`),
  the same path `kp_add` uses today. With Num Lock on, the state carries Mod2 and nothing matches. The claim holds,
  and no new compiler rule is needed for RG-3.
- **Drag lock does not break chords.** Mutter strips `MOUSE_BUTTONS_MASK` from the state before matching and notifying
  (`meta-a11y-manager.c:636`), so exact keypad grabs still match while a button is held.
- **Popups have Meta windows.** `meta_wayland_xdg_popup_managed` sets transient-for to the parent window and the type
  to DROPDOWN_MENU (`refs/mutter/src/wayland/meta-wayland-xdg-shell.c:1686-1697`). The ADR is right. The study
  (`docs/reports/mouse-emulation-design.md:70`, "xdg_popup, no Meta window") is wrong and should be corrected.
- **Presets.** I loaded all four TOMLs (global + browse) and checked every chord in §3 after modifier
  normalisation. No collisions. `gorca+7` stays on `review.focus_follows_toggle` everywhere. `shift+7/8/9` and
  `shift+m/n` exist only as non-gorca browse quick-nav chords. `extensions/*/ext.toml` binds only `gorca+shift+f9`.
  `gorca+shift+7` and `gorca+ctrl+7/8/9` are free in orca-laptop.
- **Crash rules.** The gate reuses the per-connection coalesced `SetKeyGrabs` generation
  (`crates/gorca-input-mutter/src/monitor.rs:318-405`). No Watch, Grab, Ungrab or reconnect is involved, which matches the
  decisions.md 2026-10-19 mitigation row.
- **Hybrid verdict (A only).** Agreed. B cannot place a click on a Wayland window. Note for revisit trigger (c):
  `PointerLocator.QueryPointer` already exists in `refs/mutter` main (`meta-a11y-manager.c:handle_query_pointer`, d82671c).
  Track it per GNOME release.

### Findings (ranked)

**C-1 HIGH - stalled Shell: clicks land after gorca said they failed (§2.7 EX-2/EX-3, lines 280-284).**
Scenario: the Shell stalls under host load, which is how the 2026-10-19 crash started. The user presses KP/ three
times. The first `PointerClickAt` misses its 150 ms deadline and the user hears "Mouse not responding". EX-3 then
sends the next pending request, and so on, up to H-1's burst of 40. When the main loop resumes, the helper runs every
queued call. Clicks the user was told did not happen all land, possibly on a different window, because plain clicks
act at the *current* pointer. A `press` that timed out lands later and leaves the button held, while gorca only
knows it as `Unknown`.
Fix: add opts `not-after` (CLOCK_MONOTONIC µs; `GLib.get_monotonic_time()` and Rust `Instant` share the clock). The
helper returns `outcome="expired"` and injects nothing when it is past that time. gorca must not dispatch the next
request while one is overdue: drain pending requests with "Mouse not responding". Test: SIGSTOP the private shell,
queue 5 clicks, SIGCONT, assert 0 clicks in the client log.

**C-2 HIGH/MED - the transient allowance accepts modal dialogs (§2.2 guard and occlusion, lines 98-105).**
The guard and the occlusion test both treat "a window whose transient-for chain reaches `window`" as the target.
An attached or modal dialog ("Discard changes?") is a transient of the parent and covers it. An item click aimed
at a parent control under the dialog is then neither refused nor flagged as occluded. It lands on whatever dialog
button sits at that point, which can be a destructive one. Fix: allow only popup-type transients
(DROPDOWN_MENU, POPUP_MENU, COMBO, TOOLTIP, MENU and their popup chains). A NORMAL/DIALOG/MODAL_DIALOG transient
counts as an occluder ("Covered by {dialog}"). Add an e2e row: a GTK modal dialog over a parent button.

**C-3 MED - `Unresponsive` closes the gate (§2.5 line 211, RG-5).**
A Shell stall makes the helper unresponsive, but key events stall too, because Mutter processes them on the same main
loop, so closing the gate buys nothing. It does queue one `SetKeyGrabs` into the stalled Mutter. After recovery, the
gate stays closed for at least 2 s of hysteresis. During that time KP/ and KP* type `/` and `*` into the focused
text field, and a second `SetKeyGrabs` follows. Fix: close the gate only on `Absent`, `Squatted`, `Denied`, `TooOld`,
`NoCap` or `mouse.enabled=false`. While `Unresponsive`, keep the grabs and have the command say "Mouse not responding".

**C-4 MED - `window = 0` targets have no guard (§2.2 line 89, line 105).**
Stage targets come from St extents with no guard and no occlusion check. Scenario: quick settings closes between the
resolve and the click, and the click lands in the app window underneath. Fix: for `window = 0`, require the picked
actor to be outside every `MetaWindowActor`, otherwise return `refused-outside`.

**C-5 MED - the XWayland ratio is wrong on mixed scales (§2.4 lines 163-165, WM-2 line 181).**
Mutter's X scale is global: `ceil(highest_monitor_scale)`, or the `xwayland-scaling-factor` override, or 1 in physical
layout mode (`refs/mutter/src/wayland/meta-xwayland.c:1368-1392`). It is not "ceil(scale of the row's monitor)".
On a 2x laptop with a 1x external monitor, an X window on the external monitor has ratio 2. WM-2 tries {1, 1}, finds
no size match and returns `NoLocation`. This fails safe, but the ADR claims the behaviour works. Fix: take the ratio
candidates as {1, X effective scale}, with the helper reporting the effective scale in `GetMonitors`. Add a test row
for mixed scales 1 + 2 with XWayland.

**C-6 MED - the denied path does unbounded work on the compositor (`contrib/gnome-shell-extension/gorca-helper@gorca/helper.js:229-246`, v1, inherited by v2).**
`_authorize` runs before `_rateLimit`. A caller that is not on the allow-list therefore gets no rate limit at all.
Each of its calls does synchronous `query_info` on the config, three `/proc` reads and one `console.warn`. On top of
that, the §2.5 10 s poll by a `Denied` gorca writes a DENIED line to the journal every 10 s, indefinitely. Fix: a
per-sender bucket ahead of `_authorize`; cache a negative decision per sender until the config stamp changes; log
DENIED at most once per sender and stamp. That last fix also covers H-5, which today only rate-limits injection
lines.

**C-7 LOW - spec inconsistencies.**
(a) MI-4 says "≤ 1 per 2 s", but RG-5 closes immediately. Restate as "≤ 1 open per 2 s, close immediate, ≤ 2
changes per 2 s".
(b) MI-5 is false with `if-occluded=proceed` / `mouse.if_covered=click`. Qualify it.
(c) `HoldState` (line 260) is a single value. It must be one per button, `[HoldState; 2]`; locking left while right
is held is otherwise undefined.
(d) H-3 tracks holds per caller on one shared virtual device (H-6): caller B's release frees caller A's hold. Use a
single holder, or one device per caller.
(e) `count = 2` needs a 30 ms timer, which is not "one main-loop turn". It must be cancelled in `stop()`.
(f) AL-1 does not say how `caps` combine when several rules match the same identity (first match or union).

**C-8 LOW - slices are oversized.**
MO-1 (allow-list caps, 6 methods, holds, guard, pick occlusion, audit, test script, platform doc), all of it
compositor JS, will not fit in 2 days. Split it into MO-1a (caps, `Capabilities`, `GetPointer`, `PointerMove`,
`PointerButton`, `PointerRelease`, holds, audit) and MO-1b (`PointerClickAt` guard and occlusion, after S-6). Split
MO-5 into the resolver plus window matcher, and the item-click/action/ScrollTo path plus the Orca rows.

**C-9 test-plan gaps (§6).** Add:
- the C-1 stall burst;
- the C-2 modal dialog over the parent;
- a lock taken while a button is held (H-3 lock release; the unlock dialog must not see a drag);
- C-4 stale St targets;
- C-5 mixed scales;
- a denied-caller flood (main-loop p99 and journal line count);
- `disable()` during the `count = 2` gap;
- C-3: no `/` typed in the 5 s after a 3 s SIGSTOP;
- re-running `tools/platform-probes/shell-segfault/repro.py` modes with gate flips interleaved.

### Not verified

- Pick cost (S-6) and M-7 at 1 ms p99.
- Clutter clamping and the `clamped` reporting.
- The left-handed swap on a virtual device (S-5).
- GTK4 popover `WINDOW` coordinates through the toplevel (S-1).
- Whether `global.get_window_actors()` lists popup windows in 48.7. I believe it does, since every MetaWindow gets
  an actor, but did not run it.

## MO-0/MO-1 code review r1 (2026-10-19, a45517e6, branch worktree-agent-af24dfb9f5dead4ee)

Verdict: **MERGE AFTER FIXES** (F-1..F-3 are required; the rest can follow).

What I ran: `contrib/gnome-shell-extension/test-pointer.sh /var/tmp/gorca-rv-ptest-<pid>` in a private headless shell,
on a loaded host (load 10-14). Results: A 70/70, C 4/4, B 14/14, exit 0. Teardown went by the recorded PIDs, and the
shell PID is gone afterwards. Note that the runner starts transient `systemd-run --user` units for the caller roles.

ADR r1 items, checked in the code:
- **C-1** fixed: `not-after` is required on every injecting call (helper.js:611-620), and T14 (SIGSTOP stall) passes.
- **C-2** fixed: the popup-only family rule is at helper.js:726-745.
- **C-4** fixed: the stage guard is at helper.js:679-682.
- **C-6** fixed: a per-sender bucket runs before any /proc work, and negative decisions are cached and logged once.
- **C-7(d)** fixed: there is a single holder.
- **C-7(e)** fixed: the double-click timer is cancelled on `stop()`, on lock and when the caller vanishes.

Also checked: only one `create_virtual_device(POINTER_DEVICE)` call exists, and no keyboard device is created. The
backstop sources are removed in `_releaseHolds`. Every handler runs inside the `_guard` try/catch. The timer and
signal callbacks wrap their emits.

### Findings

**F-1 MED - GLib-CRITICAL inside gnome-shell on every enable and every monitor change, and the layout mode is never read (helper.js:896).**
`GetCurrentState` returns `(u, a(...), a(...), a{sv})`, so `properties` is child **3**, not 2. The private shell log
shows `GLib-CRITICAL g_variant_lookup_value: assertion ... failed` at start (19:27:00.591) and after the T16 re-enable
(19:27:37.221). `_layoutMode` therefore stays 1, and in physical layout mode `x-scale` reports ceil(scale) instead
of 1. That fails safe through the guard, but it is still a CRITICAL in the compositor, and a session run with
`G_DEBUG=fatal-criticals` would abort. Fix: use `get_child_value(3)` and check the type before calling lookup.

**F-2 MED - a call that was already waiting on authorization runs after `disable()` (helper.js:425-436, 589-595).**
A first-time sender waits on `GetConnectionCredentials` for up to 500 ms. If `stop()` runs in that window, the
handler resumes afterwards. `_ensureDevice()` then creates a new virtual device, and a `press` creates a hold whose
NameOwnerChanged watch is already unsubscribed. The button stays down for up to 125 s, until the orphaned instance's
backstop fires. Fix: set a `_stopped` flag in `stop()`, check it after the `await` in `_guard` (fail with `Failed`),
and make `_ensureDevice` refuse when stopped.

**F-3 MED - an expired `release` leaves the button held (helper.js:560-563).**
The `not-after` check runs before `action === 'release'`. After a stall, the user's unlock (release) comes back
`expired` and the button stays down until 125 s, even though releasing is always safe. Fix: exempt `release` from
the expiry check, and say so in docs/platform/org.gorca.ShellHelper.md.

**F-4 LOW - log and work limits are per sender, not global (helper.js:385-399, 325-327).**
A same-uid process that rotates bus connections gets a fresh 20-call burst, a `GetConnectionCredentials` call and
one DENIED line per connection. When either map passes 256 entries it is cleared. Fix: add a global denied bucket
and a global 1/s DENIED log limit with a suppressed counter.

**F-5 LOW - `GetPointer` (a pick) uses the 100/s v1 bucket (burst 200), not the pointer bucket.**
The pick cost is not verified (M-7). Charging `GetPointer` to the 20/s bucket, or giving it its own small bucket,
bounds the main-loop time an allow-listed caller can spend.

**F-6 LOW - `this._device = null` relies on GJS garbage collection to destroy the device (helper.js:188).**
Buttons are released explicitly first, so this is harmless, but the old device lives on until GC runs. Document it,
or `run_dispose()` it after the releases.

**F-7 LOW - authorized calls still do synchronous `query_info` on the config and 3-4 `/proc` reads per call on the main loop.**
This is fine on local storage. Cache the config stat for about 1 s if M-7 needs the headroom.

### Not verified

M-7 (handler p99 ≤ 1 ms) needs a quiet-host rerun; the agent's 3-5 ms was measured at load 14.5. The Qt spike was
not run.

### Hot reload

**Wait.** No crate in the tree calls `org.gorca.ShellHelper` yet, so v2 gives nothing on the live session today.
It does, however, add new compositor JS, and that JS currently emits a CRITICAL (F-1). Hot-reload once F-1..F-3 are
fixed, M-7 has been rerun on a quiet host, and MO-2 (the client) exists. Even then, add `pointer` to the allow-list
only when MO-4 lands.

## MO-2 code review r1 (2026-10-19, branch mo2-helper-client, f6e01132..bfd18c00)

Verdict: **MERGE AFTER FIXES** (P-1 is required; P-2 and P-3 can follow in MO-3).

What I ran (target dir /var/tmp/gorca-review-target):
- `cargo clippy -p gorca-shell-helper -p gorca-output-api --all-targets -D warnings`: ok.
- `cargo test` on those two crates, 5 runs: 4 passed. Run 4 failed only in the gorca-output-api doctest with
  "extern location ... rlib does not exist", which is another build replacing artifacts in the shared target dir, not
  a test failure. The fake-helper suite passed 12/12 in all 5 runs, about 6.6 s each.
- `cargo test --workspace --no-fail-fast` once: rc=0.
- `xtask deps`: ok.

The reported rc=101 did not reproduce. The most likely cause is the same shared-target race; I have no evidence of
a flaky test.

Checks that hold:
- Pointer I/O runs only in the actor task. Neither the Reader nor gorca-core uses the crate yet.
- The queue is an `mpsc` with capacity 4 plus the one in flight, and `try_send` returns `Busy` when it is full.
- An overdue call drains the queue and refuses new requests until the late answer arrives or a probe succeeds. The
  late call is hard-capped at deadline + 2 s.
- `not-after` is derived from the caller's deadline using CLOCK_MONOTONIC.
- Calls are pinned to the verified unique owner name, and replies are checked against that sender.
- The `AvailMachine` matches are exhaustive and probes never chain into further probes. C-3 is implemented (gate open
  while `Unresponsive`).
- No `unwrap` outside tests. The xtask zbus allow-list entry for gorca-shell-helper matches ADR §2.9.

### Findings

**P-1 MED - shutdown does not release held buttons (client.rs:113, 326).**
The comment says dropping the connection releases holds. But `start(conn)` receives the caller's shared
session-bus connection, so cancelling the actor does not make the unique name vanish. A drag lock held at
`shutdown()` stays down until the helper's 125 s backstop. The ADR HoldState row requires a best-effort
`PointerRelease`. Fix: on cancel, if a verified owner exists, send `PointerRelease` with a 150 ms deadline before
publishing; otherwise document and require a dedicated connection. Add a fake-helper test for it.

**P-2 LOW - the caller's deadline is not capped (client.rs:465-480).**
`exec` waits inline up to the caller's `Deadline`, and `not-after` follows it. A caller bug that passes a 60 s
deadline blocks the actor, including owner-change handling and cancel, for 60 s. It also lets the helper inject up to
60 s late, which defeats C-1. Fix: clamp it to the EX-2 maximum (500 ms) in `submit`.

**P-3 LOW - the global lookup bucket can starve gorca's first authentication (helper.js, commit 217ce89d).**
Only unknown senders draw on it, so an allowed gorca keeps working. But after a helper reload, gorca's first `Ping`
is again an unknown-sender lookup and can get `rate:` while strangers are flooding. That leaves gorca `Absent` or
`Unresponsive` for up to the 30 s backoff. This is acceptable under the same-uid threat model; document it in the
platform doc.

### The flood measurement

An allowed caller's p50 went from 0.9 ms to 4.2 ms under a flood from 60 denied connections. That is **acceptable**,
and no further isolation is required. It is far below the 150 ms call deadline, it is off the key-to-speech path, and
the flooder is a same-uid process that could already inject input through RemoteDesktop. Main-loop impact on frames
should be part of the quiet-host M-7 rerun.

## MO-3 code review r1 (2026-10-19, c2fb0fe0, branch mo3-key-gating)

Verdict: **MERGE**. One follow-up below concerns pre-existing test infrastructure, not MO-3.

What I ran (own target dir /var/tmp/gorca-rv-mo3-target):
- `cargo clippy --workspace --all-targets -D warnings`: rc 0.
- `cargo test --workspace --no-fail-fast` three times: run 1 rc 101, run 2 rc 0, run 3 see the brief. In run 1, every
  unit and integration test passed. The 35 failures were all doctests ending in `E0463 can't find crate`.

Checks that hold:
- **Same connection, no new call types.** A gate flip is one `ReaderEffect::SetGrabSet`, which is the same path as
  mode, tab and reload changes and becomes one coalesced `SetKeyGrabs` generation. There is no Grab, Ungrab or
  reconnect. Settings reload keeps `open_gates` (`handlers.rs:862`), and a change in `keys.gated` counts as a key
  change.
- **No flip on a held key.** The source's generic guards apply to gated chords too. A modifier-grab change while a
  grabbed modifier is held is deferred (P-7). A chord or exact grab added or removed for a key that is down is
  deferred by `keystroke_conflict` (`source.rs:815-831`, which compares `cur.exact` with `want.exact`). Changing only
  keystroke grabs while KP_Insert is held does not touch the modifier set.
- **Mid-chord.** The lane uses the committed and wanted views. `Ambiguous` covers the pending window, and the
  predictor knows exact chords (`grab.rs:47`), so RG-6 holds.
- **Num Lock.** The gated set is exactly `(KP_Divide|KP_Multiply) × {0, Shift} × {0, Lock}` and never includes Mod2;
  a test asserts that and that the always-on and browse sets do not contain them. With Num Lock on, Mutter's exact
  match (`meta-a11y-manager.c:563`) grabs nothing, so the keys type. A Num Lock toggle changes no grab.
- **KP_Insert.** The gorca modifier set is unchanged in every mode. Laptop presets have no gated grab.
- **Flapping.** `PointerGate` closes immediately, and opens only after 2 s of wanting it and 2 s after the last open.
  That is at most one open and one close per 2 s. `Unresponsive` keeps the gate open (C-3), and the
  matches are exhaustive.
- **Presets.** NVDA chords in all four presets follow the owner's QM-1 decision (decisions.md). The preset test
  asserts no diagnostics.

### Root cause of the unexplained rc=101 (not a flaky test, and not MO-3)

`crates/gorca-daemon/tests/b1_ic.rs:44-51` runs `cargo build -p b1-fakeapp --offline` **into the same target dir**
while the outer `cargo test` is still running. On a cold or changed dir, that nested build recompiles shared crates
(gorca-model, gorca-rt, gorca-fuse, ...; `ws-1.log` lines 2140-2176, directly after `Running tests/b1_ic.rs`) with
b1-fakeapp's feature set. It replaces the rlibs the outer build produced. The doctests, which run last, then fail with
`E0463 can't find crate` across 35 crates. A warm second run passes because the nested build has nothing to do.
`shell_parity.rs`, `term_parity.rs` and `gorca-ext-host` tests also spawn cargo, but into a separate
`target/ext-wasm` dir.

Fix: give the nested build its own `--target-dir` (for example `<target>/b1-fakeapp`) and point `fakeapp_bin()` at
it, or prebuild b1-fakeapp in xtask before `cargo test`.

MO-3 run 3: rc 0. Run 1 is the only failure, and it is the only run in which `b1_ic`'s nested build recompiled
anything.

## MO-4 code review r1 (2026-10-19, 90f55ceb, branch mo4-pointer-actions)

Verdict: **MERGE**. The follow-ups below are low severity.

What I ran (reused the MO-3 target dir, then deleted it):
- `cargo clippy --workspace --all-targets -D warnings`: rc 0.
- `cargo test --workspace --no-fail-fast` twice: rc 0 both times. In run 1 the nested `b1-fakeapp` build was a no-op
  ("Finished in 0.10s"), which fits the MO-3 root cause.
- I did not rerun the private-shell e2e (10/10 reported).

Checks that hold:
- **Speech first.** Plain clicks speak at dispatch, then push one `ReaderEffect::Pointer`. The runtime runs it in a
  bounded `JoinSet` (16 tasks) with a 150 ms deadline and sends the reply as `ControlMsg::PointerReply`. No pointer
  I/O touches the key-to-speech path.
- **Lock state.** It changes only on confirmed replies; a press not reported as held goes to `Unknown`.
- **Recovery.** `Unknown` answers the next toggle with `ReleaseAll`. The 120 s expiry goes through the Reader timer.
- **Losing emulation.** Holds are dropped when the helper is lost. With `mouse.enabled=false` while calls can be
  made, a best-effort `ReleaseAll` goes out first.
- **Daemon.** `ShellHelperClient` runs on its own session connection, with a 5 s connect deadline. It is off for
  `--no-pointer`, shadow mode and no-keyboard. On shutdown it cancels and waits up to 1 s for the actor's release.
- **FSM.** `Hold` matches are exhaustive.

### Findings (all LOW)

- **Q-1, a lost reply wedges a lock.** `Pressing`/`Releasing` have no timeout. `spawn_pointer` answers the no-sink
  and over-16 cases with `try_send`, which is dropped when the control channel is full. `MAX_PENDING` overflow
  clears the pending table. In all three cases the hold stays `Pressing`/`Releasing`, and every toggle answers
  "Mouse busy" until helper loss or `mouse.enabled` changes. Fix: go to `Unknown` when a request is more than 1 s
  past due (piggyback on `pointer_timer`), or use `send().await` in a task.
- **Q-2, removing the cap during a drag.** If the owner removes `pointer` from the allow-list mid-drag, the avail
  becomes `NoCap`, gorca drops its holds, and the helper's `PointerRelease` needs the cap. The button stays down
  until the 125 s backstop. Fix in the helper: allow `PointerRelease` for any allow-listed caller, or release holds
  when a config change removes the holder's cap.
- **Q-3, disabling while unresponsive.** `mouse.enabled=false` while `Unresponsive` sends no release. That is
  covered by the backstop and acceptable; document it.
- **Q-4, the 120 s timer can be dropped.** A full Reader timer set (`MAX_TIMERS`) drops the hold timer silently;
  only the 125 s backstop remains.
- **Q-5, silent wiring failure.** `Core::with_pointer` does nothing without a log line if `Arc::get_mut` fails
  (wired after `shared` was cloned). Add a `debug_assert` or a warning.

## MO-4 follow-ups (717afb40) and MO-5a matcher (990dbda7) review r1 (2026-10-19)

Verdicts: **717afb40 MERGE** (one low finding). **990dbda7 MERGE AFTER FIXES**: W-1 is required, and W-2 is
strongly recommended.

What I ran, in my own detached worktree at 990dbda7 with a cold target dir, both deleted afterwards:
- `cargo clippy --workspace --all-targets -D warnings`: rc 0.
- `cargo test --workspace --no-fail-fast`: rc 0. This was a **cold** run that passed, so the b1_ic `--target-dir`
  fix works.
- A reviewer probe (`tests/rv_probe_matcher.rs`, not committed) that calls `resolve()` directly. It confirmed W-1
  and W-2.

### 717afb40

`PointerRelease` without the cap is safe against abuse. `_guard(cap=null)` still requires an allow-listed identity,
and the release happens only when `this._holder === ctx.sender`. A caller can therefore release only its own holds,
never another caller's. The cap watch (1 s, holder rechecked against the allow-list, fail-safe if the identity cache
entry is gone) is stopped in `stop()` and when the last hold is released.

- **R-1 LOW.** A capture-only allow-listed caller can now drain the **shared** pointer bucket (20/s, burst 40) by
  calling `PointerRelease` repeatedly, and gorca's clicks then get `rate:`. Fix: charge a non-holder's
  `PointerRelease` to the v1 bucket, or take no pointer token when the caller holds nothing.

### 990dbda7 matcher

- **W-1 HIGH - a click can land in another application's window (`matcher.rs`, the WM-1 fallback).**
  The pid filter runs only over *visible* windows. When the target app's window is on another workspace or is
  minimized, the filter comes up empty and the fallback takes **every visible window of every app**. Maximized
  windows share the same size, so a single foreign window matches WM-2 uniquely and no title check runs.
  The probe gives target pid 100 "Editor" (not visible) and visible pid 200 "Bank". The result is
  `Ok(window 2)`, and the helper's guard accepts it because the click is inside window 2.
  Fix:
  - apply the pid filter to all rows (visible or not);
  - if the pid owns any row and none of them is usable, return `NoCandidate`;
  - use the all-windows fallback only when the pid owns no row at all (the Flatpak case), and then require an equal
    title, with exactly one match;
  - add the probe's case to the table tests.
- **W-2 MED - the focused-window tie-break is a guess.** With two windows of the same app, same title and same
  size, the focused one is chosen even when the target node is in the other window (object navigation or review
  into a background window). The probe gives `Ok(window 2)`, the focused one. Fix: take the focused tie-break only
  when the hint says its toplevel is active (AT-SPI `STATE_ACTIVE`, carried in `ToplevelHint`); otherwise
  `Ambiguous`.
- Checked, fine:
  - windows come from a fresh `GetWindows` at call time, so a moved window is still right;
  - minimized windows and windows on other workspaces are excluded;
  - the XWayland ratio uses the global `x-scale`;
  - Chromium's `child_div` uses the scale of the matched window's monitor, and an unknown monitor gives
    `NoMonitor`;
  - every error maps to `NoLocation`.
  Multi-monitor needs no position comparison (Wayland AT-SPI origins are window-relative).

## MO-5a r2 (1a205918, on top of d924c9e3) review (2026-10-19)

Verdict: **MERGE**. L-1 and L-3 are required **before MO-5b item clicks**. They are not blockers for this branch,
because `route_to_review` only moves the pointer.

What I ran, in my own detached worktree with a cold target dir, both deleted afterwards:
- `cargo clippy --workspace --all-targets -D warnings`: rc 0.
- `cargo test --workspace` **fail-fast** three times, cold first, under load 17: rc 0, 0, 0.
- The reviewer probe adapted to the new hint fields. The other-workspace target now gives `Err(NotVisible)`, and two
  identical windows with an inactive toplevel give `Err(Ambiguous)`. W-1 and W-2 are fixed.

Also checked:
- **R-1** is charged to the general bucket.
- **No GNOME types reach the core.** The core emits `PointerAtReq { node: NodeKey, offset }`, and the connector
  returns a neutral `PointTarget`. The xtask dependency rules pass inside `cargo test`.

### Findings

- **L-1 MED (HIGH for MO-5b) - a target scrolled out of view inside the window aims at another control.**
  `locate.rs` clips the object's rectangle only against the **toplevel**. A list row scrolled below its viewport,
  but still inside the window's rectangle (a list pane above a status area, a sidebar, a web page under the
  toolbar), keeps a centre that lies on a *different* control of the same window. The helper's guard accepts it
  (same window), so the pointer, or later an item click, lands on the wrong control. Fix:
  - refuse when the node lacks `SHOWING` or `VISIBLE`, or has `OFFSCREEN`;
  - clip against the nearest scrolling ancestor (viewport / scroll pane / document frame) as well;
  - before an item click, verify the point with a deep `GetAccessibleAtPoint` that must return the node or a
    descendant, otherwise `ScrollTo` once or "No location".
- **L-2 MED - device-pixel (Chromium/Electron) rectangles are not clipped at all (`target_point`).**
  A page element scrolled out of view under the browser chrome, or into another part of the page, is aimed at
  blindly. Fix: pass the rectangle to the sink and clip there after dividing by the scale (the sink knows the
  monitor scale), plus the L-1 visibility check.
- **L-3 LOW/MED - an empty character rectangle falls back to the centre of the whole text node.**
  At a newline or end of line, or on an empty line, `range_extents` gives width 0, `sane_rect` rejects it, and the
  fallback is the centre of the whole text node. In a long text view that is a line far away from the review
  position. Fix: for a text offset, accept a zero-width rectangle (use a 1 px width), else try `offset - 1`; never
  fall back to the node centre for a text position.

### rc=101 (fail-fast runs)

Not reproduced: 0 failures in 3 fail-fast runs here (cold, load 17), plus 2 MO-4 runs and 1 cold MO-5a run after the
b1_ic fix. The only workspace failure logs left on disk (`/var/tmp/c2-test-workspace.log`: `gorca-sem-atspi` quirks;
`/var/tmp/ed/test4.log`: edit_model) are from other branches and earlier dates. Request: every agent keeps the full
output (`cargo test --workspace --no-fail-fast 2>&1 | tee docs/...log`) when it sees rc≠0, so the failing test name
is not lost. With fail-fast, rc=101 hides everything after the first failure.


## Implementer response to L-1..L-3 (mo5-matcher, MO-5b)

- **L-1 fixed** (`crates/gorca-connector-atspi/src/fetch/locate.rs`): `SHOWING` is required; the rectangle is clipped to the toplevel and to
  every scroll pane, viewport, document and inline frame ancestor; a hit test follows (`GetAccessibleAtPoint` from the toplevel, repeated on each
  answer because GTK3 answers one level at a time) and the final object must be the node, inside it, or an ancestor of it. Another branch is a
  disagreement. Ancestor answers are accepted because Gecko and GTK3 sometimes give a coarser object than the node; that is weaker than the
  requested "node or descendant", the sink's pick is the remaining check. Scrolled-out rows: the resolver says "No location"; item clicks (not
  route to review) then call `ScrollTo` once and locate again, because the user asked to act on that item. Unit tests: row inside the window but
  outside its pane, half-visible row.
- **L-2 fixed**: device-pixel rectangles are clipped by their scrolling ancestors (same space, no scale needed); the logical toplevel is still not
  used for them. Chromium/Electron hit test is skipped: measured in Electron, `GetAccessibleAtPoint` at a point on the button answers the
  document node (accessible/17), which is not an ancestor of the button (accessible/23 -> 21 -> 19 -> 16 -> 11 -> ... -> root), so it would refuse
  every click.
- **L-3 fixed**: an empty character rectangle takes the line's rectangle (`GetStringAtOffset` line + `GetRangeExtents`), then the previous character's;
  otherwise "No location". Only offset 0 of an empty text uses the node's extents. (Different from the proposal of a 1 px zero-width rectangle:
  the line rectangle is real data from the application.)
- Found on the way: `has_state` tested the wrong bit (it used the enum value as a bit index, but `atspi::State` values are masks), so the
  toplevel `active` hint was computed from the wrong bit since MO-5a. Fixed with a state-word test.
- Test runs: connector unit tests, see the gates in the final brief. `test_items` end to end at scales 1, 1.25, 2: see docs/adr 15.2 and
  scratchpad log; a Firefox first `DoAction` is sometimes dropped (retried in the test).

## MO-5b + L-1..L-3 review (2026-10-20, d480b36c, branch mo5-matcher)

Verdict: **MERGE**. F-1 and F-2 should be fixed in the next slice; neither can put a click outside the target window.

What I ran, in my own detached worktree with a cold target dir and JOBS=4, both deleted afterwards:
- `cargo fmt --check`: rc 0.
- `cargo clippy --workspace --all-targets -D warnings`: rc 0.
- `cargo test --workspace --no-fail-fast`: rc 0.
- I did not rerun the private-shell e2e.
- There is no `unwrap` in the new non-test code. Every IPC call runs under the Interactive budget deadline
  (`build::locate` and `try_item_action` both use `with_deadline`), and the helper call is limited to 150 ms.
- Nothing touches the live session.

### Findings (ranked)

**F-1 MED - accepting an ancestor weakens the hit test exactly where it matters (`locate.rs`, `hit_test_ok`, last loop).**
When the deepest object the toolkit reports at the point is an *ancestor* of the target, Gecko and GTK3 are saying
the target is **not** there. That happens with stale extents, a non-accessible overlay or a row that has moved. The
loop accepts any ancestor, including the document, a scroll pane or the toplevel. A pointer item click then lands on
whatever occupies that spot.

The coordinator's candidate rule ("ancestor only when the helper pick agrees") **does not help**: the helper's
pick resolves windows, not widgets, so it agrees whenever the point is in the right window.

Minimal safe rule: accept an ancestor answer only when it is the target's **direct parent** and the parent's role is
not toplevel, document, scroll pane, viewport or internal frame. That covers GTK3 labels and Gecko text leaves, which
are reported as their parent. Anything else is a disagreement: `ScrollTo` once, then "No location". Add a unit test
in which the answer is the document.

**F-2 MED - the e2e retry hides a user-visible defect (`test_items.py`, the left item click block).**
Every toolkit, not just Firefox, gets up to two extra `act-click` attempts, and the check passes on the last one.
For a user, a dropped first `DoAction` means "Left click" is spoken and nothing happens.
- The product must **not** retry blindly. A retry can double-activate toggles, submits and checkboxes, and there is
  no generic way to verify that a click took effect.
- Required in the test: record first-try success per toolkit as a separate check (or a measured rate), keep no more
  than one retry, and file the Gecko drop as a known issue with its rate.
- Product follow-up: investigate the cause (a first `DoAction` right after the window maps or focus changes is a
  likely candidate). If the cause is confirmed, add a per-toolkit settle rule, not a retry.

**F-3 LOW/MED - Chromium/Electron skip the hit test.** Acceptable for now, with these checks in place:
- `SHOWING`;
- the scrolling-ancestor clips (document);
- the helper's window guard and occlusion pick;
- action first, so the pointer path runs only for nodes without a click action.

Not covered: in-page overlays (sticky headers, cookie banners, modals) above the target inside the document.

The measurement that justified skipping the hit test may be a coordinate-space error. Blink reports children in
device px, but `GetAccessibleAtPoint(WINDOW)` may expect logical px. Re-measure at scale 1 and with point / scale. If
either agrees, enable the hit test (with the F-1 rule) for Blink too.

**F-4 LOW - `std::ptr::eq(wanted, CLICK_ACTIONS)` in `do_named_action` (review.rs) compares two uses of a `const`.**
The Rust reference does not guarantee that two uses of a constant share one address, so with several codegen units
the Gecko action-0 fallback can be silently off. Fix: pass an explicit `fallback_to_default: bool`, or make the lists
`static`.

**Q3 ScrollTo: OK.** It runs only for item clicks, so only on a user command (`route_to_review` never scrolls). There
is exactly one attempt, followed by one re-locate after 60 ms; a second failure gives "No location". It also runs
after a hit-test disagreement, which F-1 makes more frequent; that is acceptable and bounded.

**L-3: OK.** The fallback is the line rectangle, then the previous character, never the node centre (except offset 0
of an empty text). The `has_state` mask fix is correct; it affected the `active` hint since MO-5a.


## Implementer response to MO-5b review F-1..F-4 (mo5-matcher)

- **F-1 fixed**: `hit_test_ok` accepts the target, a descendant, or the direct parent whose role is not window, frame, dialog, document, scroll
  pane, viewport or inline frame; everything else is a disagreement (ScrollTo once for item clicks, then "No location"). Pure decision
  `hit_accepts(HitRel, parent_role)` with unit tests for target/descendant, ordinary parent, each refused container role, grandparent/other branch.
- **F-2**: the product does not retry. `test_items.py` now prints `FIRSTTRY` and has a separate "FIRST try" check; one retry at most.
  Measured on Firefox, scale 1, 6 runs each: first-try success 3/6 with no wait, 4/6 after a 6 s wait; earlier mixed-scale runs 9/12. The
  second `DoAction` always fired. Cause not found; a settle wait does not help, so no settle rule was added. Recorded as a known issue in
  ADR-0025 15.2. Chromium, Electron and GTK first-try succeeded in all runs.
- **F-3 partly measured; follow-up open (logical-coordinate re-measure at scale 2 not done)**: at scale 1 logical and device pixels are equal, and Electron's `GetAccessibleAtPoint(380,296)` still answered
  accessible/17, which is not in the button's parent chain (23, 21, 19, 16, 11, 9, 8, 3, 2, 1). So the disagreement is not a coordinate
  space error; the hit test stays off for device-pixel toolkits.
- **F-4 fixed**: `do_named_action` takes `default_when_unnamed: bool` (true for clicks, false for context menus).
