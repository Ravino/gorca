# M4b review, round 2 (fix round)

Branch `worktree-agent-a5fab50a294ec6e11`, head `844d319` (code `8022fcc`), rebased on main `210e144`.
Round 1: `docs/reports/m4b-review.md` (46062fc). Reviewer worktree: `.claude/worktrees/m4b-review`, detached at `844d319`.
Probes: `docs/reports/m4b-rereview-probes/` (`probes-r1-r2.patch` applies the round-1 probes P1 to P5 plus the new
round-2 probes Q1 to Q6 to `844d319`; `window_probes_r2.rs` is the new probe file on its own).

## Verdict

**MERGE (squash), with follow-ups.** Every round-1 finding is fixed and every round-1 probe now passes. No new High
finding. Two Medium UX items on the new code (M-2 duplicate speech, H-1 user switch to a focus-less app) and one known
Low (cross-app pending eviction) remain, all reproduced by probes and none a stability or latency risk. A squash merge is
required (login name and 7 MB of probe logs in branch history).

## Gates (measured)

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` (debug, load 14 to 17) | 112 binaries, 1663 passed, 0 failed, 16 ignored |
| Round-1 probes P1, P2, P3 (window), P4 (fair), P5 (queue) | **all 5 pass** (all failed at 46062fc) |
| A1/N1 strict, 3 repeats (`bench/results/e2e/m4b-rereview-an`, load 10.8 to 12.8) | `a1_ms` p99 5.46 ms (5.33 to 5.76), `a1w_ms` 3.58, `a1f_ms` 3.19, `a1_split` 0, `a1_wrong` 0, lost 0; N1 `n1_focus_ms` p99 0.62 ms, interrupted/text_wrong/lost 0. PASS |
| R1/R1s/K4 A/B vs main, same harness | see below |

Interleaved A/B, release, private session, `--impersonate-orca`, `GORCA_M1=1 GORCA_M4=1`, 3 repeats each, governor
performance, other agents building (results in `bench/results/e2e/m4b-rereview-ab-*`; the main binary is `210e144`
run with `--allow-stale-reader`, so its header says `git_rev=844d319`):

| run (load at start) | R1 p99 (min to max) | R1s p99 (min to max) | R1/R1s lost | K4 lost/doubled/leaked/resent/flap |
|---|---|---|---|---|
| m4b 1 (12.6) | 0.41 (0.41 to 0.47) | 0.50 (0.46 to 1.66) | 0/0 | 0/0/0/0/0 |
| main (14.4) | 0.41 (0.41 to 0.49) | 0.44 (0.41 to 0.46) | 0/0 | 0/0/0/0/0 |
| m4b 2 (12.7) | 0.46 (0.42 to 0.48) | 0.45 (0.44 to 0.49) | 0/0 | 0/0/0/0/0 |

No regression against main at load 11 to 14: all gates pass on both, within 0.05 ms of each other. The round-1 R1/K2
failures at load 21 to 40 were host load, as the implementer's own A/B also showed.

## Round-1 findings, verified

| id | status | evidence |
|---|---|---|
| H-1 epoch advanced by silent/background window; held focus dropped | **fixed** | `window.rs:850-876` `emit_window_line` is the only epoch advance, only for `LedMode::Focused` with a speech token; `alone_mode` (`:830`) makes other apps `Background` and releases a held focus before a same-app title. P1, P2 pass. New vectors: Q6 (same-app alone timer due exactly at hold release) passes: focus spoken once, epoch untouched; Q5 (focused app alternating activations of two windows every 101 ms for 10 s) = 32 epoch advances, i.e. bounded by the 3/s Speak budget; implementer's 58-app storm test passes. |
| M-1 fresh notification on idle lane inside grace | **fixed** | `queue.rs:85-96`; P5 passes. Grace starvation: `quiet_until` is reset by every `EpochAdvanced`, so a notice on an idle lane waits as long as advances keep arriving < 40 ms apart. After H-1 only user input (keys with `interrupt_on_keys`, mode keys) and focus changes advance the epoch; focus changes speak, so the lane is not idle. Bounded by the user's own input; released 40 ms after it stops. Accepted. Pre-existing and out of scope: `flow.rs:135` advances the epoch for every focus event, including a background app's (not verified in this round whether the focus lane filters those). |
| M-2 dialog reading order under load | **fixed, with a UX cost** (see N-1) | `desktop.rs` accepts exactly two shapes; A1 `a1_wrong` now requires the complete line. |
| M-3 desktop reads share review JoinSet | **fixed** | `runtime.rs` `desktop_tasks` bounded at 16, joined in the loop; review keeps 32. |
| L-1 `take_due` skips global ceiling | **fixed** | P4 passes; `notice_speak` re-arms at least 1 ms ahead (no busy loop). |
| L-2 activation limit drops newest | **fixed** | P3 passes (9 switches); over budget the read is skipped but the activation kept. |
| L-3 path-table lock per event | **fixed** | `AppRoute.hints` from `verified_shell` at registration; drain checks it before `hint_seen`. See N-4. |
| L-4 Shell dialog rule in core | **fixed** | `shell.dialog-notice` rule in `ext-gnome-shell`; core asks the policy (`notification.rs:187`). |
| Shell identity race (implementer's own find) | **fixed** | `connector.rs` `resolve_shell` before first discovery, under the Interactive / 250 ms credentials deadlines (no unbounded wait). Not re-run under spinners this round. |

## New findings

### N-1 (Medium, UX) The M-2 re-announcement repeats the focus, and not only on a hold miss
`window.rs:1070-1084`: whenever a `LedMode::Lead` window gets a read answer with something new, the whole line
(title, role, texts, then the focus segments `led.tail`) is spoken again. Two paths reach it:
- Q1 (hold miss, fresh plain window): the user hears `"Back, button"` then `"Files, Back, button"`. Before the fix it was
  `"Back, button"` then `"Files"`.
- Q2 (no hold at all): the dialog title is in the mirror, its label is not, and the focus arrives 12 ms after the
  activation (past `WINDOW_HOLD`, so `window_read_pending` is `None` and nothing is held). The user hears
  `"Delete file, dialog, Delete, button"` then `"Delete file, dialog, This cannot be undone., Delete, button"`: title
  and button twice. `desktop.rs` explicitly accepts this shape.
Neither NVDA (foreground event: title plus `getDialogText`, then focus, never repeated) nor Orca (speaks what each event
says, in event order) repeats the focus. On an idle host A1 shows 0 splits in 300 samples, so this is a loaded-host or
slow-app behaviour, but slow apps are exactly where it happens. Fix: when the answer arrives within ~150 ms of the
focus utterance, send the complete line at `Prio::Interrupt` in the same epoch, so the lane's replace path
(`state.rs:159`, atomic on voicego) swaps it for the focus speech that is still playing; after that, speak only what was
missing (title, text) as a queued follow-up, as round 1 did.

### N-2 (Medium, UX) A user's own switch to an app that sends no focus is a polite notice
`window.rs:836-840`: `alone_mode` returns `Background` whenever the window's app is not the app of `last_focus`. When
the user Alt+Tabs (or clicks) to an app that does not emit a focus event within 120 ms (a viewer with no focusable
widget, some Electron apps, an app slow to answer), the title is `Prio::Notification` with no epoch advance (Q3: prio
`[Notification]`, epoch unchanged). Usually harmless because the Alt+Tab key itself advances the epoch when
`interrupt_on_keys` is on, but with that setting off, or on a mouse click, the previous app's stale speech keeps
playing and the new window's title waits behind it. Also, after a Shell switcher, `last_focus` is the Shell, so any
target app counts as background. Fix: treat a window as `Focused` when its app holds the AT-SPI `active` state or the
activation followed a user key or pointer action within ~500 ms (the input lane already timestamps key presses), else
`Background`.

### N-3 (Low, known) Eight background activations within 120 ms evict the user's dialog title
`window.rs:754-758`: `PENDING_MAX` (8) evicts the global oldest. Q4: the user's dialog (fully known to the mirror)
activates, eight other apps activate 1 ms later, the focus arrives at 3 ms: the user hears `"Delete, button"` only.
`PENDING_PER_APP` does not help because each flooder has one entry. Needs a hostile multi-process source, so Low. Fix:
on overflow evict the oldest entry of an app other than the one in `last_focus` (or of the app with the most entries),
never the newest activation of the focused app.

### N-4 (Low, pre-existing, now frozen) A Shell verified after registration never gets hint paths
`router.rs:767` calls `allow_hints` and `router.rs:666` sets `AppRoute.hints` only when the Shell is verified at
registration. The fetch actor re-verifies later (`fetch/mod.rs:257-290`), but the route flag and `hint_apps` do not
follow. `resolve_shell` before discovery makes this rare (it fails only if `GetNameOwner` or the credentials call misses
its deadline at start-up). Fix: on a late verification, call `allow_hints` and flip the route flag (an `AtomicBool`).

### N-5 (Low) A1 does not measure the complete line on a split
`a1_ms` stops at the first SPEAK that carries the button; on a split that is the button-only utterance, and the time
to the title and text is not reported anywhere. Add `a1_full_ms` (activation to the first utterance that contains the
title and the text) as an informational metric so the cost of N-1 is visible in numbers.

### N-6 (Low, nit) `ShellExtension::present` emits an empty text segment for an unnamed unit slider
`extensions/ext-gnome-shell/src/lib.rs` `Verdict::Value(v)`: `n.name.clone().unwrap_or_default()` yields `""` when
the slider has no name; and the role word is dropped compared with the core's presentation. Skip the segment when the
name is empty.

## ext `Node.range` and the M5 branch

`git merge-tree` of this branch with M5 (`worktree-agent-a5aefe82e077800c6`, head `ea2c1ee`): textual conflicts in
`Cargo.lock`, `gorca-core/src/app_actor.rs`, `app_actor/tests.rs`, `reader.rs`, `runtime.rs`,
`gorca-daemon/src/lib.rs`, `docs/decisions.md`. The WIT file and `gorca-ext-api/src/api.rs` merge textually, but the
result does **not** compile: M5 builds `wt::Node` / `api::Node` without `range` in `gorca-ext-api/src/guest.rs`
(`node_to_wit`, `node_from_wit`), `gorca-ext-host/src/convert.rs:102` (`node_to_ext`), `gorca-ext-host/src/wasm.rs:449`
and `gorca-ext-host/tests/fuzz.rs:198`. Resolution for whichever branch lands second: add `range` to those five
constructors (`convert.rs` maps `Value::Range { current, min, max }` from the snapshot; `guest.rs`/`wasm.rs` convert
`wt::Range` both ways), rebuild every guest `.wasm` (the record layout changed under the same `gorca:ext@0.1.0`
package; a stale component fails to instantiate), and resolve `runtime.rs` keeping both M5's extension wiring and this
branch's `desktop_tasks` set. Recommend merging M4b first (smaller), then rebasing M5 with these edits.

## Probe logs and history

The tree at `844d319` has no login name in any M4b file (the three hits are pre-existing on main:
`.claude/settings.json`, `docs/reports/2026-10-12-voicego-p0-fixes.md`, `extensions/ext-vte/src/prompt.rs`). Branch
history still carries it: 12 files in each of the 10 commits from `45806a1` to `e63a084`, and about 9.4 MB of blobs over
`main..HEAD`. **A squash merge is required**; a merge commit or rebase-merge would publish both.

## Probe summary (`probes-r1-r2.patch` on 844d319)

| probe | asserts | result |
|---|---|---|
| P1 | background activation with nothing said does not advance the epoch | pass |
| P2 | held focus survives another app's alone timer | pass |
| P3 | last Alt+Tab title spoken over the activation budget | pass |
| P4 | `take_due` respects the global ceiling | pass |
| P5 | fresh notification inside the grace waits | pass |
| Q1 | hold miss does not repeat the focus | **fail**: `["Back, button", "Files, Back, button"]` (N-1) |
| Q2 | no hold, late text does not repeat title and focus | **fail**: title and button twice (N-1) |
| Q3 | user switch to a focus-less app interrupts | **fail**: `[Notification]`, epoch unchanged (N-2) |
| Q4 | 8 background activations do not evict the user's dialog title | **fail**: `["Delete, button"]` (N-3) |
| Q5 | focused app activation flood bounded | pass: 32 epoch advances in 10 s (3/s budget) |
| Q6 | same-app alone timer at hold release neither drops nor cancels the held focus | pass |
