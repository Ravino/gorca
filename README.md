# gorca

Русская версия: [README.ru.md](README.ru.md)

*GO, Read, CArry on.*

gorca (Gorca Orchestrates Real-time Concurrent Accessibility) is a screen reader for Linux, written in Rust. It targets the GNOME desktop on Wayland and is
meant to be a fast, stable replacement for Orca: it runs on an async event loop with explicit state
machines and is designed to stay responsive when the desktop or an application misbehaves.

**Status: pre-release.** Phase 2 of the roadmap is nearly complete (see [Status](#status)). It is
developed and tested on a single Debian 13 machine. Expect rough edges, no packages, and settings
and interfaces that can still change. Braille is not implemented yet.

## About the name

The name is a recursive backronym: Gorca Orchestrates Real-time Concurrent Accessibility. The tagline
"GO, Read, CArry on" is the playful reading of the same letters.

## Goals

- **Latency.** The target is gorca-side processing of less than 15 ms at the 99th percentile from a
  keypress to the start of speech. Stale speech is cancelled as soon as a newer event arrives.
- **Never block.** No IPC call runs without a deadline. A hung or flooding application degrades only
  its own handler, never the reader.
- **Event storms.** Events are coalesced, deduplicated and dropped when superseded, so the reader
  keeps up with tens of thousands of AT-SPI events per second.
- **Crash isolation.** Extensions run as WebAssembly components with CPU, memory and call-size
  limits; a faulty extension or per-application handler cannot take down the core.

Resource use, measured on a private session with a release build and synthetic event storms
(`docs/reports/resource-usage.md`): idle about 0.4 % of one core and about 15 MiB of memory;
memory stays flat up to 100k events per second; CPU grows about 0.55 to 0.6 % of a core per 1,000
events per second; key to speech stays at p99 0.3 to 1 ms up to 100k events per second. Real
applications such as Firefox hold more state: a live session shows about 40 MiB.

The measured numbers and the conditions they were taken under are in `docs/reports/` and
`bench/results/`. Latency gates are unreliable on a loaded host; see `docs/decisions.md`.

## Status

Phase 1 (focus reader, speech, event ingress, test harness) and most of Phase 2 are done. On
`main` today:

- Command layer: the gorca modifier (Insert or Caps Lock), NVDA-style and Orca-style key maps in
  desktop and laptop layouts, "where am I", speech controls and speech history, all configured as
  data in a TOML file with hot reload.
- Text: caret, word and line echo, typing echo, selection and say-all in native text fields,
  Firefox editables included.
- Unified editing: one caret and word presenter for GTK3 and GTK4 entries and text views, gedit,
  GNOME Text Editor, Firefox and Chromium editables and the terminal, with per-toolkit quirk rows.
  A late terminal caret event is spoken as a single utterance. Known issue: Backspace over a
  selection in a web contenteditable does not speak the removed text.
- Review: object navigation and flat review.
- GNOME Shell and desktop: window, dialog and notification speech, Alt+Tab, on-screen display,
  lock keys, battery and clock.
- GNOME Terminal (VTE): screen change reading, typing and history recall echo, password prompts
  kept silent.
- Firefox: the tab strip and address bar are announced (page title on tab switch, address bar
  autocomplete), and browse mode with structural quick navigation, tables, live regions, forms and
  focus-mode switching, and ARIA names, roles, states and descriptions modelled on NVDA.
- Chromium and Chrome: browse mode on the same web engine as Firefox, with per-engine quirk rows.
  gorca wakes the browser's accessibility tree itself, and flat review works. Known issues are listed in
  `docs/reports/m9-fixround-verify.md`.
- Speech: punctuation levels and per-language symbol tables (English and Russian), capital
  letters announced by any combination of pitch, the word "cap" and a sound (the sound is not
  working yet), language, output module and voice selection.
- Localisation: every spoken string comes from a message catalogue (English and Russian,
  ADR-0010), also for extensions.
- Terminal review follows its line by content while output streams (ADR-0018), and a terminal
  whose tracking degraded is tracked again at the next key.
- Copy: in browse mode the page selection is mirrored so that Ctrl+C copies it; a wider copy
  model is planned (ADR-0021).
- Web semantics: icon-only buttons are reachable in browse mode, mixed (half-checked) checkboxes
  are spoken, and Firefox focus names and flat review follow the browser's accessible name.
- Extensions: a WebAssembly extension host (wasmtime). The GNOME Shell and terminal policies ship
  as extensions.
- Mouse emulation (opt-in): NVDA-style click, drag lock and route-to-review keys, using the
  `gorca-helper` GNOME Shell extension. See the [mouse guide](docs/user-guide/mouse.md).
- Profiles: per-application settings with chords to save, cycle and announce them. See the
  [profiles guide](docs/user-guide/profiles.md).

Planned, not done: braille, the Newton connector, an extension (add-on) platform with
third-party add-ons and a store (ADR-0019, proposed), a typed semantic model and one
presentation pipeline (ADR-0022, proposed), a settings GUI, packaging. See `docs/plan/phase-2.md` and
`docs/plan/phase-3.md` for what is planned, and `CHANGELOG.md` for the detail.

## Architecture

The core is an API: a backend-agnostic engine with the object model, event bus, state machines,
speech and braille output and navigation. It never imports the types of an accessibility source.

Text description of the architecture: accessibility sources feed the core. Today the only source
is the AT-SPI2 connector; a Newton connector is planned and both are meant to run side by side.
The core keeps the object model, the event bus and the state machines, and sends speech to a
speech client, which talks to voicego or speech-dispatcher. Extensions plug into the core through
the `gorca-ext-api` WIT interface.

```
AT-SPI2 connector --\                                  /--> voicego (native protocol)
                     >--> core --> speech client ------<
Newton connector ---/      ^                           \--> speech-dispatcher (SSIP)
(planned)                  |
                  WASM extensions (gorca-ext-api)
```

- **Connectors** are pluggable accessibility sources. The AT-SPI2 connector is implemented.
  Newton (Wayland-native, AccessKit-based) is the second planned connector.
- **Extensions** carry app-specific behaviour (GNOME Shell, terminal). They are WebAssembly
  components built against the WIT interface in `crates/gorca-ext-api/wit/`; a native build of the
  same source is the fallback if a component is missing.
- **Speech is a separate process.** gorca never synthesizes in-process. It talks to a speech
  server over a socket.
- Rules for the crate layout and dependencies are in `docs/adr/0001-core-architecture.md`
  and are checked by `cargo xtask deps`. Other design records are in `docs/adr/`, platform contracts
  in `docs/platform/`.

### Speech servers

- **speech-dispatcher** over SSIP is the compatibility backend.
- **voicego** is an optional, separate speech server with a faster native protocol. It is its own
  program under AGPL-3.0; gorca only speaks its socket protocol and shares no code with it.
- `--speech=auto` (the default) tries voicego first and falls back to speech-dispatcher.

## Requirements

- Debian 13 (other distributions are untested), GNOME Shell 48 on Wayland, the AT-SPI2 stack
  (`org.a11y.Bus`).
- A speech server: speech-dispatcher with a working output module, or voicego.
- Rust 1.99 or newer with the `wasm32-wasip2` target (for the extensions).
- For the tests: `dbus-daemon`, `at-spi2-core` and `python3` with PyGObject.

## Build

The repository uses a project-local toolchain: `env.sh` points `CARGO_HOME`, `RUSTUP_HOME` and
related variables at a `.toolchain/` directory, which is not part of the repository. Without it,
use your own rustup installation and skip `env.sh`.

```sh
. ./env.sh              # only with a project-local .toolchain/
make build              # debug build of the workspace
make release            # extensions as WASM components, then the release daemon
```

`make release` produces `target/release/gorca` and the extensions in `target/release/extensions`,
where the daemon finds them by default.

## Run

```sh
target/release/gorca config print-default   # every setting with its default and a comment
target/release/gorca --help
```

- The settings file is `~/.config/gorca/gorca.toml` (or `$XDG_CONFIG_HOME/gorca/gorca.toml`, or
  `--config=PATH`). It is reloaded when it changes. `gorca config check [PATH]` reports problems.
  The settings are documented in `docs/config-reference.md`.
- **Switching from Orca.** gorca never stops or replaces a running Orca. Turn Orca off first (for
  example with Super+Alt+S), then start gorca with `--impersonate-orca`. That makes it own
  `org.gnome.Orca.KeyboardMonitor`, which GNOME's key monitoring and Chromium's assistive
  technology detection expect. To go back, stop gorca and start Orca.
- gorca does not set `org.a11y.Status.ScreenReaderEnabled`, because GNOME starts Orca when that
  key is set. It sets `IsEnabled` only.
- `contrib/systemd/gorca.service` is a development unit; it expects the binary in
  `~/.local/bin/gorca` and the extensions in `~/.local/lib/gorca/extensions`, so adjust it to your
  install. `tools/gorca-toggle.sh` and `tools/install-dev.sh` toggle it with
  Super+Alt+G.
- A small GNOME Shell helper extension is in `contrib/gnome-shell-extension/` (see its README).

## Test

```sh
make check                   # rustfmt, clippy -D warnings, extension build, cargo test, dependency rules
cargo test --workspace       # unit and integration tests only
tests/harness/run-smoke.sh   # AT-SPI smoke tests on a private, headless session bus
```

The tests and benchmarks start their own D-Bus and AT-SPI sessions and never touch your running
desktop. The end-to-end latency harness is described in `bench/e2e/README.md`, the AT-SPI harness in
`tests/harness/README.md`, and the manual acceptance walk-through in
`docs/plan/checklists/phase2-acceptance.md`.

## Contributing

See `CONTRIBUTING.md`. Contributions are accepted under the Developer Certificate of Origin
(`Signed-off-by` on every commit); there is no CLA. Open an issue before sending large changes.

## Licence

- The gorca core is licensed under the **GNU General Public License, version 3 or (at your option)
  any later version** (`LICENSE`).
- The core and the built-in extensions (`extensions/`) are GPL-3.0-or-later.
- The extension SDK is licensed under **MIT OR Apache-2.0**: the `gorca-ext-api` crate with its
  WIT files in `crates/gorca-ext-api/`, and `gorca-i18n`, the only workspace crate it depends on
  (`LICENSE-MIT` and `LICENSE-APACHE` sit in each crate directory).
- `LICENSE-EXCEPTION` adds a GPL-3.0 section 7 additional permission: a WebAssembly extension that
  talks to gorca only through the extension SDK may be distributed under any licence. This text is
  a draft and has not yet had a legal review. Any licence (open, closed, free or paid) is allowed for such add-ons.
- Third-party references and the origin of the symbol tables are listed in `THIRD-PARTY.md`.
- voicego is a separate AGPL-3.0 program that gorca communicates with over a socket; no voicego
  code is part of this repository.
