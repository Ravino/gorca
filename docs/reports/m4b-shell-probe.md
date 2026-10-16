# M4b ground truth: what GNOME Shell 48.7 emits over AT-SPI (private headless session)

Measured 2026-10-15/16 on this host (Debian 13, RT kernel, mutter/gnome-shell 48.7, at-spi2-core 2.56,
private `dbus-daemon` session + a11y bus). Final complete run: `docs/reports/m4b-shell-probe-logs/run6/`
(host load average 12 to 21 during the run, per-scenario value in each log header and in `summary.txt`).
Governor/quiet-host tuning was NOT applied; latencies are for a busy workstation, so treat them as upper-ish bounds.

## Summary table: scenario -> events that fire -> text a reader can read, from which node

All events are D-Bus signals from the Shell's connection (`:1.3` in the run) on interface
`org.a11y.atspi.Event.Object` / `.Window`; "S[showing]=1" means `Object.StateChanged` detail `showing`, detail1=1.
Times are first-event latency after the trigger call (monotonic ms, arrival at a raw Gio subscriber).

| # | Scenario | Events that fire (order) | Readable text: node |
|---|---|---|---|
| 1 | Notification banner (`Notify`, title+body) | ChildrenChanged[add] (hidden parent, +12..30 ms) -> ChildrenChanged[add] on the banner container (+14..37) -> S[showing]=1 on node role `notification` (+0.2 ms later) -> S[showing]=1 on each label (app name, "Just now", title, body). No focus, no window:activate, no announcement. | `notification` node has Name `''`. Text = Name (and Text iface) of its `label` descendants: app name, relative time ("Just now"), title, body, in tree order. Action buttons are `push button` nodes with Name (e.g. "Reply"); the default action has no node. |
| 2 | Banner with actions | same as 1; extra named `push button` per non-default action | button Name = action label |
| 3 | Second notification while one shows | NO showing/focus events for the second while the first is up (only a hidden ChildrenChanged[add]); `queue` is 1. After the first banner expires, the second follows (22 to 24 ms after the first's S[showing]=0; run6 N3: 4540 ms hide, 4562 ms ChildrenChanged[add], 4564 ms S[showing]=1). `replaces_id` update: the queued banner shows the replaced text | same as 1 |
| 4 | Banner hide | S[showing]=0 on labels, then on the `notification` node, then ChildrenChanged[remove] + S[defunct] for the whole subtree. Needs USER INPUT first (see Failures), then ~1.1..2.2 s | n/a |
| 5 | OSD via `org.gnome.Shell.ShowOSD` | PropertyChange[accessible-value] on `level bar` (+5 ms, BEFORE showing) -> S[showing]=1 on label and level bar. Update while shown: only PropertyChange[accessible-value] bursts (about 16 ms animation steps, ~7 events); PropertyChange[accessible-name] on the label only when the text changes. Hide: S[showing]=0 on label+bar ~1.6 s after the last show | label Name = OSD title ("Volume", "Brightness", "Muted"); `level bar` has the Value iface (Min 0.0, Max 1.0, `CurrentValue` = level; the event's any_data is NOT the value, read the property). No-level OSD (mute) = label only |
| 6 | Overview open/close (Eval or Super key) | S[showing]=1 on search entry text + `Show Apps` toggle, S[checked]=1 on `Activities` toggle button, then buttons. NO focus on open; on first typed key: S[focused] on search text, S[active]/Window.Activate "Main stage" | `panel 'Overview'`, dash buttons by Name (app names), `Show Apps` toggle, hint label "Type to search" |
| 7 | Overview search typing ("ged") | TextChanged[insert] on the entry per key, S[showing]=1 for "Searching…" label, then result `push button`s (Name `''`, child label Name = app name) | entry Text; results: label under each `push button` (unnamed button!) |
| 8 | App grid | S[checked]=1 on `Show Apps`; then ~560 events incl. one PropertyChange[accessible-name] per app label | app labels (Name = app name) |
| 9 | Alt+Tab / Alt+` switcher | S[focused]=0/1 window "Main stage" + S[active] + Window.Activate "Main stage" (+157 ms), S[showing]=1 on switcher buttons, S[selected] moves between buttons on each Tab; client frame gets Window.Deactivate. On Alt release: Window.Deactivate "Main stage", Window.Activate `<client frame>` (client's own bus name) | per item: `label` Name (app name; for Alt+` the window title) under an unnamed `push button`; selection = S[selected] on the button |
| 10 | Quick settings open | S[showing]=1 on its buttons/toggles (NOT on the menu node), then S[focused]/S[active]/Window.Activate "Main stage" (+16 ms), client Window.Deactivate. Close: reverse | buttons by Name ("Settings", "Power Off Menu", "Take Screenshot"), labels ("Dark Style", "100%") |
| 11 | Run dialog (Alt+F2) | ChildrenChanged[add] child role `dialog` (+33 ms) -> S[focused]=1 on dialog -> window "Main stage" focus/active/Window.Activate -> S[focused]=0 dialog, S[focused]=1 on entry `text` -> S[showing]=1 on dialog (+39 ms); client Window.Deactivate (+45 ms) | dialog Name `''`; title = label "Run a Command", hint label "Press ESC to close"; entry = `text` |
| 12 | EndSessionDialog (D-Bus `Open`) | same shape as 11 minus ChildrenChanged (dialog pre-built): S[focused] dialog -> window activate -> S[showing]=1 dialog (+15 ms); focus lands on button "Cancel" | title label "Log Out <user>", message label, buttons "Cancel"/"Log Out" (Name) |
| 13 | Polkit auth dialog (synthetic) | like 11/12, focus ends on `password text` (Name `''`, hint text not exposed as Name) | labels "Authentication Required", the action message, user name; buttons "Cancel", "Authenticate"; hidden "Caps lock is on" |
| 14 | Lock screen (hand-built ScreenShield) | focus/active/Window.Activate "Main stage"; ChildrenChanged[add] child role `window`; S[showing]=1 on it; sessionMode `unlock-dialog`; on key: `password text` focus, **Object.Announcement** (detail1=2, any_data string e.g. "Authentication error") | labels: clock "12:11 AM", date, "Click or press a key to unlock", user name; buttons |

Not seen at all anywhere: `Focus` interface events (focus is only `StateChanged[focused]`), `ActiveDescendantChanged`,
`SelectionChanged`, `Announcement` outside the lock screen, `Description` on Shell nodes (always `''`).

## Rig (deliverable 1)

`tests/shell/run_shell.sh [--no-unsafe] [--keep] [--] <cmd...>` = `tests/harness/run-isolated.sh` (private session bus,
private a11y bus, registryd; IsEnabled + ScreenReaderEnabled set to true BEFORE the Shell starts) + an in-world
`gnome-shell --headless --wayland --no-x11 --unsafe-mode --virtual-monitor 1280x800` + the Notifications service.
Prints `DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS XDG_RUNTIME_DIR WAYLAND_DISPLAY GORCA_SHELL_PID GORCA_SHELL_LOG
GORCA_HARNESS_DIR` on stderr and exports them (plus `GORCA_M7_CBUS` = the session bus, for `tests/web/m7/rd.py`) to `<cmd>`.

What makes the Shell register on the a11y bus: nothing special. With `IsEnabled=true` already set on the private
`org.a11y.Status`, Clutter/cally's atk-bridge registers at init (verified: Shell is `:1.3` next to `ibus-extension-gtk3`).
Do not run `tests/harness/fake_mutter.py` here: the real Mutter owns `org.freedesktop.a11y.Manager` in this session.

Facts the rig works around (all verified, error lines in "Failures"):
* `--unsafe-mode` is accepted by 48.7; `org.gnome.Shell.Eval` works (used for triggers and state dumps).
* The Shell needs a reachable system bus, so it is given the HOST system bus socket, read-only use, as `run_m7.sh` does
  (Shell takes a logind delay inhibitor, gone at exit). Nothing is written to the live session; the probe asserts the
  host logind session `LockedHint` before/after the lock test (`b false` both).
* In 48 `org.freedesktop.Notifications` is owned by a separate service `gjs -m /usr/share/gnome-shell/org.gnome.Shell.Notifications`
  (D-Bus-activated normally). The rig starts it and records its PID.
* Headless GNOME starts the session in the Overview (`Main.overview.visible` is true at start). The probe hides it first.
* `ibus-daemon` is auto-started by the Shell inside the private session (private, swept at teardown).

Teardown: the in-world script signals only the Shell PID and the notification-service PID it recorded; `run-isolated.sh`
then signals its own process groups and sweeps `/proc` for processes whose environ names its temp dir. No name-based kill.

## verified_shell for a private Shell (deliverable 2a)

Source: `crates/gorca-connector-atspi/src/connector.rs:watch_shell` and `events/router.rs:is_shell` / `proc.rs`.
* `protected_process` = (pid, start time) of the **owner of `org.gnome.Shell` on the SESSION bus**
  (`GetNameOwner` + `GetConnectionCredentials.ProcessID`, start = `/proc/<pid>/stat` field 22).
* An a11y application is `verified_shell` iff its a11y-bus connection's `ProcessID` (also `GetConnectionCredentials`)
  and start time equal that pair. Process group and cgroup are NOT part of the Shell identity (they only form the app-cap group key).
* Measured (`identity.json.gz`): app root bus `:1.3`, name `gnome-shell`, role `application`, toolkit `clutter` 48.7, AtspiVersion 2.1,
  Application.Id 0, interfaces Accessible/Application/Collection, one child (window "Main stage", path `/org/a11y/atspi/accessible/1`),
  pid = `GORCA_SHELL_PID` on the a11y bus AND on the session bus (`shell_pid_matches_session_owner: true`).
  The private dbus-daemon returns `ProcessID` (no `ProcessFD`), which is what the connector reads.
* Verdict: yes, the headless private Shell would be classified `verified_shell` provided gorca connects to BOTH private
  buses (`DBUS_SESSION_BUS_ADDRESS` and the a11y address). Nothing else is needed. Caveat for cap grouping: the Shell inherits
  the launching terminal's systemd scope (`vte-spawn-....scope`), so every client started from the same shell shares one
  `group_key` with it; harmless here because the Shell is protected by identity.

## Event-shape recipes to key on (deliverable "exact AT-SPI event shapes")

D-Bus signal body for Object events: `(s detail, i detail1, i detail2, v any_data, a{sv} properties)`; registration with
the registry is `RegisterEvent(s event, as properties, s app_bus_name)` (signature `(sass)`; the old `(sas)` is not needed).

* **Banner appearing**: signal `org.a11y.atspi.Event.Object.StateChanged`, detail `showing`, detail1 `1`, source role
  `notification` (AtspiRole 101). Preceded (0.2 to 12 ms) by `ChildrenChanged` detail `add` whose `any_data` is
  `(bus, path)` of that notification node; two such `add` events occur, the first to a hidden parent (ignore), the
  second to the visible banner container. Read the label children for text. Hide = `StateChanged[showing]=0` on the same node.
* **OSD appearing/updating**: `Object.PropertyChange` detail `accessible-value` on role `level bar` (AtspiRole 103,
  `Value` iface, Min 0.0 / Max 1.0; read `CurrentValue`) and `StateChanged[showing]=1` on the sibling `label` (Name = text); updates = more
  `accessible-value` events, plus `PropertyChange` detail `accessible-name` on the label only if its text changes.
  There is no ChildrenChanged and no window event.
* **Dialog opening**: `ChildrenChanged[add]` whose child role is `dialog` (when not pre-built), then
  `StateChanged[focused]=1` on the `dialog` node, `Window.Activate` (any_data `Main stage`, source `:1.3/1`),
  `StateChanged[active]=1` + `[focused]=1` on window `Main stage`, `StateChanged[focused]=1` on the default-focus child
  (entry `text` / `push button` Cancel / `password text`) and finally `StateChanged[showing]=1` on the dialog (all within ~1 ms).
  Do NOT use `Window.Activate "Main stage"` alone as the dialog signal: Alt+Tab, quick settings, overview search and the
  lock screen emit the same pair. Dialog Name is always `''`; its title is its first `label` descendant.

## Failures and gotchas (exact lines)

1. No system bus: `Gjs-CRITICAL: JS ERROR: Gio.IOErrorEnum: Could not connect: No such file or directory` at
   `loginManager.js:98` (`LoginManagerSystemd`), then `Segmentation fault (core dumped)`. Fix: give the Shell the host system bus socket.
2. `org.freedesktop.Notifications` is not owned by the Shell (`ServiceUnknown: The name org.freedesktop.Notifications was not provided by any .service files`). Fix: start the Notifications service (above).
3. `Main.screenShield` is null: `LoginManager.canLock()` needs `org.gnome.DisplayManager` version >= 3.5.91 on the system bus;
   the host has `fake-displaymanager.service` answering `"1.0"` (quick settings show "Screen Locking requires the GNOME display manager").
   Workaround used (private Shell only): build the real class by hand via `import('resource:///org/gnome/shell/ui/screenShield.js')`
   inside Eval, neuter `_loginSession.SetLockedHintAsync`, call `lock(true)`; host `LockedHint` stayed `b false`. The lock screen is therefore
   the real ScreenShield/UnlockDialog code, but built by hand (not by the Shell's normal startup path).
4. Polkit: the Shell cannot register as the agent: log line `GNOME Shell-Message: Failed to register AuthenticationAgent`. The dialog in scenario 13 is
   synthetic: the real `AuthenticationDialog` is created via `componentManager._allComponents.polkitAgent._onInitiate(...)`, `PolkitAgent.Session.prototype.initiate`
   is stubbed (polkit is never contacted) and the PAM prompt is injected with `_onSessionRequest(null,'Password:',false)`. A real pkexec flow was not run.
5. **A banner does not expire without user input**: after `Notify`, `trayState` stays 2 (SHOWN) for at least 21 s (run6 N1: the snapshot taken at the peak is slow, so the
   "6.5 s without user input" mark is at +21.7 s), because MessageTray waits for user activity. A Shift tap through RemoteDesktop starts the timeout (hidden 1.1 to 2.2 s later). A reader must not assume a
   banner disappears on a timer in an idle session. Also: `CloseNotification(first)` while a second was queued left `queue=1, trayState=0`
   for 20 s+ (run4 N3 peak `second_banner_shown`; a re-run with a user-activity tap showed the same 27 s stall, but that log was overwritten when I re-ran into the
   same directory) while natural expiry hands over to the queued one in ~23 ms (run5/run6 N3). Not root-caused. (The run6 N3 `second banner followed ...: False`
   mark is a probe predicate that sampled after the second banner had already come and gone; the events show it.)
6. Keyboard focus: in the headless seat the "Main stage" window is not `active`/`focused` until the first key arrives from the
   RemoteDesktop virtual keyboard (`keyFocus: null` in `shell_state`). Overview open alone gives no focus events; the first typed key does.
7. Churn: one burst of ~770 events inside 250 ms about 12 to 14 s after hiding the Overview (S0_idle; app-grid labels rebuilt), plus
   a 1000+ event burst after the first lock. Idle (nothing triggered) is otherwise silent.
8. Source enrichment is a LATE snapshot: `enrich_lag_ms` p50 608, p90 3565, p99 5495 (`facts.txt`) in event storms, key events are enriched first
   (priority queue) but names/states in `src` are not the state at emit time. For dead sources `last_known` (from earlier snapshots) is shown.
   Event timestamps are arrival times at a raw GDBus subscriber (not emit times).
9. Tree size: 1337 to 1500 nodes idle (Cache.GetItems works on the Shell: path `/org/a11y/atspi/cache`), roles mostly `panel` with Name `''`.
10. Per-run node paths (e.g. the banner container 71 vs 153 vs 1163) are not stable across Shell runs; key on role/shape, not paths.
11. Not run: pyatspi (all raw D-Bus through Gio, so the probe does not perturb the Shell as a libatspi client would); gnome-text-editor and gnome-calculator are not installed (clients: `tests/shell/gtkwin.py` GTK3 window + `gedit`); a real polkit prompt and a real gdm lock path were not testable.

## Rerun

```
. ./env.sh   # not needed for the probe itself (system python), only for cargo
tests/shell/run_shell.sh -- /usr/bin/python3 tests/shell/probe_shell.py --out docs/reports/m4b-shell-probe-logs/runN
tests/shell/run_shell.sh -- /usr/bin/python3 tests/shell/probe_shell.py --out /tmp/x --only N1_notification,O1_osd,D2_end_session_dialog
/usr/bin/python3 tests/shell/summarize.py DIR; /usr/bin/python3 tests/shell/keyshapes.py DIR [scenario...]
/usr/bin/python3 tests/shell/view.py DIR/N1_notification.json.gz [all]; /usr/bin/python3 tests/shell/facts.py DIR
gzip -9 DIR/*.json
```
Scenario names: S0_idle N1_notification N2_notification_actions N3_second_and_replace N4_critical O1_osd O2_osd_nolevel V1_overview_eval
V2_overview_super_key V3_overview_search V4_app_grid W1_windows_appear T1_alt_tab T2_alt_grave Q1_quick_settings D1_run_dialog
D2_end_session_dialog D3_polkit_dialog L1_lock_screen. A complete run takes about 9 minutes under load 15.

## Logs

`docs/reports/m4b-shell-probe-logs/run6/` is the final full run (`*.json.gz` = raw events with enrichment, per-scenario `peaks` (tree diffs taken at the visible peak), shell state before/after; `keyshapes.txt`, `summary.txt`, `facts.txt`, `identity.json.gz`, `probe.stderr.log`, `client-*.log`). The readable `*.events.txt` renderings and the earlier iterations `run1..run5`, `run4-d3`, `run4-n3` were dropped after the M4b review (duplicates and superseded runs; the user's login name is replaced by `user`); regenerate the text with `tests/shell/view.py` from the `.json.gz`. The very first runs (scratchpad, not kept) had `Main.screenShield` null (`TypeError: s is null`) before the hand-built ScreenShield.
