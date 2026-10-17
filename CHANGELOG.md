# Changelog

All notable changes to gorca are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). gorca has no
tagged release yet; the version in `Cargo.toml` is `0.0.0`. The entries below summarise the work
since the project started on 2026-10-12. The detailed record is `docs/decisions.md`.

## [Unreleased]

### Added

Phase 1 (foundation)

- Cargo workspace with enforced dependency rules between crates (`cargo xtask deps`), and a
  headless test harness: a private session bus, a real AT-SPI bus and fake AT-SPI applications.
- AT-SPI2 connector, with a raw-socket event drain and a lock-free ingress ring that coalesces and
  bounds events per application. It was chosen in a bake-off against two other designs.
- Focus reader: name, role and state speech, keyboard echo, per-application quotas, protection of
  the focused application, and a request deadline on every call.
- Speech clients for voicego (native protocol) and speech-dispatcher (SSIP), with automatic
  fallback from voicego to speech-dispatcher.
- Daemon with a development service unit and a Super+Alt+G toggle script, and a GNOME Shell
  helper extension.
- End-to-end latency harness; measured gorca-side p99 of 2.0 ms for focus to speech, 0.9 ms for
  cancelling stale speech and 3.1 ms under a 50k events/s storm, with no lost events.

Phase 2 (daily-driver reader)

- Command layer: gorca modifier (Insert or Caps Lock), keymap as data with NVDA-style and
  Orca-style presets in desktop and laptop layouts, "where am I", window title, repeat last
  speech, speech rate, volume, pause and mute controls, learn mode, TOML settings with hot reload
  (`gorca config check`, `config defaults`, `config print-default`).
- Mirror: per-application object cache with invalidation and focus-window prefetch.
- Native text: caret, word, line and character echo, typing echo, selection, say-all; Firefox
  text fields and rich editables.
- Review: object navigation and flat review, with a numpad layout and a laptop layout.
- GNOME Shell and desktop: window activation and dialog speech, notifications, on-screen display,
  Alt+Tab switcher, lock-key announcements, battery and date and time commands, per-application
  sleep mode, speech history.
- GNOME Terminal (VTE): screen-change reading after a settle period, typing, erase and history
  recall echo, review, and password prompts that stay silent.
- Firefox: browse mode for web documents (virtual cursor, quick navigation by element type,
  tables, lists, landmarks, live regions, forms, browse and focus mode switching, say-all) and
  ARIA names, roles, states and descriptions modelled on NVDA.
- Extension host on wasmtime with a WIT interface (`gorca-ext-api`), per-extension and
  per-application stores, CPU, memory and call-size limits, and a native fallback. The GNOME
  Shell policy and the terminal policy ship as WASM components.
- Speech: language, output module and voice settings; punctuation levels and per-language symbol
  tables (English, Russian); combinable capital-letter indication (pitch, word, sound).
- Chromium and Chrome support (M9): the same browse mode and quick navigation as Firefox, per-engine
  quirk rows, accessibility tree woken by gorca itself, off-screen frames woken on scroll.
- Message catalogue (`gorca-i18n`, M12a): every spoken string localised, English and Russian.
- Firefox tab and address bar announcements.
- Resource-usage measurements (`docs/reports/resource-usage.md`) and an add-on platform design
  (ADR-0019, proposed).
- `CONTRIBUTING.md` (DCO), `LICENSE-EXCEPTION` and licence files.
- Runtime topology with real-time scheduling through rtkit and core pinning, selected
  automatically (`--topology`), chosen in a bake-off.
- Model-based state tests for the reader's modes, and an acceptance gate runner
  (`bench/acceptance/phase2.sh`).

### Changed

- Extension policies for GNOME Shell and the terminal moved from the core into extensions behind
  the `gorca-ext-api` interface; the core no longer imports them.
- The mirror fill policy defaults to prefetching the focus window (measured in a bake-off).
- Key names are echoed only with `echo.command_keys`; before, they were spoken whenever typed
  characters were on.
- Keyboard grabs cover the keysyms of every configured input source, so shortcuts work with
  several layouts.
- The repository is licensed GPL-3.0-or-later; the extension SDK is MIT OR Apache-2.0. The symbol
  table headers now state their NVDA origin.

### Fixed

- voicego stability (stop and speak race, panic containment, sink reopen and fast-fail open),
  fixed upstream in the separate voicego project.
- Alt+Tab and Ctrl+Alt+Tab no longer say "unknown" when the Shell floods events; nameless
  buttons are named from their label relation.
- Terminal flat review jumping to the line start while output streams: the cursor now follows its
  line by content.
- Stale terminal screen in flat review; terminal output not spoken until the first key after a
  focus change; terminal tracking that stayed degraded after the policy slot was busy; lost
  baseline read in the terminal extension.
- Typed keys spoken as key names after flat review; review now ends when a key goes to the
  application.
- Mutter 48.7 stuck modifier after a grab change; ambiguous double-tap chords are no longer
  executed.
- Several load-dependent test flakes (fixture pacing, wall-clock bounds in unit tests, a busy
  loop on a vanished session bus).

[Unreleased]: https://github.com/Ravino/gorca/commits/main
