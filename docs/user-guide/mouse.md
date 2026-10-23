# Mouse emulation

gorca can move the mouse pointer and click for you, using the same keys as NVDA. On Wayland an
application cannot move the pointer by itself, so gorca asks its GNOME Shell helper extension
(`gorca-helper`) to do it. The design is in [ADR-0025](../adr/0025-mouse-emulation.md).

## Requirements

- GNOME Shell 48 on Wayland.
- The `gorca-helper` extension installed and enabled (version 2 of its interface).
- gorca allowed to use the `pointer` capability in the helper allow-list.

## Enable it

1. Install and enable the helper extension from the repository. The steps are in
   `contrib/gnome-shell-extension/README.md`. For a production install, run
   `GORCA_HELPER_DEV=0 contrib/gnome-shell-extension/dev.sh install`, then
   `contrib/gnome-shell-extension/dev.sh enable`.
2. Create the allow-list. gorca never writes it for you, because it gives a program control of
   your pointer. Use `/etc/gorca/shell-helper.json` (owned by root, preferred) or
   `~/.config/gorca/shell-helper.json` (not writable by group or others).
3. Add a rule for gorca with both capabilities. `exe` and the full cgroup path are required:

   ```json
   {"allowed": [
     {"exe": "/usr/bin/gorca",
      "cgroup": "/user.slice/user-1000.slice/user@1000.service/app.slice/gorca.service",
      "caps": ["capture", "pointer"]}
   ]}
   ```

   Without `caps`, only `capture` is granted and mouse emulation stays off. The first matching
   rule decides; later rules are never merged. Details are in
   `docs/platform/org.gorca.ShellHelper.md`.
4. Make sure `[mouse] enabled` is `true` in `gorca.toml` (the default).

When the helper or the capability is missing, a mouse command says that the mouse is not
available and names the allow-list file and the capability. gorca checks the allow-list again
every few seconds, so no restart is needed after you edit it. To turn the feature off, set
`[mouse] enabled = false` or start gorca with `--no-pointer`.

## Keys

All four presets (NVDA and Orca, desktop and laptop) use NVDA's mouse keys. On the desktop
presets the keypad keys work only while Num Lock is off; with Num Lock on, the keypad types `/`
and `*` as usual. gorca grabs those keys only while the helper is usable, so they type normally
when mouse emulation is unavailable. The laptop presets use gorca-modifier chords only.

| Action | Desktop presets | Laptop presets |
|---|---|---|
| Left click at the pointer | Keypad Divide | gorca+[ |
| Right click at the pointer | Keypad Multiply | gorca+] |
| Lock or unlock the left button (drag) | Shift+Keypad Divide | gorca+Ctrl+[ |
| Lock or unlock the right button (drag) | Shift+Keypad Multiply | gorca+Ctrl+] |
| Move the pointer to the review position or navigator object | gorca+Keypad Divide | gorca+Shift+M |
| Move the review cursor to the object under the pointer | gorca+Keypad Multiply | gorca+Shift+N |
| Say the pointer position (press twice for the screen position and monitor) | gorca+Shift+Keypad Divide | gorca+Ctrl+Shift+M |

The commands behind these keys are `mouse.left_click`, `mouse.right_click`,
`mouse.left_lock_toggle`, `mouse.right_lock_toggle`, `mouse.route_to_review`,
`mouse.review_to_pointer` and `mouse.say_position`.

### Item clicks (unbound by default)

Two more commands work like Orca's "click on item". They have no key; bind them in a
`[keymap.global]` table in your `gorca.toml` (see the
[configuration reference](../config-reference.md)).

| Command | What it does |
|---|---|
| `mouse.left_click_item` | Activates the review item through the application's own action first (`click`, `press`, `jump`, `open`, `activate`). If there is none, or the application refuses, it clicks the item's location with the pointer. |
| `mouse.right_click_item` | Same, using the context menu action (`show-context-menu`, `menu`). |

Item clicks do not need the pointer service when the application accepts the action. These
settings control them:

| Setting | Values | Meaning |
|---|---|---|
| `[mouse] item_click` | `action_first`, `pointer` | Try the application's action first, or always click with the pointer. |
| `[mouse] if_covered` | `refuse`, `click` | What to do when something covers the item. |
| `[mouse] announce_move` | true, false | Speak "Mouse moved" after moving the pointer to the review position. |

Spoken results include "Left click", "Right click", "Covered", "No location" and "Window not
visible".

## Drag lock

The lock commands press a button and keep it held, so you can move the pointer and release
later.

- The first press says "Left button locked" (or right). The second press says "unlocked".
- A click of the same button also releases it.
- Each button has its own lock; both can be held together.
- A lock releases itself after 120 seconds, and gorca says so. This time is fixed.
- If you change windows while a button is held, gorca reminds you that it is locked. It does
  not release it, because dragging between windows is legitimate.
- Held buttons are released when gorca exits, when the helper goes away, when the `pointer`
  capability is removed from the allow-list, and when `mouse.enabled` becomes false. As a
  backstop, the helper releases on its own after 125 seconds.

## Safe clicks on any display scale

Item clicks land on the right widget at monitor scales 1, 1.25 and 2 in GTK 4, GTK 3 (also under
XWayland), Firefox, Chromium and Electron. Before clicking, gorca checks that the item is
showing, clips it to its scrolling parents and asks the application whether the item is really
at that point. The helper also checks that the target window is not covered. When a check
fails, gorca says "No location", "Covered" or "Window not visible" instead of clicking somewhere
else.

## Safety notes

- Mouse emulation is opt-in. Only the owner of the allow-list file can grant it.
- Every pointer call has a 150 ms deadline and an expiry time, so a call that the Shell runs
  late injects nothing. If the helper stops answering, gorca says "Mouse not responding" and
  drops queued clicks instead of replaying them later.
- The helper refuses pointer input while the session is locked.
- Two quick presses are two clicks, which applications read as a double click. Holding a key
  does not repeat the click.
- Pointer calls never run on the key-to-speech path, so a slow Shell does not delay speech.

## Known issues

- Firefox sometimes drops the first click after start-up. The measured first-try success for an
  item click on a page button was 9 of 12 runs, and a second click always worked. gorca does not
  retry on its own, because a repeated click could toggle a checkbox twice or submit a form
  twice.
- In Chromium and Electron the application's hit test is skipped, because its answers do not
  match the item. Item clicks there rely on the position check and the helper's occlusion check.
