# M4b implementation report: GNOME Shell and desktop polish

Scope: `docs/plan/phase-2.md` section M4 minus M4a. Checklist for the user: `docs/plan/checklists/M4b.md`.
Real-Shell facts from a private headless Shell: `docs/reports/m4b-shell-probe.md` (when present) and
`docs/reports/m4b-shell-probe-logs/`.

## What was built

| Area | Where | Behaviour |
|---|---|---|
| Window activation | `gorca-core/src/present/window.rs` | `window:activate` is paired with the focus event that follows (either order) into one utterance: title, "dialog"/"alert" for those roles, the dialog's static text, then the focus. A window with no focus is spoken alone after 120 ms. An activation after its own focus queues behind it. A dialog the mirror lacks is read by a task (`fetch_window`), and the focus waits for it at most 10 ms (`WINDOW_HOLD`), else the text follows as a queued utterance. |
| Announcements | `present/notification.rs` | `object:announcement` (GTK `announce`) is spoken with `Prio::Notification`; assertive ones survive a focus change, polite ones die with it. |
| Notification banners | same | the Shell sets `showing` when a banner actor is mapped (probe: the `notification` node gets `state-changed:showing`). The connector flags it (`PropMask::SHOWING_NOW`), the app actor forwards it for the verified Shell (or for a node already known as an alert), the Reader reads the node and its static text and speaks "alert, title, body". Buttons are not read. |
| OSD | same + `ext-gnome-shell` | the OSD is a level bar that emits `accessible-value` changes (probe: about a dozen per 100 ms) and a sibling label ("Volume"). The Reader waits 120 ms of quiet, reads the bar and its label, asks the Shell extension, and speaks "Volume, 75 percent" once. |
| Never interrupts | `gorca-speech` | `Prio::Notification` never takes the lane's interrupting slot and never interrupts in the state machine even with a newer epoch (it waits behind focus speech, say-all, key echo). |
| Fairness | `present/fair.rs`, `directory.rs` | one token bucket per owner (a process, from the bus credentials) and kind (speech 3/s, reads 8/s, Shell probes 64/s, window activations 4/s), a global speech ceiling of 16/s, a 4096-entry table (idle entries are reclaimed, busy ones never evicted, newcomers beyond that share 64 hashed buckets), at most 64 owners with a waiting utterance (newest kept, older dropped and counted). Reconnecting, new bus names, other paths/roles/text do not reset it. Pid-less connections share one owner. |
| Role data | `gorca-connector-atspi/src/role_map.rs` | `notification` and `info-bar` map to Alert, `level-bar` to ProgressBar (connector quirk table, data). |
| Shell extension | `extensions/ext-gnome-shell` | a rule table (`RULES`, stable ids): Shell window focus claim is silent, 0..1 sliders speak percent, the OSD level says name and percent. Facts in, verdict out, only `gorca-ext-api` as a dependency; also a `Hooks` impl for the M5 host. The core sees it through the `AppPolicy` trait (`builtin_ext.rs`); `gorca-daemon/src/shell_policy.rs` is the native adapter (panic caught, three faults switch it off). |

## Gates

Host: busy workstation, load average 9 to 17 during every run (recorded in each result header), governor performance, 16 CPUs.
Head of the final runs: `15c0d1c` (rebased onto main `f80d1f5`).

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask deps`: clean.
- `cargo test --workspace`: all pass (111 test binaries ok) on the rebased head before the last lane change; `gorca-core`,
  `gorca-speech`, `gorca-daemon` re-run after it (core 510 tests, daemon `desktop.rs` 6 end-to-end tests with the real daemon).
- e2e, strict flags, `--impersonate-orca`, 3 repeats, median p99 (results in `bench/results/e2e/m4b-gates-*`):

| gate | result | threshold |
|---|---|---|
| K2 `k2_ms` p99 | 0.31 ms | 5 ms |
| K3 `k3_ms` p99 | 1.20 ms | 15 ms |
| K4 | app_lost, app_doubled, chord_leaked, flap_failed 0 | 0 |
| R1 `r1_ms` p99 (unchanged) | 0.43 ms (k-run r3/earlier: 0.42 to 0.49); final run r4: median 2.1 ms, repeats 0.35 to 2.27 (load 13, a noisy repeat set; still under the gate) | 3 ms |
| R1s `r1s_ms` p99 | 0.41 to 0.51 ms median in r/r3; r4 median 1.47 ms (repeats 0.46 to 1.53); one earlier repeat 4.7 ms (m4b-gates-r) | 4 ms |
| A1 `a1_ms` (dialogs) p99 | 6.4 ms (repeats 5.4 to 12.2) | 15 ms |
| A1 `a1w_ms` (plain windows) p99 | 7.9 ms (repeats 6.0 to 12.5) | 15 ms |
| A1 `a1_wrong`, `lost` | 0, 0 | 0 |
| N1 `n1_focus_ms` p99 (newer focus behind a queued notice) | 0.9 ms | 15 ms |
| N1 `n1_interrupted`, `n1_text_wrong`, `lost` | 0, 0, 0 | 0 |

Kept failing or flaky results (not overwritten, labelled by directory):

- `m4b-gates-a` and `-a2`, `-a3`: A1 `a1_wrong` fails (320 of 800; then 0 median with one repeat of 26; then 0) and N1
  `n1_interrupted` 1 to 2. Causes found and fixed in this order: the harness probed faster than the per-process activation limit
  (4 per second); a loaded host missing the 10 ms hold, where the dialog text correctly follows as a split (A1 now counts that as
  a split); and a real race in the speech lane, where an epoch advance let a queued notification start in the gap before the
  focus utterance that caused it and be cut off. The lane now keeps a queued notification waiting 40 ms after an epoch advance
  (`NOTICE_GRACE`, test in `queue.rs`). `m4b-gates-a4` is clean.
- `m4b-gates-k4`: K2 `lost` = 1 in one of three repeats (1199 spoken for 1200 chords), load 13. Not reproduced in five further
  repeats (`m4b-gates-k5`, lost 0 in all). K3 and K4 clean. No Notification-priority speech is involved in K2; recorded as a
  rare host-load flake, not root-caused.


## Decisions worth knowing

- Notices and window speech have three consumers of one fairness table; the speech lane stays untouched except for the
  Notification rule.
- Cross-app starvation is bounded, not eliminated: a flooding principal spends only its own bucket; many principals
  together hit the global ceiling, which drops (counted) rather than queues.
- Not covered here: web live regions (the document actor in browse mode owns them; they do not pass through
  `present/notification.rs`).
- The Shell policy cannot see value ranges through the current `gorca-ext-api::Node`; M5 should add a value field to the WIT
  `node` record (TBD already in the file) so the same `RULES` run as a WASM component.

## Accepted fairness limits

Threat model: a misbehaving or flooding local app, not a determined adversary. Invariants kept (each with a test): focus and
key-echo speech never pass through a notice budget, so no number of apps delays them; one flooding process spends only its own
buckets and cannot silence a quiet process's notices (reads, speech and window activations); all tracking state is bounded
(4096 owners, 64 overflow buckets, 64 waiting utterances, 8 pending activations, 8 answered reads, per-owner node memory capped).
Accepted, not chased:

- Many busy principals together hit the global speech ceiling (16 per second); a quiet app speaking in that instant is dropped
  (counted), not queued.
- A principal reconnecting keeps its owner (the pid), so the spent bucket carries over; a reused pid meets a bucket that has
  refilled (it refills within a second). Pid-less connections share one owner.
- More than 4096 simultaneously busy processes share 64 hashed buckets (a quiet one can collide with a flooder).
- The speech lane's FIFO (32) is shared; per-owner budgets bound each producer, not the sum.
- Web live regions are the document actor's, outside these budgets.
- A burst of window activations above four per second from one process skips the app read, but the newest activation is kept and spoken (L-2).

## Review fixes (round 1, `docs/reports/m4b-review.md`)

| finding | fix | test |
|---|---|---|
| H-1 | `window.rs`: the epoch advances only in `emit_window_line`, only when an utterance really goes out, only for a window of the app the user last focused (`LedMode::Focused`) and within that owner's speech budget. Any other app's window is a polite notice (`LedMode::Background`, `Prio::Notification`, `notice_speak`). A held focus is released (never dropped) before a same-app title | `a_background_activation_that_says_nothing_never_touches_the_epoch` (P1), `a_held_focus_survives_another_apps_alone_timer` (P2), `a_silent_window_of_the_focused_app_does_not_advance_the_epoch_either`, `a_background_windows_title_is_a_polite_notice...`, `many_apps_activating_windows_never_advance_the_epoch_or_exceed_the_speech_ceiling` |
| M-1 | `queue.rs`: inside the 40 ms grace a fresh `Notification` intent on an idle lane is enqueued like a queued one | `a_fresh_notification_inside_the_grace_waits_and_is_not_cut_by_the_focus` (P5), `...after_the_grace_starts_at_once` |
| M-2 | Reading order is deterministic. Variant chosen: keep the 10 ms hold and speak the focus at once on a miss (A1 unchanged), then re-announce the complete line once, in order (title, role, text, focus), queued behind it (`LedMode::Lead`). Waiting for the read instead would put the app's reply time into every missed A1 sample (p99 would equal the wait bound, 3 to 11 percent of dialogs at load 30); re-announcing costs one repeated button name on a loaded host only. `desktop.rs` accepts exactly the two shapes; the A1 harness counts a split only when the complete line follows (`a1_wrong` is stricter) | `desktop.rs` dialog and plain-window tests, `a_slow_window_read_does_not_hold_the_focus_beyond_the_bound` |
| M-3 | `runtime.rs`: desktop reads have their own JoinSet bound (16); review keys keep all 32 slots | `slow_desktop_reads_can_never_use_up_the_review_slots` |
| L-1 | `fair.rs::take_due` spends the global ceiling and `next_due` includes it; the notice timer re-arms at least 1 ms ahead | `waiting_notices_released_together_respect_the_global_ceiling` (P4) |
| L-2 | An activation over the per-owner budget skips its read but is kept, evicting the owner's oldest pending one, so the last Alt+Tab window leads its focus | `the_last_alt_tab_title_is_spoken_even_over_the_activation_budget` (P3, 9 switches) |
| L-3 | `AppRoute.hints` (set from the verified Shell at registration): the drain takes the path-table lock for hint events only for that app | existing router/paths tests |
| L-4 | "Shell dialogs read as notices" is the rule `shell.dialog-notice` (`Moment::Showing`, `Verdict::Notice`) in `ext-gnome-shell`; the core only asks the policy. The OSD quiet-then-read logic still keys on `has_policy` (candidate for M5) | `a_dialog_that_appears_reads_as_a_notice...`, core notice test with a policy |
| ext Node | WIT `node` and `gorca_ext_api::Node` gained `range: option<range>`; `ShellExtension::present` now yields the percent for a unit slider | `the_extension_api_speaks_a_unit_slider_as_a_percentage` |
| probe logs | 7.0 MB to 0.7 MB: only `run6/*.json.gz` and summaries kept; login name replaced by `user` (it remains in earlier git history of this branch: squash before publishing) | |

Found while running `desktop.rs` under load (24 spinning shells, load 17 to 30; 2 of 8 runs failed before, 0 of 15 after):
the connector resolved the Shell's pid/start time concurrently with the first app discovery, so the Shell could register as
unverified (no extension, no `showing` watch) and its banner or OSD was never read. The connector now resolves the Shell
before the first discovery (`resolve_shell`, `connector.rs`). Pre-existing, not specific to M4b, but real at gorca start-up.

Gate runs after the review fixes (code head `8022fcc`; private session, `--impersonate-orca`, 3 repeats, host load 11 to 20,
governor performance, other agents building):

- A1/N1 strict (`m4b-fix-an`, load 15): `a1_ms` p99 6.3 ms (repeats 5.5 to 20.5; the 20.5 repeat had 12 splits), `a1w_ms` 5.2 ms,
  `a1f_ms` 3.9 ms, `a1_wrong` 0, `lost` 0, `a1_split` median 0 (max 12); N1 `n1_focus_ms` 0.48 ms, interrupted/text_wrong/lost 0.
- K2 p99 0.39 ms, lost 0; K3 p99 1.2 ms, lost 0; K4 app_lost/doubled/chord_leaked/flap 0 (`m4b-fix-k`, load 14 to 17).
- Kept failing results, labelled: in `m4b-fix-a` and `m4b-fix-k` (load 14 to 20) R1 p99 3.4/4.5 ms, R1s 4.0 ms and K4
  `chord_resent_per_10k` 12 exceeded their gates with one noisy repeat each (up to 13 ms) and `lost` 0. Interleaved A/B of main
  (`62e9a19`) against this branch, same harness, 2 rounds x 3 repeats (`m4b-fix-ab-{main,m4b}-{1,2}`, load 12 to 18): R1 p99 main
  0.45/0.40 ms vs branch 0.41/0.41 ms; R1s 0.47/0.48 vs 0.44/0.44; K4 resent 0 vs 0; lost 0 everywhere. The failures were host-load
  spikes of the individual runs, not a branch regression.
- `desktop.rs` under 24 CPU spinners (load 17 to 30): 15 of 15 pass (before the Shell-identity fix 2 of 8 failed).
- `cargo test --workspace`: 1663 passed, 0 failed; fmt, clippy `-D warnings`, `cargo xtask deps` clean.

Not done: the oldest-pending eviction across apps (`PENDING_MAX` 8) still removes the globally oldest entry when 8 different
apps have one pending each within 120 ms; the 10 ms hold is unchanged (pre-reading dialogs on `showing` would cut misses but
needs a node-role guess for cold mirrors).

## Risks and open items

- Lock screen and polkit were probed in the private Shell only to the extent the probe report says; the live checks are the
  user's (checklist 23, 24).
- `WINDOW_HOLD` (10 ms) is a latency/ordering trade-off: on a very loaded host the dialog text arrives as a follow-up.
- Disk: sibling worktrees' `target/` directories (26-31 GB each) brought the root filesystem to 100% twice during this work;
  builds here ran with `CARGO_INCREMENTAL=0` and no debug info.
