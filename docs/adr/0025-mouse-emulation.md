# ADR-0025: Mouse emulation (pointer routing, clicks, drag lock) through the gorca-helper virtual pointer

Status: **Accepted** (r2, 2026-10-19; r1 Proposed 5c517151). Owner: architect. Review: `docs/reports/adr-0025-review.md`
(r1 ACCEPT-WITH-CHANGES, C-1..C-9 integrated, mapping in §13). QM-1 (Orca laptop chord) is pending the owner.
Study and probes: `docs/reports/mouse-emulation-design.md`, `tools/platform-probes/mouse-emulation/`.
Owner decisions (fixed): `docs/decisions.md` row 2026-10-19 "Owner: mouse emulation via the gorca-helper virtual
pointer (A) ...". Relates to: ADR-0001 R3 (results as messages, deadlines), ADR-0005 (closed command set, KM-1,
keycode chords, explicit save), ADR-0006 (review cursor, object navigation, KM-2/KM-3 keypad grabs), ADR-0016
(settings scopes), ADR-0019 (extension effects), `docs/platform/org.gorca.ShellHelper.md` (helper v1),
`docs/reports/2026-10-12-screen-capture-options.md` (helper origin, allow-list),
`docs/reports/shell-segfault-20261019.md` (Mutter 48.7 NULL-modifiers rule). Supersedes the pointer part of
`docs/plan/phase-3-features.md` §9.4 (M-D portal fallback is not built, see §8). Milestone: M21 (pointer slices MO-*).

## 1. Context

A blind user needs the mouse where keyboard access fails: controls without an AT-SPI action, hover menus,
drag and drop, canvas apps, a "click here" that only reacts to real pointer input. NVDA and Orca both ship numpad
mouse keys. On gorca today the keypad `/` and `*` type characters and no pointer command exists
(owner report 2026-10-19, `decisions.md` row "Owner bug reports taken in ... (4)").

What the platform offers on GNOME 48.7 Wayland (verified in a private headless shell, study §2):

| Path | Result |
|---|---|
| A. gorca-helper (Shell extension) `seat.create_virtual_device(POINTER_DEVICE)` | works, no prompt, stage logical px, p50 0.4 ms / p99 1.1-1.3 ms round trip; can read the pointer (`global.get_pointer()`); knows window rects and stacking |
| B. `org.gnome.Mutter.RemoteDesktop` + `ConnectToEIS` + libei, from gorca | works, no prompt, no indicator; D-Bus absolute motion needs a ScreenCast; cannot read the pointer; knows only monitor regions |
| C. Portal RemoteDesktop | consent dialog; not needed |
| D. Mutter a11y interfaces | no pointer injection (`PointerLocator.QueryPointer` is not in 48.7) |
| E. AT-SPI `GenerateMouseEvent` | X11 only (`refs/at-spi2-core/atspi/atspi-device-x11.c:883`); this is why Orca's pointer path is a no-op on Wayland |
| F. uinput | root only |

The hard part is not injection but **coordinates**: AT-SPI on Wayland gives window-relative extents (GTK4 `SCREEN`
is 0,0, ADR-0006 facts), and the only source of a Wayland window's global origin is the compositor. GNOME Shell's
`org.gnome.Shell.Introspect.GetWindows` is sender-allow-listed and returns only width and height
(`refs/gnome-shell/js/misc/introspect.js:51,154-165`). The helper already exports frame and buffer rects
(`GetWindows`, helper v1).

### 1.1 Prior art

- **NVDA** (`refs/nvda/source/globalCommands.py`): left/right click act at the **current pointer** and speak first
  (GC:333-350); lock toggles (GC:357-373); "move mouse to navigator object" targets the review position's start
  point, else the navigator's centre, and refuses "Object has no location" (GC:1385-1422); clicks honour swapped
  buttons (`mouseHandler.py:310-330`). Desktop: KP/ KP* Shift+KP/ Shift+KP* NVDA+KP/ NVDA+KP*; laptop NVDA+[ ]
  NVDA+Ctrl+[ ] NVDA+Shift+M NVDA+Shift+N.
- **Orca** (`refs/orca/src/orca/mouse_presenter_command_definitions.py:50-83`, `ax_event_synthesizer.py:200-230,
  459-480`): "click review item" tries AT-SPI actions `click press jump open activate` first, then routes and
  clicks at the character or object centre; desktop KP/ KP* Orca+KP/, laptop Orca+7/8/9. Its pointer fallback uses
  `Atspi.Device.generate_mouse_event`, X11 only, so on Wayland only the action path works.
- **VoiceOver** (macOS, from documentation, not in `refs/`): VO+Shift+Space clicks the VO cursor item,
  VO+Cmd+F5 moves the mouse to the VO cursor. Same split as Orca: item click is the primary verb.
- **AccessKit** (`refs/accesskit/accesskit/src/lib.rs:291,349`): `Action::Click`, `Action::ShowContextMenu`; no pointer.
  The action-first path therefore maps onto Newton too.
- **Odilia**: no pointer support.

## 2. Decision

### 2.1 Summary

1. **Injection only through gorca-helper v2** (owner: A). Direct RemoteDesktop/libei (B) is **not built**; the
   output-side `PointerSink` trait keeps it addable (§8, verdict with evidence).
2. Pointer access is a **capability** `pointer` in the helper allow-list, **opt-in**, written by the owner; gorca
   never writes the allow-list.
3. Both command families ship: NVDA **click at pointer** and Orca **click review item** (action first, then route +
   click). NVDA presets bind the first, Orca presets the second; every command is bindable in every preset.
4. Non-gorca keypad chords are grabbed **only with Num Lock off and only while the pointer gate is open** (helper
   v2 + `pointer` cap + `mouse.enabled`). Gate changes reuse the existing coalesced `SetKeyGrabs` path on the same
   connection, rate-limited, never a reconnect and never Grab/UngrabKeyboard.
5. No refusal during Shell modal grabs (owner); the occlusion check in the helper handles Overview and dialogs.
6. Drag lock auto-releases after **120 s** (gorca, announced); the helper's backstop is 125 s.
7. Orca laptop left click moves to **gorca+Shift+7**; `gorca+7` stays `review.focus_follows_toggle` (owner).
8. Pointer I/O never runs on the key→speech path; every call has a deadline and its result comes back as a message.

### 2.2 Helper API v2 (`org.gorca.ShellHelper`, version 2)

Additive: interface name, object path and all v1 methods are unchanged. `Ping` returns 2. An incompatible change
would get a new interface name (`org.gorca.ShellHelper2`), never a reused one.

| Method | In | Out | Cap | Notes |
|---|---|---|---|---|
| `Ping` | - | `u` version | any | v1 |
| `Capabilities` | - | `as` caps granted to **this caller** | any | `["capture"]`, `["capture","pointer"]`, ... |
| `GetWindows`, `GetMonitors` | | | any | v1; v2 adds `transient-for` t to windows and `x-scale` d (XWayland's global effective scale, C-5) to every monitor row |
| `CaptureArea`, `CaptureWindowArea` | | | `capture` | v1 |
| `GetPointer` | - | `a{sv}`: `x` `y` d (stage), `window` t (topmost window at the pointer, 0 = Shell UI or none), `wx` `wy` d (relative to that window's frame), `monitor` i | `pointer` | read-only |
| `PointerMove` | `t` window, `s` origin, `d` x, `d` y, `a{sv}` opts | `a{sv}` reply (below) | `pointer` | moves only |
| `PointerClickAt` | `t` window, `s` origin, `d` x, `d` y, `u` button, `a{sv}` opts | `a{sv}` reply | `pointer` | expiry + guard + occlusion + move + press/release in one main-loop turn (`count = 2`: second click from a timer, H-7) |
| `PointerButton` | `u` button, `s` action, `a{sv}` opts | `a{sv}` reply | `pointer` | at the current pointer; action `click` (opts `count` 1 or 2, 30 ms gap), `press`, `release` |
| `PointerRelease` | - | `u` buttons released | `pointer` | releases every button this caller holds |

- `window` = Meta window id (`GetWindows.id`), 0 = stage space with no window guard (Shell UI, St extents).
- `origin` = `"frame"` | `"buffer"` | `"stage"`; for frame/buffer, (x, y) is relative to that rect of `window`,
  **resolved inside the compositor at call time** (a window moved since `GetWindows` is still hit correctly).
- `button` 1 = left, 2 = middle, 3 = right; anything else is `Error.Failed` (Clutter only logs unknown buttons).
  opts `logical` b (default true): swap 1 and 3 when `org.gnome.desktop.peripherals.mouse left-handed` is true,
  so "left click" means the primary button (NVDA `mouseHandler.py:310-330` parity; spike S-5).
- opts `if-occluded` s = `"refuse"` (default) | `"proceed"`.
- opts `not-after` x (**required** on every injecting method; CLOCK_MONOTONIC µs, the clock of
  `GLib.get_monotonic_time()` and Rust `Instant`; C-1): if the handler runs after it, nothing is injected and
  `outcome = "expired"`. A call without it is `bad-args:`. gorca sets it to the command's dispatch time + 150 ms.
- Reply `a{sv}`: `x` `y` d (stage point used), `window-at` t (topmost window at the point), `occluded` b, `clamped` b,
  `outcome` s = `"done"` | `"expired"` | `"refused-occluded"` | `"refused-outside"`, `held` u (bitmask of buttons this caller holds).
- **Popup family (C-2).** `family(window)` = `window` plus every window whose `transient-for` chain reaches
  `window` **through popup-type windows only**: `DROPDOWN_MENU`, `POPUP_MENU`, `COMBO`, `TOOLTIP`, `MENU`
  (xdg_popup menus are Meta windows of type DROPDOWN_MENU transient for the parent:
  `refs/mutter/src/wayland/meta-wayland-xdg-shell.c:1390,1686-1697`). A `NORMAL`, `DIALOG` or `MODAL_DIALOG`
  transient (an attached "Discard changes?" sheet) is **not** in the family; it is an occluder.
- **Window guard (MI-5):** for `window ≠ 0` the stage point must lie inside the frame or buffer rect of a member of
  `family(window)`, else `outcome = refused-outside` and nothing is injected.
- **Occlusion:** `occluded` is true when the reactive actor picked at the point
  (`global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y)`) does not belong to a member of `family(window)`:
  another window, a modal or attached dialog over the parent, a notification banner, the top bar, the Overview.
  Falls back to the v1 stacking-rect test (same family rule) if the pick is unavailable.
- **Stage guard (C-4):** for `window = 0` the picked actor must not be inside any `MetaWindowActor` (the St element
  that was resolved is gone if an app window is now under the point, e.g. quick settings closed between resolve and
  click), else `outcome = refused-outside`.
- **Errors:** `Error.AccessDenied` (not allow-listed or cap missing), `Error.Failed` with a short machine prefix:
  `locked:`, `rate:`, `bad-args:`, `no-window:`.

Helper-side rules (all handlers are a few Clutter calls plus validation, never a loop or a wait on the main loop):

- **H-1 Rate limit.** Own token bucket for `pointer` methods: 20 tokens/s, burst 40; a call costs 1, a `count = 2`
  click costs 2. `GetPointer` uses the v1 bucket (100/s).
- **H-1a Denied callers (C-6, also fixes v1).** A per-sender bucket (10 calls/s, burst 20) runs **before**
  `_authorize`; over it the call fails `rate:` with no `/proc` or config work. A negative authorization is cached per
  sender until the sender vanishes or the config stamp changes, and `DENIED` is logged at most once per (sender,
  stamp). Total sender-cache size stays bounded (v1: 256).
- **H-2 Validation.** Finite numbers only, |x|,|y| ≤ 65535; points outside the monitor union clamp (Clutter does it,
  `clamped` reports it).
- **H-3 Held buttons, single holder (C-7d).** The device has at most one **holder** (a unique bus name). A `press`
  from another caller while any button is held fails `busy:`; `release`/`PointerRelease` from a non-holder release
  nothing. Holds are released on: `release`, `PointerRelease`, the holder's name vanishing (`NameOwnerChanged`,
  already watched), `disable()`, session lock (the unlock dialog never sees a drag), and 125 s after the press
  (backstop; gorca releases at 120 s, §2.6). A `press` of a held button by the holder is a no-op that reports `held`.
- **H-4 Lock.** Every `pointer` method fails `locked:` while `Main.sessionMode.isLocked` (v1 `_guard` already does).
- **H-5 Audit.** One journal line per injection, rate-limited to 1/s with a suppressed counter:
  `gorca-helper: pointer <op> b<n> stage=(x,y) win=<id> outcome=<o> caller=<exe>`.
- **H-6 One device.** One `POINTER_DEVICE` virtual device, created lazily on the first `pointer` call, destroyed in
  `stop()`. No keyboard or touch device is ever created.
- **H-7 Double click timer (C-7e).** `count = 2` presses/releases once, then a 30 ms `GLib.timeout_add` does the second
  click after re-checking `not-after` + 30 ms, the lock state and that the caller is still present. The timer id is
  kept and removed in `stop()`, on lock and on caller vanish; the reply is sent after the second click (or with
  `outcome = expired` if it was dropped).

### 2.3 Allow-list format with capabilities

```json
{"allowed": [
  {"exe": "/usr/bin/gorca",
   "cgroup": "/user.slice/user-1000.slice/user@1000.service/app.slice/gorca.service",
   "caps": ["capture", "pointer"]}
]}
```

- **AL-1** `caps` absent ⇒ `["capture"]` (v1 files keep working, pointer stays off). Unknown cap strings are ignored
  with one warning. A rule without `"capture"` denies capture. **First matching rule wins** (as v1 `_authorize`
  already returns on the first match); caps of later matching rules are not merged (C-7f), so a narrower rule
  placed first can never be widened by a later one.
- **AL-2** `"pointer"` is opt-in. Only the owner writes it (root-owned `/etc/gorca/shell-helper.json`, authoritative
  if present, or the user's `~/.config/gorca/shell-helper.json`, mode not group/world-writable). gorca never writes or
  rewrites either file (MI-9); its "Mouse not available" message names the file and the cap instead.
- **AL-3** Identity is unchanged from v1 (exe + full cgroup + pidfd + start time, optional label).
- **AL-4** Config changes are picked up by the v1 stamp check on the next call; gorca re-probes (§2.5).

### 2.4 Coordinate mapping (connector + sink, never core)

The core never sees stage coordinates. Two neutral types cross the core boundary:

```rust
// gorca-output-api (neutral, no zbus)
pub struct ToplevelHint { pub pid: u32, pub title: Arc<str>, pub atspi_origin: (i32, i32), pub atspi_size: (i32, i32),
                          pub child_space: ChildSpace }
pub enum ChildSpace { Toplevel, DevicePixels }       // DevicePixels = Chromium quirk row (below)
pub enum PointTarget {
    Window { top: ToplevelHint, x: f64, y: f64 },   // in the toplevel's own AT-SPI coordinate space
    Stage { x: f64, y: f64 },                       // St (Shell) extents are stage coordinates
}
```

The AT-SPI connector (`PointerResolver`, `gorca-connector-atspi`) builds a `PointTarget` from a node or a
`(node, offset)`: target point = character rect centre for a text position, else the centre of the object's
extents clipped to its toplevel's extents (empty intersection ⇒ `NoLocation`, see PR-3). The sink
(`gorca-shell-helper`, §2.9) matches the toplevel to a Meta window and computes the window-relative offset;
the helper adds the origin at call time. The rule from the study, verified by a real click per row:

`offset = (p - atspi_origin) / ratio / child_div`, `origin` = the Meta rect (frame or buffer) whose size equals
`atspi_size / ratio`; `ratio` = 1, or `ceil(monitor scale)` for an XWayland window whose AT-SPI size is that multiple
of the frame; `child_div` = monitor scale for `ChildSpace::DevicePixels`, else 1.

**XWayland ratio is global (C-5).** Mutter's X scale is one value for the whole X screen: `ceil(highest monitor
scale)`, or the `xwayland-scaling-factor` override, or 1 in physical layout mode
(`refs/mutter/src/wayland/meta-xwayland.c:1368-1392`). The helper reports it as `x-scale` in `GetMonitors` (read
from the same Mutter state, not recomputed in gorca), and `ratio` for X11 windows is that value whatever monitor the
window is on (a 2x laptop plus a 1x external screen: ratio 2 on both).

| Client | AT-SPI space | Mapping (study §3) |
|---|---|---|
| GTK4 Wayland | `WINDOW` relative to the GtkWindow = Meta frame | frame + WINDOW |
| GTK3 Wayland | `WINDOW` = `SCREEN`, relative to the buffer incl. CSD shadow | buffer + WINDOW |
| Firefox Wayland | toplevel (0,0) = Meta frame | frame + extent |
| Chromium / Electron Wayland | toplevel logical, children in device px | frame + child / scale (quirk row `child_space = DevicePixels`, toolkit Chromium; Electron unverified, spike S-2) |
| XWayland (global X scale, C-5) | X root space; toplevel `WINDOW` origin includes the SSD title bar | frame + (WINDOW - toplevel origin) / `x-scale` |
| St (Shell) | stage | `PointTarget::Stage` |
| Qt | unverified | spike S-3; until then `NoLocation` is acceptable, action-first still works |

Window matching (sink, pure function, table-tested):

- **WM-1** Candidates: `GetWindows` rows that are visible, not minimized, with `pid == hint.pid`. If none (Flatpak:
  the AT-SPI pid may be the a11y bus proxy's), all visible rows.
- **WM-2** Keep rows whose frame or buffer size matches `atspi_size / ratio` within ±2 px, with ratio = 1 for
  Wayland rows (`client-type`) and ratio = `x-scale` for X11 rows; this also picks `origin`.
- **WM-3** Tie-break: equal title, then the focused window, then the topmost. Still ambiguous or empty ⇒
  `NoLocation` ("No location"), never a guess.
- **WM-4** The final safety net is the helper's window guard: a wrong mapping is refused, not clicked.

`mouse.review_to_pointer` uses the inverse: `GetPointer` gives `window` and `wx, wy`; the sink inverts the rule
(× `ratio`, × `child_div`, + `atspi_origin`) for the AT-SPI app whose toplevel matches that window (same WM rules
backwards: window → pid + size → AT-SPI toplevel), then `Component.GetAccessibleAtPoint(WINDOW)` descends to the
deepest hit (bounded: ≤ 8 calls, 50 ms each).

### 2.5 Availability and runtime-gated grabs

```rust
enum PointerAvail {                 // owned by the pointer actor, mirrored to the Reader as PointerMsg::Avail
    Absent,                         // no owner of org.gorca.ShellHelper
    Squatted,                       // owner != owner of org.gnome.Shell (MI-7)
    Denied,                         // AccessDenied: gorca not allow-listed
    TooOld { version: u32 },        // Ping < 2
    NoCap,                          // Capabilities() lacks "pointer"
    Unresponsive,                   // 3 consecutive deadline misses
    Ready { version: u32 },
}
```

| From | Event | To | Effect |
|---|---|---|---|
| any | helper or Shell `NameOwnerChanged`, owner present | (probe) | owner check, `Ping`, `Capabilities` (150 ms each) |
| probe | owner mismatch / AccessDenied / v<2 / no cap / ok | Squatted / Denied / TooOld / NoCap / Ready | publish |
| any | helper owner gone | Absent | publish; held buttons reported released |
| Ready | 3 consecutive deadline misses or expired outcomes | Unresponsive | publish; re-probe after 2 s, 4 s, ... 30 s; **gate stays open** (C-3) |
| Unresponsive | probe succeeds | Ready | publish |
| Denied, NoCap, TooOld | poll every 10 s (one `Ping`/`Capabilities`) or any mouse command | (probe) | owner may have just edited the allow-list |
| Ready | `AccessDenied` or `Error.Failed locked:` on a call | probe / stays Ready | locked is not loss; the command speaks "not available while locked" |

Gate (Reader): `pointer_open = avail ∈ {Ready, Unresponsive} && settings.mouse.enabled` (C-3). A Shell stall
stalls key delivery on the same main loop, so closing the gate then buys nothing, queues a `SetKeyGrabs` into the
stalled Mutter and, with RG-5 hysteresis, lets KP/ and KP* type into the focused field for ≥ 2 s after recovery.
While `Unresponsive` the keys stay grabbed and the commands say "Mouse not responding" without calling the helper
(except one probe). The gate closes only on `Absent`, `Squatted`, `Denied`, `TooOld`, `NoCap` or
`mouse.enabled = false`.

- **RG-1 Static availability stays static.** `Availability` keeps `Implemented | Planned`; `CmdSpec` gains
  `gate: Option<RuntimeGate>` with `enum RuntimeGate { Pointer }`. `bound_of()` is unchanged (gated commands are
  active in the keymap); the grab compiler splits each mode's chords into always-on and `gated[RuntimeGate]`.
- **RG-2 Only non-gorca chords are affected.** gorca+ chords need no grab (Mutter grabs every key while the gorca
  modifier is held, KM-4) and always reach the command, which answers "Mouse not available ..." when the gate is
  closed. Laptop presets bind only gorca+ mouse chords, so their grab set never changes with the gate.
- **RG-3 Keypad chords are exact and Num Lock off only** (ADR-0006 KM-2/KM-3, `chord_grabs` "exact"): `KP_Divide`
  and `KP_Multiply` have one keysym in both Num Lock states, so the `(KP_Divide, 0)` grab plus its Caps Lock variant
  matches only Num Lock off; with Num Lock on nothing is grabbed and the keypad types `/` and `*`.
- **RG-4 One path, no churn.** `Mode::grab_set(keys, gates)` adds `gated[g]` when `g` is open. A gate change emits
  `ReaderEffect::SetGrabSet` exactly like the D-2 `tab_grabs` change: it becomes a new generation of the existing
  session's coalesced `SetKeyGrabs` on the **same** connection. A gate change never opens a connection, never sends
  `GrabKeyboard`/`UngrabKeyboard` (only a real `grab_all` change does), and never ends a session on a slow reply
  (`decisions.md` 2026-10-19 Mutter 48.7 mitigation).
- **RG-5 Hysteresis.** Closing is immediate (fail-safe: the keys type again). Opening waits until `Ready` has held
  for 2 s and happens at most once per 2 s (latest state wins), so a helper toggled by the owner's hot reload costs
  two `SetKeyGrabs`, and a flapping helper at most one open and one close per 2 s.
- **RG-6 Keys in flight.** A gated chord that arrives while the committed `GrabView` does not contain it is watched,
  not grabbed: the lane must not execute it (the app already received it). Existing `Ambiguous` classification
  covers the window between wanted and committed.
- **RG-7 Learn mode** describes gated commands normally and appends "not available now" when the gate is closed.

### 2.6 Core commands

All are `Implemented` (when their slice lands), `contexts = ANY`, `interrupts = true`, `repeatable = false` (holding
KP/ never spams clicks), `max_press = 1` unless noted (two quick presses are two clicks, which applications already
read as a double click at the same point; no multi-press delay is added).

| Command | Semantics | Gate (grab) |
|---|---|---|
| `mouse.left_click`, `mouse.right_click`, `mouse.middle_click` | NVDA: click at the current pointer. Speak "Left click" etc. at dispatch. If that button is locked: release it instead and say "Left button unlocked". | Pointer |
| `mouse.left_click_item`, `mouse.right_click_item` | Orca: target = review position (character) or navigator object. `mouse.item_click = action_first` (default): left tries `ActionReq::Click` (names `click press jump open activate`, Orca order), right tries `ActionReq::ContextMenu` (`show-context-menu`, `menu`); on no action (or `pointer` mode) route + `PointerClickAt`. Speech after the outcome. | Pointer (the action path also works with the gate closed) |
| `mouse.left_lock_toggle`, `mouse.right_lock_toggle` | Press and hold at the current pointer, or release. "Left button locked"/"unlocked". Auto release at 120 s with "Left button released". | Pointer |
| `mouse.route_to_review` | NVDA "move mouse to navigator": review position in screen/text review, else navigator object centre. "Mouse moved" (or silent per `mouse.announce_move`). | Pointer |
| `mouse.review_to_pointer` | NVDA "navigator to mouse": review cursor (object) to the deepest object under the pointer, speak it. | Pointer |
| `mouse.say_position` (`max_press = 2`) | ×1 window title and window-relative position; ×2 stage position and monitor. | Pointer |

`ActionReq` (connector API) gains `Click { node }` and `ContextMenu { node }`; AT-SPI maps names as above,
AccessKit/Newton maps to `Action::Click` / `Action::ShowContextMenu`.

Drag lock and button state (Reader-owned, pure, **one machine per lockable button**, C-7c):

```rust
enum HoldState { Up, Pressing { req: PtrReqId }, Held { since: Instant }, Releasing { req: PtrReqId }, Unknown }
struct Holds { left: HoldState, right: HoldState }   // independent; both may be Held (left drag with right held)
```

The 120 s timer is per button and is a Reader timer message (`PointerMsg::HoldExpired { button, since }`, ignored if
`since` no longer matches). The table below applies to each button separately.

| From | Event | To | Effect |
|---|---|---|---|
| Up | lock toggle | Pressing | `PointerButton(press)` |
| Pressing | reply `held` has button | Held | speak "locked", arm 120 s timer |
| Pressing / Releasing | deadline miss, `expired` or error | Unknown | speak error; next successful call sends `PointerRelease` first |
| Held | lock toggle / click of same button | Releasing | `PointerButton(release)` |
| Held | 120 s timer | Releasing | speak "released after 2 minutes" when done |
| Held | focus window changes | Held | speak "Left button locked" (MP-2 reminder; no release, drags between windows are legitimate) |
| Held, Unknown | avail leaves Ready / gorca shutdown | Up | `PointerRelease` best effort; the helper releases on vanish anyway (H-3) |
| Releasing | reply | Up | speak "unlocked" |

### 2.7 Execution model, latency, backpressure

- **EX-1** The Reader never does pointer I/O. A command produces `ReaderEffect::Pointer(PointerReq)` (a snapshot of
  the target taken at key time) and receives `PointerMsg::{Done, Refused, Failed, Avail}`. One pointer actor owns
  the helper proxy, the `PointerAvail` machine and the window cache; it runs on its own task and connection-call
  budget (ADR-0001 R3), never on the input lane.
- **EX-2 Deadlines and expiry (C-1).** Each helper call 150 ms; a whole command (resolve extents + `GetWindows` +
  `PointerClickAt`) 250 ms; `review_to_pointer` 500 ms. Every injecting call carries `not-after` = command dispatch
  time + 150 ms (item clicks: time of the injecting call + 150 ms, within the 250 ms command budget), so a call the
  Shell runs late injects nothing (`expired`). A late reply is still read: `done` after gorca already said "Mouse not
  responding" is logged (cannot happen when the helper honours `not-after`, so it is a helper bug signal).
- **EX-3 Queue and overdue stop (C-1).** One in flight plus at most 4 pending (FIFO, user clicks are not
  coalesced); overflow drops the new request with "Mouse busy". **While the in-flight call is past its deadline
  nothing else is dispatched**: every pending request is dropped with one "Mouse not responding", and new requests
  are refused the same way until the overdue call answers or the probe succeeds. A plain click that speaks at
  dispatch (EX-4) and then expires adds "Mouse not responding". Epoch advances do not cancel pointer requests (they
  are user actions), only speech.
- **EX-4 Speech timing.** Plain clicks and lock toggles speak at dispatch (gorca-side work is a table lookup and an
  effect: within the 15 ms budget). Item clicks and routes speak on the outcome (study p99 1.3 ms per helper call,
  so typically ≤ 5 ms after the key). Speech produced by the clicked application's focus change supersedes ours.
- **EX-5** Hit testing and extents fetches use the existing connector calls with their deadlines; nothing new
  enters ingress or the coalescer.

### 2.8 Settings (ADR-0016 rows)

| Key | Type, default | Scope | Vendor | Privacy | Meaning |
|---|---|---|---|---|---|
| `mouse.enabled` | bool, true | Base | no | no | master switch; false closes the gate (keys type) |
| `mouse.item_click` | `action_first` \| `pointer`, `action_first` | App | yes | no | item click policy; add-ons may set `pointer` for apps whose actions misbehave |
| `mouse.if_covered` | `refuse` \| `click`, `refuse` | App | no | no | item clicks when the target is covered |
| `mouse.announce_move` | bool, true | Focus | yes | no | speak "Mouse moved" after a route |

No timeout setting: 120 s is the owner's fixed value (MI-6).

### 2.9 Crates and interfaces

- `gorca-output-api`: `PointerSink` trait (output side, like speech), `PointTarget`, `ToplevelHint`, `Button`,
  `ButtonAct`, `PointerOutcome`, `PointerError`. Neutral, no zbus.
- `gorca-shell-helper` (new; zbus): the helper client (owner check, probe, deadlines), `PointerAvail`, window
  matcher (WM-1..4), `PointerSink` impl. Later the capture code (ADR-0002) moves here too.
- `gorca-connector-atspi`: `PointerResolver` (node/offset → `PointTarget`, Chromium quirk row), `ActionReq::Click`
  and `ContextMenu`, inverse hit test.
- `gorca-core`: commands, `RuntimeGate`, `HoldState`, gated grab compilation; no connector or zbus types.

```rust
pub trait PointerSink: Send + Sync {
    fn click_at(&self, t: PointTarget, b: Button, opts: ClickOpts, d: Deadline) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;
    fn move_to(&self, t: PointTarget, d: Deadline) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;
    fn button(&self, b: Button, act: ButtonAct, d: Deadline) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;
    fn release_all(&self, d: Deadline) -> BoxFuture<'_, Result<(), PointerError>>;
    fn position(&self, d: Deadline) -> BoxFuture<'_, Result<PointerPos, PointerError>>;
}
```

### 2.10 Invariants

- **MI-1** Every injection traces to a user command key event of this session. No automatic clicks; no WIT world or
  extension effect can inject pointer input (ADR-0019: no pointer effect exists; adding one needs a new ADR); page
  content can only influence *where* a user-requested click lands, through its extents.
- **MI-2** Pointer I/O never blocks the Reader or the input lane (EX-1).
- **MI-3** Non-gorca mouse chords are grabbed iff the gate is open, Num Lock off variants only (RG-3).
- **MI-4** Gate-driven grab changes: same connection, `SetKeyGrabs` only; ≤ 1 open per 2 s, close immediate, so
  ≤ 2 changes per 2 s (RG-4, RG-5).
- **MI-5** A window-targeted injection lands inside `family(window)` (popup-type transients only) or not at all
  (helper guard). It lands on an *unoccluded* point unless the caller passed `if-occluded = proceed`
  (`mouse.if_covered = click`), in which case it may hit whatever covers the point. A stage target lands only
  outside every app window (C-4). Nothing lands after `not-after` (C-1).
- **MI-6** A held button is released by gorca at 120 s and by the helper at 125 s at the latest; the helper has at
  most one holder (H-3).
- **MI-7** gorca calls the helper only if its owner equals the owner of `org.gnome.Shell` (v1 client rule).
- **MI-8** Nothing is injected while the session is locked (H-4).
- **MI-9** gorca never writes the helper allow-list.

### 2.11 Failure modes

| Failure | Behaviour |
|---|---|
| Helper absent, disabled, JS error at load | `Absent`; keypad types; gorca+ chords say "Mouse not available: gorca helper is not running"; item clicks still use actions |
| Helper v1 | `TooOld`; message asks to update the helper |
| No `pointer` cap | `NoCap`; message names the allow-list file and the cap |
| Squatter owns the name | `Squatted`; no call is made; logged once |
| Shell stalls (host load) | deadline miss → "Mouse not responding", pending requests dropped, nothing dispatched while overdue; queued calls the Shell runs later are `expired` and inject nothing; `Unresponsive` after 3, gate stays open (keys stay grabbed, no `/` typed); key→speech unaffected |
| Shell restarts / hot reload | owner change → `Absent` → probe → `Ready`; helper released buttons on `disable()`; gorca reports a lock it held as released |
| Window moved/closed between resolve and click | origin resolved at call time; closed ⇒ `no-window:` ⇒ "No location" |
| Target covered (other window, banner, Overview, modal or attached dialog over the parent) | `refused-occluded` ⇒ "Covered by {window}" (or "Covered" for Shell UI); `mouse.if_covered = click` proceeds |
| Mapping ambiguous or wrong | `NoLocation` (WM-3) or `refused-outside` (guard) ⇒ "No location" |
| Item off-screen | `ActionReq::ScrollTo`, re-fetch extents once (bounded), else "No location" |
| Session locked | `locked:` ⇒ "Not available while the screen is locked"; held buttons already released by the helper |
| Rate limit | `rate:` ⇒ "Mouse busy" |
| Another caller holds a button | `busy:` ⇒ "Mouse busy" |
| St element gone (popover closed) | stage guard `refused-outside` ⇒ "No location" |
| gorca crashes with a button held | helper releases on name vanish (H-3); Mutter also releases when the device is destroyed |

## 3. Preset bindings (conflict-checked against all four presets, 2026-10-19)

| Command | nvda-desktop | nvda-laptop | orca-desktop | orca-laptop |
|---|---|---|---|---|
| `mouse.left_click` | `kp_divide` | `gorca+bracketleft` | - | - |
| `mouse.right_click` | `kp_multiply` | `gorca+bracketright` | - | - |
| `mouse.left_click_item` | - | - | `kp_divide` | **`gorca+shift+7`** (new) |
| `mouse.right_click_item` | - | - | `kp_multiply` | `gorca+8` |
| `mouse.left_lock_toggle` | `shift+kp_divide` | `gorca+ctrl+bracketleft` | `shift+kp_divide` | `gorca+ctrl+7` |
| `mouse.right_lock_toggle` | `shift+kp_multiply` | `gorca+ctrl+bracketright` | `shift+kp_multiply` | `gorca+ctrl+8` |
| `mouse.route_to_review` | `gorca+kp_divide` | `gorca+shift+m` | `gorca+kp_divide` | `gorca+9` |
| `mouse.review_to_pointer` | `gorca+kp_multiply` | `gorca+shift+n` | `gorca+kp_multiply` | `gorca+ctrl+9` |
| `mouse.say_position` | `gorca+shift+kp_divide` | `gorca+ctrl+shift+m` | `gorca+shift+kp_divide` | `gorca+shift+9` |
| `mouse.middle_click` | unbound | unbound | unbound | unbound |

- Every chord above was checked free in every context of its preset (`global`, `browse`) by loading the four TOMLs;
  `shift+m`/`shift+n` exist only as non-gorca browse quick-nav, a different chord. `gorca+7` stays
  `review.focus_follows_toggle` in all presets (owner). Orca laptop `gorca+shift+7` keeps the 7-8-9 muscle memory
  with one extra modifier; `gorca+ctrl+7/8` follow NVDA's "Ctrl = lock" on laptops. Orca has no lock or
  "navigator to mouse" commands; gorca adds them on free chords.
- Gated (RG-1) non-gorca chords: `kp_divide`, `kp_multiply`, `shift+kp_divide`, `shift+kp_multiply` (desktop presets
  only). Laptop presets have no gated grab.
- MO-4 adds a preset test: no duplicate resolved chord per (preset, context), every `mouse.*` chord free of the
  extension default chords (`extensions/*/ext.toml`) and of the ADR-0016 profile chords.

## 4. Strings (catalog `mouse.msg`, en and ru)

| id | en | ru |
|---|---|---|
| `mouse-left-click` | Left click | Левый щелчок |
| `mouse-right-click` | Right click | Правый щелчок |
| `mouse-middle-click` | Middle click | Средний щелчок |
| `mouse-left-locked` | Left button locked | Левая кнопка зажата |
| `mouse-left-unlocked` | Left button unlocked | Левая кнопка отпущена |
| `mouse-right-locked` | Right button locked | Правая кнопка зажата |
| `mouse-right-unlocked` | Right button unlocked | Правая кнопка отпущена |
| `mouse-lock-expired` | {button} button released after 2 minutes | {button} кнопка отпущена через 2 минуты |
| `mouse-button-left` / `-right` | Left / Right | Левая / Правая |
| `mouse-moved` | Mouse moved | Мышь перемещена |
| `mouse-no-location` | No location | Нет расположения |
| `mouse-covered` | Covered by {window} | Закрыто окном {window} |
| `mouse-covered-shell` | Covered | Закрыто |
| `mouse-unavailable-helper` | Mouse not available: gorca helper is not running | Мышь недоступна: помощник gorca не запущен |
| `mouse-unavailable-old` | Mouse not available: gorca helper is too old | Мышь недоступна: помощник gorca устарел |
| `mouse-unavailable-cap` | Mouse not available: allow pointer in {file} | Мышь недоступна: разрешите pointer в {file} |
| `mouse-unavailable-denied` | Mouse not available: gorca is not allowed by the helper | Мышь недоступна: помощник не разрешает gorca |
| `mouse-unavailable-off` | Mouse emulation is off | Эмуляция мыши выключена |
| `mouse-locked-session` | Not available while the screen is locked | Недоступно при заблокированном экране |
| `mouse-not-responding` | Mouse not responding | Мышь не отвечает |
| `mouse-busy` | Mouse busy | Мышь занята |
| `mouse-nothing` | Nothing under the mouse | Под мышью ничего нет |
| `mouse-position` | {window}, {x}, {y} | {window}, {x}, {y} |
| `mouse-position-screen` | Screen {x}, {y}, monitor {n} | Экран {x}, {y}, монитор {n} |
| `mouse-not-available-now` | not available now | сейчас недоступно |

Learn-mode descriptions (`commands.msg`):

| id | en | ru |
|---|---|---|
| `cmd-mouse-left-click` | click the left mouse button at the mouse position | щёлкнуть левой кнопкой мыши в позиции указателя |
| `cmd-mouse-right-click` | click the right mouse button at the mouse position | щёлкнуть правой кнопкой мыши в позиции указателя |
| `cmd-mouse-middle-click` | click the middle mouse button at the mouse position | щёлкнуть средней кнопкой мыши в позиции указателя |
| `cmd-mouse-left-click-item` | click the review item with the left button | щёлкнуть левой кнопкой по элементу просмотра |
| `cmd-mouse-right-click-item` | click the review item with the right button | щёлкнуть правой кнопкой по элементу просмотра |
| `cmd-mouse-left-lock-toggle` | lock or unlock the left mouse button | зажать или отпустить левую кнопку мыши |
| `cmd-mouse-right-lock-toggle` | lock or unlock the right mouse button | зажать или отпустить правую кнопку мыши |
| `cmd-mouse-route-to-review` | move the mouse to the review cursor | переместить мышь к курсору просмотра |
| `cmd-mouse-review-to-pointer` | move the review cursor to the object under the mouse | переместить курсор просмотра к объекту под мышью |
| `cmd-mouse-say-position` | say the mouse position, twice for screen coordinates | сообщить позицию мыши, дважды — экранные координаты |

## 5. Threat model and mitigations

Baseline (study §4, `docs/platform/libei-and-portals.md`): any process of this user can already inject pointer and
keyboard input without consent through `org.gnome.Mutter.RemoteDesktop` and can overwrite the extension directory.
The helper is therefore **not** a boundary against malicious same-uid code; it must not *widen* what a confused
or partly compromised gorca, or a non-gorca caller, can do.

| Threat | Mitigation |
|---|---|
| Another local process uses the helper to click | allow-list identity (exe + cgroup + pidfd), `pointer` cap opt-in (AL-1..3) |
| Non-allow-listed caller floods the helper (compositor main-loop work, journal spam) | H-1a pre-auth bucket, negative cache, one DENIED line per sender and stamp |
| Click into a destructive dialog that appeared over the target | C-2 popup-only family; dialogs are occluders |
| Clicks gorca reported as failed land later | C-1 `not-after`, overdue stop |
| Squatter impersonates the helper (fake `GetPointer`, swallowed clicks) | MI-7 owner check before every session |
| Web page or app content steers gorca into clicking | MI-1 (user command only); window guard MI-5; occlusion refusal; every click spoken |
| Extension/WASM plugin injects | no WIT pointer effect (MI-1); plugins run in the host, not the core |
| Click on lock screen / unlock dialog | H-4, MI-8 |
| Stuck button (drag lock forgotten, crash) | MI-6, H-3, MP-2 reminder |
| Click storm (bug loop, held key) | `repeatable = false`, EX-3 queue bound, H-1 bucket |
| Clicks into Shell modal dialogs (polkit, quick settings) | allowed by owner decision; they need `window = 0` (stage) targets, which only come from St extents; spoken |
| JS bug freezing the compositor | handlers bounded (no loops/waits, H-6), rate limit, review of `helper.js` diff as compositor code |
| Hot re-import of user-writable JS | `.dev` marker only on the owner's dev install; production install without it (study §4 item 10) |
| Audit | H-5 journal line; gorca log line per click with outcome (no window titles in default logs) |

Residual risk: JavaScript in the compositor process. It is the same exposure the v1 capture code already has.

## 6. Test plan

All pointer tests run in a **private** headless shell (`tools/platform-probes/mouse-emulation/rig.sh`, unique Wayland
display, runtime dir and buses; teardown only by recorded PIDs and the rig's process group). No test touches the live
session: no pointer injection, no `org.gnome.Orca.KeyboardMonitor` ownership, no edits of the live allow-list.

| Layer | Tests |
|---|---|
| Helper (`contrib/gnome-shell-extension/test-pointer.sh`) | positives: move, click 1/2/3, count 2, press/move/release drag, `GetPointer`, `PointerClickAt` with frame and buffer origin; negatives: not allow-listed, caps absent (v1 file ⇒ capture only), cap missing, rate limit (41st call in a burst), NaN/huge coords, button 4, outside window, occluded by another window, Overview open, locked session, caller vanishes with a button held (released ≤ 1 s), `disable()` with a button held, 125 s backstop (time-scaled build constant) |
| Mapping (Rust unit, table) | every study §3 row as data (AT-SPI rects + Meta rects + scale → expected stage point); WM-1..3 incl. Flatpak pid miss, two same-size windows, XWayland ratio 2, Chromium device px at 1, 1.25, 1.5, 2 |
| Core (fake helper on a private bus) | `PointerAvail` table; gate flips ⇒ exactly one `SetKeyGrabs` each, zero Grab/UngrabKeyboard, zero reconnects; 50 flips in 10 s ⇒ ≤ 5 `SetKeyGrabs`; laptop preset ⇒ zero grab changes; `HoldState` table incl. Unknown; queue overflow; late replies ignored; Reader K1 unchanged with the helper stalled 5 s |
| Keyboard (private shell + KeyboardMonitor harness) | Num Lock on: KP/ and KP* type `/` `*` (0 swallowed in 1000 presses); gate closed, Num Lock off: type too; gate open, Num Lock off: grabbed and executed |
| Review r1 rows (C-9) | (1) stall burst: SIGSTOP the private shell, queue 5 clicks, SIGCONT ⇒ 0 clicks in the client log, all `expired`; (2) GTK modal and attached dialog over a parent button ⇒ `refused-occluded`, 0 clicks on the dialog; (3) lock taken while a button is held ⇒ released before the unlock dialog shows, no drag seen by it; (4) stale St target: close quick settings between resolve and click ⇒ `refused-outside`; (5) mixed scales 1 + 2 with an XWayland window on each monitor ⇒ both clicked; (6) denied-caller flood 1000 calls/s for 10 s ⇒ main-loop p99 within M-7, ≤ 1 DENIED journal line; (7) `disable()` during the `count = 2` gap ⇒ no second click, no stuck button, timer removed; (8) C-3: 3 s SIGSTOP then 5 s of KP/ presses ⇒ 0 `/` typed; (9) `tools/platform-probes/shell-segfault/repro.py` modes re-run with gate flips interleaved ⇒ 0 shell crashes |
| End to end matrix (oracle: client click log or page title) | toolkits GTK3, GTK4, Firefox ESR, Chromium, Electron, XWayland GTK3; scales 1, 1.25, 1.5, 2; two monitors with an offset; popups: GTK3 menu, GTK4 popover, Firefox `<select>` dropdown, Chromium context menu; St: top bar, quick settings; item click action-first vs `pointer`; route then click; drag lock across two windows; `review_to_pointer` round trip |

## 7. Rollout

1. Helper v2 lands in `contrib/gnome-shell-extension/` with `docs/platform/org.gorca.ShellHelper.md` updated; only
   `helper.js` changes (no `metadata.json`/`extension.js` change, so no re-login on the `.dev` install).
2. **Owner** hot-reloads on the live session: `contrib/gnome-shell-extension/dev.sh sync && dev.sh disable &&
   dev.sh enable`, then adds `"caps": ["capture", "pointer"]` to the rule. **Agents never run these on the live
   session** and never edit the live allow-list. A production install (no `.dev`) needs a re-login per `helper.js`
   update.
3. gorca with a v1 helper or no cap behaves as today (keys type), so gorca and helper can land in either order.

## 8. Alternatives and the hybrid verdict

| Option | Verdict |
|---|---|
| **A only (helper)** | **chosen**; the only path with window origins, occlusion, window guard, pointer read-out |
| A + B fallback (direct RemoteDesktop/libei when the helper is unavailable) | **rejected now**, see below; seam kept (`PointerSink`) |
| B only | rejected: cannot map Wayland window coordinates, cannot read the pointer, no guard |
| Portal RemoteDesktop (phase-3 §9.4 M-D) | rejected: consent dialog, no window origins either, more moving parts |
| AT-SPI actions only | kept as the first step of item clicks; cannot click at the pointer, drag or hover |

**Hybrid verdict: not worth it now (confidence high).**

1. *B serves almost none of the use cases without the helper.* With the helper gone there is no source of a Wayland
   window's global origin (GTK4 `SCREEN` = 0,0; `Shell.Introspect.GetWindows` is allow-listed and has no x/y,
   `introspect.js:51,154-165`), so B could route only to St (Shell) items and XWayland windows, and it cannot read
   the pointer (study §2). What remains is "click wherever the pointer happens to be", which a blind user cannot
   aim. Route, item click (pointer step), `review_to_pointer` and `say_position` all fail. Item clicks still work
   through AT-SPI actions without either path.
2. *It would bypass consent.* The usual reason the helper is unavailable is that the owner disabled it or did not
   grant `pointer`. A silent fallback to an unguarded path defeats the opt-in (AL-2); it would need its own opt-in
   setting, a second consent model to explain.
3. *Security:* B adds no new attack surface (any same-uid process can already use RemoteDesktop), but gorca would
   lose the helper's lock refusal, window guard and occlusion check and would have to re-implement the lock check
   client-side (a race against the Shell's own state).
4. *Cost:* a libei sender (C FFI via `c-cpp-engineer`, or a new pure-Rust EIS dependency), a RemoteDesktop session
   state machine with its own failure modes, EIS region mapping, its own deadlines, and a second end-to-end matrix
   (6 toolkits × 4 scales × popups) that the owner requires for any fallback: about 4-5 extra slices for a path that
   only clicks at an unknown position.
5. *Failure correlation:* the helper fails mainly when the Shell's JS environment breaks (GNOME upgrade, JS error);
   B then still works, but (1) still applies.

Revisit when any of these holds: (a) the K-2 key re-injection work (`gorca-input-inject`) builds an EIS client
anyway (cost of B drops to one slice); (b) a GNOME release breaks the extension for longer than one release cycle;
(c) the compositor exposes window origins or a pointer query to ATs (`PointerLocator.QueryPointer` already exists in
`refs/mutter` main, `meta-a11y-manager.c:handle_query_pointer`, d82671c; track it per GNOME release); (d) the Newton connector needs injection without a Shell extension.

## 9. Consequences

- Positive: NVDA and Orca users get their mouse keys with exact Wayland coordinates on all target toolkits; Orca
  semantics exceed Orca on Wayland; the keypad still types with Num Lock on or without the helper.
- Negative: mouse emulation depends on a Shell extension (GNOME-only, version-coupled JS); per-toolkit coordinate
  quirks need a test matrix and quirk rows; a new runtime-gated grab notion in the compiler.
- Neutral: the helper becomes a two-capability service; its allow-list gains a field; core stays connector-free.

## 10. Metrics to validate

| Id | Stimulus → end | Gate |
|---|---|---|
| M-1 | mouse key → feedback speech (plain click, lock), gorca side | p99 ≤ 15 ms (same budget as every key) |
| M-2 | mouse key → click delivered in the client (private shell, item click, pointer path) | p99 ≤ 20 ms gorca + helper, ≤ 50 ms wall (phase-3 R3) |
| M-3 | accuracy matrix (§6) | 100 % intended control clicked; 0 clicks outside the target window |
| M-4 | gate flip → grabs committed | ≤ 2.5 s open, ≤ 0.5 s close; exactly 1 `SetKeyGrabs` per flip; 0 Grab/Ungrab; 0 reconnects; 0 flips caused by `Unresponsive` |
| M-9 | stall burst (C-9 row 1) | 0 clicks injected after their `not-after` |
| M-5 | Num Lock on, or gate closed | 0 swallowed `/` `*` in 1000 presses |
| M-6 | held button lifetime | gorca release at 120 s ± 1 s; helper backstop 125 s; release ≤ 1 s after caller vanish |
| M-7 | helper handler main-loop time (`GORCA_TRACE`) | p99 ≤ 1 ms per pointer call |
| M-8 | Reader K1 (key → speech) with the helper stalled 5 s and 4 pending clicks | unchanged within noise |

## 11. Implementation slices (≤ 2 days each)

| Slice | Content | Owner | Depends |
|---|---|---|---|
| MO-0 | Spikes in the private rig: S-1 GTK3/GTK4 popup coordinates, S-2 Electron, S-3 Qt extents, S-4 scales 1.25 and 2 incl. Chromium, S-5 left-handed swap of a virtual device, S-6 pick-based occlusion cost | linux-platform-engineer | - |
| MO-1a | Helper v2 part 1: caps (AL-1 first match), `Capabilities`, H-1 and H-1a buckets + negative cache, `GetPointer`, `PointerMove`, `PointerButton` (with `not-after`, H-7 timer), `PointerRelease`, H-3 single holder, `x-scale` in `GetMonitors`, audit; `test-pointer.sh` part 1; platform doc | linux-platform-engineer | - |
| MO-1b | Helper v2 part 2: `PointerClickAt` with expiry, popup family, window guard, stage guard, pick occlusion; C-9 rows 1-4, 6, 7 | linux-platform-engineer | MO-1a, MO-0 S-6 |
| MO-2 | `gorca-output-api` pointer types + `PointerSink`; `gorca-shell-helper` client: owner check, `PointerAvail`, probe/poll, deadlines, queue; fake helper for tests | rust-engineer | - |
| MO-3 | Core gating: `RuntimeGate` in `CmdSpec`, gated grab split, `Mode::grab_set(keys, gates)`, RG-5 hysteresis, RG-6/RG-7; SetKeyGrabs-count tests | rust-engineer | MO-2 |
| MO-4 | Commands click at pointer (L/R/M), locks + `HoldState` + 120 s, `say_position`; NVDA preset rows; strings en/ru; preset conflict test | rust-engineer | MO-3 |
| MO-5a | `PointerResolver` + window matcher (table tests from study §3, global `x-scale`), `route_to_review` | rust-engineer | MO-1b, MO-4 |
| MO-5b | `ActionReq::Click/ContextMenu`, item clicks (action first, `ScrollTo` once, pointer path), `mouse.item_click`/`if_covered`; Orca preset rows | rust-engineer | MO-5a |
| MO-6 | `review_to_pointer` (inverse mapping, `GetAccessibleAtPoint` descent) | rust-engineer | MO-5a |
| MO-7 | End-to-end matrix, C-9 rows 5, 8, 9, gates M-2..M-9 in the private rig | test-engineer + perf-engineer | MO-5b, MO-6 |

MO-1a and MO-2 run in parallel; MO-4 (NVDA click at pointer) is useful on its own once MO-1a is hot-reloaded.

## 12. Open questions

- **QM-1** Orca laptop left click chord: `gorca+shift+7` (recommended) or `gorca+0`. **Pending the owner**; MO-5b
  binds whichever is confirmed.
- **QM-2** Item clicks default `action_first` (Orca parity); should the NVDA presets' `gorca+kp_enter`
  (`review.activate`) and Orca item click stay distinct when both exist? (Recommended: yes, activate = default action
  only, item click = click semantics.)
- **QM-3** Spikes S-1..S-6 may add quirk rows or move a toolkit to "action only" until fixed upstream.

## 13. Review r1 mapping (`docs/reports/adr-0025-review.md`)

| Item | Where |
|---|---|
| C-1 stale clicks after a stall | §2.2 `not-after` + `expired`, H-7, EX-2, EX-3 overdue stop, MI-5, failure table, M-9, C-9 row 1 |
| C-2 transients | §2.2 popup family, guard and occlusion, MI-5, threat table, C-9 row 2 |
| C-3 Unresponsive closes the gate | §2.5 gate formula and table, M-4, failure table, C-9 row 8 |
| C-4 stage targets | §2.2 stage guard, MI-5, failure table, C-9 row 4 |
| C-5 global XWayland scale | §2.2 `x-scale`, §2.4 rule, table row, WM-2, C-9 row 5 |
| C-6 denied callers | H-1a, threat table, C-9 row 6, MO-1a |
| C-7 a-f | MI-4 (a), MI-5 (b), `Holds` per button + timer message (c), H-3 single holder (d), H-7 timer (e), AL-1 first match (f) |
| C-8 slices | MO-1a/1b, MO-5a/5b |
| C-9 tests | §6 "Review r1 rows" (nine rows) |
| Study correction | `docs/reports/mouse-emulation-design.md` §3 (popups have Meta windows) |
| Note: QueryPointer in mutter main | §8 revisit trigger (c) |
