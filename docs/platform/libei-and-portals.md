# Input injection and alternative input paths (Mutter RemoteDesktop, portals, libei, evdev)

All facts below were checked with `busctl --user introspect` on this host (GNOME Shell 48.7,
libei 1.3.901, `libei-dev` installed) unless marked "source".

## What each path can and cannot do for a screen reader

| Path | Observe all keys | Swallow keys | Inject keys | Permission model | Verdict |
|---|---|---|---|---|---|
| `org.freedesktop.a11y.KeyboardMonitor` | yes (Watch) | yes (Grab / SetKeyGrabs) | no | owner of `org.gnome.Orca.KeyboardMonitor` | primary path |
| `org.gnome.Mutter.RemoteDesktop` direct | no | no | yes | **none observed**: `CreateSession` succeeded for an unprivileged busctl | injection fallback |
| Portal `RemoteDesktop` + `ConnectToEIS` | no | no | yes (via libei sender) | portal consent dialog; `restore_token` persistence | injection, sandbox-friendly |
| Portal `InputCapture` / `org.gnome.Mutter.InputCapture` | only while "active" | yes while active | no | portal dialog | not usable: capture is triggered only by pointer barriers; the portal doc says "There is currently no way for an application to activate immediate input capture" |
| Portal `GlobalShortcuts` | named shortcuts only, no raw stream | the bound shortcuts | no | portal dialog, user binds | not a general keyboard monitor |
| evdev (`/dev/input/event*`) | yes (read) | no (EVIOCGRAB would starve the compositor) | via `/dev/uinput` | group `input` (this user is in it; `/dev/uinput` is root-only 0600) | last resort, observe-only, layout/xkb state must be rebuilt by the client |

## org.gnome.Mutter.RemoteDesktop (session bus, owner gnome-shell)

Object `/org/gnome/Mutter/RemoteDesktop`, interface `org.gnome.Mutter.RemoteDesktop`:
`CreateSession() -> o`; properties `SupportedDeviceTypes u` (=7 live: keyboard|pointer|touch),
`Version i` (=1 live). XML: `refs/mutter/data/dbus-interfaces/org.gnome.Mutter.RemoteDesktop.xml`.

Live: `busctl --user call org.gnome.Mutter.RemoteDesktop /org/gnome/Mutter/RemoteDesktop org.gnome.Mutter.RemoteDesktop CreateSession`
-> `o "/org/gnome/Mutter/RemoteDesktop/Session/u1"` (the session object vanishes when the caller's
connection closes). I did not `Start` a session (that would inject input into the user's desktop).

`org.gnome.Mutter.RemoteDesktop.Session` (48.7 XML): `Start()`, `Stop()`, signal `Closed`, property
`SessionId s`, `CapsLockState b`, `NumLockState b`; input: `NotifyKeyboardKeycode(u keycode, b state)`,
`NotifyKeyboardKeysym(u keysym, b state)`, `NotifyPointerButton(i,b)`, `NotifyPointerAxis(d,d,u)`,
`NotifyPointerAxisDiscrete(u,i)`, `NotifyPointerMotionRelative(d,d)`, `NotifyPointerMotionAbsolute(s,d,d)`,
`NotifyTouchDown/Motion(s,u,d,d)`, `NotifyTouchUp(u)`; clipboard: `EnableClipboard(a{sv})`,
`DisableClipboard`, `SetSelection(a{sv})`, `SelectionWrite(u)->h`, `SelectionWriteDone(u,b)`,
`SelectionRead(s)->h`, signals `SelectionOwnerChanged(a{sv})`, `SelectionTransfer(s,u)`;
`ConnectToEIS(a{sv}) -> h` (unix fd of an EIS socket).

Caveats to verify before relying on it: (a) whether Mutter 48.7 really has no caller check beyond the
ScreenCast/RemoteDesktop session ownership (observed: none for CreateSession); (b) injected keys pass
through `meta_display_handle_event` and are therefore also reported to our own `KeyEvent` stream:
gorca must tag/skip its own injected events (match keycode+state+time window); (c) Mutter drops
duplicate presses from virtual devices.

## Portals (org.freedesktop.portal.Desktop, live)

* `org.freedesktop.portal.RemoteDesktop`: `CreateSession(a{sv}) -> o`, `SelectDevices(o,a{sv}) -> o`,
  `Start(o,s,a{sv}) -> o`, `ConnectToEIS(o,a{sv}) -> h`; property `AvailableDeviceTypes u` = 7.
* `org.freedesktop.portal.InputCapture`: `CreateSession(s,a{sv}) -> o`, `ConnectToEIS(o,a{sv}) -> h`,
  property `SupportedCapabilities u` = 15 (live host exposes v1 `CreateSession`; the refs/ checkout of
  `data/org.freedesktop.portal.InputCapture.xml` documents a newer v2 with `CreateSession2`, which this host does not have).
* `org.freedesktop.portal.GlobalShortcuts`: `CreateSession(a{sv}) -> o` (+ `BindShortcuts`, source).

## libei

`libei1`, `libeis1`, `libei-dev` 1.3.901 are installed (`pkg-config libei-1.0`). Sender context
(`ei_new_sender`) injects keyboard/pointer events over an EIS fd obtained from the portal or from
`Session.ConnectToEIS`; a receiver context is for InputCapture. Mutter is the EIS server. libei does
not provide global key observation. Source: `refs/libei/README.md`, `refs/libei/doc/`.

## evdev (last resort)

`/dev/input/event0` is `root:input 0660`; the current user has group `input` (id 996), so reading works
here but on a default Debian install ordinary users are not in `input`; logind `TakeDevice` is only for
the session controller (the compositor). Never `EVIOCGRAB`. Reading evdev duplicates what libinput does
and needs its own XKB state to map keycodes to keysyms.
