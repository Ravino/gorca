# M1 platform measurements: P-7, P-8, P-10, P-13, P-14, P-15, XKB CapsLock

Date 2026-10-13. Host: Debian 13, kernel 7.1.8 RT, GNOME Shell/Mutter 48.7, libei 1.3.901, libwayland 1.23.1, GTK 4.18.6.
Author: linux-platform-engineer. Nothing was committed.

## Method and safety

* Every measurement ran in a PRIVATE headless gnome-shell (`contrib/gnome-shell-extension/headless-shell.sh` copy, own
  dbus-daemon, own Wayland display `lp-m1-wl`, keyfile gsettings). The probe refuses to start if the bus address contains
  `/run/user/`. The live shell (pid 532394) was never signalled, no grab or name ownership on the real bus.
* gorca's real authorisation path was used: the probe owns `org.gnome.Orca.KeyboardMonitor` on the private bus
  (no `--unsafe-mode`, no debug env var), calls `WatchKeyboard`, `SetKeyGrabs`, and subscribes to `KeyEvent`.
* Keys are injected through `org.gnome.Mutter.RemoteDesktop` -> `CreateSession` -> `ConnectToEIS({device-types:1})` -> `Start`
  -> libei sender (evdev keycodes). Key delivery to a real Wayland app is observed with a GTK4 client (`tools/platform-probes/gtkkeys.py`).
* Code: `tools/platform-probes/m1probe.c` (modes `latency`, `rtt`, `settle-mod`, `settle-key`, `settle-neg`, `reinject`, `p13`, `skew`, `keymap`,
  `sanity`, `press`) and `gtkkeys.py`. Build line in the C file header (also needs `xkbcommon`).
* Harness findings: (1) the headless shell starts with the Overview open, which blocks key focus for new windows; one Super tap
  closes it (`OverviewActive` property). (2) Another agent's `pkill`/kill took down my private shell twice (15:47, 15:58) because
  all test shells use Wayland display `gorca-test-0`; I switched to a unique display name. Test rigs should kill by exact pid.
  (3) Mutter's `wl_seat` has no keyboard capability until a keyboard device exists (the RemoteDesktop/EIS device provides it).

## P-7: when does Mutter apply `SetKeyGrabs`?

Source (`refs/mutter/src/backends/meta-a11y-manager.c`, `native/meta-seat-impl.c`):
`handle_set_key_grabs` rewrites the keystroke list and `all_grabbed_modifiers` synchronously on the main thread, emits
`a11y-modifiers-changed`, and only then calls `complete_set_key_grabs`. The emit queues `set_a11y_modifiers` on the input thread
(`meta_seat_impl_run_input_task`, G_PRIORITY_HIGH idle). That input-thread copy decides the `FIRST_CLICK` swallow flag. There is NO ack
beyond the method reply and no signal.

Measurement (injected key at delta us after the async call was sent; "applied" = app did not receive the press for an ADD, did receive it for a REMOVE).
"reply-before" = cases where the method reply had already arrived before the key was injected.

| Grab kind | Direction | cases with reply before inject | of those NOT applied |
|---|---|---|---|
| keystroke `a` (mask 0) | add | 76 | 0 |
| keystroke `a` | remove | 83 | 0 |
| modifier Insert | add | 39 | 0 |
| modifier Insert | remove | 35 | 0 |
| modifier Caps_Lock (xkb state probe) | remove | 63 | 0 |

296 of 296 cases: once the reply is in, the grab is in force. Raw tables: keystroke add/remove deltas 0/100/300/1000/3000 us give
applied = 0/1/11/49/60 of 60 (add) and 0/3/4/60/60 (remove), i.e. the transition tracks the reply time, not a later input-thread delay.
`SetKeyGrabs` reply RTT (1000 calls, sync, private idle shell): min 201 us, p50 876 us, p90 1461 us, p99 1663 us, p99.9 22.4 ms.
Caveat: an earlier Caps_Lock-based "add" probe was invalid (CapsLockState does not reveal a swallowed first click reliably); the
Insert/GTK probe replaced it.

Verdict: the 5 ms settle in ADR-0001 section 7.3 is not needed for ordering; the reply is the barrier. Keys that arrive before the reply
are classified by the old set, which is exactly the `Ambiguous` rule. The only residual risk is input-thread starvation, which I
could not provoke. Recommended: `Pending` ends at reply + 1 ms (configurable; keep 5 ms only as the safe default until K7 data exists);
send deadline 200 ms is fine given p99.9 22 ms.

Hazard found: changing the MODIFIER grab while that modifier is physically down desynchronises XKB. If the press passed and the
release is then swallowed as a "first click", Mutter's xkb state never sees the release; Caps_Lock then stuck on and later presses did not
toggle it, and `KeyEvent.state` kept the Lock bit (0x2). Related K-6 evidence: a keystroke grab `(a, mask 0)` did not match while CapsLock was on
(state 0x2), so the 4 NumLock/CapsLock variants are mandatory. Do not change modifier grabs while a modifier is held.

## P-8: re-injection (short verdict)

Feasible, no prompt, using Mutter's direct interface (`CreateSession` needs no caller check; no `access_checker` in `meta-remote-desktop*.c`).
The portal path prompts: with `org.freedesktop.portal.RemoteDesktop` on the private bus, `CreateSession` and `SelectDevices` return
immediately, `Start` produced no Response (consent dialog pending). Minimal code path (works in `m1probe.c setup()`):
`CreateSession` -> `ConnectToEIS(a{sv}{device-types:u=1})` returns an fd -> `ei_new_sender`, `ei_setup_backend_fd`, bind keyboard on
`SEAT_ADDED`, wait `DEVICE_RESUMED` -> `Start()` -> `ei_device_keyboard_key` + `ei_device_frame`. Round trip until it is a `KeyEvent` ~0.7 ms (P-10).
Semantics verified with the GTK client:
* injected keys DO come back as `KeyEvent` (suppress with the expectation table);
* an injected key that is still in the grab set is swallowed again (app saw 0); after a sync `SetKeyGrabs` that removes it (883 us RTT) the injected key reaches the app;
* chord keys while a grabbed modifier is held are swallowed; the next key after the modifier is released is not;
* EIS ignores client timestamps (uses `g_get_monotonic_time()` in `meta-eis-client.c`), virtual repeats are dropped.
Since pass-next-key empties the grab set, P-8 is off the M1 critical path; the above is the M7 recipe.

## P-10: key to `KeyEvent` latency with gorca owning the name

Definition (ADR-0001 table, row P-10): "End-to-end key -> KeyEvent latency with gorca owning the name". Measured: injection call (just
before `ei_device_frame`) to `KeyEvent` handler in a spinning GDBus client, private idle shell, 3000 samples each:

| Path | min | p50 | p90 | p99 | p99.9 | max |
|---|---|---|---|---|---|---|
| watch only | 123 us | 712 us | 1173 us | 1756 us | 2037 us | 4535 us |
| key in grab set | 121 us | 696 us | 1738 us | 7694 us | 24.8 ms | 28.9 ms |
| (1000-sample first run, watch only) | 130 us | 457 us | 845 us | 3822 us | 11.5 ms | 11.5 ms |

Includes the EIS socket hop and the GDBus worker-to-main hop, so it is an upper bound for a physical key, but also includes compositor
frame noise (the host was shared with other agents). Assumption "sub-millisecond p99" is NOT met in this rig (p99 1.8 to 3.8 ms; grabbed
tail 7.7 ms). Still far below the 5 ms diagnostic threshold at p50/p90; the p99 tail needs a quiet-host rerun with a real keyboard.

## P-13: autorepeat of a held, grabbed Insert

1.5 s hold, compositor repeat 500 ms delay / 33 ms interval:

| Case | KeyEvents to gorca | GTK4 client presses |
|---|---|---|
| Insert, no grab | 35 | 35 |
| Insert grabbed as modifier | 35 | 0 |
| 'a' held while Insert held (chord) | 35 | 0 |
| Caps_Lock grabbed | 1 | 0 |

No leak to apps here. Reason from `meta-wayland-keyboard.c:278-284`: repeated events are sent only to `wl_keyboard` v10 (`REPEATED`)
and skipped for older; the live compositor advertised `wl_seat` version 8 (`WAYLAND_DEBUG`: `wl_registry.global(16, "wl_seat", 8)`, libwayland 1.23.1)
although `META_WL_SEAT_VERSION` is 10. Source also shows why it WOULD leak on v10: repeats get `should_ignore = FALSE`, and the main-thread check
`is_grabbed_modifier_key && !is_ignorable` returns "not grabbed". So: not an issue on this stack; becomes one when libwayland/mutter advertise v10
and a client binds it. Consequence for gorca: gorca itself receives 34 repeat presses with no repeat flag in `KeyEvent`; the input lane must debounce
(press while already down = repeat).

## P-14: Mutter double-tap cutoff vs gorca's view

Rule (`is_a11y_modifier_first_click`): gap = release1 -> press2 of the same grabbed modifier, `< repeat_delay` (here 500 ms from
gsettings `delay`); timestamps are Mutter's event times in ms. EIS events are stamped with `g_get_monotonic_time()` at main-thread
dispatch (client times ignored); real keyboards use libinput kernel times.
Scan with Insert grabbed, 250 trials with injected gap uniform in [490, 510] ms, pass detected by the GTK client:
* coarse scan 130 trials: every gap <= 497 ms passed, every gap >= 503 ms did not;
* fine scan: cutoff at 499.5 +- 0.3 ms of injected gap (7/250 mispredicted by the injected gap, all within 0.5 ms below 500);
* gorca's receipt gap minus injected gap: p50 +238 us, p99 +2324 us, range -7.8 to +7.0 ms;
* a gorca predictor `recv_gap < 500 ms` mispredicted 2/250 trials, both within 0.7 ms of the cutoff.
Verdict: skew is under 1 ms around the decision boundary on an idle host and under 8 ms worst case in 250 samples, so G = 15 ms (ADR-0005) holds
with margin; widen only if K7 shows more. Not testable here: real-keyboard timestamps (kernel stamps, ms quantisation adds up to 1 ms).

## P-15: gorca XKB keymap vs Mutter

xkbcommon 1.7.0 compiled from RMLVO `evdev / pc105+inet / us,ru / (no options)` (the private session's defaults) and compared against the
keysym in `KeyEvent` for every evdev keycode 1..247 (233 keys, modifier/lock/power keys excluded), mask taken from `KeyEvent.state` (Mod2 = 0x10 NumLock, Lock = 0x2):

| State | Compared | Mismatches |
|---|---|---|
| group 0 (us), plain / Shift / NumLock on | 233 each | 0 |
| group 1 (ru) plain / Shift (gorca state set to group 1) | 233 each | 0 |
| ru active but gorca assumes group 0 | 233 | 35 (all letters and punctuation) |

So the keymap matches provided gorca knows the active layout group, and `KeyEvent.state` does NOT carry the group (state was 0x0 under ru).
Gorca must therefore track the group (gsettings `current` / `mru-sources`, Shell input-source change, or learn `keycode -> keysym` from observed events, K-8).
`KeyEvent.keysym` is already group-resolved, so command matching on the keysym is safe; only key naming and echo for keys that were not observed need the table.
The layout switch itself (Super+Space injected via EIS, handled by the Shell) worked in the private session.

## XKB CapsLock (read-only on the live settings)

* `gsettings get org.gnome.desktop.input-sources xkb-options` -> `@as []` (empty).
* `sources` -> `[('xkb', 'us'), ('xkb', 'ru')]`; `current` 0; `mru-sources` `[('xkb','ru'),('xkb','us')]`; `xkb-model` `pc105+inet`.
* `localectl status`: X11 layout/options unset; `/etc/default/keyboard` absent; `switch-input-source` is `['<Super>space']`.
Verdict: CapsLock is NOT used for layout switching on this host; Caps stays a valid gorca modifier now.
Auto-drop rule to implement (evaluated at startup and on `xkb-options` change): drop Caps_Lock as the gorca modifier if any option matches
`grp:*caps*`, `lv3:caps_switch*`, `lv5:caps_switch`, `compose:caps*`, `caps:*` other than `caps:internal*`/`caps:shift*`/`caps:capslock`/`caps:shiftlock`
(remaps such as `caps:escape`, `caps:swapescape`, `caps:backspace`, `caps:super`, `caps:hyper`, `caps:menu`, `caps:numlock`, `caps:none`
change the keysym, so a `Caps_Lock` grab would never match or would grab Escape), `ctrl:nocaps`, `ctrl:swapcaps`, `ctrl:hyper_capscontrol`. The option list is in
`/usr/share/X11/xkb/rules/evdev.lst`. Insert is unaffected by these and has the same keysym in every layout.

## Open items

* P-10 and P-14 need a quiet-host rerun with a physical keyboard (EIS-path numbers only here).
* P-13 must be rechecked when Mutter starts advertising `wl_seat` v10.
* Input-thread starvation (P-7 tail) not provoked.
