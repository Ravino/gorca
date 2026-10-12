# org.gorca.ShellHelper (GNOME Shell extension `gorca-helper@gorca`)

Session bus, name `org.gorca.ShellHelper`, path `/org/gorca/ShellHelper`, interface `org.gorca.ShellHelper`
(version 1). Source: `contrib/gnome-shell-extension/gorca-helper@gorca/`. Verified on GNOME Shell 48.7 (private
headless shell, see `docs/reports/2026-10-12-screen-capture-options.md`).

| Method | In | Out |
|---|---|---|
| `Ping` | - | `u` version |
| `GetWindows` | - | `aa{sv}`: `id` t (Meta window id, same as ScreenCast `window-id`), `stable-seq` u, `pid` u, `wm-class` s, `app-id` s, `title` s, `type` u, `client-type` s, `frame-{x,y,w,h}` i, `buffer-{x,y,w,h}` i, `monitor` i, `focused` b, `minimized` b, `visible` b |
| `GetMonitors` | - | `aa{sv}`: `index` i, `x y w h` i, `scale` d, `primary` b |
| `CaptureArea` | `iiii` x y w h (stage logical px), `a{sv}` options (reserved) | `(uuusd)` (pixel w, pixel h, stride=0, format `"PNG"`, scale), `ay` PNG bytes |
| `CaptureWindowArea` | `t` window id, `iiii` rect relative to the window frame (`origin`="buffer" in options: relative to the buffer rect incl. shadows), `a{sv}` | same as above, plus `a{sv}` meta: `stage-x`, `stage-y`, `occluded` b (a window stacked above overlaps the rect) |

Errors: `org.gorca.ShellHelper.Error.AccessDenied` (caller not allow-listed), `org.gorca.ShellHelper.Error.Failed`
(bad rect, locked session, rate limit, no such window).

Limits: area <= 2048x2048 logical px, 100 calls/s (burst 200), refused while the session is locked, the cursor is never
painted. Pixels never touch disk (MemoryOutputStream, then the D-Bus message).

Access control: config `/etc/gorca/shell-helper.json` (root-owned, authoritative if present) else
`~/.config/gorca/shell-helper.json` (owned by the user, not group/world-writable):
`{"allowed":[{"exe":"/path/to/gorca","cgroup":"/user.slice/user-1000.slice/user@1000.service/app.slice/gorca.service"}]}`.
Both `exe` and the FULL cgroup path (v2 `0::` line) are required per rule; optional `label` (LinuxSecurityLabel).
Identity is bound to the bus connection: the broker's `GetConnectionCredentials` must return `ProcessFD` (a pidfd; no pidfd
means deny, there is no pid fallback). The pid is read from `/proc/self/fdinfo/<pidfd>`, the pidfd is kept open while the
sender lives (pid cannot be recycled), start time (`/proc/<pid>/stat` field 22) and label are recorded at first contact and
must not change, and exe and cgroup are re-read on every call and re-validated against the pidfd afterwards. Cache entries
(and pidfds) are dropped on `NameOwnerChanged`. Empty or missing config denies everyone. Test: `contrib/gnome-shell-extension/test-access.sh`.

Client rule: before trusting replies, check that the owner of `org.gorca.ShellHelper` is the owner of `org.gnome.Shell`
(name squatting by another process would otherwise let it feed fake pixels).

Why PNG and not raw: GJS copies `(array)` in-parameters, so `Clutter.Stage.paint_to_buffer` / `Cogl.Texture.get_data`
cannot fill a JS buffer (both "succeed" and leave it zeroed). `Shell.Screenshot.screenshot_area` is the safe path.
