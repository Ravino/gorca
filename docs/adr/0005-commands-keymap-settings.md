# ADR-0005: Commands, keymap and settings

Status: **Accepted** (2026-10-13). Owner: architect. Milestone: M1 (`docs/plan/phase-2.md` §3 M1).

Inputs: `CLAUDE.md`; ADR-0001 §3.4 (epoch), §6.2 (channels), §7.2 (Mode), §7.3 (MonitorState, GrabSetState,
`KeyClass`, K-1…K-7), §10 (extension manifest keymap), §16.3 (P-7, P-8, P-10); ADR-0003 §11 (browse
state machine, BM-1), §12 (structural navigation); `docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`;
user decisions of 2026-10-13 in `docs/decisions.md` (modifier, keymap family, layouts, TOML settings,
typing echo). Companion document: `docs/design/modes.md` (mode table and full keymaps).

## 0. Changelog

- 2026-10-13: first version, accepted with the user decisions already recorded.

## 1. Context

Phase 1 reads focus but has no commands: `gorca-daemon` classifies every key as `PassToApp`
(`crates/gorca-daemon/src/lib.rs`, `pump_keys`), the grab-set watch channel is never written, and
`ReaderEffect::RunCommand(CmdId)` is dropped by the runtime. P0 created the seams this ADR fills:
`gorca-core/src/commands/` (Key and Mode arms of the Reader), `gorca-input-api` (`Keymap`, `classify`,
`GrabSet`, `CmdId(u32)`), `gorca-core/src/mode.rs` (`Mode`, `ModeKeys`, pure `step`).

User decisions (2026-10-13, binding for this ADR):

| # | Decision |
|---|---|
| U-1 | The gorca modifier is **Insert (and KP_Insert) and CapsLock**. CapsLock is dropped automatically when XKB uses it for layout switching. |
| U-2 | The default keymap is **NVDA-style**; an **Orca-style preset** also ships. |
| U-3 | The primary layout is **desktop with numpad review**; **laptop** is the alternative layout. |
| U-4 | Settings live in **`~/.config/gorca/gorca.toml`**. No GUI, no Orca settings import. |
| U-5 | Typing echo defaults: **characters on, words off**. |

### 1.1 Platform facts this design depends on (verified in source)

- **PF-1 Modifier grabs swallow everything while held.** `SetKeyGrabs(modifiers, keystrokes)`: while a
  grabbed modifier's press is in `grabbed_keypresses`, *every* key press is grabbed and delivered
  (`should_grab_keypress`, `refs/mutter/src/backends/meta-a11y-manager.c:540-553`). So `gorca+X` chords need
  no keystroke entries, and unbound `gorca+X` chords are swallowed too.
- **PF-2 Double-tap rule.** `is_a11y_modifier_first_click` (`refs/mutter/src/backends/native/meta-seat-impl.c:521-575`)
  swallows the first press and release of a grabbed modifier and passes the second press if it follows
  within `seat_impl->repeat_delay` (GSettings `org.gnome.desktop.peripherals.keyboard delay`, default
  500 ms, `refs/mutter/src/backends/meta-input-settings.c:1152`). A swallowed first click does not update
  the XKB state, so CapsLock does not toggle; the passed second press does (line 631). `last_keysym` is
  updated on *every* non-repeat key event, so any key in between breaks the double tap. The passed press
  is not added to `grabbed_keypresses` (`meta-a11y-manager.c:649-650`), so keys pressed while it is held
  go to the app.
- **PF-3 Keystroke grabs match `(keysym, mask)` exactly** and are applied on Mutter's input thread
  asynchronously to the D-Bus reply (P-7). The keysym depends on the layout group, the shift level, Caps
  Lock and Num Lock: `t` is `Cyrillic_ie` in the Russian layout, `KP_Home` becomes `KP_7` with Num Lock on.
- **PF-4 `KeyEvent` has no timestamp and no repeat flag** (`b u u u q`: released, state, keysym, unichar,
  keycode). gorca stamps receipt time and detects autorepeat as a press of a key already down.
- **PF-5 Wayland server-side repeat.** For `wl_keyboard` v10 clients Mutter forwards `REPEATED` key states
  (`refs/mutter/src/wayland/meta-wayland-keyboard.c:278-282`). Repeats are never "first clicks", so the
  autorepeat of a *held, grabbed* Insert may reach such a client (P-13).

### 1.2 Prior art and its failures

| System | What it does | Failure or lesson for gorca |
|---|---|---|
| Orca (`refs/orca/src/orca/*_command_definitions.py`, `keybindings.py`) | Keymap is Python code: one `KeyBinding` per desktop/laptop variant, `click_count` bindings (`KP_Enter` ×2). Grabs via libatspi: sync `SetKeyGrabs` after a 10 ms coalescing timer, no deadline. | Keymap not data: users cannot rebind without code; grab churn on every script change; sync D-Bus with no deadline on the GLib main loop (ADR-0001 F-1). Lesson kept: double-tap pass-through, desktop/laptop split, learn mode with Escape to exit (`learn_mode_presenter.py:140`). |
| Orca settings (`gsettings_registry.py`) | GSettings schemas with migrations. | Needs compiled schemas installed system-wide; not hand-editable; out of scope by U-4. |
| NVDA (`refs/nvda/source/globalCommands.py`, `inputCore.py`, `config/configSpec.py`) | Gestures are data (`kb(desktop):`/`kb(laptop):` strings, user `gestures.ini` overrides). Multi-press runs the script immediately and passes a repeat count (`scriptHandler.py:273-308`, `multiPressTimeout = 500`). Input help captures every gesture; `bypassInputHelp` scripts still run. Config in `configobj` with a spec, validation and defaults. | Default config saved on exit can clobber hand edits. Gestures match virtual-key codes, so layouts work; gorca must match keycodes too (K-8). |
| Odilia (`refs/odilia/input-server-keyboard`, `odilia/config.toml`) | evdev/rdev grab of the whole keyboard; `config` crate layering. | Needs root/uinput and re-emits all keys; a hang kills the keyboard. Modes are placeholders. Confirms K-1 (minimal grabs) and the learn-mode safety rule LM-3. |
| VoiceOver | Commander tables per input device, all as data; VO-key lock. | Validates per-context tables and an explicit "keyboard help" mode. |

## 2. Decision (summary)

1. **Commands are a closed Rust enum `CoreCmd` plus an extension range**, compiled into dense `CmdId`
   handles. Every command has a static `CmdSpec` (name, milestone, contexts, multi-press behaviour,
   interrupt policy, help text). Unimplemented commands are *inert*: bound in presets, never grabbed.
2. **The keymap is data (TOML)**: four shipped presets (`nvda-desktop`, `nvda-laptop`, `orca-desktop`,
   `orca-laptop`) plus user overrides in `gorca.toml`, merged by fixed layering and conflict rules, compiled
   once per load into an immutable `CompiledKeymap` published with `ArcSwap`. Lookup is O(1) on the input
   lane. Extension keymaps merge in M5 as a middle layer.
3. **Context = `ModeKind` × optional app context.** The Reader publishes the current `InputContext` to the
   input lane together with every grab-set change.
4. **The input lane replicates Mutter's modifier logic exactly** (`ModLatch`) to know when the gorca modifier
   is held, when a double tap passed through, and which chords Mutter swallowed.
5. **Chords match by keycode**, resolved from key names through the XKB keymap (all layout groups), with a
   keysym fallback. Keystroke grabs are compiled from keycodes into exact `(keysym, mask)` pairs.
6. **Commands run as Reader messages and produce effects** (`Speak`, `SpeechCtl`, `Fetch`, `Mode`,
   `Config`). The Reader never awaits (R3); IPC-backed commands continue on the reply.
7. **`gorca-config` is a pure crate** (no tokio, no I/O): schema-versioned parsing, defaults, per-key
   validation that never fails, migrations, preset merge. The daemon's config task does inotify, file I/O
   with a deadline and publication.
8. **Runtime changes (rate, toggles) live in an in-memory overlay** owned by the Reader; they are written to
   `gorca.toml` only by an explicit save command, format-preserving and hash-guarded.
9. **Every state-changing command speaks a short confirmation** from a message catalog, at the key's epoch.
10. **Pass-next-key changes the grab set instead of re-injecting** (K-1), so M1 does not depend on P-8.

## 3. Commands

### 3.1 Types (interface sketch; `gorca-core/src/commands/`)

```rust
/// Closed set of core commands. Exhaustive `match`, no wildcard (ADR-0001 §11 rule 6).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u16)]
pub enum CoreCmd { /* §3.2 */ }

/// Extension command, allocated when an extension manifest is loaded (M5).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ExtCmd { pub ext: ExtId, pub local: u32 }

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Cmd { Core(CoreCmd), Ext(ExtCmd) }

/// `gorca_input_api::CmdId(u32)` stays an opaque dense index into `CommandTable`.
/// 0..0x8000 = CoreCmd discriminants; 0x8000.. = extension commands, assigned per load.
pub struct CommandTable { specs: Vec<CmdSpec>, by_name: HashMap<Box<str>, CmdId> }

pub struct CmdSpec {
    pub cmd: Cmd,
    pub name: &'static str,          // stable TOML name, e.g. "speech.rate_up"
    pub available: Availability,     // Implemented | Planned(Milestone)
    pub contexts: ContextMask,       // where it may be bound (Focus, Browse, Review, ...)
    pub multi: MultiPress,           // Single | Repeat { max: u8 }  (repeat count passed to handler)
    pub interrupts: bool,            // K-3: advance the epoch on the input lane (default true)
    pub input_help_bypass: bool,     // runs even in InputHelp (learn-mode toggle, stop speech)
    pub help: MsgId,                 // learn-mode description
}
```

Command names are **stable API**: they appear in user files and extension manifests. Renames go through
schema migrations (§7.3), never silently.

### 3.2 Command set

M1 commands are implemented in M1. Later commands are declared now (`Availability::Planned`) so presets
can reserve their keys (`docs/design/modes.md` §4) and learn mode can say "not available yet".

| Group | Name | Milestone | Notes |
|---|---|---|---|
| speech | `speech.stop` | M1 | Watched gesture (Control press), §4.6 |
| speech | `speech.pause_toggle` | M1 | Watched gesture (lone Shift tap), `interrupts = false` |
| speech | `speech.mode_cycle` | M1 | talk ↔ off (NVDA+S; on-demand mode later) |
| speech | `speech.repeat_last` | M1 | from the speech lane's last-spoken cell, §6.4 |
| speech | `speech.ring_next`, `speech.ring_prev` | M1 | select rate / pitch / volume / voice (NVDA synth ring) |
| speech | `speech.ring_inc`, `speech.ring_dec`, `speech.ring_inc_large`, `speech.ring_dec_large` | M1 | change the selected parameter |
| speech | `speech.rate_up/down`, `speech.pitch_up/down`, `speech.volume_up/down` | M1 | direct variants, unbound in the NVDA preset |
| speech | `speech.copy_last` | Planned (M4) | needs clipboard access (Wayland: §11 Q) |
| info | `info.where_am_i` | M1 | ×1 basic, ×2 detailed, ×3 spell name |
| info | `info.window_title` | M1 | ×2 spell; ×3 copy (Planned) |
| info | `info.time_date` | M1 | ×1 time, ×2 date; GNOME `clock-format` |
| info | `info.current_line`, `info.selection`, `info.formatting` | M3 | |
| info | `info.status_bar`, `info.read_window`, `info.battery`, `info.focus_shortcut` | M4 | |
| info | `info.link_destination`, `info.details` | M7 | |
| echo | `echo.typed_chars_toggle`, `echo.typed_words_toggle`, `echo.command_keys_toggle` | M1 | toggles settings; typing echo itself lands in M3 |
| verbosity | `verbosity.cycle` | M1 | brief / normal / verbose |
| verbosity | `speech.punctuation_cycle` | M3 | verbalizer |
| input | `input.learn_mode_toggle` | M1 | `input_help_bypass = true` |
| input | `input.pass_next_key` | M1 | §5.3 |
| input | `input.sleep_toggle` | Planned (M6) | `PassThrough{UntilToggle}` per app |
| settings | `settings.reload`, `settings.save`, `settings.report_problems` | M1 | §7 |
| debug | `debug.node_info`, `debug.app_info` | M1 | mirror only: role, states, ids, toolkit, `AppHealth` |
| system | `gorca.quit` | Planned | requires ×2 to confirm |
| text | `text.say_all`, `text.spell_*`, `text.say_char/word/line` | M3 | ADR-0006 |
| review | `review.*` (line/word/char prev/cur/next, top/bottom, start/end of line, say all, to focus, move focus, activate) | M4 | ADR-0006 |
| object | `object.parent/first_child/next/prev/current/next_in_flow/prev_in_flow` | M4 | ADR-0006 |
| review | `review.caret_follows_toggle`, `review.focus_follows_toggle`, `review.toggle` (Orca preset), `review.mode_next/prev` | M4 | |
| terminal | (reuses `review.*`; `ext-vte` adds its own via M5) | M6 | |
| browse | `browse.toggle`, `browse.single_letter_toggle`, `browse.elements_list`, `browse.find/next/prev`, `browse.refresh`, `browse.activate`, `browse.container_start/end`, `browse.live_regions_toggle` | M7 | |
| quicknav | `quicknav.next/prev.<kind>` for heading, heading1…9, link, visited, unvisited, form_field, list, list_item, button, edit, frame, separator, radio, combobox, checkbox, graphic, blockquote, not_link_block, landmark, embedded, annotation, error, paragraph, table, (unbound: slider, article, live_region, tab, …) | M7/M8 | ADR-0003 §12 |
| table | `table.next/prev_row/column`, `table.first/last_row/column`, `table.say_row/column`, `table.say_all_row/column` | M8 | |

## 4. Input lane: classification, modifier latch, watched gestures

### 4.1 Amendments to `gorca-input-api` (sketch)

```rust
bitflags! { impl Modifiers {
    /// Virtual bit: a gorca modifier is held (ModLatch::Held). Never sent to Mutter (stripped by to_wire).
    const GORCA = 1 << 24;
}}

pub enum PressKind { Press, Repeat, Release }      // Repeat = press of a key already down (PF-4)

pub struct KeyIntent {
    pub event: KeyEvent,
    pub kind: PressKind,
    pub class: KeyClass,
    pub epoch: Epoch,
    pub at: Instant,                               // receipt time on the input lane
}

pub enum KeyClass {
    Command(CmdId),                                // swallowed by Mutter, bound
    Swallowed,                                     // swallowed by Mutter (gorca+X), unbound: no effect
    Watched(CmdId),                                // reached the app AND triggers a command (Control, Shift tap)
    PassToApp,
    ModifierPassThrough,                           // double tap: Mutter passed the modifier press (PF-2)
    Ambiguous { committed: Option<CmdId> },        // ADR-0001 §7.3, plus the ModLatch guard band (§4.3)
    Injected,
}

/// Physical chord identity used for lookup. Lock bits never take part (K-6).
pub struct ChordKey { pub mods: Modifiers /* incl. GORCA, without locks */, pub key: KeyRef }
pub enum KeyRef { Code(u16), Sym(u32) }            // Code when the XKB resolver is available (K-8)
```

### 4.2 Classification order (input lane, O(1), per `KeyEvent`)

1. `PressKind` from the pressed-keycode set (repeat detection, PF-4).
2. Injection expectation table → `Injected`.
3. `ModLatch.step(event)` (§4.3). If the event is a gorca modifier, the result is `Swallowed`
   (first click, modifier alone has no command), `ModifierPassThrough` or `Ambiguous`.
4. Build `ChordKey` with `GORCA` set iff `ModLatch` is `Held`. Look it up in the `CompiledKeymap` for the
   current `InputContext` (§6.2).
5. Swallowed by Mutter? (`ModLatch::Held`, or the keystroke is in the committed grab set.) Then `Command`
   if bound and `Implemented`, else `Swallowed`. During `GrabSetState::Pending`, keys in
   `committed Δ wanted` are `Ambiguous` (unchanged rule).
6. Not swallowed: `Watched(cmd)` if it is a watched gesture (§4.6), else `PassToApp`.
7. K-3: advance the `EpochCell` iff `kind == Press` and the class is `Command` with `spec.interrupts`,
   `Swallowed`, `Watched(speech.stop)`, or `PassToApp` of a non-modifier key (policy
   `keyboard.interrupt`, §7.4). Modifiers alone (Shift, Control, Alt, Super, the gorca modifier) never
   advance it, except Control through `speech.stop`. Phase 1's `pump_keys` advances on every press,
   including modifiers; M1 fixes that.

### 4.3 `ModLatch`: replica of Mutter's a11y modifier state

```rust
enum ModLatch {
    Idle,
    /// First press swallowed by Mutter; `GORCA` is set on chords. `chorded` = some key was pressed.
    Held      { key: u32, since: Instant, chorded: bool },
    /// Released alone; a press of the same key before `until` is passed through by Mutter.
    TapArmed  { key: u32, until: Instant },
    /// The second press reached the app; keys pressed while it is down are not grabbed (PF-2).
    PassedThrough { key: u32 },
    /// A second press landed inside the guard band around `repeat_delay`: Mutter's verdict is unknown.
    Uncertain { key: u32 },
}
```

The replica follows `is_a11y_modifier_first_click` line by line (`last_keysym`, `last_keysym_time`,
`saw_first_release`, `pressed_modifiers`), fed with every non-repeat event of every key. The only
non-replicable input is time (PF-4): gorca uses receipt time, so a press whose interval to the previous
event lies within **±G** (`keyboard.double_tap_guard_ms`, default 15 ms) of `repeat_delay` is
`Ambiguous` and moves to `Uncertain`. In `Uncertain`, chord keys are `Ambiguous { committed: None }`:
not executed, not echoed, counted (M-16). `Uncertain` ends at the modifier's release.

| From | Event | To | Class |
|---|---|---|---|
| Idle | modifier press | Held{chorded: false} | `Swallowed` |
| Held | other key press | Held{chorded: true} | that key: `Command`/`Swallowed` (step 5) |
| Held | modifier release, `chorded == false` | TapArmed{until: now + repeat_delay} | `Swallowed` |
| Held | modifier release, `chorded == true` | Idle (Mutter state: `saw_first_release = false`) | `Swallowed` |
| TapArmed | same modifier press, Δt < repeat_delay − G | PassedThrough | `ModifierPassThrough`; speaks nothing |
| TapArmed | same modifier press, \|Δt − repeat_delay\| ≤ G | Uncertain | `Ambiguous` |
| TapArmed | same modifier press, Δt > repeat_delay + G, or any other key | Held / Idle as Mutter would | as in Idle |
| PassedThrough | key press | PassedThrough | `PassToApp` (Mutter grabs nothing) |
| PassedThrough / Uncertain | modifier release | Idle | `PassToApp` / `Ambiguous` |
| any | modifier autorepeat | unchanged | ignored (Mutter ignores repeats for this logic) |
| any | `MonitorState` leaves `Watching` | Idle | — |

`repeat_delay` comes from the platform probe (§8.2), default 500 ms. Insert and CapsLock are separate
keysyms; double-tapping CapsLock toggles Caps Lock exactly as Mutter does, and the first tap never does
(acceptance item of M1).

### 4.4 Keycode matching and grab compilation (new invariant K-8)

- **K-8** Command chords match **physical keys** (`KeyRef::Code`). Key names in TOML are XKB keysym names
  resolved to keycodes by searching every group of the current XKB keymap at levels 1–2 (first hit wins,
  Latin groups first). `gorca+T` therefore works in the Russian layout. If no keymap is available, lookup
  falls back to `KeyRef::Sym` with letters case-folded.
- **K-8a** Keystroke grabs (non-modifier chords: numpad review in M4, browse keys in M7) are compiled from
  `(keycode, mods)` into the exact `(keysym, mask)` pairs Mutter will see, for every layout group and
  every Caps Lock state. The compiled grab set replaces the hand-written ×4 expansion of K-6 for these
  chords.
- **K-6a** Keypad chords are registered **without the Num Lock variants**: with Num Lock on the keypad
  types digits into the app, as in NVDA and Orca. `KP_Insert` as the gorca modifier therefore works only
  with Num Lock off (same as Orca).
- The resolver is a `KeyResolver` trait in `gorca-input-api`; the implementation (xkbcommon, compiled from
  the GNOME `input-sources` RMLVO) lives in `gorca-input-mutter`. Every observed `KeyEvent` cross-checks
  `keycode → keysym`; a mismatch rate above 1 % logs once and triggers a recompile (P-15).

### 4.5 Grab sets per mode

`Mode::grab_set` (`gorca-core/src/mode.rs`) takes its keys from the `CompiledKeymap`, never from literals.
K-1 holds: only **implemented** chords are grabbed. In M1 the Focus grab set is the modifiers only (all
M1 commands are `gorca+X`); numpad review keys join it in M4, browse keys in M7. The full per-mode list
is in `docs/design/modes.md` §2.

### 4.6 Watched gestures

Some NVDA gestures are keys the app must still receive. They are never grabbed (K-1):

| Gesture | Command | Rule |
|---|---|---|
| Control press | `speech.stop` | On press, alone or as the start of a chord (NVDA parity). Advances the epoch. |
| Shift tap | `speech.pause_toggle` | Press and release with no other key between, release within 400 ms. Fires at release. Disabled automatically if XKB uses Shift alone for layout switching (`grp:lshift_toggle`, `grp:shifts_toggle`, …). |

Both can be turned off (`keyboard.stop_speech_on_control`, `keyboard.pause_speech_on_shift`).

## 5. Modes and contexts

### 5.1 Mode model

`Mode` (ADR-0001 §7.2) stays the Reader-owned state machine; the full table is `docs/design/modes.md`.
Two refinements:

- `PassThrough` and `InputHelp` remember where to return: `PassThrough { scope, resume: BaseMode }`,
  `InputHelp { resume: BaseMode }`, with `enum BaseMode { Focus, Browse{..}, Review{..} }` (`Copy`). Today's
  `step` returns to `Focus`; that is a bug once Browse exists.
- `ModeKind` (fieldless mirror of `Mode`) is what the keymap is keyed by.

### 5.2 Input context

```rust
pub struct InputContext { pub gen: u32, pub mode: ModeKind, pub app: Option<AppCtxId> }
```

The Reader publishes `InputContext` and the grab set **together** (one `SetGrabSet { set, ctx }` effect);
the input lane stores the context with the `wanted` grab set and switches lookup context when the grab set
commits. A key that arrives during `Pending` uses the committed context, consistent with the committed-set
rule. `AppCtxId` is assigned by extension manifests (M5); in M1 it is always `None`.

Lookup order for a chord: `(mode, app)` → `(mode, *)` → `(global, app)` → `(global, *)`; the first hit
wins. `global` bindings apply in Focus, Browse and Review; PassThrough and InputHelp have their own
fixed handling (modes.md §3).

### 5.3 Pass-next-key without re-injection

`input.pass_next_key` → `Mode::PassThrough { OneKey, resume }`, whose grab set is **empty** (not even the
modifiers). The next key or chord, including `gorca+X`, reaches the app natively; no injection is needed.
The mode ends at the first non-modifier press (restoring `resume`'s grab set) or after 10 s of no key
(announced). Human reaction time (> 150 ms) dwarfs the P-7 settle, so the only lossy case is a chord
typed within the settle window right after the command, which is `Ambiguous` and counted. This removes
the M1 dependency on P-8.

### 5.4 Learn mode (InputHelp)

- **LM-1** Entering sets `grab_all` (Mutter `GrabKeyboard`) while keeping the modifiers grabbed, so
  `ModLatch` keeps working and CapsLock does not toggle on a single tap.
- **LM-2** Each key or chord is described, not executed: "Insert T: report window title", "not bound",
  or "report current line, not available yet". The description uses the same lookup as §4.2 against the
  `resume` context, so it tells the truth about what the key would do there. Multi-press variants are
  described on repeated presses.
- **LM-3 Safety.** Exits: `input.learn_mode_toggle` (`input_help_bypass`), Escape (reserved, not
  rebindable in InputHelp), and an idle timeout of 60 s. The **input lane** enforces the timeout and also
  exits on its own if the Reader has not answered a description within 1 s, so a stalled Reader can
  never leave the keyboard grabbed. If the input lane itself stalls, the watchdog (ADR-0001 §6.6) aborts
  the process; Mutter drops all grabs when the bus name vanishes (`key_grabber_bus_name_vanished_callback`,
  `meta-a11y-manager.c:158`).
- **LM-4** `speech.stop` works in learn mode (Control is delivered under `GrabKeyboard`, and `speech.stop`
  has `input_help_bypass`).
- **LM-5** The gorca modifier double tap still passes the modifier to the app in learn mode (PF-2 runs
  before the grab check). Learn mode says "Insert passed to application".

## 6. Commands as Reader messages (R3)

### 6.1 Flow

```
[input] KeyEvent → classify (§4.2) → K-3 epoch → KeyIntent{class: Command(id), at, epoch}
   → Reader (priority lane, ADR-0001 §6.2)
[reader] on_key → MultiPress.step → CmdCtx{cmd, repeat, epoch(E-4), now} → handler (pure)
   → ReaderEffects: Speak | SpeechCtl | Fetch(+continuation) | Mode(ModeMsg) | Config(ConfigOp)
         | PublishEffective(Arc<Settings>)
```

```rust
pub(crate) struct CmdCtx<'a> {
    pub cmd: Cmd, pub repeat: u8, pub epoch: Epoch, pub now: Instant,
    pub settings: &'a Settings,           // effective snapshot (file ⊕ overlay)
    pub view: ReaderView<'a>,             // focus SM, mode, mirror reads (lock-free, PC-1)
}
pub(crate) enum CmdOutcome {
    Done,
    /// Waiting for an Interactive query; resumed by ReaderMsg::Reply with the same QueryId.
    Pending { query: QueryId, cont: Continuation, deadline: Instant },
}
```

- **CR-1** Handlers are pure functions of `CmdCtx`; they never await, never touch I/O and never read the
  clock except through `ctx.now`. Unit tests drive them with a fake `ReaderView`.
- **CR-2** `ReaderEffect::RunCommand(CmdId)` (P0 placeholder) is removed: the dispatcher runs the command
  inline in `on_key`.
- **CR-3** Continuations live in a bounded table (16 entries) keyed by `QueryId`, each tagged with its
  epoch. A reply whose epoch is stale (E-3) or whose entry is gone is dropped. Deadline expiry speaks
  the partial result plus "not responding" (where am I) and frees the entry.
- **CR-4** Multi-press: `MultiPress::{Idle, Armed{chord, cmd, count, at}}`. Same chord within
  `keyboard.multi_press_ms` (default 500, NVDA `multiPressTimeout`) increments `count` (max 3); any other
  key press resets. **The first press runs immediately**; the second press runs the command again with
  `repeat = 2`, and its new epoch cancels the first answer (E-2). No command waits to see whether a
  second press follows. A preset may bind `chord ×2` to a different command (Orca `KP_Enter ×2`); the
  dispatcher prefers an explicit `(chord, n)` binding over `repeat = n`.
- **CR-5** Mode changes go through `ModeMsg` and `Mode::step`, which emits `AdvanceEpoch` and
  `SetGrabSet` (unchanged P0 contract).

### 6.2 Speech intents and speech control

- Confirmations and answers are `SpeechIntent`s with `Prio::Interrupt` and the key's epoch (E-4), so
  they cut off stale speech and are never cancelled by their own key press.
- Voice parameters travel as `SpeechCtl::SetParams(VoiceParams)` on the **same ordered channel** as
  utterances; the speech lane applies them immediately (not queued behind the FIFO) and before the next
  utterance, so "rate 60" is spoken at rate 60. Parameter ranges are normalised 0–100 in gorca and
  mapped by each `SpeechClient` backend.
- `speech.pause_toggle` emits `SpeechCtl::{Pause, Resume}` (ADR-0001 `SpeechCmd::Pause/Resume`);
  `speech.mode_cycle` to `Off` makes the speech lane drop everything except command confirmations of
  `speech.mode_cycle` itself.

### 6.3 Ownership of settings state (PC-1, single writer per cell)

| Cell | Writer | Readers | Content |
|---|---|---|---|
| `ArcSwap<FileSettings>` | config task (workers) | Reader | validated file + presets, with diagnostics |
| `ArcSwap<CompiledKeymap>` | config task | input lane, Reader (learn mode) | per-context chord tables, grab sets per `ModeKind`, multi-press table |
| `ArcSwap<Settings>` (effective) | **Reader** | speech lane, input lane, presenters, extensions (M5) | `FileSettings ⊕ Overlay` |
| `ArcSwap<LastSpoken>` | speech lane | Reader | last utterance actually started, excluding key/typing echo and repeats |

The Reader owns the `Overlay` (runtime changes from commands) because commands execute there; publishing
the effective snapshot is one `Arc` allocation of a small struct per change.

### 6.4 "Where am I" and other M1 handlers

- `info.where_am_i` reads the focused node and its ancestry from the mirror (lock-free). On a miss it
  emits one `Fetch` (Interactive) for the ancestry with `PRESENT_MASK` and continues on the reply (K3
  gate). ×1: app, window, container chain ("list, 3 of 12", "tab 2 of 5"), role, name, states,
  shortcut. ×2: adds description and position details. ×3: spells the name.
- `info.window_title`, `debug.*`: mirror reads, one Interactive fetch on a miss.
- `info.time_date`, `speech.*`, `echo.*`, `verbosity.cycle`, `settings.*`: zero IPC (K2 gate).
- `speech.repeat_last` speaks `LastSpoken` again with a fresh id and the key's epoch.

## 7. Settings: `gorca-config`

### 7.1 Crate boundary

`gorca-config` is pure: depends on `serde`, `toml` (with `preserve_order`), `toml_edit` (save only),
`thiserror`. No tokio, no I/O, no XKB, no dependency on `gorca-core`. Added to ADR-0001 §11 dependency
rule 1 and rule 4 (no tokio). It exposes:

```rust
pub const SCHEMA: u32 = 1;
pub fn load(text: Option<&str>, presets: &Presets) -> Loaded;     // never fails
pub struct Loaded { pub settings: FileSettings, pub keymap: ResolvedKeymap, pub diags: Vec<Diag>, pub hash: u64 }
pub struct Diag { pub severity: Severity, pub path: String, pub span: Option<(u32, u32)>, pub msg: DiagMsg }
pub fn save(original: &str, overlay: &Overlay) -> Result<String, SaveError>;  // toml_edit, comments preserved
pub static SETTINGS: &[SettingDef];   // one row per key: path, type, default, range, since, Live|Restart
```

`ResolvedKeymap` holds normalised `ChordSpec { gorca, mods, key_name, presses }` → command **names** per
context; gorca-core maps names to `CmdId` and reports unknown names as diagnostics; the daemon resolves
key names through the `KeyResolver` into `CompiledKeymap`.

### 7.2 File, layering and defaults

Path: `$XDG_CONFIG_HOME/gorca/gorca.toml` (default `~/.config/gorca/gorca.toml`), overridable with
`--config`. A missing file means "all defaults" and is not a problem. Layering, lowest first:

1. Built-in defaults (`SETTINGS`).
2. Keymap preset selected by `keyboard.preset` × `keyboard.layout` (embedded TOML, same format as user files).
3. *(M5)* Extension manifests' keymaps and setting defaults, in their own namespaces.
4. `gorca.toml`.
5. Runtime overlay (in memory, §7.6).

Schema v1 (M1 keys; full defaults also printed by `gorca config defaults`):

```toml
schema = 1

[keyboard]
preset = "nvda"                 # "nvda" | "orca"
layout = "desktop"              # "desktop" | "laptop"
modifiers = ["Insert", "KP_Insert", "Caps_Lock"]
caps_lock_layout_guard = true   # drop Caps_Lock when XKB uses it for layout switching (U-1)
multi_press_ms = 500
double_tap_guard_ms = 15
interrupt = "non-modifier"      # "non-modifier" | "never"   (K-3 policy)
stop_speech_on_control = true
pause_speech_on_shift = true

[speech]
rate = 50                       # 0..100
pitch = 50
volume = 100
rate_step = 5                   # ring_inc/ring_dec; *_large uses 4x
voice = ""                      # backend default

[echo]
typed_characters = true         # U-5
typed_words = false             # U-5
command_keys = false            # NVDA+4

[verbosity]
level = "normal"                # "brief" | "normal" | "verbose"
confirmations = "full"          # "full" | "brief"

[keymap.global]                 # user overrides; also [keymap.focus], [keymap.browse], [keymap.review]
# "gorca+t" = "info.window_title"
# "gorca+F12" = "none"          # unbind
```

### 7.3 Schema versioning

- **SV-1** `schema` absent → treated as the current version (minimal hand-written files are valid).
- **SV-2** `schema < SCHEMA` → pure migration chain `migrate_n_to_n1(&mut toml::Table, &mut Vec<Diag>)` in
  memory. The file is rewritten only by `settings.save`, which then writes the current `schema`.
- **SV-3** `schema > SCHEMA` (file from a newer gorca) → keys known to this version are honoured, others
  ignored with one diagnostic; `settings.save` refuses to write (announced) so a downgrade never destroys
  newer settings.
- **SV-4** Renamed keys and renamed command names are migrations; removed keys produce an "obsolete
  key" diagnostic. Every migration has a golden-file test.

### 7.4 Validation that never fails startup

| Problem | Behaviour |
|---|---|
| File missing | defaults, no diagnostic |
| Unreadable, > 1 MiB, read deadline (1 s) exceeded | defaults at startup / last-good on reload; one diagnostic |
| Invalid UTF-8 or TOML syntax | same; diagnostic with line and column |
| Wrong type | that key's default; diagnostic |
| Number out of range | clamped; diagnostic |
| Unknown enum value | default; diagnostic |
| Unknown key | ignored; diagnostic with "did you mean" (edit distance ≤ 2) |
| Panic inside parsing | caught at the config task boundary (`catch_unwind`); defaults/last-good; diagnostic |

- **CV-1** Parsing goes to a `toml::Table` first and extracts each key separately, so one bad key never
  invalidates the others (a whole-struct `serde` derive would fail on the first error).
- **CV-2** Diagnostics are logged with path and span every time, and **spoken once per distinct set**
  (hash of the diagnostics): "Settings: 2 problems. Insert Control Shift R for details."
  (`settings.report_problems` reads them one by one.) At startup this is spoken after the greeting at
  `Prio::Notification`.
- **CV-3** `gorca config check [PATH]` prints diagnostics and exits non-zero; this is the only path where a
  bad file is an error.

### 7.5 Keymap conflict rules

| Rule | Situation | Resolution |
|---|---|---|
| KC-1 | Same normalised chord twice in one context of one layer (`"Insert+t"` and `"gorca+T"`) | the later entry in file order wins; diagnostic |
| KC-2 | User binds a chord the preset binds in the same context | user wins (that is the point of overrides) |
| KC-3 | `"chord" = "none"` | unbinds in that context and every less specific one it would fall through to |
| KC-4 | A command keeps its preset chords when the user adds another; to move a command, unbind the old chord too. `[keymap] replace_preset = true` starts from an empty keymap instead. |
| KC-5 | Chord without the gorca modifier in `global` or `focus` context | allowed only for keypad keys, for chords with at least two of Ctrl/Alt/Super, and for F13–F24; otherwise rejected with a diagnostic (it would break typing). Single letters are allowed in `browse` and `review` only. |
| KC-6 | Reserved: Escape in InputHelp; the gorca modifiers themselves; Control and Shift alone (watched gestures, only on/off) | binding rejected; diagnostic |
| KC-7 | Chord equals a known GNOME Shell/Mutter keybinding (read from `org.gnome.desktop.wm.keybindings`, `org.gnome.shell.keybindings`) | allowed; warning diagnostic, since gorca's grab would take precedence over the Shell |
| KC-8 | Command bound in a context its `CmdSpec.contexts` does not allow | rejected; diagnostic |
| KC-9 | Command name unknown | rejected; "did you mean"; diagnostic |
| KC-10 | Binding to a `Planned` command | accepted, inert (not grabbed, learn mode says "not available yet") |

**Extension keymaps (M5).** An extension manifest declares bindings only in its own app contexts
(`app:<matcher>` × mode), for its own commands (`ext.<name>.<cmd>`) or core commands. Merge layer 3 sits
between presets and the user file. Rules: an extension may not bind in `global`; may not shadow the
safety set (`speech.stop`, `input.learn_mode_toggle`, `input.pass_next_key`, `settings.reload`); its
bindings win over core bindings inside its app context (more specific context, §5.2); the user can
rebind or unbind any extension chord in `[keymap."app:firefox".browse]`. Two extensions binding the same
chord in the same context: manifest order (app-specific before toolkit-specific, ADR-0001 §10), with a
diagnostic. Extension commands get `CmdId`s ≥ 0x8000 at load; a reload of the extension recompiles the
keymap.

### 7.6 Hot reload and the overlay

```
inotify(dir of gorca.toml: CLOSE_WRITE | MOVED_TO | CREATE | DELETE, name filter)
  → debounce 100 ms (latest wins, at most one load in flight)
  → read on spawn_blocking with a 1 s deadline (home may be NFS/FUSE)
  → hash; equal to the last load or to our own last save → stop
  → gorca_config::load (worker) → gorca-core command-name mapping → KeyResolver compile
  → store ArcSwap<FileSettings>, ArcSwap<CompiledKeymap>
  → ReaderMsg::Settings(FileReloaded{gen, cause, diags})
Reader: effective = file ⊕ overlay → store ArcSwap<Settings>; SpeechCtl::SetParams if voice changed;
        recompute the grab set for the current mode → SetGrabSet; speak "settings reloaded" (Notification)
```

- **HR-1** Each cell swaps atomically; no reader ever sees a half-applied file. Keymap and grab set
  changes go through `GrabSetState::Pending`, so keys in flight follow the Ambiguous rule.
- **HR-2** After startup, a failed load (syntax, I/O, deadline) keeps the **last good** settings.
- **HR-3** Reload never runs on a hot lane; the lanes only `ArcSwap::load`.
- **HR-4** Overlay vs file: on an inotify reload, overlay entries for keys whose file value changed are
  dropped (the user's edit wins); others survive. `settings.reload` (explicit) clears the overlay
  ("revert to saved", NVDA+Ctrl+R).
- **HR-5** `settings.save` writes overlay values into the original text with `toml_edit` (comments and
  order preserved), atomically (temp file + `rename`), and only if the file's hash still equals the last
  loaded hash; otherwise it refuses and says "settings file changed on disk, not saved". The written
  hash is remembered so the resulting inotify event is ignored.
- **HR-6** Settings marked `Restart` (none in v1) are applied on next start and say so.

## 8. Platform probe (daemon)

### 8.1 Facts gorca needs

| Fact | Source | Default if unavailable |
|---|---|---|
| `repeat_delay` | GSettings `org.gnome.desktop.peripherals.keyboard delay` | 500 ms |
| XKB RMLVO and options | `org.gnome.desktop.input-sources sources`, `xkb-options` | `us`, no options |
| Caps Lock usable as gorca key | compile the keymap; keycode 66 level 1 group 1 is `Caps_Lock` | assume usable |
| Shift alone switches layout | options `grp:*shift*_toggle` | assume no |
| Clock format | `org.gnome.desktop.interface clock-format` | 24h |
| Shell keybindings (KC-7) | `org.gnome.desktop.wm.keybindings`, `org.gnome.shell.keybindings` | skip the check |

### 8.2 Behaviour

Read at startup and on `ca.desrt.dconf.Writer` `Notify` for those paths (session bus, one match rule),
through `gsettings get` subprocesses with a 500 ms deadline each, on workers. The result is a
`PlatformFacts` value fed into keymap compilation. When CapsLock is dropped (U-1), gorca says once:
"Caps Lock switches keyboard layout, so the gorca key is Insert." Re-enabling happens automatically when
the option goes away. The probe never blocks startup: gorca starts with the defaults and recompiles when
the probe answers.

## 9. Spoken confirmations

| Command | `full` | `brief` |
|---|---|---|
| ring inc/dec, direct rate/pitch/volume | "rate 55" | "55" |
| ring next/prev | "pitch 50" (name and value) | same |
| echo toggles | "speak typed characters on" / "… off" | "on" / "off" |
| `verbosity.cycle` | "verbosity brief" | "brief" |
| `speech.mode_cycle` | "speech off" (spoken before muting) / "speech on" | same |
| `input.learn_mode_toggle` | "learn mode on, Escape to exit" / "learn mode off" | "learn on" / "learn off" |
| `input.pass_next_key` | "pass next key" | "pass" |
| `settings.reload` | "settings reloaded" (+ ", 2 problems") | same |
| `settings.save` | "settings saved" / "not saved: file changed on disk" | same |
| CapsLock dropped, keyboard `Lost` / restored | fixed one-time messages (ADR-0001 §7.3) | same |

- **SC-1** Strings come from a `MsgId` catalog in `gorca-core` (English now; localisation is a later ADR).
- **SC-2** Confirmations are `Prio::Interrupt` at the key's epoch, never logged with values of secret
  fields (not applicable to M1 commands, kept as a rule for later ones).
- **SC-3** When ADR-0002 SE-5 earcons exist, toggles may use on/off earcons (`confirmations = "earcon"`,
  reserved value).

## 10. Latency gates and metrics

### 10.1 Gates (from `docs/plan/phase-2.md` §2.1; all p99 gorca-side, median of interleaved repeats, also under L3-50k with Δp99 ≤ 1 ms, `lost = 0`, `stale = 0`)

| ID | Stimulus → end | Gate (ceiling / target) | Budget split |
|---|---|---|---|
| K2 | command chord `KeyEvent` → first byte of `Speak` written; zero-IPC commands (`speech.repeat_last`, `info.time_date`, `speech.ring_inc` incl. `SetParams` before `Speak`) | 5 ms / 2 ms | classify + ModLatch ≤ 5 µs; input→Reader hop ≤ 50 µs; handler ≤ 100 µs (M-14); verbalize + speech lane ≤ 200 µs |
| K3 | `info.where_am_i` → `Speak`, fake app replying in 1 ms, cache cold | 15 ms / 5 ms | one Interactive fetch of the ancestry; continuation ≤ 200 µs |
| K4 | 10k randomized mode / grab-set switches (learn mode, pass-next-key, keymap reload; Browse once M7 exists) during synthetic typing; `fake_mutter.py` applies grabs after a random 0–20 ms delay | 0 lost, 0 doubled keys; `Ambiguous` count reported (M-16) | — |

### 10.2 Additional gates proposed by this ADR (test-engineer and perf-engineer own them)

| ID | Stimulus → end | Gate |
|---|---|---|
| K5 | learn mode with the Reader artificially stalled | input lane releases `GrabKeyboard` ≤ 1.2 s; with the input lane stalled, the watchdog aborts and Mutter drops the grab ≤ 3 s |
| K6 | `gorca.toml` rewritten 100× during L3-50k | L1 Δp99 ≤ 1 ms; file write → effective settings published p99 ≤ 150 ms wall; 0 torn reads |
| K7 | 10k synthetic modifier double taps with intervals uniform in 300–700 ms against `fake_mutter.py`'s exact port of `is_a11y_modifier_first_click` | 0 misclassifications outside the ±G guard band; band share reported |
| K8 | fuzzed `gorca.toml` (cargo-fuzz, 1 h) | gorca always starts; `load` never panics; diagnostics deterministic |

### 10.3 Correctness metrics

- Every preset compiles with zero diagnostics and no KC-1 collisions (unit test over all four presets).
- Every `CoreCmd` has a `CmdSpec`, a help string and, if `Implemented`, a handler test (exhaustive match).
- The Orca and NVDA presets bind every M1 command that the source screen reader binds (mapping table in
  `docs/design/modes.md` §5 is the test oracle).

## 11. Open measurements: assumptions and fallbacks

| ID | Question | Assumption used now | If wrong (fallback) |
|---|---|---|---|
| **P-7** | Delay between the `SetKeyGrabs` reply and Mutter's input thread applying it | ≤ 5 ms settle (ADR-0001 §7.3). M1 mode switches are user-paced (learn mode, pass-next-key), so the window is rarely hit. | Settle = 2× measured p99.9, configurable. Keys in the window stay `Ambiguous` (counted, not executed). If the delay is unbounded, compare via a probe keystroke the input lane can detect, or patch Mutter to signal application (upstream, user allows patching). Hard requirement only from M7 (auto focus mode). |
| **P-8** | Do RemoteDesktop/EIS-injected keys come back as `KeyEvent`, how fast, with what auth? | **Not needed in M1** (§5.3). | M7 swallowed-quick-nav-letter cases fall back to the ADR-0003 behaviour without injection: the letter is spoken as "not in edit field, press again" and the user retypes; re-injection added when P-8 is positive. |
| **P-10** | End-to-end key → `KeyEvent` latency with gorca owning the name | sub-millisecond p99 (Ping proxy 0.87 ms) | Gates are gorca-side and unaffected. If > 5 ms p99, report as a platform diagnostic (M-12) and raise upstream; no gorca design change. |
| P-13 (new) | Does the autorepeat of a held, grabbed Insert reach `wl_keyboard` v10 clients as `REPEATED` (PF-5)? | No visible effect in GTK apps | If confirmed: Mutter patch (treat repeats of a modifier in `grabbed_keypresses` as grabbed); meanwhile the user guide recommends CapsLock for long holds. CapsLock does not autorepeat in standard XKB. |
| P-14 (new) | Skew between Mutter's event timestamps and gorca's receipt times | < 5 ms p99, inside G = 15 ms | Widen G from K7 data; the band only costs counted `Ambiguous` keys. |
| P-15 (new) | Does an xkbcommon keymap compiled from GSettings RMLVO match Mutter's keymap? | Yes for `xkb` sources; IBus sources map to their XKB layout | Learn `keycode → keysym` from observed `KeyEvent`s; keysym fallback (K-8). |

Owner of P-7, P-8, P-10, P-13…P-15: linux-platform-engineer, before the M1 merge (P-8 before M7).

## 12. Alternatives considered

| Alternative | Why not |
|---|---|
| Keymap in code (Orca) | Not user-editable; every app script changes grabs; keymap changes need releases. |
| GSettings/dconf storage (Orca) | Needs installed compiled schemas; not hand-editable or diffable. Excluded by U-4. |
| One `serde`-derived struct for the whole file | First error invalidates everything; violates "never fails startup". |
| Auto-save on exit (NVDA default) | Clobbers concurrent hand edits of a file the user owns; SIGKILL by `orca --replace` (ID-3) loses it anyway. Explicit save chosen; `save_on_exit` can be added later as an opt-in. |
| Wait for a possible second press before running a command | Adds `multi_press_ms` to every first press; violates K2. |
| Match chords by keysym | Breaks `gorca+T` in the Russian layout; NVDA and Orca both match physical keys. |
| `GrabKeyboard` with re-injection for all modes | Rejected in ADR-0001 K-1/K-2; a hang would freeze the keyboard. |
| Pass-next-key by re-injection (plan's original M1 idea) | Depends on P-8; grab-set swap is simpler and lossless at human speed. |
| `notify` crate for file watching | Works, but `inotify` + tokio is smaller and we watch one directory on Linux only. Either is acceptable; rust-engineer decides. |
| Settings behind `RwLock` | Readers on hot lanes would contend with reloads; `ArcSwap` load is wait-free. |

## 13. Consequences

- M1 can ship commands without P-8; pass-through and learn mode rely only on grab-set changes.
- The input lane gains real state (`ModLatch`, pressed-key set, multi-press needs `at`), and its tests must
  include an exact port of Mutter's first-click logic (`fake_mutter.py`).
- `gorca-input-mutter` gains an xkbcommon dependency (C library already on every GNOME system).
- Presets are data and are tested as such; adding Firefox keys in M7 is a preset edit plus `CmdSpec`
  availability flips, not a classifier change.
- ADR-0001 amendments on acceptance: §7.2 `PassThrough`/`InputHelp` carry `resume`; §7.3 adds `ModLatch`,
  `KeyClass::{Swallowed, Watched, ModifierPassThrough}`, K-6a, K-8, K-8a and the refined K-3 policy;
  §6.2 the input→Reader message carries `at` and `PressKind`; §11 adds `gorca-config` to rules 1 and 4.
- ADR-0003 §12's provisional quick-nav letters are replaced by NVDA's (`docs/design/modes.md` §4.4):
  W = error, A = annotation; live region and article are unbound by default.

## 14. Questions for the user

| # | Question | Recommendation |
|---|---|---|
| Q-1 | Should runtime changes (rate, toggles) be saved automatically on exit, or only with gorca+Ctrl+C? | **Explicit save only** in Phase 2 (medium confidence): the file is hand-edited and auto-save can overwrite edits. |
| Q-2 | Double press of where am I: detailed (Orca, the plan) or spell (NVDA)? | **×2 detailed, ×3 spell** (medium confidence). |
