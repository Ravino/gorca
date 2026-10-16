# Extensions: host, API and porting guide (M5)

Status: implemented in M5. Sources: ADR-0001 section 10 (model), ADR-0008 section 3.3 (one source,
two builds). Code: `crates/gorca-ext-api` (WIT 0.1, Rust mirror, guest adapter),
`crates/gorca-ext-host` (wasmtime host), `crates/gorca-core/src/ext.rs` (the hook points),
`extensions/ext-test` (the reference and fault-injection extension).

## 1. What an extension is

A per-application policy module. The core stays connector- and app-neutral (WE-1); everything a
product needs that the generic engine should not know (Alt+Tab in the Shell, terminal prompts) is an
extension. An extension sees a narrow, copied subset of the core model, speaks through the host, and
cannot touch anything else: no IPC, no files, no network, no clock beyond reading it.

One source, two builds (ADR-0008 3.3):

| Build | How | Isolation | Use |
|---|---|---|---|
| `wasm` (default) | `cargo build -p ext-x --target wasm32-wasip2 --release` gives a component | wasmtime: fuel + epoch limit, 32 MiB, trap becomes reset | shipping |
| `native` | the same crate linked as an `rlib`, built by the daemon or a test | `catch_unwind`, post-hoc deadline check; **cannot pre-empt a hang** | tests, debugging, emergency fallback |

The manifest says which (`backend = "native"` must be chosen explicitly and is in-tree only).

## 2. Where the hooks run

```
app actor  --on_event--> AppExt      after the mirror update; never between raw event and dirty mark (WE-2)
Reader     --present---> AppExt      inside PresenterRegistry::present; try_lock, busy = "no opinion"
Reader     --policy----> AppExt      the application policy (focus, value, activate, showing); try_lock, busy = "no opinion"
Reader     --command---> AppExt      queued to the host worker; answer returns as BackgroundMsg::ExtSpeech
keymap     compile()  -> ExtCommands names + default chords merged (never over the preset or the user)
```

- `gorca_core::{ExtSource, AppExt, ExtSink, ExtCommands, ExtVerdict}` are the only things the core
  knows. `gorca-ext-host` implements them; only `gorca-daemon` wires it (`--ext-dir`).
- Verdicts compose left to right across matching extensions (app-specific, then toolkit, then
  connector); `present` takes the first `Some`.
- `FocusChanged` and `NodeRemoved` are always routed whatever the verdict (the core keeps its focus
  bookkeeping); a `Rewrite` there still speaks. `ReEvaluate` routes the event and sets
  `resync_needed`.
- A hook never blocks the Reader or an app actor: `present` and `on_event` only `try_lock` the
  slot (a slot busy with a command answers "no opinion"); `command` runs on the host's two worker
  threads (`gorca-ext-worker`), never on the Reader or a tokio worker.
- Nothing slow happens on a caller's thread. A faulted instance is moved into a job; **the worker
  drops it and builds the replacement** (about 0.2 to 3 ms), and until it is ready the slot answers
  "no opinion". Only attach builds on the app actor (once per application).
- Per slot at most four commands are queued or running; further presses are dropped. The queue is
  bounded (256). The one inline cost left is the faulting call itself: a looping `present` runs in
  the Reader for at most its fuel limit (measured 0.3 ms for a bare loop, ~1 to 4 ms typical) and
  three strikes end it.

## 3. Limits and faults

| Limit | Value | Enforced by |
|---|---|---|
| `on-event`, `present`, `nav-*` | 2 ms CPU | fuel (`3_000_000` per ms, calibrated on this host) |
| `command` | 20 ms CPU | fuel |
| wall-clock backstop | 10 x the CPU limit | epoch deadline (5 ms tick, one ticker thread, started on first use) |
| memory | 32 MiB per instance | pooling allocator `max_memory_size` + `ResourceLimiter` |
| memory shape | one linear memory, 4 tables of at most 10,000 elements, 8 core instances per component | pooling allocator + `ResourceLimiter` |
| answer size | 32 segments, 2048 characters each, 8192 in all | host clamps (`convert.rs`), stops pulling at the cap |
| host calls per hook call | `speak` 8, `log` 8, `set_setting` 16, `get_node` 256 | `InstShared` counters |
| speech rate | 10 messages per second (burst 10) per (extension, application), whatever the path (`speak`, rewrite verdict, command answer); over budget a rewrite becomes `Suppress` | token bucket |
| settings | 128 keys, 64 B keys, 4 KiB values per extension | `SettingsKv` |
| manifest | 64 KiB, 64 commands, 32 globs per list, 64-character chords, 64 extensions per host | parser and builder |

Why fuel first: the limits are *CPU* limits. An epoch deadline is wall time: on a loaded workstation a
healthy hook that is merely descheduled for 2 ms would be interrupted and counted as a fault. An early
epoch-only storm run at load 25 showed this (per-call medians collapsed to ~35 ns, a disabled slot
answering "no opinion"); with fuel the same storm has 0 faults.

Strikes are counted at two levels, and every kind of failure counts:

- per (extension, application) slot: any fault (trap, panic, interrupt, memory, native overrun) is a
  reset; the third in 60 s disables the slot. An instance that cannot be rebuilt disables it at once.
  Re-announcing an application keeps its slot, so re-attaching cannot reset the count.
- per extension: an instance that cannot be built at attach (pool exhausted, start function traps) and
  every disabled slot is a strike; the third in 60 s disables the extension for every new
  application, with one spoken notice. A stream of applications therefore cannot make the host rebuild
  a hostile extension forever.

A **fault** is a trap, a panic, an out-of-fuel or epoch interrupt, a memory-cap hit, or (native) a call
over its limit. Every fault drops the instance and builds a fresh one from the `InstancePre` (WASM:
new `Store`, measured 0.2 ms with the next call included; native: new value). The third reset within
60 s (`InstanceGuard`) disables that extension for that application, with one spoken notice
("Extension ext test disabled for gnome-terminal"). A faulted hook returns "no opinion" to the caller,
so the core keeps speaking.

## 4. The sandbox

`wit/gorca-ext.wit` (package `gorca:ext@0.1.0`): `host` (`get-node`, `fetch`, `text`, `speak`,
`get-setting`, `set-setting`, `log`) and `hooks` (`on-event`, `present`, `policy`, `nav-units`,
`nav-match`, `command`). Capabilities in the manifest gate `speak`, `settings` and `fetch:*`; an undeclared one is
denied (`fetch` and `text` return `denied`, or `not-supported` when declared: they need an asynchronous
prefetch path that M6 adds for `ext-vte`).

`get-node` reads the mirror of the extension's *own* application only; another application's keys
resolve to nothing. Password-field names are withheld.

WASI: a `wasm32-wasip2` extension pulls in WASI imports through the Rust standard library. The host
links the pure ones (`wasi:random/*`, `wasi:clocks/*`, from the host's own clock, not cryptographic)
so `HashMap` and `Instant` work, and everything else (filesystem, sockets, environment, stdio, process)
is a trap. There is no `wasmtime-wasi`: it would run its own tokio blocking calls from inside the app
actor's runtime and grant ambient capability.

## 5. Manifest (`ext.toml`)

```toml
name = "ext-vte"                      # [a-z0-9_-]; commands become ext.ext_vte.<command>
backend = "wasm"                      # or "native"
module = "ext_vte.wasm"               # relative; wasm only
hooks = ["on-event", "present", "policy", "nav-units", "command"]
capabilities = ["speak", "settings", "fetch:prefetch", "fetch:background"]

[matcher]                             # glob lists; omitted = any
app_name = ["gnome-terminal*"]
toolkit = ["gtk*"]
connector = ["atspi"]
document_role = []                    # needs a document role, which attach() does not have yet (M7)
verified_shell = false                # true: only the connector-verified Shell, never a name

[[keymap]]
command = "say_prompt"                # ext.ext_vte.say_prompt
id = 1                                # what the command hook receives
help = "say the shell prompt"         # learn mode
contexts = ["focus"]                  # focus | global | browse | review
chord = "gorca+shift+p"               # optional default, bound in the first context
max_press = 1
interrupts = true
```

The loader (`ExtHost::load_dir`) reads `<dir>/<name>/ext.toml`. A bad manifest, a missing or corrupt
component, a duplicate name: reported in `LoadReport`, logged, skipped; never fatal.

Default chords are subject to hard rules, enforced in `ExtCommands::bind` and `compile`:

- they must use the gorca modifier: an extension never grabs a plain key (typing, passwords) or an
  application shortcut;
- precedence is **user > core preset > extension**: a chord the preset or the user mentions in *any*
  context (a tombstone included) is never taken, because a context table is consulted before
  `global` and would shadow it; the skipped default is logged (`warn`). Between extensions the first
  loaded wins;
- the chord only dispatches to an application the extension matched: the Reader sends the command to
  the focused application's `AppExt`, and an application with no matching extension ignores it.
  Names are `ext.*`, so they cannot collide with core commands.

User rebinding works by name in `gorca.toml` (`[keymap.focus] "gorca+f10" = "ext.ext_vte.say_prompt"`),
with the usual did-you-mean diagnostics; a user's entry, a tombstone included, beats the extension's
default chord.

## 6. Daemon

`gorca --ext-dir=DIR [--ext-settings=PATH] [--shell-policy=auto|native]` (env `GORCA_EXT_DIR`,
`GORCA_EXT_SETTINGS`, `GORCA_SHELL_POLICY`). When no directory is named, the binary (not the library,
so tests never pick one up) uses the first existing of `<exe dir>/extensions`,
`<exe dir>/../lib/gorca/extensions`, `/usr/local/lib/gorca/extensions`, `/usr/lib/gorca/extensions`;
`--ext-dir=` (empty) loads none. `cargo xtask build-ext` (`make ext`, part of `make check` and
`make release`) builds the shipped components and installs them as
`target/<debug|release>/extensions/<name>/{ext.toml,<module>.wasm}`, which is `<exe dir>/extensions`.
The settings KV defaults to `$XDG_STATE_HOME/gorca/ext-settings.toml` when extensions are loaded; it
is written by the worker thread, atomically, never from a hook.

**The Shell policy** (`ext-gnome-shell`) runs as the component when the host loaded it, else as the
native build of the same source linked into the daemon (`--shell-policy=native` forces the native
build; the log says which one runs: `shell policy: ext-gnome-shell WASM component` or `... native
build`). The two are exclusive: the daemon installs the native `AppPolicy` only when the component
is not loaded. If the component faults, three strikes disable it and the core presents the Shell
like any application (the same degradation as the native adapter's three panics).

## 7. Writing an extension

```rust
use gorca_ext_api::{Event, EventVerdict, Hooks, export_extension, host};

#[derive(Default)]
struct MyExt;

impl Hooks for MyExt {
    fn on_event(&mut self, ev: &Event) -> EventVerdict { /* ... */ EventVerdict::Pass }
    // present, nav_units, nav_match, command have defaults that defer to the core.
}
export_extension!(MyExt);
```

`Cargo.toml`: `crate-type = ["cdylib", "rlib"]`, a dependency on `gorca-ext-api` only (xtask rule 5),
`[lints] workspace = true`. Host functions are the free functions in `gorca_ext_api::host`
(`speak`, `get_node`, `get_setting`, ...): the same call compiles to a WIT import in the component
and to a thread-local `Host` in the native build.

Rules for good extensions: keep per-call work small (the limit is 2 ms of CPU); no unbounded loops
over the mirror (WE-3: at most one hook call per key per extension); keep state in the struct, not
statics; do not rely on random numbers being unpredictable.

## 8. Porting guide: `ext-vte` and `ext-gnome-shell`

Both are native code today (`ext-vte` is a skeleton; the Shell switcher policy is the
`AppProfile::speak_selection` switch in `gorca-core/src/builtin_ext.rs`). Porting is mechanical:

1. **Crate shape.** `extensions/ext-<name>/Cargo.toml`: add `[lib] crate-type = ["cdylib", "rlib"]`; the
   only workspace dependency stays `gorca-ext-api`. Implement `Hooks` (the existing `ext-vte`
   skeleton already does; the nav hooks now have defaults and can be deleted). Add
   `export_extension!(YourType);` (the type needs `Default`).
2. **Manifest.** Copy `extensions/ext-test/ext.toml`. `ext-gnome-shell` (ported, see
   `extensions/ext-gnome-shell/ext.toml`): `verified_shell = true`, hooks `["policy"]`, no
   capabilities. `ext-vte`: `toolkit` or
   `app_name` for GNOME Terminal (`gnome-terminal*`), hooks `["on-event", "present", "command"]`, its
   review/prompt commands as `[[keymap]]` entries.
3. **Event shape.** Hooks get the WIT subset (`gorca_ext_api::Event`): `FocusChanged`,
   `PropertiesChanged(node, mask)` (mask bits in `gorca_ext_api::prop`; `SELECTED_NOW` is the Shell's
   Alt+Tab signal), `ChildrenChanged`, `TextChanged`, `CaretMoved(node, offset)`, `SelectionChanged`,
   `Announcement`, `WindowActivated`, `NodeRemoved`. If a port needs more payload (text deltas for the
   `ext-vte` prompt heuristics), extend the WIT `event` variant and the one `event_to_ext` function in
   `gorca-ext-host/src/convert.rs`, then the guest and host conversions (`guest.rs`, `wasm.rs`): all
   are plain `match`es, additive, and the WIT/Rust role test fails if the two drift.
4. **Moving the policy.** (Done for the Shell *rule table*: dialog-as-notice, OSD level, unit slider
   percentage, window focus claim, "Main stage" activation. They moved behind the `policy` hook, a
   fourth core hook with the core's `AppPolicy` facts and verdict as WIT `policy-facts` /
   `policy-verdict`; the Alt+Tab switcher routing below is still the `AppProfile::speak_selection`
   switch.) The Shell switcher today is an app-actor routing rule (`speak_selection`
   -> `FocusLaneMsg::ItemSelected`). As an extension it becomes `on_event(PropertiesChanged with
   SELECTED_NOW)` answering `Rewrite(segments)` (speak the item) or `Suppress`; the core's focus
   presenter is untouched. Keep the `AppProfile` switch until the extension reproduces the M1/M4
   behaviour under the e2e gates, then delete it (WE-1).
5. **Verdict semantics to remember.** `Suppress`/`Rewrite` do not stop `FocusChanged` or
   `NodeRemoved` routing. `present` returns `Some(segments)` to replace the focus utterance, `None`
   to defer to the core.
6. **State.** Per-(extension, application) instance: a struct field is per application and is
   discarded on a reset. Anything that must survive goes through `host::set_setting`.
7. **Build and test both ways.** `cargo test -p ext-x` (native, plain Rust tests against `Hooks`);
   the same behaviour on the component with the host fixture in
   `crates/gorca-ext-host/tests/common/mod.rs` (copy `host(Backend)` and point it at the new
   manifest and factory). Run `cargo run --release -p gorca-ext-host --example x1` for the X1 rows,
   and the e2e gates with `--ext-dir`.
8. **Fetching.** `host::fetch` and `host::text` return `NotSupported` (declared) or `Denied` today.
   `ext-vte` needs a prefetch path: add it in `InstShared` (`fetch_error` is the single place), as an
   asynchronous mirror prefetch the next hook reads through `get_node` (keeps the no-blocking rule).
9. **Do not** add app names to the core, depend on any gorca crate but `gorca-ext-api`, or call the
   connector. Whatever two extensions both need moves into the core or the connector (ADR-0001 10).

## 9. Known limits

- `document_role` matching needs a role at `attach`; the host passes `None` until M7 wires documents,
  so a manifest that requires one matches nothing.
- Native extensions are linked in by the daemon (only the native build of `ext-gnome-shell`, as the
  fallback when its component is not installed); a directory only loads components.
- `nav-units` and `nav-match` exist in the WIT and the host but no core caller runs them yet (ADR-0003
  section 15, M7).
- The Reader runs `present` inline: a looping `present` costs up to its fuel limit (about 1 to 4 ms)
  once before the reset; three strikes end it for that application.
