# gorca AT-SPI test harness

Headless, isolated AT-SPI sessions plus scriptable fake applications. No compositor, no
toolkit, no network, nothing from the user's real desktop.

```
tests/harness/
  run-isolated.sh       private session bus + a11y bus + registryd; runs <cmd> inside
  fakeapp.py            fake AT-SPI app (speaks the D-Bus interfaces directly)
  fake_mutter.py        emulated org.freedesktop.a11y.KeyboardMonitor (Mutter) on the private session bus
  fakeapp_ctl.py        Python helpers: FakeApp (spawn + JSON control), A11yWatcher (event collector)
  run-smoke.sh          smoke tests inside a fresh session + real-bus isolation check
  smoke/test_*.py       unittest smoke tests (one process per file)
```

Quick start:

```sh
tests/harness/run-smoke.sh                       # all smoke tests, ~15 s
tests/harness/run-isolated.sh /usr/bin/python3 tests/harness/fakeapp.py --scenario form
tests/harness/run-isolated.sh bash               # interactive shell inside the session
```

Always run the fake apps and Python tests with **`/usr/bin/python3`** (python3-dbus, PyGObject,
pyatspi are Debian packages; the project venv does not have them).

## 1. Isolation (`run-isolated.sh`)

`run-isolated.sh [--hold] [--no-screen-reader] [--keep] [--] <cmd...>`; the exit status is the command's.

| Piece | How | Why |
|---|---|---|
| Directory | `mktemp -d /tmp/gorca-h.XXXXXX`; private `XDG_RUNTIME_DIR`, `HOME`, `XDG_*` inside | short path (unix socket limit 108), removed on exit |
| Session bus | `dbus-daemon --config-file=<generated> --nofork`, listening on `$RT/bus`, **no service dirs** | nothing on the host can be D-Bus-activated by accident |
| a11y bus | the real `at-spi-bus-launcher --launch-immediately`, `ATSPI_DBUS_IMPLEMENTATION=dbus-daemon`, socket `$RT/at-spi/bus` | same bootstrap path as production (`org.a11y.Bus.GetAddress`) |
| Registry | `at-spi2-registryd` (without `--use-gnome-session`) | there is no session manager |
| Enablement | `org.a11y.Status.IsEnabled=true` and `ScreenReaderEnabled=true` on the private bus (F-9) | apps build trees only when enabled; `--no-screen-reader` sets only `IsEnabled` |
| Environment | `env -i` plus an allowlist (below); `DISPLAY`/`WAYLAND_DISPLAY` never set; system bus address points to a nonexistent socket; `GSETTINGS_BACKEND=memory` | no leakage in either direction |
| Teardown | every daemon runs under `setsid`; on EXIT/INT/TERM/HUP: SIGTERM the process groups, 1 s grace, SIGKILL, then sweep `/proc/*/{cmdline,environ}` for the temp dir path and kill leftovers | no orphans even if the command crashes or the runner is killed (SIGKILL of the script itself is not catchable, see Limits) |

Environment passed to `<cmd>`: `PATH LANG LC_ALL TERM CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR RUST_LOG
RUST_BACKTRACE GOROOT GOPATH VIRTUAL_ENV LD_LIBRARY_PATH` (if set; add more with
`GORCA_PASSTHROUGH="FOO BAR"`), plus the session variables:

| Variable | Value |
|---|---|
| `DBUS_SESSION_BUS_ADDRESS` | private session bus |
| `AT_SPI_BUS_ADDRESS`, `GORCA_A11Y_BUS_ADDRESS` | private a11y bus (what `org.a11y.Bus.GetAddress` returns) |
| `XDG_RUNTIME_DIR`, `HOME`, `XDG_{CONFIG,DATA,CACHE,STATE}_HOME` | inside the temp dir |
| `GORCA_HARNESS_DIR` | temp dir root (logs of the daemons are in `$GORCA_HARNESS_DIR/log/`) |
| `DBUS_SYSTEM_BUS_ADDRESS` | nonexistent socket |

### Why `dbus-daemon`, not `dbus-broker`, for the private buses

* `dbus-broker-launch` needs a systemd user manager (activation, `--scope user`); the `at-spi-bus-launcher`
  picks dbus-broker whenever it runs inside a systemd user unit (`sd_pid_get_user_unit`), which would talk to the
  **real** `systemd --user`. Forcing `ATSPI_DBUS_IMPLEMENTATION=dbus-daemon` makes startup deterministic (1.4 s,
  no systemd) and cannot touch the host.
* Consequence: bus-level overload semantics (F-3: unbounded buffering, then disconnect of a slow reader) are
  dbus-broker specific. Tests of that behaviour need a separate broker-based fixture; the harness only
  guarantees protocol correctness and load generation. Open item.

### Isolation proof

`run-smoke.sh` snapshots the clients of the real session bus and the real a11y bus before and after the run,
fails on any difference or a leftover temp dir, and `smoke/test_smoke.py::Isolation` checks the environment from
inside. One-off stronger check (done at delivery): `busctl --user monitor` and `busctl --address=<real a11y bus>
monitor` ran during a smoke run; 0 messages mentioning `gorca-fakeapp` or the harness dir.

## 2. Fake application (`fakeapp.py`)

Implements the AT-SPI D-Bus interfaces itself (python3-dbus FallbackObject under
`/org/a11y/atspi/accessible/*`): `Accessible`, `Component`, `Text`, `EditableText`, `Action`,
`Application` (root), `org.freedesktop.DBus.Properties`, `Cache` at `/org/a11y/atspi/cache`
(`GetItems` + `AddAccessible`/`RemoveAccessible`), registration through `Registry` `Socket.Embed`, and the
`org.a11y.atspi.Event.{Object,Window,Focus}` signals (`siiva{sv}`; every event carries a `seq` entry in the
property dict so receivers can check order and loss). Node ids: root = `/org/a11y/atspi/accessible/root`,
others `/org/a11y/atspi/accessible/<id>`. Interfaces not supported by a node answer `UnknownMethod`.

Why not a real toolkit: GTK3/GTK4 import fine headless, but need a display (Xvfb/Wayland), and cannot be
made to hang, storm or lie. Direct implementation gives full control over every byte.

```
fakeapp.py --scenario {form,tree,storm,hung,terminal,malformed,switcher} [--name N] [--seed S] [options]
```

| Scenario | Options | What it is |
|---|---|---|
| `form` | | frame > panel > labels, entries `name`/`email` (single-line, editable), check box `Subscribe`, buttons `Submit`/`Cancel`, multi-line `notes`. Node names are returned in the `ready` line (`paths`) |
| `tree` | `--nodes N` (10 .. 100000+), `--fanout F` (1 = N-deep chain), `--tree-text` | `node-<i>` list items; with `--fanout 10 --nodes 100000`: `Cache.GetItems` returns 100k items in 1.5 s |
| `storm` | `--storm-rate R` (default 5000), `--storm-mix children=1,text=1,state=1,focus=1`, `--storm-duration S` | N events/s of mixed types over random nodes, paced by a monotonic clock (measured: 5k/s exact, 40k/s sustained, received = emitted on a same-user bus). Events are synthetic (the tree is not mutated), so refs can be stale like in real apps |
| `hung` | `--hang never` or `--hang delay:MS`, `--hang-methods GetText,Get`, `--hang-after N` | never reply / reply late; optionally only for named members, optionally after N good calls. Control channel keeps working. `Get` is the Properties member |
| `terminal` | `--term-rows/--term-cols`, `--term-gen LINES_PER_FRAME`, `--term-fps 60`, `--[no-]term-charcount-quirk` | `TERMINAL` role node with Text. Every frame rebuilds the screen, diffs by common head/tail and emits **one `delete` + one `insert` covering everything between, then `text-caret-moved`** (VTE behaviour, F-4). `CharacterCount` is 1.5x `len(GetText)` by default |
| `switcher` | run with `--name gnome-shell`; six push-button items | GNOME Shell's Alt+Tab popup: `select` moves `state-changed:selected` (0 on the old item, 1 on the new one) and nothing else; no focus event |
| `malformed` | `--malformed bad_refs,neg_offsets,...` or `all` (default for this scenario) | see below; the modes work on every scenario via `--malformed` or the `malformed` command |

Malformed modes (all seeded, so reproducible with `--seed`):

| Mode | Effect |
|---|---|
| `bad_refs` | `GetChildren`/`GetChildAtIndex`/`Parent`/`GetApplication` return null refs, refs to nonexistent peers (`:1.999999`), garbage bus names, empty bus name with path `/`, self refs, nonexistent object paths |
| `neg_offsets` | text events with negative / `i32::MAX` / `i32::MIN` offsets and lengths; `CaretOffset` negative; `GetTextAtOffset` returns `start > end`, negative bounds; children-changed with negative / huge index |
| `bad_payload` | event `any_data` of the wrong variant type (int for text, string or `(ss)` for a child ref, empty array), empty / absurd `detail` strings |
| `count_mismatch` | `ChildCount` disagrees with `GetChildren`; `CharacterCount` negative/huge; `GetIndexInParent` out of range |
| `bad_role_state` | role 9999, state arrays of 0 / 1 / 5 words, unknown interface names in `GetInterfaces` |
| `huge` | 1 MB names, `GetText` returning 2 MB more than the count |

Limits: libdbus validates object paths and bus names for method calls when *it* marshals, so a syntactically
invalid *path* in an `o` value cannot be sent (the fake app would abort itself). Garbage bus names in `s`
fields are fine. If raw malformed wire bytes are needed (truncated messages, wrong signatures), add a raw
socket sender; not built yet.

**Finding (reproducible, `Malformed.test_libatspi_client_vs_bad_payload`):** a libatspi client (pyatspi) aborts
with `dbus_message_new_method_call ... _dbus_check_is_valid_bus_name` when a `ChildrenChanged` event carries
`any_data` with an empty/invalid bus name. gorca must validate refs before building calls. Other clients on
this stack (atspi-rs) are expected to hit the offset and payload cases; run them against `--malformed all`.

### Control channel

JSON lines on stdin; replies `{"event":"reply","id":..,"ok":true,...}` and notifications (`ready`, `action`,
`storm_done`, `error`) on stdout. EOF on stdin quits the app (so the app dies with its owner).

| `cmd` | Fields | Effect |
|---|---|---|
| `focus` | `node` (name/id/path) | state-changed `focused` 0/1 + `Focus:Focus`; the reply carries `t_emit_ns` (`CLOCK_MONOTONIC`, taken right before the focused=1 signal, which is flushed immediately) for latency probes |
| `type`, `backspace`, `set_text`, `caret` | `node`, `text`/`offset` | text-changed insert/delete + text-caret-moved |
| `action` | `node`, `index` | like a client `DoAction`: JSON `action` line, pressed/checked state events |
| `emit` | `node`, `class` Object/Window/Focus, `member`, `detail`, `d1`, `d2`, `data` | any raw event |
| `add_child`, `remove` | `parent`/`node`, `name`, `role`, `text` | tree mutation + `Cache.Add/RemoveAccessible` + children-changed |
| `hang` | `mode` never/delay/off, `ms`, `methods`, `after` | change hang policy at runtime |
| `storm` / `storm_stop` | `rate`, `mix`, `duration` | start / stop (returns `emitted`, `achieved_rate`, `max_lag_s`) |
| `select` | `node` | switcher scenario: move the selection (see above) |
| `malformed` | `modes` (list or `"all"`, `[]` = off) | toggle bad data at runtime |
| `term_feed`, `term_gen`, `term_stop` | `text`, `lines`, `fps` | terminal output |
| `stats`, `paths`, `quit` | | counters (`calls hung delayed events storm_events term_frames bad_values`), name->path map |

Python: `with FakeApp.start("--scenario", "form") as app: app.cmd(cmd="focus", node="email")`.

## 3. Using the harness from Rust integration tests

No crate is provided here (rust-engineer owns the workspace). Two supported patterns:

**A. Wrap the whole test run** (simplest; all tests share one session, apps are started per test):

```sh
tests/harness/run-isolated.sh cargo test -p gorca-atspi --test integration
```

The test reads `AT_SPI_BUS_ADDRESS` / `DBUS_SESSION_BUS_ADDRESS` (`zbus::connection::Builder::address`), and
spawns apps itself (pattern C). Do not use `zbus::Connection::session()` autodetection against the real bus: the
env is already private, so it connects to the private session bus.

**B. One session per test binary or per test** with `--hold`: prints `KEY=VALUE` lines, then `READY`, and tears
everything down when its stdin closes (or it is killed):

```rust
// helper sketch, put in the test-support crate
use std::{io::{BufRead, BufReader}, process::{Child, Command, Stdio}, collections::HashMap};

pub struct Session { child: Child, pub env: HashMap<String, String> }

impl Session {
    pub fn start() -> std::io::Result<Self> {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/harness/run-isolated.sh");
        let mut child = Command::new(script).arg("--hold")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let mut env = HashMap::new();
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line?;
            if line == "READY" { break; }
            if let Some((k, v)) = line.split_once('=') { env.insert(k.into(), v.into()); }
        }
        Ok(Self { child, env })
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());      // EOF -> script tears down
        let _ = self.child.wait();
    }
}
```

**C. Spawn a fake app** (inherits the session env; reads JSON lines from stdout until `"event":"ready"`):

```rust
let mut app = Command::new("/usr/bin/python3")
    .args([fakeapp_py, "--scenario", "tree", "--nodes", "100000"])
    .envs(&session.env).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY")
    .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
// ready line: {"event":"ready","bus_name":":1.5","root":"/org/a11y/atspi/accessible/root",...}
// send a command: writeln!(app.stdin, r#"{{"cmd":"storm","rate":20000}}"#)
// drop(app.stdin) -> the app exits
```

The ready line is printed only after `Socket.Embed` returned, so the app is visible in the registry root.
For pattern A the child inherits the env automatically; for B pass `session.env`.

## 4. Smoke tests

`run-smoke.sh` (18 + 2 tests, ~15 s): environment privacy, `IsEnabled`, registry present; form tree via pyatspi,
focus/text events, DoAction/EditableText over D-Bus; tree 10 (pyatspi), 100k (`Cache.GetItems`), 5000-deep chain;
storm 5k/s exact and 40k/s peak with loss/order checks and call latency during the storm; hung (never,
delay, filtered, after-N; `NoReply` on the client side, `busctl --timeout` "timed out"); terminal diff events and
the `CharacterCount` quirk; malformed values observed on the wire and the libatspi abort reproduction.
Extra args are passed to each unittest file (e.g. `-k Hung`). Note that libatspi's own call timeout
is 800 ms but 15 s during an app's first 15 s of life, so pyatspi hang tests need a warm app or raw D-Bus calls.

## Limits / open items

* SIGKILL of `run-isolated.sh` itself cannot run the trap; daemons are left running. They carry the temp dir path in
  their command line or environment, so `grep -alF gorca-h. /proc/[0-9]*/environ` finds them.
* dbus-broker fidelity (F-3) is not covered, see above.
* Event emission is unconditional (a real atk-bridge filters by `Registry.GetRegisteredEvents`).
* Rate numbers are from this host (RT kernel, idle); they bound the load generator, not gorca.

## fake_mutter.py (keyboard monitor emulation)

Owns `org.freedesktop.a11y.Manager` at `/org/freedesktop/a11y/Manager` on the private session bus
(refuses to start outside a harness bus). `WatchKeyboard`, `UnwatchKeyboard`, `GrabKeyboard`,
`UngrabKeyboard`, `SetKeyGrabs` are access-checked like Mutter: the caller must own
`org.gnome.Orca.KeyboardMonitor`, otherwise `AccessDenied`. Control: `{"cmd":"key","keycode":38,
"keysym":97,"unichar":97,"state":0,"released":false}` emits the unicast `KeyEvent(b u u u q)` to every
watcher and replies with `t_emit_ns`; `stats` returns watcher and denied counts. Used by the
end-to-end latency harness (`bench/e2e/README.md`).
