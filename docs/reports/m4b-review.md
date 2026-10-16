# M4b review, round 1 (GNOME Shell and desktop polish)

Reviewed: branch `worktree-agent-a5fab50a294ec6e11` at `46062fc` (diff `main...46062fc`, main `b8521b1`).
Spec: `docs/plan/phase-2.md` M4, ADR-0006, ADR-0008, fairness threat model (`docs/decisions.md`, 2026-10-16).
Reviewer worktree: `.claude/worktrees/m4b-review` (detached). Probes: `docs/reports/m4b-review-probes/`.

## Verdict: MERGE AFTER FIXES

The structure is sound (window state machine, bounded dialog walk, per-process buckets, `AppPolicy` seam, data rule
table, Notification never takes the lane slot). But the fairness invariant "focus speech is never delayed or starved by
any number of apps" is broken by the window-activation path (H-1, two failing probes), the 40 ms grace leaves the race
it was added for open for a freshly arriving notification (M-1), and the daemon integration test `desktop.rs` fails
deterministically under load because its oracle does not accept the code's own degraded shape (M-2).

## Gates run (this review)

Host: 16 CPUs, governor performance, **load average 21 to 40** throughout (other agents' builds). Treat every timing
number as an upper bound; failing results are kept, not overwritten.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` (debug, load ~35) | 111 binaries, 1637 passed, **2 failed**, 16 ignored: `desktop.rs` `a_dialog_opening_speaks_its_title_its_text_and_the_focused_button_in_that_order` (line 188) and `the_shells_osd_level_is_announced_once_when_it_stops_changing` (line 369, said `[]`). A first fail-fast run also failed `b1-fakeapp` `storm_emits_signals_at_roughly_the_requested_rate` (2620 events, load 34; passed on rerun; not M4b code). |
| `desktop.rs` alone, 3 more runs (load 35-39) | dialog test failed 3/3; plain-window test failed 1/3 (line 210); OSD test passed 4/4 when run with the plain-window test only |
| A1/N1 strict, 3 repeats (`m4b-review-a`, load 31) | PASS on median: `a1_ms` p99 13.4 ms (repeats 12.1-**15.2**), `a1w_ms` p99 14.3 ms (6.9-**19.5**), `a1f_ms` p99 10.5 ms, `a1_split` 5 (4-17), `a1_wrong` 0, lost 0; N1 `n1_focus_ms` p99 5.5 ms, interrupted/text_wrong/lost 0 |
| R1/R1s, K2 strict | `m4b-review-r`, `m4b-review-k`: harness misconfiguration (reader started without `--impersonate-orca`, "no SetKeyGrabs"), kept and labelled. `m4b-review-r2`/`-k2` (load 40): R1 p99 4.9 ms FAIL, R1s 4.6 ms FAIL, K2 p99 4.9 ms, **K2 lost median 2 (max 3) FAIL** |
| A/B main vs M4b, interleaved, same harness (`m4b-review-ab-{r,k}-{main,m4b}`, load 21-33) | see below: main fails the same gates at this load |

A/B (median p99 over 3 repeats per run; main binary built from `b8521b1` via `git archive`, run with `--allow-stale-reader`):

| run (load at start) | R1 p99 | R1 lost max | R1s p99 | R1s lost max | K2 p99 | K2 lost max |
|---|---|---|---|---|---|---|
| m4b round 1 (37-38) | 5.24 | 4 | 5.33 | 5 | 5.69 | 1 |
| m4b round 2 (32-36) | 5.44 | 0 | 6.00 | 1 | 5.27 | 6 |
| main round 3 (27-31) | 4.53 | 2 | 4.72 | 2 | 3.26 | 1 |
| m4b round 3 (26-33) | 6.84 | 1 | 8.86 | 3 | 4.02 | 1 |
| main round 4 (21-24) | 3.38 | 1 | 6.50 | 2 | 4.59 | 0 |
| m4b round 4 (21-26) | 1.78 | 1 | 6.54 | 2 | 3.75 | 0 |

Reading: at this load **main loses R1 and K2 events too** (K2 lost 1 in main round 3, R1 lost up to 2), so the
single K2 `lost=1` in `m4b-gates-k4` is not evidence against M4b. I found no M4b code on the K2 path (no
Notification-priority speech, no window activation in that scenario; the lane changes only touch `Prio::Notification`
and an idle `Tick`). R1: no consistent regression (m4b 1.8 to 6.8 ms vs main 3.4 to 4.5 ms, overlapping); R1s skews
slightly higher on m4b in 2 of 4 pairs, which fits the hint-path lock in L-3 but is inside the noise. Needs a quiet-host
(`bench/b1/quiet-host.sh`) A/B before anyone claims either way.

## Findings

### H-1 (High) A window activation from any app, with no focus following, cancels the user's speech; during the 10 ms hold it drops the user's focus utterance entirely

`crates/gorca-core/src/present/window.rs:796-812` (`speak_window_alone`): when the `ALONE_DELAY` timer fires,
`interrupts == true` and the epoch is advanced **before** the code knows whether anything will be said. When the window
is not in the mirror (the app never answers the read) or the policy silences it (`window_silenced`), nothing is spoken,
yet the epoch advance has already stopped the current speech and purged queued speech that does not survive a focus
change. Activations are accepted from every application (`app_actor.rs` `WindowEvent::Activate`), not only the focused
one; the per-process limit is 4/s, so N processes cut the user's speech 4N times per second.

Worse, `release_held` (`window.rs:962`) drops the held focus utterance when `u.epoch < self.epoch.current()`. Another
app's `Alone` timer firing inside the 10 ms hold advances the epoch, so the user's focus (a freshly opened dialog's
button) is never spoken at all, and nothing re-requests it.

- Probe P1 `probe_p1_background_activation_does_not_silently_advance_epoch`: user focus spoken; app 2 activates an
  unknown window; at +120 ms: `epoch Epoch(1) -> Epoch(2), said []`. FAILS.
- Probe P2 `probe_p2_held_focus_survives_another_apps_alone_timer`: app 2's alone timer due 5 ms into app 1's hold;
  `the user's focus was never spoken: []`. FAILS.
- Violates invariant 1 of the 2026-10-16 threat model (focus speech never delayed or starved by any number of apps).

Fix: (a) an `Alone` for an application that is not the focused application never advances the epoch and speaks at
`Prio::Notification` (queued, fair-budgeted); (b) for the focused app, advance only after the snapshot is known and not
silenced; (c) a held focus utterance is dropped only by a newer focus (`window_on_focus` already clears `held`), not by
any epoch advance: remember the focus epoch in `Held` and compare against the Reader's last focus epoch instead.

### M-1 (Medium) The 40 ms notice grace only covers notifications already queued; a fresh one starts and is cut off

`crates/gorca-speech/src/queue.rs:164-179` applies `quiet_until` only in `pop`. `state.rs` `idle()` speaks any valid
`Intent` immediately. A notification that reaches an idle lane after the epoch advance but before the focus utterance
(the exact N1 race the grace targets: a key press, then an app announces in response) is spoken and then replaced by the
focus utterance; an assertive announcement that is promised to "survive a focus change" is lost after a blip.

- Probe P5 `probe_p5_fresh_notification_inside_grace_waits`: `notification started inside the grace: [Speak(UttId(2))];
  then the focus did [Replace { stop: UttId(2), then: UttId(3) }]`. FAILS.

Fix: in `SpeechQueue::handle`, while `now < quiet_until` turn an `Intent` with `Prio::Notification` into an `Enqueue`
(the lane already wakes at `next_wake`). Add P5 as a regression test.

### M-2 (Medium) `desktop.rs` fails deterministically under load; the 10 ms hold eats most of the A1 budget

- `crates/gorca-daemon/tests/desktop.rs:184-190`: the `late` escape only accepts "focus first". The real degraded shape
  when the window read misses the 10 ms hold but the mirror holds the dialog node is
  `["Delete item?, dialog, Confirm, button", "Item will be lost."]` (title led, dialog text after the button). Observed
  4/4 at load 35-39 (debug). The plain-window test also failed once (title after the focus, line 210). The
  implementer's "all pass" was not re-run after the last lane commit and does not reproduce here.
- The degraded shape is also a UX defect: the dialog's message is read after its button, which is the reading-order
  failure M4b exists to fix. On a loaded host it is the common case (`a1_split` 4-17 per 150 dialogs).
- A1 at load 31: `a1f_ms` (focus emit to speech) p99 10.5 ms; `a1_ms` repeats up to 15.2 ms and `a1w_ms` up to 19.5 ms.
  The hold is counted from the activation, so it costs at most 10 ms, but that is two thirds of the 15 ms budget on
  every cold dialog.

Fix: make the oracle accept both documented shapes (or assert the split counter), and consider pre-reading the dialog
on `ChildrenChanged`/`showing` of a `Dialog` so the hold rarely waits; alternatively hold only for dialogs (a plain
window's title can follow without breaking reading order).

### M-3 (Medium, static) Desktop reads share the 32-task review cap with user review navigation

`crates/gorca-core/src/runtime.rs:1112,1128` spawn desktop reads into the same `review_tasks` `JoinSet` that
`spawn_review` admits against `REVIEW_TASKS_MAX = 32` (`runtime.rs:1234`). Each desktop read may run `FETCH_BUDGET`
600 ms; per process 8 reads/s plus 4 activations/s, the verified Shell 64 probes/s. A few slow-but-alive apps (or a
slow Shell during an overview storm) can hold all 32 slots, and the user's numpad review keys then get
`NavFetchError::Refused`. Not executed (needs a slow multi-app rig); the code path is unambiguous.

Fix: a separate `JoinSet` with its own bound for desktop effects (drop desktop work when full, never review work).

### L-1 (Low) Waiting notices bypass the global speech ceiling

`crates/gorca-core/src/present/fair.rs:282-297` `take_due` spends only the owner's token. Probe P4: 64 waiting owners
release **64** utterances at once against `GLOBAL_SPEAK_BURST = 16`. Many-flooder starvation is an accepted limit, but
the documented ceiling is not what the code enforces. Fix: `take_due` also takes `global_take`, keeping the rest
waiting.

### L-2 (Low) The activation limit drops the newest activation, not the oldest

`window.rs:620-623`: over budget, the activation is discarded, so the title of that switch is never spoken. Probe P3
(9 Alt+Tab switches, one per 125 ms, alternating two windows of one process): the 8th utterance is `"Go, button"`
without "Other". At 5 switches 150 ms apart all titles are spoken, so a real Alt+Tab user is unaffected; the "last
activation is always spoken" property only fails above about 7 switches per second. Fix (cheap): do not charge an
activation that pairs with a focus in the same app (user-driven); only the `Alone` path needs the budget.

### L-3 (Low) The drain takes the global `PathTable` mutex for every `showing`/value event of every app

`crates/gorca-dbus/src/drain/raw.rs:603` calls `hint_seen` for each hint event; `paths.rs:186`
`insert_hint` locks the table just to read `hint_apps`, even for apps that will be refused. The B-2 workload now marks
half the state events as `SHOWING_NOW`. Fix: carry a `hints` flag on `AppRoute` (set at registration for the verified
Shell) or keep `hint_apps` in an `ArcSwap`. Not shown to regress R1s (A/B above is noise-bound).

### L-4 (Low) ADR-0008 drift: Shell product rules in the core

`notification.rs:187-189` reads `Dialog` nodes as notices when `has_policy(app)`: a Shell rule ("its dialogs have no
`window:activate`") decided by the core from a flag rather than by the policy. Same for the OSD quiet-then-read logic
gated on `has_policy`. Acceptable for M4b; when M5 lands, add a `Showing` moment so the extension decides.

### Notes (no action required for merge)

- **"Verified Shell"** = the registering connection's pid and process start time equal the protected Shell process
  (`gorca-connector-atspi/src/events/router.rs:271`). A spoofed name cannot pass; code running inside the Shell process
  (Shell JS extensions) can, by design. Non-Shell apps still get `showing` forwarded for nodes the mirror already knows
  as alerts, under the 8 reads/s per-process budget. OK.
- **Ext `Node` has no value field**: confirmed. `PolicyFacts.range` exists only on the native seam;
  `gorca-ext-api::Node` cannot carry it, so the `Hooks` impl in `ext-gnome-shell` cannot produce percentages. M5 port
  blocker, as the implementer says.
- **Probe logs (7.0 MB)**: `docs/reports/m4b-shell-probe-logs/` holds run1 to run6 with both plaintext `.events.txt`
  (up to 737 KB each) and `.json.gz` copies, plus the user's login name in end-session/polkit labels. Keep `run6`
  summaries and the `.json.gz` files, drop superseded runs and plaintext duplicates (or keep them outside git).
- **Lock screen and polkit are synthetic** in the probe (hand-built `ScreenShield`, stubbed `PolkitAgent` session).
  The lock window is role `window`, so it is announced only through focus, and the "Authentication error"
  announcement (assertive, repeated about once a second) is deduplicated within 500 ms only. Checklist items 23 and 24
  stay user-only.
- Focus speech under a notification storm: the lane design holds (Notification never takes the slot, lowest FIFO
  priority, N1 `n1_focus_ms` p99 5.5 ms at load 31). The one-flooder-vs-quiet-app invariant holds for speech, reads and
  activations (unit tests in `fair.rs` and `window.rs`); the breach is H-1's epoch path, not the buckets.

## Probes

`docs/reports/m4b-review-probes/window_probes.rs` (P1 to P3; install as
`crates/gorca-core/src/present/window/m4b_review_probes.rs` plus `#[cfg(test)] mod m4b_review_probes;` at the end of
`window.rs`) and `probes.patch` (the `window.rs` hook, P4 in `fair.rs`, P5 in `gorca-speech/src/queue.rs`). Run
`cargo test -p gorca-core probe_` and `cargo test -p gorca-speech probe_`. At `46062fc`: P1, P2, P4, P5 fail; P3 fails
in its 9-switch form (the original 5-switch form passes).

## Must fix before merge

H-1, M-1, M-2 (test oracle at least). M-3 is a small, contained change and should go in with them.
