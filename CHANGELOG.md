# Changelog

All notable changes to gorca are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). gorca has no
tagged release yet; the version in `Cargo.toml` is `0.0.0`. The entries below summarise the work
since the project started on 2026-10-12. The detailed record is `docs/decisions.md`.

## [Unreleased]

### Newest changes (2026-10-24 to 2026-10-26)

Added

- One semantic model and one presentation pipeline (ADR-0022). Every surface now reads only the
  fused model and speaks through the shared presentation record `PresIr` (version 1): focus and
  change, browse mode, quick navigation and say-all, review and object navigation, where-am-I,
  window titles and notifications, extension words and "explain". Each surface was cut over
  behind a replay gate (`cargo xtask cutover <surface>`: every corpus family, English and
  Russian, zero unclassified differences, byte-identical reruns and shuffled arrival order) and
  its old path was then deleted. The last commit before each deletion is tagged
  `legacy/<surface>-last` (focus, browse, window, whereami, review, ext, carrier, model).
- No-legacy rule (`cargo xtask legacy`, rule 12): the allowlist file is gone, and any use of a
  banned legacy API (`Role`, `StateSet`, raw attribute maps) in ported code fails the build. The
  node snapshot fields are private and the mirror node holds the fused entry only.
- Secrets: a secret mark on a node is sticky, one shared question decides what is secret for
  every speak and log sink, and browse-mode text reading is secret-aware. A review path that
  spoke the contents of a toolkit password entry whose fused role was a text box was found by a
  security scan and fixed. Rule 14 bans hand-written password checks.
- WebAssembly plugin system (ADR-0026), experimental and off by default (`[plugins] enabled`):
  wasmtime Component Model runtime, trust tiers, default-deny capabilities, system scopes that
  third parties cannot reach, fuel and wall-clock budgets with strikes, a MAC-verified `.cwasm`
  cache in a 0700 directory, and each plugin as its own actor with a bounded wait on the key
  path. Third-party plugins are not loaded in the product yet.
- Statistical plugin scheduler (SPS) behind `[plugins] scheduler = "labels" | "sps"`. The default
  stays `"labels"` because SPS did not meet its acceptance gate on every measure
  (`docs/reports/m18a-3-sps.md`).
- Hybrid-CPU placement of plugin workers (`[plugins] placement = "auto" | "off"`, effective with
  `scheduler = "sps"`) and an optional `/dev/cpu_dma_latency` idle-state lease
  (`[plugins] idle_lease = "auto" | "off"`) with a shipped udev rule,
  `packaging/udev/70-gorca-cpu-dma-latency.rules`, that you install yourself. See
  `docs/reports/m18a-5-placement.md` for the measurements and the cases where the goal was not
  met.
- First-party `telegram-web` plugin for Telegram Web A and K in Firefox and Chromium (early and
  experimental): chat-list rows are named "name, unread count, last message, time" without the
  avatar's initials. In an open Web A chat the messages are a list of list items, your own
  messages are described as "Your message", the chat title is a level-1 heading, date separators
  are level-2 headings, and the page has landmarks named Chats, Messages and Message input (all in
  English and Russian). To use it set `[plugins] enabled = true`, run the identity helper and
  install the package root-owned under `/usr/lib/gorca/addons/gorca.telegram-web` with every
  directory, including `i18n/`, readable by your user.
- Planned, recorded as design decisions (no code yet): ADR-0028 (accepted), virtual actions, which
  make mouse-only affordances reachable from the keyboard through one actions menu opened with
  gorca+Shift+A; and ADR-0029 (proposed), synthetic nodes, which let a plugin declare virtual
  regions such as a chat header. ADR-0022 (one semantic model and presentation pipeline) is
  marked Accepted.
- Application identity helper: `gorca-identity.service` (`gorca --identity-helper`) verifies
  application identity outside any user namespace, so `gorca.service` keeps its full sandbox
  (`ProtectSystem`, `PrivateTmp`, `ProtectKernel*`). It uses a tagged, pipelined protocol with a
  cache, deadlines and retry; 200 applications were verified in about 25 ms in the report's
  measurement (`docs/reports/2026-10-26-linux-platform-identity-helper.md`). Without the helper
  applications are unverified and only non-URL first-party plugins apply.
- Extension interface `gorca:ext` 0.4.0: extensions read the fused entry (`role-code`, `flags`,
  `policy-view`, and a `read-group` accessor). Secret nodes withhold every text group. The
  interface is frozen until extensions and plugins share one ABI.
- Generic fixes from the Telegram Web study: re-anchoring the browse cursor after a removed pane,
  the page following the browse cursor so virtual lists load, a content-named scroll container,
  and an inferred label for nameless web fields (a port of Orca's label inference) stored as a
  low-trust fact.

Changed

- `install-dev.sh` installs both units; with `GORCA_BIN` both run the same binary.
- Web menu popups: the extra focus Gecko fires on a `role=menu` popup is folded into the item,
  which is now spoken with the menu name and role (Chromium too).

Fixed

- Doubled typing echo: Mutter 48.7 delivers keys routed through the input method twice about 1 ms
  apart; the input lane drops an identical event within 8 ms. Synthetic input faster than 8 ms
  would lose events.
- A plugin job that was too large faulted (Watchdog or Malformed) on live Telegram pages;
  background jobs are now sized from measured cost, retried smaller, and facts are owned by their
  target.
- `info.window_title` climbs to the window by fetching parents when the mirror lacks the
  ancestry.
- The input lane no longer spins on the real-time keyboard thread while a gorca modifier is held
  for more than 1.5 s: the stale-key deadline was left in the past, so the session loop never
  slept, and the kernel's real-time CPU limit (SIGXCPU) demoted the lane for the rest of the run.
  See `docs/reports/input-lane-sigxcpu.md`; the lane is still not promoted again after a
  demotion.
- Telegram Web plugin on a live session: gorca warns when plugins run inside a private user
  namespace, where application identity cannot be verified; an unreadable plugin message
  catalogue now produces a warning instead of silently dropping the chat names; and a fact on a
  node a plugin only read is admitted once (`docs/reports/telegram-web-live-diagnosis.md`).

Known issues

- Object navigation `next` at the end of a GTK4 list, and `next` or `previous` from a Firefox
  check box, can say "not responding" instead of "no next" (present before the cut-over).
- Shell flat review reads the actor "layout" as unavailable.
- The `term_parity` test can fail under load when the first Terminal call of the VTE native hook
  exceeds its limit.
- Plugin latency gates were measured on a loaded, `powersave` host and are not yet confirmed on a
  quiet one; the SPS and idle-lease goals are met on the median in some cases and not in others.

### Newest changes (2026-10-19 to 2026-10-23)

Added

- Mouse emulation through the `gorca-helper` GNOME Shell extension (opt-in: the `pointer`
  capability in the helper allow-list). See [the mouse guide](docs/user-guide/mouse.md).
  - NVDA-style keypad keys on the desktop presets (Num Lock off) and gorca chords on the laptop
    presets: left and right click at the pointer, drag lock for each button with automatic release
    after 120 seconds, move the pointer to the review position, move the review cursor to the
    pointer, and say the pointer position.
  - Item clicks (`mouse.left_click_item`, `mouse.right_click_item`, unbound by default) that use the
    application's own action first and the pointer second.
  - Safe clicks at monitor scales 1, 1.25 and 2 in GTK 3, GTK 4, XWayland, Firefox, Chromium and
    Electron: visibility, hit-test and occlusion checks, and a deadline and expiry on every call.
- Configuration profiles: per-application settings that are created by edits, a rule that
  profiles override only keys of the global settings, and chords to save, save to global, cycle,
  switch off, choose the edit target and announce. See [the profiles guide](docs/user-guide/profiles.md).
- Web-app modes: browse mode works in Chrome, Chromium and Electron applications, and
  gorca+Space (gorca+A in the Orca presets) is a sticky focus mode there, so arrow keys go to the
  application. A built-in row turns browse mode off for the Visual Studio Code family.

Fixed

- Firefox: gorca+Space after a window is activated no longer says "not in a web page".
- Firefox: Enter clicks only the item that was announced; if the item changed, the first Enter
  announces it and the next one clicks.
- Firefox: Ctrl+C copies the page selection when a text field holds the focus.
- Chromium and Electron editors (Signal-style composers): Backspace and Delete runs are heard, and
  typing is echoed once.
- Mutter 48.7 crash mitigation: gorca can no longer trigger the `rebuild_all_grabbed_modifiers`
  crash when the keyboard monitor connection is slow (see the earlier entry below).

Known issues

- Firefox sometimes drops the first click after start-up (an item click succeeded on the first
  try in 9 of 12 measured runs); the second click works.
- Chromium and Electron: the application hit test is skipped for item clicks.
- `browse.mode_default` is saved but not yet applied when a document first gets focus, and there
  is no built-in Signal row yet.

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
- Unified editing model: one shared caret and word presenter with per-toolkit quirk rows for
  GTK3/GTK4 entries and text views, gedit, GNOME Text Editor, Firefox and Chromium editables and
  the terminal; a late terminal caret event is spoken as one utterance. Known issue: Backspace
  over a selection in a web contenteditable does not speak the removed text.
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
- Firefox tab and address bar announcements; Firefox focus names and flat review follow the
  accessible name.
- Web semantics: icon-only buttons reachable in browse mode, mixed (half-checked) checkboxes.
- Browse-mode selection model: the page selection is mirrored so that Ctrl+C copies it.
- Chromium and Chrome flat review (text extents in screen coordinates, repeated static leaves
  skipped, container names on their first child row).
- Editing polish: the key that leaves review is echoed in terminals and fields; clipboard and
  undo confirmations are spoken only when the application confirms them; boundary indication is
  combinable (off, cue, say) like capital-letter indication; word-granularity levels; extension
  interface `gorca:ext@0.3.0`.
- Terminal focus-protection watch: while a terminal is unprotected its focus is re-confirmed,
  and a stale reply fails over instead of being trusted.
- Acceptance harness fixes (resume across commits, Chromium and Firefox rigs).
- Resource-usage measurements (`docs/reports/resource-usage.md`) and an add-on platform design
  (ADR-0019, proposed); ADR-0021 (copy) and ADR-0022 (semantic model) are planned.
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

- Caps Lock no longer breaks single-letter browse navigation: the quick-navigation grabs also
  cover the opposite-case keysym of every letter, including Cyrillic.
- Insert, KP_Insert and Caps Lock now behave as one gorca modifier, so the same chord runs the
  same command with any of them under every lock state. The double tap passes the modifier key
  itself, and keypad zero types `0` only with Num Lock on.
- Key and typed-text diagnostics are redacted: logs no longer contain typed characters or keypad
  digits.
- Protection against a GNOME Shell (Mutter 48.7) crash that a reader reconnect could trigger:
  `SetKeyGrabs` is sent first on every new keyboard-monitor connection, a slow reply is retried
  on the same connection instead of counting as lost, reconnects wait for the old connection to
  close, and grabs change only when needed. Details in `docs/reports/shell-segfault-20261019.md`.
- The terminal no longer goes silent after Alt+Tab.
- Firefox single-page applications: Enter no longer activates a stale or wrong node after the
  page re-renders. If the item under the browse cursor was replaced or relabelled (other than a
  toggle whose pressed, checked or expanded state flipped), the first Enter announces it instead
  of activating it; the next Enter activates. A click is retried only when the target is
  unambiguous. gorca+T falls back to the last known window title.
- voicego stability (stop and speak race, panic containment, sink reopen and fast-fail open),
  fixed upstream in the separate voicego project.
- Alt+Tab and Ctrl+Alt+Tab no longer say "unknown" when the Shell floods events; nameless
  buttons are named from their label relation.
- A terminal whose tracking stayed degraded is tracked again at the next key.
- Ctrl+C not copying the page selection in browse mode.
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
