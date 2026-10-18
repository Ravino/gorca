# Keyboard report: Caps Lock, Insert and the keypad Insert (2026-10-18)

Three bugs from the daily user (laptop; Insert and/or Caps Lock as the gorca modifier; NVDA presets).

## 1. Caps Lock ON breaks browse-mode quick navigation (fixed)

**Root cause.** Quick-navigation keys are grabbed with `SetKeyGrabs` as `(keysym, modifier mask)`, and
Mutter compares both for exact equality (`refs/mutter/src/backends/meta-a11y-manager.c`,
`should_grab_keypress`). With Caps Lock on, XKB types `H` for the bare H key and `h` for Shift+H (the
alphabetic key type maps Lock to the Shift level, and Shift+Lock back to the base level). gorca grabbed
only `(h, none)` and `(H, Shift)`, each expanded with the Num Lock and Caps Lock mask variants, so
`(H, Lock)` and `(h, Shift+Lock)` were never grabbed: the key was not swallowed, reached the page, and
gorca echoed the capital letter. The input lane was already right: it matches by keycode and strips the
lock bits (`ChordKey` never contains them), so Caps Lock was never treated as Shift there.

**Fix.** `grabs_where` (`crates/gorca-core/src/commands/compile.rs`) adds the case partner of every
keysym of a letter key (`KeysymTable::case_partner`: ASCII by rule, other scripts from pairs the XKB
table builder records with `xkb_keysym_to_upper`, `crates/gorca-input-xkb`). Keys without case (digits,
punctuation, keypad, arrows) are untouched. Only real Shift selects the reverse direction, since the lane
looks chords up by keycode and the Shift bit.

**Caps Lock as the gorca modifier.** Mutter ignores the first press of a grabbed modifier (no XKB update,
so no toggle) and processes a double tap normally, so a double tap of Caps Lock toggles the lock, as in
NVDA. That is how the lock gets switched on by accident for a Caps Lock user; gorca announces it
(`quick.rs`). Nothing to change.

**Evidence.**
- Unit: `caps_lock_does_not_break_the_single_letter_quick_navigation_grabs`,
  `caps_lock_on_the_russian_layout_grabs_the_other_case_cyrillic_keysym`,
  `caps_lock_variants_add_only_case_partners` (compile.rs; they check the model and the wire form
  against Mutter's exact rule), `caps_lock_swaps_the_case_of_ascii_letters_and_registered_pairs_only`
  (keys.rs), `letter_keys_register_their_case_pairs_for_caps_lock` (xkb, real libxkbcommon).
- e2e `CL` (`crates/gorca-bench/src/e2e/web.rs`, run with `tests/web/m7/run_m7.sh <label> --only CL`):
  `H` with state Lock and `Shift+H` with Shift+Lock on a 10k-node page in a real Firefox.
  With the fix: lost 0, not consumed 0, p99 0.98 ms (`bench/results/e2e/kb-cl/`).
  **Negative control** (fix disabled, same binary otherwise): lost 150 of 150, not consumed 150 of 150
  (`bench/results/e2e/kb-cl-negative/`, kept).

## 2. Insert and Caps Lock are not always equivalent; Insert+F12 sometimes fails

**Checked.** All three keys are in `keyboard.modifiers` by default and are grabbed in Focus, Review,
Browse and learn mode (not in pass-next-key, by design). `gorca+f12` is one chord with the virtual GORCA
bit, so which physical key is held does not matter to the keymap. In every preset and layout, the same
chord runs the same command for Insert, KP_Insert and Caps Lock, with Caps Lock and Num Lock set or not,
and repeated within 200 ms (`insert_keypad_insert_and_caps_lock_run_the_same_commands`). The modifiers
are now one table (`MODIFIER_KEYS` in `gorca-input-api/src/keys.rs`: name, keysym, keycode, autorepeat),
read by `modifier_keysym`, the notices and the tests; `every_modifier_key_is_grabbed_in_every_mode...`
covers the grab sets. `km_*` in e2e `KM` measures the three at 0.8 to 0.9 ms p99.

**The real differences are in Mutter, not in gorca's tables.**
1. Insert and KP_Insert autorepeat, Caps Lock does not. Mutter 48.7 runs its double-tap rule for repeat
   events, so holding Insert past the repeat delay while looking for F12 can leak repeats to the
   application or leave a stale entry that swallows later keys (`docs/reports/live-keys-swallowed.md`,
   detected and announced as "stuck" by the lane). Holding Caps Lock while reaching F12 is harmless. This
   is the most likely cause of "Insert+F12 sometimes doesn't work" and matches Caps Lock working every
   time. Mitigations: tap the modifier and press F12 while it is down without long holds; or
   `modifiers = ["Caps_Lock"]`; the proper fix is the upstream `AUTOREPEAT_VALUE` guard.
2. The keypad Insert is the modifier only with Num Lock off. With Num Lock on, the same key types `KP_0`,
   which is a digit and not grabbed (ADR-0005 K-6a, same as NVDA and Orca). On a laptop with an embedded
   keypad the Num Lock state is easy to change by accident, so "Insert" is then silently a digit.
   Reading two Insert keycodes: Insert is 118 and KP_Insert is 90, both registered everywhere.

Unverified: which of the two the user hit; I could not reproduce "sometimes" without the laptop.

## 3. Numpad 0 types "0" on a quick double press (not reproducible with Num Lock off)

**Where the pass-through happens.** gorca injects nothing (`gorca-input-inject` is a stub). The double
tap is Mutter's: the second press of a grabbed modifier is "processed normally" and reaches the
application as the same key. I verified this against the real Mutter 48 in a private headless Shell
(`tests/shell/run_shell.sh`, RemoteDesktop keyboard, a real `gorca` daemon with `--impersonate-orca`
on the private bus, GTK3 and GTK4 text views, with and without IBus): KP_Insert, Num Lock off, tap-tap
delivers `KP_Insert` (65438, keycode 90) press and release to the application and no text.

**What types "0".** With Num Lock **on**, the same key is `KP_0` (65456, state Mod2): it is not a grabbed
modifier, so every press types `0` at once, single or double. The log of that run shows `0`, `00`, `000`
for three presses. Num Lock on a laptop is the only path I found that produces a digit from this key.

**Change.** None in behaviour (a spoken Num Lock hint was added and removed again after review: it fired
for ordinary number entry).
The lane tests pin both sides (`the_double_tap_of_the_keypad_insert_passes_the_key_itself`): Num Lock
off gives swallowed, swallowed, `ModifierPassThrough` with keysym KP_Insert; Num Lock on gives
`PassToApp` for every event. e2e `KM` asserts that after a double tap the application received exactly
the second press and release of the modifier itself, for all three modifiers
(`km_passthrough_wrong` = 0).

**Not done.** Making the keypad Insert a modifier with Num Lock on (grabbing `KP_0`) would swallow every
numpad zero for everyone who types numbers. It could be an opt-in setting; I did not add it. If the user
confirms Num Lock was off, the cause is elsewhere (keyboard firmware emitting a fake Shift, IBus engine)
and needs an evdev trace from the laptop (`evtest`).

## Autorepeat and Num Lock tracking (coordinator's hypothesis, checked)

- The lane classifies a repeat (a press of a key already down) as `PressKind::Repeat`; for a gorca
  modifier a repeat is only ever `Swallowed` or `PassToApp`, never `ModifierPassThrough`, and the replica
  keeps the modifier in `down_mods`, so a chord pressed during repeats still carries the GORCA bit. Tests:
  `autorepeat_of_a_held_modifier_is_no_double_tap_and_the_chord_still_runs` (Insert and KP_Insert) and
  `a_double_tap_then_hold_passes_once_and_repeats_do_not_pass_again`. No code change was needed.
- Pass-through never synthesises a key: `KeyInjector` has no implementation, nothing in the core sends
  keys. e2e `KM` now also holds Insert and KP_Insert for 900 ms (repeats at 30 ms) and checks that
  everything reaching the application is the modifier key itself (`km_passthrough_wrong` = 0).
- gorca keeps no Num Lock or Caps Lock state: it reads the Mod2 and Lock bits of each `KeyEvent`
  (Mutter's own state), so there is no cache to go stale at startup or after a Shell restart. The lock
  announcements come from the same bits.
- The Num Lock hint was removed after review (F-1): it fired for ordinary numpad typing with Num Lock on.

## Owner evidence (Num Lock off, double tap types "0") and what was tried

New facts from the owner: Num Lock is off; a single numpad Ins is fine; a fast double tap types "0" in
gedit; holding numpad Ins makes gorca speak the stuck-modifier hint (so Mutter 48.7's stale entry is live
on that machine).

Reproduction attempts, all in the private headless Shell with a real `gorca --impersonate-orca` and a
real `gedit --standalone` (text read back through AT-SPI), RemoteDesktop keyboard, Shell's own IBus
(`ibus-daemon` is started by the Shell, so the text-input path is the production one):
- double tap with 80, 150, 250 and 50 ms gaps, with and without a preceding 1 s hold (stale entry
  armed), repeated: gedit text never contains "0"; the application receives `KP_Insert` (keycode 90,
  65438) and nothing else (`bench/results/e2e/kb-km3`, KM).
- GTK3 and GTK4 text views, GTK IBus module on and off: same.
- Mutter source (`refs/mutter`): the pass-through is not a replay or a virtual device. `notify_key_in_impl`
  builds the event from the real xkb state (`xkb_state_key_get_one_sym`) before updating it; the second
  press of a grabbed modifier is simply not flagged `A11Y_MODIFIER_FIRST_CLICK`, so
  `meta_a11y_manager_notify_clients` returns FALSE and the event is queued like any key. IBus forwards
  (`clutter_input_method_forward_key`) re-emit with the keyval IBus supplies. No code path in Mutter or
  gorca turns keycode 90 at level 1 into `KP_0`. gorca has no pass-through mechanism of its own
  (`KeyInjector` is a stub; `ModifierPassThrough` only drives the learn-mode notice).
- Because the first press is swallowed, a single tap never shows the application's own Num Lock view.
  Only the passed second press does. A client whose idea of Num Lock differs from Mutter's (for example a
  window that kept a stale `wl_keyboard.modifiers` state, or an IBus engine with its own state) would show
  exactly this symptom: single tap silent, double tap "0". Unverified; the rig cannot produce the mismatch.

Because no fix can be justified without the real event stream, gorca now logs the relevant keys on demand
(`RUST_LOG=gorca_keys=debug`: only the configured gorca modifiers Insert, KP_Insert and Caps Lock are named;
Shift, Control, letters, digits and every keypad key are never logged, and a keypad Insert key that
arrives as a digit is not logged at all, since that would record each typed numpad 0).
Trace for the owner (read-only, nothing is grabbed, the live session is not touched):

1. `libinput list-devices | grep -B1 -A3 -i keyboard` to find the keyboard's `/dev/input/eventN`.
2. In one terminal: `sudo evtest /dev/input/eventN` (no `--grab`), and press numpad Ins twice quickly.
   Note whether anything besides `KEY_KP0` (code 82) appears (a `KEY_LEFTSHIFT` or `KEY_NUMLOCK` around
   the press is the signature of keyboard firmware emulating the keypad).
3. In another terminal: `sudo libinput debug-events --show-keycodes` for the same gesture (it shows what
   Mutter receives, also read-only).
4. gorca side: start gorca with `RUST_LOG=warn,gorca_keys=debug` and read `journalctl --user -u gorca -f`
   (or its stderr); each modifier event prints keycode, keysym, state (Mod2 = `0x10` means Num Lock on),
   press kind and class. A passed second press shows `ModifierPassThrough` with keysym `0xff9e`; a digit
   keysym is not logged, so a Num Lock mismatch shows as the absence of any line for that key.
5. In gedit (or `wev`), the keyval the application saw: `wev` prints `sym: KP_Insert` vs `KP_0`.

## Owner result (after the trace)

`evtest` on the real keyboard shows a clean `KEY_KP0` (no Shift or Num Lock emulation). The old live
build predicted Mutter's stuck modifier for KP_Insert twice during the test. After the session restart
the "0" no longer reproduces, which fits stale Mutter or client state that outlived the old Shell and
not a gorca pass-through. Nothing more is changed for it beyond the autorepeat tests and the
stuck-modifier hint that already exist.

## Typed-text logging

`TextKey` has a redacting `Debug` (`Typed { .. }`, like `LazyText`), so the `gorca_text` debug line no
longer records typed characters; a test pins it. The other tracing of text paths was checked: edit
deltas use `LazyText`, window and unit replies log character counts, and no tracing macro prints a
`KeyEvent` or `KeyIntent`.

## Gates

`cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`cargo xtask strings` (see the brief for results). Private-session tools used: `tests/shell/run_shell.sh`
(real Shell), `bench/e2e/run.sh --only KM`, `tests/web/m7/run_m7.sh --only CL`.
