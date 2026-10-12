# Screen-pixel acquisition for gorca: options (2026-10-12, linux-platform-engineer)

Host: GNOME Shell 48.7 (Debian `48.7-0+deb13u2`), Wayland, Yama `ptrace_scope=3`. Answers ADR-0002 section 10 and O-2.

## 1. gsettings / dconf: no
Enumerated all live schemas (`gsettings list-keys` over `list-schemas`) and `refs/gnome-shell/data/org.gnome.shell.gschema.xml.in`,
`refs/mutter/data/org.gnome.mutter.gschema.xml.in`: nothing controls the Screenshot allowlist, the screen-sharing indicator or
unsafe mode. Only related keys: `org.gnome.desktop.lockdown disable-save-to-disk` (restricts, never relaxes),
`org.gnome.shell development-tools` (enables Looking Glass, see 2), `disable-user-extensions` (currently **true** here).
The allowlist is hardcoded (`screenshot.js:2848-2851`), the indicator is driven by ScreenCast session state (`remoteAccess.js`).

## 2. Unsafe mode
`global.context.unsafe_mode` (MetaContext property). Enabled by the hidden `--unsafe-mode` shell option (restart) or at runtime
from Looking Glass (Alt+F2 `lg`, needs `development-tools`; `lookingGlass.js:1240-1252`); the shell posts a notification with Undo
(`main.js:289-303`). Effects (verified in a private shell): `org.gnome.Shell.Eval` runs arbitrary JS in the shell
(`shellDBus.js:72`; returns `(false,'')` otherwise), `DBusSenderChecker` accepts everyone (`util.js:400`: Screenshot passes), and
Mutter's `MetaDBusAccessChecker` accepts everyone (`meta-dbus-access-checker.c:197`, which also covers the a11y KeyboardMonitor
ACL). Cost: any same-UID process gets code execution inside the compositor (all pixels, all keys). Not acceptable as a default.
Dev-only trick (untested): toggle in LG, Eval a dynamic `import()` of the helper, toggle off, no logout needed.
Even with the allowlist open, `ScreenshotArea` returns pixels only via a file or the clipboard, never in memory.

## 3. Extension `gorca-helper@gorca` (prototype, works)
Code: `contrib/gnome-shell-extension/`; interface: `docs/platform/org.gorca.ShellHelper.md`.
- `CaptureArea`, `CaptureWindowArea` (PNG in the reply, no disk, no portal, no ScreenCast), `GetWindows`, `GetMonitors`.
- Verified in a private headless GNOME Shell 48.7 (own session bus, real GPU via gbm render nodes, virtual 1280x800 monitor):
  GetWindows returns the Meta window id (uint64, the same `window-id` ScreenCast takes), pid, app-id, title, frame and buffer
  rects; a GTK4 window's red 64x64 box was found at the expected place via `CaptureWindowArea` (rect relative to frame).
  Screen-sharing, screen-recording and privacy indicators stayed `visible=false` after 30+ captures (Eval walk of the stage in an
  unsafe-mode shell). Stock `org.gnome.Shell.Screenshot` and `Introspect.GetWindows` are `AccessDenied` for the same client.
- **Raw pixels from JS are impossible**: GJS copies `(array)` in-params, so `Stage.paint_to_buffer` and `Cogl.Texture.get_data`
  return success and leave the buffer zeroed (confirmed: all-zero vs the PNG). `Shell.Screenshot.screenshot_area` (synchronous
  paint/readback, PNG encode on a worker thread) is the only safe path. gorca decodes the PNG (Rust `png`, ~1 ms at 256x256).
- **Latency** (client-observed round trip, gjs client, 1000 calls, 20 ms apart; shell main-loop floor for a stock JS property
  Get is 0.78 ms p50):

| crop | p50 | p90 | p99 | shell-side sync paint+readback p50 / p99 | PNG encode (thread) p50 / p99 |
|---|---|---|---|---|---|
| 32x32 | 3.4 ms | 5.6 | 7.7 | 1.31 / 3.09 ms | 0.34 / 0.84 ms |
| 64x64 | 3.4 ms | 4.6 | 6.7 | 1.22 / 2.60 ms | 0.44 / 1.13 ms |
| 128x128 | 4.2 ms | 6.5 | 9.3 | 1.24 / 2.70 ms | 0.97 / 4.07 ms |
| 256x256 | 7.0 ms | 10.7 | 17.2 | 1.38 / 2.70 ms | 3.12 / 13.1 ms |

  Max tails 12-50 ms (first call, GC). The synchronous paint runs on the compositor thread: about 1.3 ms stall per grab
  (p99 2.7 ms, max 6 ms). Fine for node crops, but do not grab at frame rate. Caveats: measured in a headless private shell with
  one idle window, not on the user's loaded session; the dGPU/iGPU split may differ; session-bus round trip excluded
  nothing (it is all included). Compare with option A: ScreenCast >= 5 s indicator per grab.
- Window mapping (O-2): match AT-SPI application pid + toplevel name against `GetWindows` (`pid`, `title`, `app-id`); `id` feeds
  ScreenCast too. `occluded` flags a foreign window above the rect. Not yet verified against a live AT-SPI tree (hs had no a11y
  registry).
- **Cannot be loaded without a logout.** GNOME Shell scans extensions only at startup (`extensionSystem.js` `_loadExtensions`;
  `gnome-extensions info` says "doesn't exist" right after copying, tested). Per instructions nothing was installed in the real
  session and nothing restarted; the file was copied and removed. `disable-user-extensions` is also true and must be set false.
  Install steps: `contrib/gnome-shell-extension/README.md`; remove: `dev.sh uninstall [--purge]`.

### Security review and threat model
What it enforces (after the review fix): identity bound to the bus connection by pidfd (no pid fallback), exe path AND the full
cgroup path of the unit, start-time pinning, LinuxSecurityLabel pinning, per-call re-read, config must be user/root-owned and not
group/world-writable (root-owned `/etc/gorca/shell-helper.json` wins if present), rate limit 100/s, 2048x2048 cap, refused while
locked, no cursor, no disk. The pid cannot be reused (the pidfd is held), `exec` after first contact changes exe (re-read each call),
a new connection is a new unique name and is re-checked.
Verified (private shell, `contrib/gnome-shell-extension/test-access.sh` steps run by hand):
- T1 allowed: `/usr/bin/gjs-console` inside transient unit `gorca-shtest.service`: `Ping -> (1,)`, captures OK.
- T2 same exe from the terminal scope: `GDBus.Error:org.gorca.ShellHelper.Error.AccessDenied: caller /usr/bin/gjs-console in /user.slice/.../app-org.gnome.Terminal.slice/vte-spawn-....scope is not allow-listed`.
- T3 copied exe inside the right unit: `...AccessDenied: caller .../gjs-copy in /user.slice/user-1000.slice/user@1000.service/app.slice/gorca-shtest.service is not allow-listed`.
- Production cgroup is `.../app.slice/gorca.service` (same shape; the real `gorca.service` was not started).
Does NOT protect against: (a) any same-UID attacker who can write the user config, the unit file, the gorca binary
(`<repo>/target/release/gorca` is user-writable) or the extension files: they can authorize themselves. Mitigation: root-owned
`/etc/gorca/shell-helper.json` and a root-owned install path for gorca; (b) code injection into gorca: `ptrace_scope=3` here (no
attach at all) closes ptrace and /proc/pid/mem, so this boundary does matter on this host, but LD_PRELOAD/env of a unit the
attacker can edit and restart is the same class as (a); (c) a name squatter on `org.gorca.ShellHelper` before the shell: clients must check
that the owner is gnome-shell's unique name; (d) the extension itself is in-compositor code: a bug there is a shell crash, mitigated
only by small size and per-call deadlines on the client.
Before the fix the check read `/proc/<pid>/exe` from a cached pid without pidfd (review finding, MEDIUM): fixed as above.
Not unit-tested: forced pid reuse and exec-in-place on an authorized connection (argued from pidfd semantics only).

## 4. Patching gnome-shell / mutter (assessed, not built)
Minimal patch, `js/ui/screenshot.js` ScreenshotService: add a new method (the existing ones can only return via file or clipboard) such
as `ScreenshotAreaToFd` plus an ACL of "owner of `org.gnome.Orca.KeyboardMonitor`" (mirrors Mutter's a11y ACL, `meta-dbus-access-checker.c`)
or a gorca well-known name in the `DBusSenderChecker` list (~25 lines). The JS is compiled into the gnome-shell gresource, so it
needs a full `dpkg-buildpackage` of gnome-shell (about 10-15 min plus build-deps), a local apt pin/hold, and a rebuild for every
Debian security update (`deb13uN`); a missed rebuild leaves the user on an unpatched or stale shell. Window ids and geometry need a
second patch (Introspect is also allowlisted). Costs more to maintain than the extension for the same capability.
Upstream draft (for GNOME Shell/Mutter issue tracker, to be filed by the user):
> **Screen readers cannot read pixels or key events without a portal prompt or an indicator.** Orca-class assistive technology needs
> small, silent, in-memory crops of the screen (unlabelled icons, canvas content) and the same window-id/geometry data the compositor
> already has. Today `org.gnome.Shell.Screenshot` is restricted to two bus names, ScreenCast shows a sharing indicator for >= 5 s per
> grab, the portal screenshot writes to disk, and `org.freedesktop.a11y.KeyboardMonitor` accepts only the owner of
> `org.gnome.Orca.KeyboardMonitor`. Proposal: an `org.freedesktop.a11y.Manager` method `CaptureArea(x,y,w,h) -> (fd, format, size)`
> (and window-relative variant), allowed for the registered screen reader when `org.gnome.desktop.a11y.applications screen-reader-enabled`
> is true, with a generic registration (e.g. a `RegisterScreenReader` handshake) replacing the Orca-specific name check, the cursor
> excluded, not available on the lock screen, rate limited. Prior art: the KeyboardMonitor ACL.
Upstream chance: low-medium, long lead (a GNOME design discussion; privacy reviewers will object to a silent capture primitive).

## Comparison and recommendation
| Option | Indicator | Prompt | Needs | Latency | Maintenance | Security |
|---|---|---|---|---|---|---|
| gsettings | n/a | n/a | impossible | | | |
| Unsafe mode | none | none | LG toggle / restart | ~4 ms | none | unacceptable (Eval for all) |
| Extension (this) | none | none | one logout, user installs | 3.4-7 ms p50 | small JS, per shell major | pidfd+exe+cgroup; weak vs same-UID writers unless root-owned config |
| Patch shell | none | none | rebuild pkg per update | similar | high | allowlist by name |
| ScreenCast (A) | >= 5 s per grab | none | nothing | unmeasured | low | honest |

**Recommendation (medium-high): extension**, with PNG crops, root-owned `/etc/gorca/shell-helper.json`, and ScreenCast kept as the
no-install fallback (honest indicator). File the upstream proposal in parallel. The same in-shell position could also broker key
events (bypassing the Orca-only KeyboardMonitor ACL): unverified, worth an experiment.
Open: live-session latency, real AT-SPI to window-id mapping, Shell 49/50 API drift (`shell-version` is pinned to 48), `GetWindows`
excludes nothing but DESKTOP windows (may leak titles to an allowed caller only).
