# org.a11y.atspi.Registry (a11y bus) and event registration

Verified live on the a11y bus (`busctl --address unix:path=/run/user/1000/at-spi/bus ...`) and in
`refs/at-spi2-core/{xml/Registry.xml,registryd/registry.c,atk-adaptor/bridge.c,atk-adaptor/event.c,atspi/atspi-event-listener.c}`.

## Objects

| Name | Path | Interface |
|---|---|---|
| `org.a11y.atspi.Registry` (live owner `:1.1`, at-spi2-registryd) | `/org/a11y/atspi/registry` | `org.a11y.atspi.Registry` |
| same | `/org/a11y/atspi/accessible/root` | `org.a11y.atspi.Accessible`, `.Component`, `.Socket` (desktop root) |
| same | `/org/a11y/atspi/registry/deviceeventcontroller` | X11-era key/mouse device listener API; not usable for Wayland keys |

`org.a11y.atspi.Registry` (live):

| Member | Signature | Notes |
|---|---|---|
| `RegisterEvent(event, properties, app_bus_name)` | `s as s -> ` | `event` = `Class:major:minor:detail`, e.g. `Object:TextChanged:Insert`, `Window:Activate:`. `properties`: extra per-event property names the app should attach to the signal in its `a{sv}` dict (implemented by atk-bridge, `_atk_bridge_find_property_func`). `app_bus_name`: empty = all apps. |
| `DeregisterEvent(event, app_bus_name)` | `s s ->` (legacy `s`) | |
| `GetRegisteredEvents()` | `` -> `a(ss)` | (listener unique name, event string). Live result for Orca: 38 entries from `:1.79`, e.g. `Object:TextCaretMoved:`, `Object:StateChanged:Focused`, `Object:TextChanged:Insert`, `Window:Activate:`, `Mouse:Button:`. |
| `EventListenerRegistered` (signal) | XML says `ss`; **code emits `ssas`** (sender, event, properties) | Broadcast. Code bug: it calls `dbus_message_set_destination(message, ...)` on the *method call*, not on the signal, so the per-app destination is ignored. |
| `EventListenerDeregistered` (signal) | `ss` | |
| `version` (property) | `u` | |

Root `GetChildren()` (`a(so)`) lists every application's root as `(unique name, /org/a11y/atspi/accessible/root)`.
Live: 14 entries including gnome-shell `:1.0`, gsd daemons, gnome-terminal, gedit, Chrome, Firefox, Orca itself.
App appearance/disappearance is also visible as `ChildrenChanged` from the root and via
`org.freedesktop.DBus.NameOwnerChanged` on the a11y bus.

## Two-layer filtering (answers "does app-side event filtering exist?")

1. **Broker side**: libatspi adds a D-Bus match rule per listener,
   `type='signal',interface='org.a11y.atspi.Event.<Category>'[,sender='<app>'][,member='<Name>']`
   (`atspi-event-listener.c`, around line 400 and `dbus_bus_add_match` at ~802) *and* calls
   `Registry.RegisterEvent` (~line 649). Signals reach only peers with a matching rule; the broker
   routes them. A client that registers nothing gets no events, but also cannot learn about new
   objects (see Cache).
2. **App side**: atk-bridge (GTK3, Firefox, Chromium, Electron via ATK) fetches
   `GetRegisteredEvents` at start (`bridge.c: get_registered_event_listeners`) and tracks
   `EventListenerRegistered/Deregistered`; `signal_is_needed()` (`atk-adaptor/event.c:426`) drops
   every event nobody asked for, **except** an always-on set that keeps the cache valid:
   `ChildrenChanged`, `StateChanged` (all states), and `PropertyChange` for
   `AccessibleName/Description/Parent/Role` (skipped only for transient/manages-descendants objects).
   GTK4 (`gtk/a11y/gtkatspiroot.c` lines 514-834) and Qt (`qtbase/src/gui/accessible/linux/atspiadaptor.cpp`
   lines 877-902, per-event bit flags) implement the same GetRegisteredEvents + signal tracking.
   Caveat: for text events the ATK bridge calls `atk_text_get_text()` **before** `signal_is_needed`
   (`text_changed_event_listener`, event.c:979), so the app pays the text extraction cost even when
   nobody listens.
3. gorca consequence: register the minimum event set per focus context; deregistering is global
   per listener, so keep one registration set and filter further on our side. Wide registrations
   (`Object:*`) turn every app into an event firehose.

## Event signal shape

All `org.a11y.atspi.Event.Object` signals have body `(siiva{sv})`:
`detail string (minor), detail1 i, detail2 i, any_data v, properties a{sv}`; sender path is the source
object, sender = application's unique name. Interfaces and members (from `xml/Event.xml`):
`Event.Object`: PropertyChange, BoundsChanged, LinkSelected, StateChanged, ChildrenChanged,
VisibleDataChanged, SelectionChanged, ModelChanged, ActiveDescendantChanged, Announcement,
AttributesChanged, RowInserted, RowReordered, RowDeleted, ColumnInserted, ColumnReordered,
ColumnDeleted, TextBoundsChanged, TextSelectionChanged, TextChanged, TextAttributesChanged,
TextCaretMoved; `Event.Window` (Minimize, Maximize, Restore, Close, Create, Reparent, DesktopCreate,
DesktopDestroy, Destroy, Activate, Deactivate, Raise, Lower, Move, Resize, Shade, uUshade, Restyle,
PropertyChange); `Event.Mouse` (Abs, Rel, Button); `Event.Keyboard` (Modifiers); `Event.Terminal`
(LineChanged, ColumncountChanged, LinecountChanged, ApplicationChanged, CharwidthChanged; VTE does
not emit these); `Event.Document` (LoadComplete, Reload, LoadStopped, ContentChanged,
AttributesChanged, PageChanged); `Event.Focus` (Focus).
The minor part is encoded as `a:b` -> `a/b` for `arg0path` matching
(`adapt_minor_for_dbus`). Method names with `-` are CamelCased.
