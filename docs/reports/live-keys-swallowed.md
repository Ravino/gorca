# Live bug: ordinary keys swallowed, gorca speaks the key names (2026-10-14)

## Verdict

Root cause is a **Mutter 48.7 bug**, not gorca state. Holding a grabbed a11y modifier that autorepeats
(Insert, KP_Insert) for more than about 0.57 s leaves that modifier in Mutter's global
`grabbed_keypresses` table. From then on `should_grab_keypress` swallows **every** key whenever any
client has that modifier in its `SetKeyGrabs` modifier list. The table lives in the compositor, so it
survives gorca restarts, build changes and Orca/gorca switches ("saved and forgotten"). It is cleared
by one tap of the stuck modifier (or by emptying the modifier set, which is what CapsLock+F2
pass-next-key does, so that key "works"). Caps_Lock does not autorepeat and is not affected.

gorca's own state is innocent: no mode is persisted, nothing is defaulted on.

## Answers to the three questions

1. **Persisted/defaulted mode?** No. `~/.config/gorca/gorca.toml` holds only `[echo]` (written by
   `settings.save` at 01:44, i.e. the keys worked then). `Mode` is never restored: `Mode::InputHelp`
   is only built by `ModeMsg::ToggleInputHelp` (`crates/gorca-core/src/mode.rs:268`), sent only from
   `CoreCmd::InputLearnModeToggle` (`commands/handlers.rs:168-171`), i.e. the `gorca+1` chord. No state
   directory exists (`~/.local/state/gorca` absent). Startup grab set (measured, fake and real Mutter):
   modifiers `[Insert, KP_Insert, Caps_Lock]` plus 26 unmodified keypad strokes, `grab_all=false`.
2. **GrabKeyboard / Tab grabs?** `GrabKeyboard` is sent only when `set.grab_all`
   (`gorca-input-mutter/src/source.rs:800`), set only for InputHelp (`mode.rs:166`); every other set is
   followed by `UngrabKeyboard`. Tab is never grabbed outside learn mode. Mutter key grabbers are keyed by
   the *unique* sender and removed on `NameOwnerChanged` (`mutter meta-a11y-manager.c`
   `key_grabber_bus_name_vanished_callback`), not by the well-known name. A queued Orca is denied
   (`meta-dbus-access-checker.c`: only the current owner of `org.gnome.Orca.KeyboardMonitor`).
   BUT `grabbed_keypresses` and the seat's `pressed_modifiers` are global and never cleared on owner
   loss; that is the stale state.
3. **Why it speaks the key.** The input-lane replica of Mutter (`gorca-input-api/src/mutter_model.rs:145`)
   treats autorepeat as never first-click, so it believes Tab reaches the app: `KeyClass::PassToApp`
   -> `keys.rs:45-60` `key_echo_text` -> "tab" (echo `typed_characters = true`). Mutter in fact swallows
   it. So the user hears key echo for keys the app never gets. Not input help.

## Mechanism (Mutter 48.7, `src/backends/native/meta-seat-impl.c`)

`meta_seat_impl_notify_key_in_impl` calls `is_a11y_modifier_first_click(..., state)` for **every** event
including `AUTOREPEAT_VALUE` (state 2, treated as "press"). Upstream main has the guard
`if (state == AUTOREPEAT_VALUE) should_ignore = FALSE; /* never ignoring autorepeat ... */`; 48.7 does not
(`git show 48.7:src/backends/native/meta-seat-impl.c`, line 512, fetched into `refs/mutter` FETCH_HEAD;
`refs/mutter` checkout itself is 51.0 main and shows the fixed code, do not read it for 48.7 behaviour).
Repeats of the held modifier alternate ignorable / non-ignorable (`saw_first_release` toggles, a
non-ignorable repeat adds the key to `pressed_modifiers`). If the last repeat left it in
`pressed_modifiers`, the real release returns FALSE (non-ignorable), `notify_clients` returns early
(`is_grabbed_modifier_key && !is_ignorable`) and never removes the key that the first-click press added to
`grabbed_keypresses`. `tools/platform-probes/stuck-modifier/model48.py` reproduces the measured
thresholds exactly (stale for holds >= 0.57 s, ok at <= 0.56 s; 93 of 100 hold times in 0.50-1.49 s).

## Reproduction (private headless gnome-shell 48.7, real Mutter, RemoteDesktop key injection, GTK4 probe window)

| Case | Result |
|---|---|
| bare client, `SetKeyGrabs([Insert],[])`, hold Insert 0.3 s / 0.45 / 0.55 | Tab and a reach app |
| same, hold 0.7 s / 0.9 s | **nothing reaches app**; a tap of Insert fixes it |
| same with KP_Insert (kc 82) 0.9 s | stuck |
| same with Caps_Lock (kc 58) 0.9 s | fine (no autorepeat events) |
| gorca main 9d44caa binary (target/release, built 14:06), hold Insert 0.9 s | stuck |
| ... then stop gorca: Tab and a work (no grabber); start a NEW gorca: **stuck again** | persists across restarts |
| gorca, any key mix with random overlaps (350 sequences, fake Mutter replica) | no stuck state: the fake does not emulate autorepeat, so it cannot see this bug |

The build the user ran (before 9d44caa) was not run separately: the bare-client case has no gorca in it,
so the result does not depend on the build.

## Related observations

* Orca's own autostart is relaunched at each gorca start (gorca sets `ScreenReaderEnabled`; journal shows
  `app-gnome-orca-autostart` 0.1-3 s after `gorca.service` at 01:36:47 and 11:07:56). It queues for
  `org.gnome.Orca.KeyboardMonitor` behind gorca and takes it the moment gorca stops (live now:
  `org.gnome.Orca.KeyboardMonitor` owned by `orca` pid 2605925, started 11:07:56). Orca's own Insert grab
  triggers the same Mutter bug, so a stale entry left under Orca is inherited by gorca.
* A foreign owner holding `GrabKeyboard` (Orca learn mode) also swallows every key (measured with a
  stand-in owner); gorca then only reports "held by another screen reader". Not the user's case: gorca had
  the keyboard (journal 11:08/11:09: 195 and 386 keys seen by the lane).
* gorca never sets `comm=orca` (`set_thread_comm` has no caller outside tests); irrelevant here. Do not run
  `orca --replace`/`orca` in a private test: `pgrep -x orca` sees the live Orca (refused in my run).
* Single stale non-modifier keys also appear in real Mutter under overlapping hold/release patterns
  (2 of 350 random sequences: one Tab or one a lost once, self-healing on the next press). Same table, not
  analysed further.

## Fix proposal

1. **Immediate (user):** tap Insert once to clear. Persistent workaround: `[keyboard] modifiers =
   ["Caps_Lock"]` in gorca.toml (drops Insert and KP_Insert; Caps_Lock never autorepeats).
2. **gorca, minimal and safe:** make the replica bug-compatible with 48.7 (run `first_click` for repeats,
   `mutter_model.rs:145`, and `Timing::timing` line ~72 `ev.repeat` branch) **and** heal: when a
   repeat-capable gorca modifier was held past `repeat_delay` and released, wait `repeat_delay + 50 ms`
   with no key down, then inject a tap of that modifier with the planned `KeyInjector` (K-2; today
   `gorca-input-inject` is a stub returning `NotAvailable`). The tap is a first click for Mutter, so it is
   swallowed and removes the stale entry (verified by hand: one real tap clears it). Until the injector
   exists, fall back to a spoken notice ("Insert stuck, tap it").
   Cheaper variant for now: do not default `KP_Insert`/`Insert` as grabbed modifiers when running on
   Mutter < the fixed release; keep Caps_Lock.
3. **Upstream/Debian:** one-line backport of the `AUTOREPEAT_VALUE` guard to mutter 48.7; file upstream
   issue. Not doable on the live session without restarting gnome-shell.
4. **Test gap:** `tests/harness/fake_mutter.py` should emulate 48.7 autorepeat (state 2 through
   `is_a11y_modifier_first_click`) so a gate catches this; `model48.py` is the reference.

Not recommended: `repeat=false` in gsettings (removes key repeat for the user), re-grabbing without the
modifier mid-hold (P-7 XKB desync risk, loses chord swallowing).

## Evidence / files

* `tools/platform-probes/stuck-modifier/{bare.py,replay2.py,model48.py,fresh.sh,README.md}`
* scratch logs: `/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/lk/` (rp-*.err, rf*.out)
* Journal: 11:07:56 gorca start, Orca autostart 11:07:56, lane stats keys=195/386 ambiguous=9/17, stop 11:09:56.
* Private rig only; nothing touched the live session (read-only journalctl, gsettings get, busctl list, ps).
* Unverified: the user's exact trigger (long Insert hold) is inferred from the symptom; the live Mutter
  table cannot be read. Not tested: the heal by injection (injector missing), Num Lock on with KP_Insert.

## Read-only checks for the user

    busctl --user list | grep -i KeyboardMonitor          # who owns the name (expect gorca, or orca if gorca is off)
    ps -o pid,lstart,args -C orca                         # is an Orca running next to gorca
    gsettings get org.gnome.desktop.a11y.applications screen-reader-enabled
    journalctl --user -u gorca -n 40 --no-pager           # look for "holds the keyboard" notices

Then, with gorca on and keys swallowed, press and release Insert once: if keys come back, this is the bug.

## Resolution (2026-10-14)

### 1. Replica now matches 48.7 (`crates/gorca-input-api/src/mutter_model.rs`)
`Replica::step` runs the first-click rule for autorepeat (as a press), like 48.7. Because gorca sees receipt
times and Mutter's repeat timer never fires early, `Replica::timing` resolves a repeat's time rule by position,
not by measured dt: the first repeat after a press is "late" (>= repeat_delay), later ones are "soon". No guard-band
forks for repeats. The replica also tracks which grabbed modifiers are physically down (`down_mods`):
`Verdict::stale` = a grabbed modifier sits in `grabbed_keypresses` while up. While stale, `gorca_held` is false, so a
swallowed key is never read as `gorca+key` (it would have run a command), and the lane classifies it `Swallowed`
(never `PassToApp`, so no key echo). Unit tests reproduce the measured thresholds (stale at >= 570 ms, clean at
<= 560 ms) and the one-tap heal. The replica cannot know the stale state after a gorca (re)start, because Mutter keeps
it across clients; the first long hold after start is the first thing it can predict.

### 2. Heal: none without injection, so gorca tells the user
Measured on real Mutter 48.7 in a private headless shell (`tools/platform-probes/stuck-modifier/heal.py`), each
followed by a Tab/a probe in a GTK4 window: re-sending `SetKeyGrabs([Insert],[])`; `SetKeyGrabs` with more
modifiers; `GrabKeyboard`+`UngrabKeyboard`; `UnwatchKeyboard`+`WatchKeyboard`; `SetKeyGrabs([],[])` then
GrabKeyboard/Ungrab then re-adding Insert; releasing `org.gnome.Orca.KeyboardMonitor` and a new owner grabbing Insert:
**all still swallowed**. Dropping Insert from the modifier set lets keys through (the entry is ignored, not removed) and
the entry is back the moment Insert is grabbed again. Code reading explains why: `grabbed_keypresses` is changed in
exactly two places, the press path adds, and the release path of `notify_clients` removes, so only a key event of the
stale keysym that is not an unignored grabbed-modifier event can remove it (`refs/mutter` `meta-a11y-manager.c`
lines 649-679). Control: with Insert not grabbed, one physical Insert tap clears it for good.
Decision: no automatic heal. The first time the replica predicts the stale state gorca speaks
"Insert stuck, tap Insert" (`Keypad Insert` / `Caps Lock` named too), once per episode; a tap re-arms it
(`LaneOut::stale_modifier`, `InputNotice::ModifierStuck`). Not done: the injection heal (K-2 injector is a stub).
A cheaper permanent option remains `[keyboard] modifiers = ["Caps_Lock"]`.

### 3. Orca autostart
What gorca wrote (`gorca-connector-atspi/src/connector.rs`, ID-5): `org.a11y.Status.IsEnabled` **and**
`ScreenReaderEnabled` on the session bus (`org.a11y.Bus /org/a11y/bus`), not the GSettings key directly. In
at-spi2-core 2.56.2 the launcher implements `ScreenReaderEnabled` as a proxy of the GSettings key: measured in the
private session (keyfile backend), `Set ScreenReaderEnabled=true` wrote `org.gnome.desktop.a11y.applications
screen-reader-enabled=true` and `toolkit-accessibility=true`; `Set IsEnabled=true` wrote only `toolkit-accessibility`.
`gsd-a11y-settings`/gnome-session then start Orca from that key, which is the autostart in the journal.
Fix: gorca writes `IsEnabled` only and leaves `ScreenReaderEnabled` exactly as found (`enablement.rs`, ADR-0001 ID-5
amended, `docs/platform/org.a11y.Bus-and-Status.md`). Verified in the private session with the real binary:
before/running/after = `sre=false` throughout (28a18be: `sre=false -> true`, left true).
App enabling with `IsEnabled=true, ScreenReaderEnabled=false`: Firefox ESR connected to the a11y bus, and did not with
both false (control); GTK3 and GTK4 connect regardless. Chromium is not installed here: by source it reads `IsEnabled`
(`atk_util_auralinux.cc:155`) and finds the reader by process name, so it is unaffected (unverified live).
**Restore on stop was broken**: after SIGTERM 28a18be left both properties true. `TaskSet::spawn` selects on the
cancel token and drops the future, so the connector's shutdown code never ran (no "atspi connector stopping" line).
Fixed with `TaskSet::spawn_graceful` for the connector and by restoring first in the connector's tail; the restore
also skips when `ScreenReaderEnabled` became true meanwhile (a user-started Orca). Verified: `IsEnabled` false -> true
(running) -> false (stopped), `ScreenReaderEnabled` false throughout; with a screen reader already active (both
true) nothing changes at start or stop. The e2e harness still sets both properties on its own private bus (F-9).

### 4. Harness and regression gate
`tests/harness/fake_mutter.py` emulates 48.7 autorepeat (`repeat` flag on `key`, `hold` command, `set_autorepeat_guard`
for upstream behaviour). New e2e scenario **K4h** (`m1.rs::k4h`, 5 episodes of 0.9 s+ Insert holds): hint exactly once,
no echo of swallowed keys, no hint for a 450 ms hold, one tap heals. Result with this tree: all five metrics 0
(`bench/results/e2e/k4h-new`); negative control with the pre-fix binary 28a18be (`--allow-stale-reader`):
`stuck_echoed=9`, `stuck_hint_wrong=5` FAIL (`bench/results/e2e/k4h-old-control`).
