# Review: keyboard Caps Lock / Insert / keypad Insert (b21ca599)

Date: 2026-10-19. Reviewed commit: b21ca599 (branch `worktree-agent-aa129b307e9aa1778`) against `main`.
The review ran in a clean detached worktree at b21ca599, because the branch worktree had uncommitted
implementer edits (`keys.rs`, report). Later commits get a delta review.

**Verdict: MERGE WITH FIXES** (F-1 and F-2 below).

## Gates (clean b21ca599, CARGO_BUILD_JOBS=4)

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` | ok |
| `cargo test --workspace` | 80 test binaries ok, 0 failed. One `b1-fakeapp` doctest failed with `error[E0463]: can't find crate for gorca_testkit` while another build was using the same target dir. It passes when rerun alone (`cargo test -p b1-fakeapp --doc`). The branch does not touch that crate. |
| `cargo xtask strings` | `ok (332 literals, 113 allowed)` |

No new `unwrap`/`expect` in non-test code. The one new `unsafe` (the `xkb_keysym_to_upper` call) is a
pure FFI function, and it has a SAFETY comment. No new state enums, so there are no exhaustiveness
changes.

## Probes (run, then removed; patch kept in the reviewer scratchpad)

1. `probe_rv_caps_lock_does_not_invert_direction`: a real `InputLane` over the compiled nvda/desktop
   keymap with the Browse grab set, keycode 43:
   - `h`/none gives NextHeading and `H`/Shift gives PrevHeading.
   - `H`/Lock gives **NextHeading** and `h`/Shift|Lock gives **PrevHeading**. `H`/Lock|Mod2 gives NextHeading.
   - Result: PASS. Caps Lock does not invert the direction. This matches NVDA, where gestures come from
     the virtual key plus real Shift and the lock state is ignored. The lane builds the chord as
     `KeyRef::Code` + `without_locks()` (lane.rs ~230).
2. `probe_rv_grab_counts`: Browse grab set before vs after. "Before" is the same set with the partner
   chords filtered out. Wire strokes are counted after the lock-variant expansion in `to_wire`.

| table/preset | chords | wire strokes | payload (8 B/stroke) |
|---|---|---|---|
| us/nvda | 101 → 148 | 432 → 620 (+44%) | ~3.5 → ~5.0 KB |
| us/orca | 73 → 114 | 316 → 480 (+52%) | ~2.5 → ~3.8 KB |
| us+ru/nvda | 107 → 158 | 456 → 660 (+45%) | ~3.6 → ~5.3 KB |
| us+ru/orca | 79 → 124 | 340 → 520 (+53%) | ~2.7 → ~4.2 KB |

The Focus set is unchanged (nvda: 0 plain chords, 26 exact).

## Findings (ranked)

### F-1 (Medium): the Num Lock hint fires during ordinary number entry

`crates/gorca-core/src/commands/handlers.rs` `numlock_hint` (called from `keys.rs` in the `PassToApp`
branch).

**Scenario:** `KP_Insert` is a default modifier (the branch's own test
`every_modifier_key_is_grabbed...` asserts this), and many desktop users keep Num Lock on. The first
time such a user types `0` on the numpad in a run, the hint fires: in a spreadsheet, a calculator, or a
form field. They hear "Num Lock is on, so keypad zero types a digit and is not the gorca key. Turn Num
Lock off to use it", although they never tried to use it as the gorca key. This repeats after every
gorca restart. It is noise in the hot path of typing, and NVDA and Orca say nothing here.

The owner has since reported Num Lock **off** for the actual bug, so the hint does not address the
reported failure.

**Fix:** drop the hint, or fire it only on evidence of modifier intent. Evidence means one of:
- `KP_0`+Mod2 still held when another non-modifier key is pressed (an attempted chord);
- a `KP_0`+Mod2 double press within the double-tap window.

Keep the once-per-run flag, and add a test that a single `0` typed with Num Lock on is silent.

### F-2 (Low): the direction is not pinned by a test, and the CL e2e cannot see an inversion

**Unit tests:** `caps_lock_does_not_break_the_single_letter_quick_navigation_grabs` checks only that
the key is grabbed (wire and replica), not which command runs.

**e2e:** `web.rs::cl` accepts any utterance containing "heading level", so a Next/Prev inversion would
still pass. The fake Mutter takes the keysym and state as given, so XKB's Caps Lock translation is
asserted, not exercised.

**Provenance:** the `kb-cl` and `kb-cl-negative` results were recorded at `git_rev=9aabf7f6`,
`git_dirty=true`, not at the committed head.

**Fix:**
- Port probe 1 above as a unit test in `compile.rs` tests.
- In CL, check alternation: after `H`/Lock the heading index increases, and after Shift+`h`/Shift|Lock
  it decreases.
- Rerun CL at the committed head.

### F-3 (Low, informational): grab-set growth and the 00:09:50 Shell crash

- **Growth:** +44–53% wire strokes, about 5 KB per `SetKeyGrabs` at most.
- **Mutter has no limit:** `refs/mutter/src/backends/meta-a11y-manager.c` `handle_set_key_grabs`
  appends to an unbounded `GArray` in O(n). Per-key matching is a linear scan over the keystrokes, so
  about 620 entries add a negligible per-key cost.
- **Frequency and re-register churn are unchanged:** the branch does not touch
  `gorca-input-mutter` (source.rs send path, monitor.rs `GrabsTimeout` retry → `lost` → re-register).
  The set is pushed on the same mode and letter/Tab toggles as before; only the payload is larger.
- **No evidence that a 5 KB payload causes a 2 s `SetKeyGrabs` timeout or a libmutter segfault.** That
  crash path is in the retry/re-register logic on main, and it needs its own investigation
  (re-registering while Mutter is wedged).

**Optional trim (halves the growth):** a partner keysym only ever arrives with the Lock bit set. Put
partners in `exact` as `mods|LOCK` and `mods|LOCK|MOD2` instead of in `chords`, which `to_wire`
expands to 4 lock variants. The unlocked partner strokes, (`H`, none) and (`h`, Shift), cannot be typed
on standard layouts.

### F-4 (Low): case pairs are a global keysym→keysym map

- `KeysymTable::case` is a global map, so the last insert wins.
- The ASCII rule short-circuits the map. On Turkish, Caps Lock+`i` types `Iabovedot`, which is never
  grabbed, and `I` maps to both `i` and `ı`.
- Result: an edge-layout miss with Caps Lock on. The key reaches the page, which is no regression from
  main.
- Fix later: record partners per (keycode, level) from xkb, and grab what the key's Lock level types.

## Checked and fine

- **Non-letter keys:** digits and punctuation get no partner. `case_partner` is ASCII-letter-only plus
  the xkb pairs. Pairs are recorded only when `to_upper(level0) == level1`, so Shift+1 → `!` is not a
  pair. The xkb unit test covers this.
- **Single-letter toggle:** `letter_grabs` goes through the same `grabs_where`, so partners are released
  together with their letters. The Tab grabs have no case.
- **K-8b collision check:** still applied to partners outside Browse.
- **Double-tap pass-through:** autorepeat is never `ModifierPassThrough`, and the chord keeps GORCA
  during repeats. The new lane tests cover this, and the pass-through logic itself is untouched.
- **`MODIFIER_KEYS` table:** consistent with `static_keycode`/`static_keysym`, and the names, keysyms
  and keycodes are unique.
- **The hint:** fires at most once per run (`CmdState::told_numlock`). It needs `PressKind::Press`,
  keycode 90 and the `KP_0` keysym, and it is silent when `KP_Insert` is not a modifier. The trigger
  condition is the problem (F-1), not repetition.

## Delta review b21ca599..dcc7b92d (2026-10-19)

**Verdict: MERGE WITH FIXES.** D-2 is small and belongs in this branch. D-1 is pre-existing on main but
inside the scope of the security fix.

- **F-1 resolved.** The Num Lock hint, its catalogue strings, `told_numlock` and `modifier_by_keysym` are
  removed.
- **F-2 resolved.** `caps_lock_does_not_invert_the_quick_navigation_direction` is a unit test. CL now
  gates `cl_direction_wrong`; at 3c05e863 with a clean tree it measured 0, `lost` 0, `not_consumed` 0
  and p99 0.81 ms. d9cc6247 changes only the logging after that.
- **Gates at dcc7b92d** (own target dir, then deleted): `fmt --check` ok, `clippy -D warnings` ok,
  `cargo test --workspace` 126 test result lines with 0 failed, `xtask strings` `ok (335 literals, 116 allowed)`.
- **KM e2e at dcc7b92d** (git_dirty=false, load about 15; `bench/results/e2e/rv-km-head/`):
  - p99: insert 0.80 ms, kp_insert 0.76 ms, caps_lock 0.84 ms (max over 3 repeats 3.36 ms, gate 5).
  - `km_passthrough_wrong` 0, `lost` 0, `not_consumed` 0. All PASS.
- **Allowlist.** The three new entries are a debug message, another debug message and the
  `modifier_key("KP_Insert")` identifier. None is user-facing, so all three are legitimate.

### D-1 (Medium, pre-existing on main): typed characters reach the debug log

`crates/gorca-core/src/text/mod.rs:660` has
`tracing::debug!(target: "gorca_text", key = ?key, "text key")`. `TextKey` derives `Debug`, so
`Typed { ch: 'x' }` logs every character typed in an editable object. Password fields are not excluded:
`text_active` has no protected-role check. With `RUST_LOG=debug` or `gorca_text=debug`, a log contains
the passwords typed. The keysym redaction in d9cc6247 is therefore not complete.

**Fix:** log only the variant, or give `TextKey` a redacting `Debug`, as `LazyText` already does (its
`Debug` prints only the byte count). Add a test that `format!("{:?}", TextKey::Typed{..})` holds no
character.

### D-2 (Low): the Num Lock diagnostic line itself names the digit

`keys.rs` `Diag::ModifierKeyWithNumLock` is emitted only for keycode 90 arriving as `KP_0` with Mod2.
So every "modifier key, numlock on" line, with its timestamp, means "keypad 0 was typed": the count and
timing of zeros in a PIN leak, although the keysym is omitted.

**Fix:** emit it at most once per run, or drop it.

`Diag::Modifier` also logs every Shift press. That reveals the timing of capital letters, which is low
risk; consider leaving Shift and Control out.
