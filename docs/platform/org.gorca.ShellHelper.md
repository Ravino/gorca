# org.gorca.ShellHelper (GNOME Shell extension `gorca-helper@gorca`)

Session bus, name `org.gorca.ShellHelper`, path `/org/gorca/ShellHelper`, interface `org.gorca.ShellHelper`
(version 2; `Ping` returns 2; v1 methods unchanged). Source: `contrib/gnome-shell-extension/gorca-helper@gorca/`. Verified on GNOME Shell 48.7 (private
headless shell, see `docs/reports/2026-10-12-screen-capture-options.md`).

| Method | In | Out |
|---|---|---|
| `Ping` | - | `u` version |
| `Capabilities` | - | `as` caps granted to this caller (`capture`, `pointer`) |
| `GetWindows` | - | `aa{sv}`: `id` t (Meta window id, same as ScreenCast `window-id`), `stable-seq` u, `pid` u, `wm-class` s, `app-id` s, `title` s, `type` u, `client-type` s, `frame-{x,y,w,h}` i, `buffer-{x,y,w,h}` i, `transient-for` t (parent window id, 0 = none; v2), `monitor` i, `focused` b, `minimized` b, `visible` b |
| `GetMonitors` | - | `aa{sv}`: `index` i, `x y w h` i, `scale` d, `primary` b, `x-scale` d (v2: XWayland's global effective scale, as `meta_xwayland_get_effective_scale`: 1 in physical layout mode, else round(`xwayland-scaling-factor`) if the key exists and is set, else ceil(highest monitor scale)) |
| `CaptureArea` | `iiii` x y w h (stage logical px), `a{sv}` options (reserved) | `(uuusd)` (pixel w, pixel h, stride=0, format `"PNG"`, scale), `ay` PNG bytes |
| `CaptureWindowArea` | `t` window id, `iiii` rect relative to the window frame (`origin`="buffer" in options: relative to the buffer rect incl. shadows), `a{sv}` | same as above, plus `a{sv}` meta: `stage-x`, `stage-y`, `occluded` b (a window stacked above overlaps the rect) |
| `GetPointer` | - | (pointer bucket) `a{sv}`: `x` `y` d (stage), `window` t (topmost window at the pointer by pick, 0 = Shell UI), `wx` `wy` d (relative to that window's frame), `monitor` i |
| `PointerMove` | `t` window, `s` origin (`frame`\|`buffer`\|`stage`), `d` x, `d` y, `a{sv}` opts | `a{sv}` reply |
| `PointerClickAt` | `t` window, `s` origin, `d` x, `d` y, `u` button (1 left, 2 middle, 3 right), `a{sv}` opts | `a{sv}` reply |
| `PointerButton` | `u` button, `s` action (`click`\|`press`\|`release`), `a{sv}` opts | `a{sv}` reply |
| `PointerRelease` | - | `u` number of buttons released (this caller's) |

Pointer methods (cap `pointer`, ADR-0025 section 2.2). opts: `not-after` x (REQUIRED on every injecting method:
CLOCK_MONOTONIC microseconds; a handler that runs later injects nothing and replies `outcome = expired`), `if-occluded`
s (`refuse` default \| `proceed`), `count` u (1 or 2, click methods; the second click comes from a 30 ms timer and the
reply follows it), `logical` b (default true: swap buttons 1 and 3 when `org.gnome.desktop.peripherals.mouse left-handed`
is set; Mutter does not swap for a virtual device, spike S-5). Reply: `x` `y` d (stage point), `window-at` t, `occluded` b,
`clamped` b, `outcome` s (`done`\|`expired`\|`refused-occluded`\|`refused-outside`), `held` u (bitmask of buttons this
caller holds: left 1, middle 2, right 4). `window` = Meta window id, 0 = stage space; for `frame`/`buffer` the origin is
resolved in the compositor at call time. Guards: window guard (the point must lie in the frame or buffer rect of the
window or of its popup family: windows of type DROPDOWN_MENU, POPUP_MENU, COMBO, TOOLTIP, MENU reaching it through
`transient-for` through popup-type windows only), pick-based occlusion (`get_actor_at_pos(REACTIVE)`; an actor outside the
family, e.g. another window, an attached dialog, the Overview or a banner, is an occluder; stacking-rect fallback if the
pick API is missing), stage guard for `window = 0` (a point inside any app window is `refused-outside`). One
`POINTER_DEVICE` virtual device is created lazily and dropped in `stop()`. Held buttons: a single holder (unique bus
name); a press or click from another caller while any button is held fails `busy:`; holds are released by `release`,
`PointerRelease`, the holder vanishing, `disable()`, session lock and a 125 s backstop timer; a click of a held button just
releases it. Audit: one journal line per injection at most once per second with a suppressed counter.

Errors: `Error.Failed` messages start with a machine prefix: `locked:` `rate:` `bad-args:` `no-window:` `busy:`. Also
`org.gorca.ShellHelper.Error.AccessDenied` (caller not allow-listed), `org.gorca.ShellHelper.Error.Failed`
(bad rect, locked session, rate limit, no such window).

Limits: area <= 2048x2048 logical px, 100 calls/s (burst 200) for every v1 method; pointer methods including `GetPointer` have their own bucket of 20/s (burst 40, a `count = 2` click costs 2); a caller that is not (yet) allow-listed is limited by its OWN bucket (10/s, burst 20) as the first gate, before any /proc, config or broker work; only a real lookup (unknown sender, or a cached denial past its 5 s TTL) also draws on ONE global lookup bucket (10/s, burst 20); a cached negative decision answers with no lookup; once a sender is allowed it never pays any of this and its cache entry is never evicted (only denied/unknown entries are, oldest first, bound 512); `DENIED` lines are limited globally to 1/s with a suppressed counter; a `release` ignores `not-after` (a late release is always safe); work in flight when the extension is disabled (an authorization lookup) is dropped, no device is created after `disable()`; refused while the session is locked, the cursor is never
painted. Pixels never touch disk (MemoryOutputStream, then the D-Bus message).

Access control: config `/etc/gorca/shell-helper.json` (root-owned, authoritative if present) else
`~/.config/gorca/shell-helper.json` (owned by the user, not group/world-writable):
`{"allowed":[{"exe":"/path/to/gorca","cgroup":"/user.slice/user-1000.slice/user@1000.service/app.slice/gorca.service"}]}`.
Both `exe` and the FULL cgroup path (v2 `0::` line) are required per rule; optional `label` (LinuxSecurityLabel); optional `"caps"` (absent = `["capture"]`; `"pointer"` is opt-in and written only by the owner; unknown strings are ignored with a warning; a rule without `"capture"` denies capture; the FIRST matching rule decides, later rules are never merged). Example: `{"exe":"/usr/bin/gorca","cgroup":"...","caps":["capture","pointer"]}`.
Identity is bound to the bus connection: the broker's `GetConnectionCredentials` must return `ProcessFD` (a pidfd; no pidfd
means deny, there is no pid fallback). The pid is read from `/proc/self/fdinfo/<pidfd>`, the pidfd is kept open while the
sender lives (pid cannot be recycled), start time (`/proc/<pid>/stat` field 22) and label are recorded at first contact and
must not change, and exe and cgroup are re-read on every call and re-validated against the pidfd afterwards. Cache entries
(and pidfds) are dropped on `NameOwnerChanged`. Empty or missing config denies everyone. Tests: `contrib/gnome-shell-extension/test-access.sh` (v1 identity), `contrib/gnome-shell-extension/test-pointer.sh [WORKDIR] [--slow]` (v2: caps, validation, expiry, guards, occlusion, holds, rate, left-handed, stall, hot reload; private shell, caller identities are private copies of the python binary, no service manager involved).

Client rule: before trusting replies, check that the owner of `org.gorca.ShellHelper` is the owner of `org.gnome.Shell`
(name squatting by another process would otherwise let it feed fake pixels).

Why PNG and not raw: GJS copies `(array)` in-parameters, so `Clutter.Stage.paint_to_buffer` / `Cogl.Texture.get_data`
cannot fill a JS buffer (both "succeed" and leave it zeroed). `Shell.Screenshot.screenshot_area` is the safe path.

Client notes (`gorca-shell-helper`, ADR-0025 MO-2): (1) the first call of a NEW bus connection goes through the helper's
pre-auth buckets; right after an extension reload, or during a flood of strangers, the shared lookup bucket can answer
`Error.Failed rate:` to the very first `Ping`. The client probe retries once after 250 ms and otherwise schedules its
normal re-probe. (2) The gorca client shares one bus connection, so the helper never sees "gorca vanished"; on shutdown
the client sends a best-effort `PointerRelease` (150 ms). The helper additionally releases when the holder's unique name
vanishes (`NameOwnerChanged`, H-3) and after the 125 s backstop. (3) Callers' deadlines are clamped to 500 ms; every call
goes to the verified unique name of the Shell's owner of the helper name, never to the well-known name.

Release safety (MO-4 review Q-2): `PointerRelease` needs no cap (any allow-listed caller may let go of what it holds). While buttons are
held the helper re-reads the allow-list once a second; a holder whose rule lost `pointer` (or vanished) has its buttons released at once.
Client side (Q-1, Q-4, Q-5): a press or release whose reply is more than 1 s late is given up (state unknown, the next toggle sends
`ReleaseAll`); a full Reader timer set evicts the farthest deadline, never the nearest; `Core::with_pointer` after the shared state was
cloned logs an error and asserts in debug builds.
