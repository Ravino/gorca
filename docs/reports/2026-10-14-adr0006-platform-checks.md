# ADR-0006 platform checks V-1 .. V-5 (2026-10-14, linux-platform-engineer)

Rig: private headless gnome-shell 48 (own dbus-daemon, own runtime dir, own a11y bus, `GSETTINGS_BACKEND=keyfile`), started with
`contrib/gnome-shell-extension/headless-shell.sh`; torn down by exact PID/own process group. Debian 13: GTK 3.24.49, GTK 4.18.6,
libadwaita (Adw-1 typelib), gnome-terminal (VTE GTK3). Input via Mutter RemoteDesktop `NotifyKeyboardKeycode` (evdev keycodes);
grabs via `org.freedesktop.a11y.Manager` `SetKeyGrabs(au, a(uu))` while owning `org.gnome.Orca.KeyboardMonitor` on the private bus only.
Probes: `tools/platform-probes/adr6/`. Nothing touched the live session.

## Verdicts

| # | Verdict | One line |
|---|---|---|
| V-1 | **REFUTED as written; fixed rule below** | Shift does NOT move keypad keys to the KP_<digit> level. Mutter matches the exact event keysym AND the exact full modifier state. |
| V-2 | **PARTLY CONFIRMED, F-10 refuted** | WINDOW is consistent with truth on GTK3/GTK4/Adw; GTK4 popover WINDOW is relative to the parent window, not its own surface; Adw dialogs are in-window. |
| V-3 | **CONFIRMED** | GTK4 and GTK3 `GetOffsetAtPoint(top-left, WINDOW)` returned the true first visible offset in 6/6 scroll positions, ~1 ms. Editables (GTK4) NotSupported. |
| V-4 | **TTL cannot be relaxed** | No toolkit reports scrolling as bounds change. Resize/relayout is reported (GTK3, GTK4) but as a storm of ~138 events. St never emits. |
| V-5 | **CHEAP** | VTE Text exposes only the visible viewport (about 1 650 chars, 24 rows) even with 10 000 lines scrollback; every call is 0.1-2 ms. |

## V-1 Shift+KP_* (files: `v1.py`, `v1b.py`)

Delivered `KeyEvent` (keysym:state) for every keypad key, from `v1b.py`:

| NumLock | plain KP7 | Shift+KP7 |
|---|---|---|
| off | `KP_Home` (0xff95):0 | `KP_Home`:1 (Shift) |
| on  | `KP_7` (0xffb7):16 (Mod2) | `KP_Home`:17 (Shift\|Mod2) |

The same holds for all 16 keypad keys (KP_Up, KP_Prior, KP_Begin, ... and KP_Enter/+/-/*// unchanged under Shift).
So with NumLock off, Shift changes nothing in the keysym; the chord is `(KP_Home, Shift)`. The ADR text "Shift+KP_Home produces the shifted-level keysym KP_7" is wrong for this keymap; with NumLock on, Shift gives the *navigation* keysym.

Grab matrix (swallow detected by a focused GTK4 client; `v1.out`):
- NumLock off: `(KP_Home, 0)` swallows plain KP7; `(KP_Home, Shift=1)` swallows Shift+KP7; `(KP_7, *)` never matches anything.
- NumLock on: `(KP_7, Mod2=16)` swallows plain KP7; `(KP_Home, Shift|Mod2=17)` swallows Shift+KP7. Masks `0`, `Shift`, `Mod2` alone do NOT match (state equality is exact, `refs/mutter/src/backends/meta-a11y-manager.c:563`).
- Caps Lock (mask 2) breaks mask-0 grabs too (known, M1 memory).

Rule for the grab compiler (K-8a): resolve each chord to the keysym the keymap produces for that keycode at the **unshifted level of the current NumLock state**, and register the Cartesian product of lock masks {0, Mod2(16), Lock(2), Mod2|Lock} x the chord's modifiers. For KP keys register both keysyms (nav and digit) per NumLock state, or re-register on NumLock change. This also makes "no NumLock variants" (K-6a) achievable only as a deliberate omission of the Mod2 masks. Events carry no layout group; the keysym is already resolved.

## V-2 GetExtents (files: `v2.py`, `v2pop.py`, `v2shell.py`)

Truth came from the apps themselves (GTK3 `translate_coordinates`, GTK4 `compute_bounds`).

- **GTK3 on Wayland (CSD HeaderBar):** SCREEN == WINDOW == PARENT for every node, all relative to the whole GdkWindow *including* the CSD shadow (frame 572x519 for a 520x420 client; headerbar panel at (26,23)). They match the app's `translate_coordinates` truth exactly (label (38,82,496,38) both). PARENT is NOT parent-relative on GTK3. Invisible nodes return `(-2147483648,-2147483648,1,1)`: the reader must treat INT_MIN origin as missing.
- **GTK4:** WINDOW matches `compute_bounds(window)` exactly (label (12,59,496,36), button (12,101,...)). The origin is the GtkWindow widget, not the wl_surface: the surface is 548x449 with `surface_transform` (14,12) (shadow), so WINDOW coords are offset by (14,12) from surface pixels. SCREEN returns origin (0,0) with valid size (F-1 confirmed); PARENT is genuinely parent-relative.
- **Popovers (F-10 refuted for 4.18):** a `GtkPopover` label has WINDOW (194,452,143,18), i.e. relative to the parent window (it sits below the button at y=374+34), while its own-surface position would be (32,43). PARENT=(20,20). The popover is in the a11y tree under its MenuButton. Reader needs no per-surface correction.
- **libadwaita 1.x `AdwAlertDialog`:** NOT a separate surface or toplevel. The app still has one toplevel; the dialog is an `alert dialog` child of the same window with WINDOW coords in the window frame (content box (74,115,372,191)). The main content stays in the tree (not hidden), so flat review must use the dialog subtree as the surface root when a modal `alert dialog` exists. (State flags of the underlying content were not examined.)
- **Shell/St (F-5 confirmed):** SCREEN == WINDOW == PARENT == stage coordinates (panel (0,0,1280,32), left/centre/right boxes at x=0/554/1172). The `Main stage` window node itself reports a bogus rect (0,54,87,55). Note: the shell was only reachable as `:1.0` directly; registry `GetChildren` was empty because registryd started after the shell (private-rig artefact).

## V-3 GetOffsetAtPoint (file: `v34.py`)

`GetOffsetAtPoint(x0+1, y0+1, WINDOW)` on the scrolled `GtkTextView`/GTK3 `GtkTextView` vs. the app's first visible offset:
GTK4: scroll 0/23/100/311/600/1500 px -> 0/57/285/977/1905/4051 (truth identical, 6/6). GTK3 -> 0/57/285/919/1789/3877 (truth identical, 6/6). Latency p50 ~1 ms. Bottom-left point returns the last visible line start. SCREEN is `NotSupported` on GTK4; wrapped `GtkLabel` returns correct line offsets (0, 70). GTK4 editables (`GtkEntry`): `GetOffsetAtPoint` and `GetRangeExtents` NotSupported (F-3 confirmed). GTK3 entry: `GetRangeExtents` works; `GetOffsetAtPoint` returned -1 at an inside point.
Recommendation: the `VisibleLines` start offset via `GetOffsetAtPoint` is sound; keep the line-iteration fallback only for editables and `-1`/error.

## V-4 bounds-changed (file: `v34.py`, `v4shell.py`; listener registered via `RegisterEvent object:bounds-changed`)

| action | GTK4 | GTK3 | St (Overview open/close, typing) |
|---|---|---|---|
| scroll TextView / ListBox | 0 BoundsChanged | 4 per scroll, only scrollbar/scrolled-window/ancestors, same values, nothing for content | 0 |
| button grows 34 -> 120 px | 4 (button, its ancestors up to window) | 6 (button, 3 following siblings, ancestors) | n/a |
| window resize 520x420 -> 640x500 | 138 | 137 | n/a |
| Overview toggle (hundreds of actor moves) | n/a | n/a | 0 BoundsChanged (92-96 StateChanged, 21-23 ChildrenChanged) |

Payload is the new rect in PARENT coordinates (e.g. `(0,42,496,120)`). Conclusions: scrolled content moves with NO event in GTK3 or GTK4, and St never emits, so RR-1's 2 s TTL stays. BoundsChanged can still be used to invalidate on resize/relayout (GTK3/4 only), but a single resize costs ~140 events, so coalesce per app. Events only flow if a listener for the class is registered.

## V-5 VTE (files: `v5.py`, `v5b.py`)

gnome-terminal with 14 000 lines output; scrollback depth verified: scrolling to the top showed line 4076 (about 9 900 lines above). `Text` is a **viewport snapshot**: `CharacterCount` 1 632-1 856 (24 rows x ~68 chars), offsets are relative to the visible screen; Shift+PageUp changes the whole text (first line 13979 -> 13499 -> 13019 ...) and the caret offset. Scrollback is not addressable at all.
Timings with the scrollback at its deepest (3 runs, ms; host noise visible, report ranges): `GetText(0,-1)` p50 0.15-0.86; `GetOffsetAtPoint` p50 0.18-0.60 (p99 <= 1.0); `GetRangeExtents` visible range p50 0.6-1.1, whole text 0.9-1.3, one line 0.2-0.7; `GetCharacterExtents` 0.2-0.3; `GetBoundedRanges` (whole widget, 24 ranges) 0.6-1.9; 24 `GetOffsetAtPoint` calls total 5.9-18 ms. Extents SCREEN == WINDOW.
Consequence for M6/`review_terminal_generic`: no cap on scrollback is needed; cost is bounded by one screen. Review of scrollback needs the terminal to scroll (Shift+PageUp) and re-snapshot, not Text offsets.

## Open / unverified
- Tiled/maximized GTK4 windows (no shadow) not measured; the (14,12) offset above is for a floating CSD window.
- Real hardware keyboards with other layouts: the NumLock-level mapping is from this host's XKB (us). K-8a should read the live keymap, not assume.
- St NOT exercising app-window geometry (Shell exposes only its own stage).
- libadwaita sheet/bottom-sheet dialogs and `GtkPopoverMenu` not tested separately from the `AdwAlertDialog`/`GtkPopover` cases.
