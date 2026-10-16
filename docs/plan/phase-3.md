# Phase 3 plan: from a daily driver for the developer to a releasable product for blind users

Status: **Proposed** (2026-10-16). Owner: architect. Decisions taken from this plan are recorded by the
orchestrator in `docs/decisions.md`; the user questions in §9 block the milestones named there.

Inputs: `CLAUDE.md`; `docs/plan/phase-2.md`; `docs/decisions.md` (through 2026-10-16); ADR-0001 (§5.5
identity, §6.5 failure isolation, §8 speech, §10 extensions, §16.3 open questions, §17.2 sign-offs);
ADR-0002 (§13 phased rollout); ADR-0003 (web mirror, §11 browse mode, §21); ADR-0005 (commands, keymap,
settings); ADR-0006 (review); ADR-0007 (text); ADR-0008 (web engine placement + Amendment 1);
`docs/plan/m8-spec.md`; reports `m4b-followups.md`, `m5-review.md`, `m6-impl.md`, `m6-review.md`,
`m8-baseline.md`, `b5-pilot.md`, `2026-10-14-b1-judgement.md`,
`2026-10-16-perf-engineer-k2-loss-under-load.md`, `speech-lane-priority.md`, `ext-gnome-shell-wasm.md`,
`2026-10-12-voicego-audit.md`; `contrib/systemd/gorca.service`; `docs/platform/org.gorca.ShellHelper.md`.

## 0. Goal

Phase 2 made gorca a daily driver **for its developer**: Shell, Terminal, Firefox and (soon) Chromium
read well, the WASM extension host isolates per-app policy, and every user path has an e2e latency
gate. Phase 3 turns that into **a product a blind user who did not build it can install, start, learn,
configure, trust and recover from**, in English and Russian, with speech and braille, on Debian 13 /
GNOME 48. The exit is a closed alpha (§9 Q1), not a public 1.0.

"Releasable" is defined by six properties, each owned by one milestone and gated in §2.1:

| Property | Means for the user | Milestone |
|---|---|---|
| Survives itself | a crash or hang of gorca costs ≤ 2 s of silence, never a lost session; a crash loop hands over to Orca | M11 |
| Speaks their language | every message, role, state and symbol in Russian (and English); mixed-language text in the right voice | M12 |
| Configurable without a text editor | settings app that is itself accessible; pronunciation dictionaries | M13, M14 |
| Reads by touch | braille display with routing keys | M10 |
| Complete where it matters | elements list, find, copy in browse mode; zsh/fish/wide characters in the terminal | M15, M16 |
| Installs, starts, teaches | Debian package, GNOME screen-reader switch, Orca fallback, first-run tutorial, user guide | M17 |

## 1. Starting point (2026-10-16)

Merged on `main`: M1 commands/keymap/settings v0, B-1 hybrid native mirror (focus-window prefetch),
M3/M3b text and caret (native + Gecko), M4a review, M4b Shell/desktop polish with per-process speech
fairness, M5 WASM extension host (`ext-gnome-shell` runs as a WASM component by default), M6 GNOME
Terminal (`ext-vte`, native build), M7a/M7b Firefox browse mode, say-all and auto-read, M8 prep
(318-step NVDA parity harness), speech lane priority-aware slot.

In flight at the time of writing (Phase 2 tail, closed by C0 below): M8 implementation (final fixes),
M9 Chromium (worktree, W2b/W3 and typing probes), B-5 runtime-topology judgement (pilot only,
non-authoritative), `ext-vte` WASM port.

Explicitly deferred out of Phase 2 and therefore candidates here:

| Item | Source | Phase 3 milestone |
|---|---|---|
| Braille (M10) | decisions 2026-10-13, 2026-10-14; ADR-0008 "next free number" | **M10** (ADR-0009) |
| Settings GUI, speech dictionaries UI | phase-2 Q7, §3 "Out of Phase 2" | M13, M14 |
| Elements list, find, clipboard/copy in browse mode | m8-spec §3.11–3.12, D-3, §8 Q3 | M15 |
| zsh/fish/wide chars, 8 h terminal soak | decisions 2026-10-15/16 (M6 follow-ups) | M16 |
| Extension settings persistence (`ProtectSystem=strict`) | decision 2026-10-16 (ext-gnome-shell WASM) | M11 (StateDirectory), M18 (API) |
| `disabled_quirks` switch (ADR-0008) | decision 2026-10-15/16 | C0 |
| Crash reporting, telemetry-free diagnostics, config migration | user brief | M11 |
| Localisation (Russian UI/messages) | user brief; all strings are English literals today (`gorca-core/src/commands/info.rs` etc.) | M12 |
| Packaging, autostart, onboarding, Orca coexistence, tutorial | user brief; dev unit hard-codes `<repo>/target` | M17 |
| Third-party extension SDK and docs | user brief; WIT 0.1 (ADR-0008 Amendment 1) | M18 |
| Newton connector | R5, ADR-0001 §5.7, N-1…N-4 | M19 (readiness only) |
| voicego deep improvement | decision 2026-10-14 ("separate effort after Phase 2") | track **V** |
| Upstreaming (Mutter 48.7 stuck modifier, at-spi2-core, Chromium `IsEnabled`) | decisions 2026-10-14, ADR-0001 §5.5 | track **U** |
| ADR-0002 perception beyond scaffolding | phase-2 Q6 | stretch **MP** (phases 1–2 only) |
| Text editors: gedit and GNOME Text Editor (baseline app scope, user request 2026-10-16) | e2e scenarios for editing, word/line/paragraph navigation, selection, find/replace, tabs; user checklist | M11 (wave 1) |
| LibreOffice and other apps | CLAUDE.md "later" | Phase 4 (§10) |

## 2. Rules that apply to every milestone

Phase 2 rules **PR-1…PR-6 stay in force** unchanged (user-testable checklist, e2e gates under L3-50k with
`lost = 0` and `stale = 0` by conservation accounting, gate targets, state machines first, reviewer on
every merge, no core special cases). Added for Phase 3:

- **PR-7 No literal user-facing text after M12a.** Every spoken, brailled or displayed string goes through
  the message catalog by id. `cargo xtask strings` fails CI on a new literal in a presenter, command,
  notice or extension `say` (allow-list for tests and log lines).
- **PR-8 Nothing leaves the machine.** No telemetry, no network calls from gorca or its tools. Crash
  reports and diagnostics are written locally, redacted (no presentation text, no typed text, no window
  titles unless the user opts in per bundle), and shared only by the user's explicit action.
- **PR-9 Every persisted format is versioned and migratable.** A change to `gorca.toml`, keymaps,
  dictionaries, extension state or label memory bumps a schema version and ships a pure migration
  function plus a fixture of the previous version. Loading never fails startup (ADR-0005 rule extended to
  every file).
- **PR-10 Release gates run authoritatively.** Every gate in §2.1 that is cited by the release checklist
  (M20) is measured on the quiet-host procedure (B-5 SPEC §7: interleaved arms, load and governor
  recorded). A gate that passes only on a quiet host is reported as such, not waived; failing results are
  kept and labelled (CLAUDE.md).
- **PR-11 Two languages from day one.** Every user-testable increment is checked in English and Russian
  (UI language and content language). Checklists carry both columns.
- **PR-12 The user's safety net is part of every feature.** A feature that can leave the user without
  speech (settings, dictionaries, extensions, braille, takeover of the GNOME switch) ships with its
  recovery path: a reset command, a safe mode, or the Orca fallback.

### 2.1 New e2e scenarios (p99 gorca-side, median over interleaved repeats unless stated)

| ID | Stimulus → measured end | Gate (ceiling / target) | Milestone |
|---|---|---|---|
| H1 | injected core panic / `SIGSEGV` to the gorca PID the harness started → first SPEAK of the re-read focus | 2 s / 1 s wall; "gorca restarted" spoken once | M11 |
| H2 | 3 crashes in 60 s → fallback reader (Orca, or a stub in the private session) owns the KeyboardMonitor name | 5 s wall; announced once; no fight over the name afterwards | M11, M17 |
| H3 | Reader actor livelock (test hook) → watchdog restarts the process | detection ≤ 1 s, speech ≤ 2 s wall | M11 |
| H4 | every historical `gorca.toml`/keymap/dictionary fixture → load | 0 startup failures; round-trip keeps comments and unknown keys | M11 |
| H5 | 72 h soak, scripted desktop + terminal + Firefox walk at 1 action/s, L3 storms every 10 min | 0 core restarts, RSS flat (M-8 guard rail 600 MB, slope < 1 MB/h), no gate drift > 1 ms | M11, M20 |
| I1 | K2 and L1h with the Russian catalog and Russian verbalizer | Δp99 ≤ 0.2 ms vs English | M12 |
| I2 | mixed-script corpus (ru/en prose, code, URLs, `lang` attributes) → speech segments | 0 wrong-language segments on the corpus; switch cost ≤ 0.1 ms per segment | M12 |
| D1 | utterance through dictionaries (1k literal + 100 regex rules) | ≤ 50 µs per utterance p99; K2 Δp99 ≤ 0.2 ms | M13 |
| G1 | settings change in the GUI → applied in the daemon + spoken confirmation | 50 ms wall; daemon L1 unchanged while the GUI is frozen (SIGSTOP of the PID we started) | M14 |
| B1 | focus / caret move → BrlAPI write | 15 ms / 5 ms | M10 |
| B2 | routing key → caret or review cursor moved and re-brailled | 15 ms / 5 ms (+ app IPC) | M10 |
| B3 | display unplug, replug, brltty restart (our private brltty) during L1 runs | L1 Δp99 ≤ 0.5 ms; reconnect ≤ 2 s | M10 |
| W4 | elements list open (10k-node page) → first item spoken | 50 ms / 15 ms | M15 |
| W5 | find in page, next match at 100k nodes | 5 ms / 2 ms | M15 |
| W6 | virtual copy of a 10k-character range → clipboard owned | 50 ms wall | M15 |
| V4 | zsh, fish and bash with a wide-character (CJK, emoji, combining) corpus | 0 wrong characters in review; caret echo matches the shell's cursor | M16 |
| V5 | 8 h real gnome-terminal soak (`make -j`, `journalctl -f`, `yes` bursts, tab switches) | 0 server aborts, 0 gorca restarts, V1–V3 at the end within 10 % of the start | M16 |
| N2 | same synthetic app exposed through AT-SPI and the AccessKit test connector | spoken once; L1 unchanged with both connectors loaded | M19 |

All Phase 2 gates (K2–K4, L1h, S1, C1/C2, E1/E2, R1, A1, N1, X1, V1–V3, W1–W3, FC*, `e2e-m8`) stay
mandatory on every merge. Phase 3 adds no new latency path to the keypress → speech budget; anything new
on the hot path (catalog lookup, verbalizer language tables, dictionaries, braille) is gated by
I1/D1/B1 deltas against the existing gates.

## 3. Milestones

### C0 — Phase 2 close-out (not a new feature; starts now)

**Scope.**
1. Merge M8 (NVDA parity P0 rows of `docs/plan/m8-spec.md`) and M9 (Chromium; W-6 answered; Chromium
   `text_insert_slack` row), each through the reviewer.
2. **B-5 judgement** on the full SPEC (interleaved arms, load recorded, repeated rounds) and adopt the
   winning `--topology` as the default; record the decision. Every Phase 3 gate is measured on it.
3. `ext-vte` WASM port (same pattern as `ext-gnome-shell`), with native fallback.
4. ADR-0008 `disabled_quirks` switch (connector quirk rows can be disabled from config).
5. Authoritative low-load reruns of every gate labelled "load-induced" in Phase 2 (M3b/M7b Firefox set,
   M6 final pass, K2s, W2b 3 s warm-up trial), kept beside the loaded results.
6. User checklists not yet run: M6 (30 items), M8, M9.
7. ADR-0001 §17.2 sign-offs still open, and P-7/P-8/P-10 closure or explicit deferral.

**Acceptance.** `docs/decisions.md` records "Phase 2 done" with links; all Phase 2 gates green on the
adopted topology on one authoritative run; the user has run M6/M8/M9 checklists.
**Agents:** orchestrator (merges), perf-engineer (B-5, reruns), wasm-engineer (`ext-vte`),
web-a11y-engineer (M8/M9), reviewer.
**Risk.** C0 drags. Rule: C0 items 5–7 may run in parallel with Wave 1; items 1–2 must merge before
M12a's string sweep (they touch the same presenters).

### M11 — Hardening: supervision, crash recovery, diagnostics, config migration

**User-testable increment.** The user (or the checklist's fault hook) kills gorca: speech resumes within
two seconds with "gorca restarted" and the current focus. After three crashes in a minute gorca says it is
handing over and Orca starts. "gorca, save diagnostics" (gorca+Ctrl+D, provisional) writes a redacted
bundle to `~/gorca-diagnostics-<date>.tar.zst` and says where. Editing `gorca.toml` with an old schema
upgrades it on the next explicit save, with a backup. Extension settings survive a restart.

**Scope.**
- **Supervisor state machine** (ADR-0013, new): `Starting → Running → Degraded{reason} → Restarting →
  CrashLoop → HandedOver`, driven by systemd (`Restart=on-failure`, `RestartSec` lowered to 200 ms,
  `StartLimitBurst=3`/`StartLimitIntervalSec=60`, `OnFailure=gorca-fallback.service`), never by gorca
  killing anything it did not start (CLAUDE.md hard rules). State restore on start: no persisted
  "session"; the first focus re-read is the restore (cheap after B-1).
- **Watchdog**: `Type=notify` + `WatchdogSec=2`; the notify ping is sent from a task that requires a
  round-trip through the Reader, input and speech lanes (a livelocked Reader stops the ping; ADR-0001
  §6.6 becomes real).
- **Panic and crash reports**: panic hook writes a local report (version, build id, backtrace, actor
  counters, last 256 event *kinds* — never text) into `$STATE_DIRECTORY/crash/`, rotated (10 files);
  `systemd-coredump` stays the source of truth for native crashes. Spoken once after restart: "gorca
  restarted after an error; report saved".
- **Diagnostics bundle**: `gorca diag` (CLI) and a command: config (secrets none by construction), keymap,
  versions of GNOME/Mutter/at-spi/Firefox/Chromium/speech server, last N crash reports, journal of the gorca
  unit, counters snapshot, extension list with fault counts. Redaction rules in one module with tests
  (PR-8); the presentation log is excluded unless the user opts in for that bundle.
- **Safe mode**: on the second restart in 60 s gorca starts with extensions disabled and default
  verbosity, and says so; `--safe-mode` and a keyboard command to leave it.
- **Config migration**: `gorca-config` gets `schema_version`, a migration chain of pure functions,
  `toml_edit` round-trip (comments and unknown keys kept), backup `gorca.toml.bak-vN` before any write,
  downgrade tolerance (newer unknown keys preserved and ignored).
- **State directory**: `StateDirectory=gorca` (fixes extension settings persistence under
  `ProtectSystem=strict`), `XDG_STATE_HOME/gorca` outside systemd; layout documented.
- Phase 1 follow-ups R4-2 (group apps by systemd scope cgroup) and R4-3 (async `/proc` reads) if still open.

**Agents:** architect (ADR-0013 supervision, crash and diagnostics), rust-engineer (supervisor, watchdog,
panic hook, migration), linux-platform-engineer (systemd units, coredump, fallback unit), test-engineer
(fault-injection hooks behind a test feature, H1–H4), perf-engineer (H5 soak rig), reviewer.
**Depends on:** C0 item 2 (topology). Independent of M12.
**Bake-off:** none.
**Acceptance.** H1–H4 green; H5 72 h soak run once before M20 (24 h acceptable for the milestone merge);
a fault-injection test per actor (Reader, input lane, speech lane, drain, extension host) shows the
expected supervisor transition; diagnostics redaction test proves no typed text, password or document
text in a bundle produced after a scripted password entry and a say-all.
**Risks.** Restart storms during Shell restarts (Shell restart is not a gorca crash: `Lost` →
`Authorizing` stays in-process); `OnFailure` starting Orca while the user has deliberately stopped gorca
(only failures trigger it, an explicit stop does not — verified in the private session).

### M12 — Localisation and language (ADR-0010)

**User-testable increment.** With GNOME in Russian, gorca speaks every message, role, state, command
confirmation, help text and learn-mode description in Russian; punctuation and symbol names are Russian;
switching GNOME to English and reloading switches gorca. Reading a Firefox page with English and Russian
paragraphs switches voices per paragraph (by `lang` and by script); Russian UI with English code in the
terminal reads the code in the English voice (setting).

**Scope.**
- **M12a seams (first, 2–3 days, like Phase 2 P0):** new pure crate `gorca-i18n`: message ids as a
  generated `enum MsgId` (exhaustive, no stringly-typed lookups on the hot path), catalog loaded at
  startup into an immutable table swapped with `ArcSwap` on reload, `msg!(Id, args)` returning a
  pre-resolved pattern. Mechanical sweep of all literals in `gorca-core`, `gorca-daemon`, extensions'
  `say` calls (WIT gets message ids + arguments, extensions ship their own catalogs). CI check (PR-7).
- Catalog format: **Fluent** (`fluent-bundle`) recommended in ADR-0010 for Russian plural and case
  agreement ("3 из 12", "1 элемент / 2 элемента / 5 элементов"); gettext `.po` export for translators.
  Hot-path formatting is pre-compiled per id at load (I1 gate).
- Role and state names, position phrases, verbalizer tables (punctuation levels, symbols, capitals,
  spelling alphabets) per language; sourced from CLDR annotations like NVDA (`refs/nvda/source/locale/*/symbols.dic`
  pattern) and Orca's Russian translations (`refs/orca/po/ru.po`) as reference, written fresh.
- **Language and voice switching**: utterance segments carry a language tag (`lang` from the web mirror,
  document language, script detection for untagged text: Cyrillic vs Latin runs with a minimum run
  length to avoid flapping on single letters); speech client maps language → voice through the backend
  (SSIP `SET LANGUAGE`/`SET VOICE`; voicego equivalent). Setting: auto-switch on/off, per-language voice.
- UI language from the GNOME locale (`LANG`/`LC_MESSAGES` of the session), overridable in config.
- Keymap descriptions and the user guide's command reference generated from keymap data + catalog.

**Agents:** architect (ADR-0010), rust-engineer (`gorca-i18n`, sweep, verbalizer), web-a11y-engineer
(`lang` propagation in the doc mirror), wasm-engineer (WIT message ids), tech-writer (Russian and English
catalogs; the user reviews Russian), test-engineer (I2 corpus), perf-engineer (I1).
**Depends on:** C0 items 1–3 merged before M12a's sweep (CF-6).
**Bake-off:** none (format choice is an ADR decision; the risk is cost, gated by I1).
**Acceptance.** I1, I2; 0 literal user-facing strings (PR-7 check); 100 % of message ids translated to
Russian in CI; the Russian checklist (≥ 40 items across Shell, Terminal, Firefox) passes with the user.
**Risks.** Russian TTS quality differs by engine (espeak-ng vs RHVoice); voice switching latency in the
speech server is outside gorca's budget — measured and reported (V track).

### M13 — Speech completeness: dictionaries, sound icons, voices

**User-testable increment.** The user adds a pronunciation ("gorca" → "горка") with a command or in the
settings app, it applies at once and survives restart; browse/focus mode switches and errors play short
sounds instead of words (setting); voice, variant, rate per language are selectable; numbers, dates,
emoji and URLs are read sensibly in both languages.

**Scope.**
- Dictionaries (in `gorca-speech` verbalizer): default (shipped), user, per-voice, temporary (session);
  literal rules compiled to an Aho-Corasick automaton, regex rules to a `RegexSet`; applied per language
  segment after the verbalizer; versioned files (PR-9); "add pronunciation for the current word" command.
- Sound icons (earcons) through the speech service (SSIP `SOUND_ICON`; voicego equivalent from track V),
  never played in-process (CLAUDE.md speech rule); fallback to words when the server lacks them.
- Voice inventory and selection API in `SpeechClient` (list voices per language, select, preview).
- Emoji names from CLDR per language; URL and path reading modes.

**Agents:** rust-engineer, go-engineer (voicego side, coordinated with V), tech-writer (default
dictionaries), perf-engineer (D1).
**Depends on:** M12 (segments carry language). **Bake-off:** none.
**Acceptance.** D1; dictionary edits apply ≤ 100 ms wall and persist; sound icons on speech-dispatcher
and voicego, word fallback tested with a server that has none.

### M10 — Braille (ADR-0009; carried over from Phase 2)

**User-testable increment.** With a BrlAPI display (or `brltty -b xw` / a virtual display): focus,
caret, review and browse lines are shown; panning, routing keys and display keys move the caret or review
cursor; braille follows the UI language (Russian and English tables, computer braille in terminals and
fields, contracted English optional); flash messages for notices.

**Scope.** ADR-0009: cell model and region list (pure `gorca-braille` crate: regions per presentation,
cursor shapes, routing map from cell to `(node, offset)`), translation through liblouis (FFI, tables per
language; translation off the Reader lane with a deadline, result applied by epoch), BrlAPI client
(`gorca-braille-brlapi`, today a 35-line stub) with deadlines and reconnect state machine
(`Disconnected → Connecting → Ready{cols,rows} → Lost`), output scheduling that shares epochs with speech
but never waits on it, braille key commands merged into the ADR-0005 keymap (per-display-driver bindings
as data), braille-only verbosity (abbreviated roles from the catalog, M12).
**Agents:** architect (ADR-0009), c-cpp-engineer (liblouis and libbrlapi FFI or a pure-Rust BrlAPI
protocol client, decided in the ADR by a short spike, not a bake-off), rust-engineer, test-engineer
(virtual display via brltty's `xw`/`ba` driver in the private session; key injection), perf-engineer (B1–B3).
**Depends on:** M12a (abbreviations from the catalog); the backend and cell model have no dependency and
start in Wave 1. **Bake-off:** none.
**Acceptance.** B1–B3; review and browse cursor routing work in Text Editor, Terminal and Firefox; a
braille checklist passes with a real display (§9 Q4) or, failing hardware, with a tester who has one in M20.
**Risks.** No display on the dev host → virtual-only validation until the alpha; liblouis table names and
quality for Russian (`ru-litbrl`, `ru-compbrl`) to be verified; brltty owning the keyboard for some
drivers.

### M14 — Control API and settings app (ADR-0011)

**User-testable increment.** "gorca, open settings" (and the GNOME app grid entry "gorca Settings") opens
an accessible GTK4/libadwaita window: speech (backend, voice per language, rate, pitch, volume,
punctuation, capitals, echo), verbosity, keyboard (modifier, preset NVDA/Orca, laptop layout), braille,
dictionaries, extensions (enable, settings), language, diagnostics and reset. Changes apply live and are
spoken; "Save" writes the file; "Revert" restores. The window reads perfectly with gorca itself.

**Scope.**
- **Control API** `org.gorca.Control1` on the session bus, owned by the daemon: typed settings get/set with
  validation, schema introspection (the GUI is generated from the schema, not hand-wired per setting),
  dictionary CRUD, extension list/enable/settings, voice inventory, diagnostics trigger, `Reload`,
  `Save`, `Revert`. The daemon is the only writer of config files (keeps ADR-0005 explicit-save and
  hand-edit rules; no two writers). Every method handled off the hot lanes with deadlines; a slow or
  frozen client cannot affect reading (G1). Access: same-user only (bus policy) and peer credentials
  checked like `org.gorca.ShellHelper` (pidfd, exe) for mutating calls.
- **Settings app** `gorca-settings` (gtk4-rs + libadwaita, separate process and package), opened via
  `xdg-activation` token passed from the command so Wayland focus works (spike P-13 first).
- Extension settings declared in the extension manifest as a schema (feeds M18).
**Agents:** architect (ADR-0011), rust-engineer (Control API in the daemon), a GTK-capable engineer
(rust-engineer or python-engineer for a prototype; production is Rust), linux-platform-engineer (P-13
activation, bus policy), test-engineer (GUI driven over AT-SPI in the private session — gorca reading
its own settings is the e2e test), perf-engineer (G1), tech-writer (labels in both languages).
**Depends on:** M11 (migration, state dir), M12a (catalog for labels). **Bake-off:** none: GTK4 is the
platform toolkit with the best AT-SPI exposure on GNOME; a web UI or a TUI would be a regression in
accessibility, not a contested choice.
**Acceptance.** G1; every setting in the schema is reachable and labelled (CI test walks the GUI's AT-SPI
tree: no unnamed control, no control without a mnemonic or keyboard path); a settings checklist in both
languages passes with the user.
**Risks.** Wayland focus-stealing prevention when the window is opened from a background daemon (P-13);
GTK4 a11y gaps found by our own reader become upstream bugs (track U).

### M15 — Browse-mode completeness: chooser, elements list, find, copy

**User-testable increment.** In Firefox and Chrome: gorca+F7 opens an elements list (headings, links,
landmarks, form fields, buttons, tables; type to filter, Enter to move or activate), gorca+Ctrl+F finds in
the page (next/previous with gorca+F3), Shift+arrows select in browse mode and Ctrl+C copies the selected
text, "read clipboard" speaks the clipboard, the M8 parity P1/P2 rows that remain.

**Scope.**
- **`Mode::Chooser`**: a spoken (and brailled) in-reader list with type-ahead filter, owned by the core,
  zero IPC, used by the elements list, find history, extension menus, the tutorial and "choose voice". It
  avoids opening a window from a background process on Wayland and is instant; the settings app (M14) is
  the only GUI. Defined as an amendment to ADR-0005 (modes) and `docs/design/modes.md`; the decision
  record lists the alternative (GTK dialog as in NVDA/Orca) and why it loses on Wayland.
- Elements list and find over the existing doc-mirror indexes (`Cat::*`, flat text); find is a flat-text
  search with incremental results (W5).
- Copy (D-3 virtual selection) and clipboard read: clipboard access from the daemon. GNOME 48 Mutter has no
  data-control protocol for a background client; options are (a) the existing `org.gorca.ShellHelper`
  Shell extension (St.Clipboard) with the same pidfd allow-list, (b) native selection mode only. Spike
  P-14 picks; (a) recommended.
- Remaining `e2e-m8` P1/P2 rows; the `m8_baseline.rs` expected-fail table must shrink to the rows tied to
  documented browser bugs.

**Agents:** web-a11y-engineer (lead), rust-engineer (chooser mode), linux-platform-engineer (P-14),
test-engineer, perf-engineer (W4–W6), reviewer.
**Depends on:** C0 (M8, M9 merged), M12a. **Bake-off:** none.
**Acceptance.** W4–W6; `e2e-m8` 0 GAP at P0/P1; chooser checklist in both languages.

### M16 — Terminal completeness

**User-testable increment.** zsh (with oh-my-zsh-style prompts and right prompts), fish (autosuggestions
are not read as typed text), bash; CJK, emoji and combining characters are reviewed and echoed correctly;
`tmux` and `screen` panes, `vim`/`htop`/`mc` usable with review and with the "read changes" heuristic
tuned; an 8 h session is stable.

**Scope.** `ext-vte` rules for zsh/fish prompt and suggestion classification (OSC 133 prompt marks
documented as the exact path; heuristics as fallback), wide-character cell/scalar mapping in terminal
review (`LineSource::Terminal`), V4/V5 gates, `tmux` status-line suppression rules as data.
**Agents:** rust-engineer, wasm-engineer (`ext-vte`), linux-platform-engineer (real VTE probes),
test-engineer (V4 corpus), perf-engineer (V5).
**Depends on:** C0 item 3 (`ext-vte` in WASM). **Bake-off:** none.
**Acceptance.** V4, V5; the remaining items of the M6 30-item checklist plus 15 shell/TUI items pass with
the user.

### M17 — Distribution, system integration and onboarding (ADR-0012)

**User-testable increment.** On a fresh Debian 13 GNOME VM: `apt install gorca` installs everything;
the GNOME screen-reader switch (Super+Alt+S and Settings → Accessibility → Screen Reader) starts gorca
instead of Orca; the first start runs a spoken tutorial (choose language, keyboard preset, modifier
key, speech rate; learn the five essential commands with practice); "gorca, help" opens the user guide;
uninstalling restores Orca as the GNOME screen reader. When gorca crash-loops, Orca starts.

**Scope.**
- **Packaging**: Debian source package (debhelper; `cargo-deb` only for dev snapshots), binary packages
  `gorca` (daemon, extensions in `/usr/lib/gorca/extensions`, user unit in `/usr/lib/systemd/user`),
  `gorca-settings`, `gorca-doc`; dependencies at-spi2-core, rtkit (Recommends), speech-dispatcher or
  voicego (one of, §9 Q6), brltty and liblouis (Recommends); reproducible build; lintian and piuparts
  clean; signed apt repository for the alpha (§9 Q1). Paths come from build-time config, never `<repo>`.
- **Takeover of the GNOME switch** (P-15 verifies on GNOME 48): gorca's autostart entry uses the same
  `AutostartCondition=GSettings org.gnome.desktop.a11y.applications screen-reader-enabled` as Orca's;
  Orca's entry is masked per user (`~/.config/autostart/orca-autostart.desktop` with `Hidden=true`),
  reversible by a command and by uninstall. gorca still never *writes* `screen-reader-enabled` (decision
  2026-10-14). Identity (R1, `comm=orca`, `org.gnome.Orca.KeyboardMonitor`) is unchanged.
- **Orca fallback**: `gorca-fallback.service` (`OnFailure=` of M11) starts Orca after a crash loop and
  unmasks nothing permanently; gorca announces the handover; the user returns with Super+Alt+G or the
  switch.
- **Migration from Orca**: import of rate, pitch, voice, punctuation, echo and keyboard layout from
  `~/.local/share/orca/user-settings.conf` (read-only, one-shot, reported in speech).
- **Onboarding**: first-run state machine (`NotStarted → Language → Keyboard → Speech → Practice → Done`,
  resumable, skippable with one key), built on `Mode::Chooser` (M15) and the catalog (M12); a practice
  mode that confirms each essential command; "what's new" after an upgrade.
- **User guide** in English and Russian (accessible HTML in `gorca-doc`, opened in the default browser;
  command reference generated from keymap data), plus a quick-reference card.
**Agents:** architect (ADR-0012), linux-platform-engineer (packaging, autostart, P-15, fallback),
tech-writer (guide, tutorial script), rust-engineer (first-run SM, Orca import), test-engineer
(fresh-VM install test, piuparts), reviewer.
**Depends on:** M11 (supervisor, fallback), M12 (catalogs), M14 (settings app is packaged), M15
(chooser for the tutorial). Packaging skeleton and P-15 start in Wave 1.
**Bake-off:** none (system integration is fixed by GNOME's mechanisms; P-15 is a verification, not a
competition).
**Acceptance.** Install → speech in ≤ 10 s on a fresh VM with no manual step; switch on/off 20 times
without Orca and gorca fighting (0 double speech, 0 orphan processes); uninstall leaves Orca working;
tutorial completed by the user in Russian and in English; piuparts and lintian clean.
**Risks.** GNOME 48 Shell may start Orca by a path other than the autostart condition (P-15 must read
`refs/gnome-shell` and gnome-session); per-user masking does not cover other users (fine for alpha).

### M18 — Extension SDK preview

**User-testable increment.** A developer follows the guide, generates an extension from a template
(`cargo xtask new-ext`), builds a WASM component, drops it in `~/.local/share/gorca/extensions`, and gorca
loads it with its manifest, permissions and settings (visible in the settings app); a faulty extension is
disabled with one announcement and listed in diagnostics.

**Scope.** WIT 0.2 → **0.9 "preview"** with a written compatibility policy (additive within a minor,
deprecation window), the 0.2 hooks deferred by ADR-0008 Amendment 1 that M15 needs, extension manifest
with declared capabilities (keys, settings schema, message catalogs, app matchers), user extension
directory and precedence over system ones, per-extension state in the state dir (M11), SDK crate
(`gorca-ext-sdk`, MIT/Apache per §9 Q2), template, guide, examples (ext-gnome-shell as the reference).
No signing or store in Phase 3 (Phase 4).
**Agents:** wasm-engineer (lead), architect (ADR-0015 extension API stability), tech-writer, reviewer
(security review of the manifest/capability model).
**Depends on:** M11 (state dir), M12 (catalogs in extensions), M14 (settings schema).
**Bake-off:** none. **Acceptance.** X1 unchanged; a third-party example built outside the repo loads;
fuzzed manifests never crash the host; capability denial tested.

### M19 — Second connector and Newton readiness (ADR-0014)

**User-testable increment.** None for the user (a developer feature): a test app exposed through both
AT-SPI and an AccessKit-tree connector is read once; the connector can be loaded beside AT-SPI from config.

**Scope.** `gorca-connector-accesskit`: consumes AccessKit `TreeUpdate`s (current AccessKit, not Newton's
pinned 0.16 JSON) from an in-process test source and from a socket fed by a test producer; identity
mapping per ADR-0001 N-3 (cursors anchored to container + offset); deduplication policy for an app
exposed by two connectors (N-4: decided per surface/app identity when available, otherwise "prefer
AT-SPI" by config); `cargo xtask deps` proves the core imports no connector types. Live Newton against
`refs/newton` Mutter branch is **not** in scope (stale since mid-2024, no security, N-1…N-2).
**Agents:** architect (ADR-0014), rust-engineer, test-engineer (N2).
**Depends on:** C0 only. Lowest priority of Phase 3; may slip to Phase 4 without blocking the release.
**Bake-off:** none.

### M20 — Release candidate and closed alpha

**Scope.** Release checklist (all §2.1 gates and Phase 2 gates on one authoritative run on the RC build,
PR-10), H5 72 h soak, versioning and changelog, alpha cohort onboarding (§9 Q1), a feedback channel that is
accessible (plain e-mail or a Git forge issue template; diagnostics bundle attached by the user), weekly
RC builds during the alpha, triage rules (crash and silence bugs block, polish bugs queue).
**Agents:** orchestrator, perf-engineer, test-engineer, tech-writer, reviewer (release review).
**Depends on:** M10, M11, M12, M13, M14, M15, M16, M17 (M18, M19, MP do not block).
**Acceptance (Phase 3 exit).**
- RG-1 every gate green on the RC, authoritative run, results kept.
- RG-2 H5 72 h soak: 0 core restarts; the developer's own 14-day daily use on the RC with 0 unrecovered
  silences (crash reports reviewed).
- RG-3 fresh-VM install, switch, tutorial, uninstall pass (M17).
- RG-4 Russian and English: 0 untranslated ids; both checklists pass.
- RG-5 at least three blind alpha testers other than the developer complete the onboarding and a
  one-week use without a blocking bug open longer than one RC cycle.
- RG-6 licence chosen and applied to every crate and file header (§9 Q2); third-party licence audit
  (`cargo deny`) clean.

### Track V — voicego (separate repository, parallel)

voicego is the user's AGPL-3.0 Go server (`refs/voicego`); gorca drives it over a process boundary only.
V0 (measure first): end-to-end stop → silence and speak → first audio for SSIP-framed voicego vs
speech-dispatcher on the real PipeWire path, under the L3 storm. V1: merge the P0 stability fixes
(`fix/p0-stability`), panic containment per session, PipeWire sink, sound icons, index marks, voice
inventory per language (needed by M12/M13). V2: the native protocol only if bake-off B-7 (§4) says so.
**Agents:** go-engineer (lead), rust-engineer (`gorca-speech-voicego`), perf-engineer.
**Acceptance for "voicego is the release default"** (§9 Q6): V0 numbers no worse than speech-dispatcher
on every metric, a 72 h soak with 0 server crashes, Russian voices available, sound icons supported.

### Track U — Upstreaming (continuous, never blocking)

Mutter 48.7 stuck grabbed modifier (reproduction from `docs/reports/live-keys-swallowed.md`, patch
offered); at-spi2-core `RegisterEvent` per-app argument (P-11 source finding) and any registry issue
found in M11 soaks; Chromium AT detection without `comm=orca` (path to dropping impersonation, R1);
GTK4 a11y bugs found by M14. Owner linux-platform-engineer; the measure is "filed with a minimal
reproduction and tracked in `docs/upstream.md`", not "merged".

### Stretch MP — Perception layer phases 1–2 (ADR-0002 §13), behind a flag

Semantic find and label memory without pixels ("label this control", encrypted persistence, forget
commands), only if Wave 3 has free capacity. Never blocks M20. Pixels/OCR and MapSurface stay Phase 4.

## 4. Bake-off schedule

Phase 3 is mostly product integration whose choices are fixed by the platform (GTK4 for GNOME settings,
liblouis/BrlAPI for braille, GNOME autostart for takeover) or are cheap to reverse (catalog format behind
`gorca-i18n`). Those are settled by ADRs with short spikes. One decision is genuinely contested and
measurable:

| Bake-off | Starts when | Decision | Judge on |
|---|---|---|---|
| **B-7 speech transport (conditional)** | after V0, **only if** V0 shows SSIP-framed transport costing > 2 ms p99 on stop → silence or speak → first audio vs the server's internal floor | (a) SSIP over a unix socket, tuned (pipelining, no round trip per `SPEAK`, `STOP` on a priority path) vs (b) a voicego native binary protocol (cancel tokens, index marks, per-utterance ids) | end-to-end stop → silence p99 and speak → first audio p99 (PipeWire, real server) idle and under L3-50k; gorca-side K2/L2 Δ; robustness under server kill (H-series); code size and maintenance in both repos |

Decision rule for B-7: (b) wins only with ≥ 30 % lower p99 on both end-to-end metrics and no regression
on robustness; otherwise (a) stays, and the SSIP compatibility path remains the only protocol to
maintain. Rejected bake-off candidates: Fluent vs gettext (reversible behind `gorca-i18n`; I1 gates the
cost), GTK vs other GUI (not contested on GNOME), liblouis vs own translator (no credible alternative),
virtual chooser vs GTK dialog (decided by Wayland focus rules; P-13 spike result attached to the ADR).

## 5. Order and the recommendation

```
C0 (M8, M9, B-5, ext-vte) ──┬─► M12a seams ──┬─► M12 ──► M13 ─────────────┐
                            │                ├─► M15 (chooser, list, find) ┤
                            │                ├─► M10 integration ──────────┤
                            ├─► M11 ─────────┼─► M14 ──► M18              ├─► M17 ──► M20 (RC, alpha)
                            ├─► M16 ─────────┘                            │
                            ├─► M10 backend + ADR-0009 (no deps) ─────────┘
                            └─► M19 (any time, lowest priority)
Track V (V0 → V1 → B-7?)  and Track U run alongside everything.
M17 packaging skeleton + P-15 start in Wave 1; MP only if Wave 3 has capacity.
```

**Start first (Wave 1, as soon as C0 items 1–2 merge): M11 and M12a together, plus the M10 backend,
the ADR-0009…0013 drafts, P-13/P-14/P-15 spikes, the M17 packaging skeleton and V0.**
Rationale: M12a is a mechanical sweep across every presenter, so it must land before anyone adds new
strings (same reason as Phase 2's P0 seams); M11 makes every later milestone's failures recoverable and
gives the alpha its diagnostics; both touch disjoint files (CF-7). The braille backend and V0 have no
dependency on anything.

Wave 2: M12 (rest), M15, M16, M14, M10 integration, M13. Wave 3: M17, M18, M19, then M20.

## 6. Parallelism and file ownership

| Area (crate / path) | C0 | M10 | M11 | M12 | M13 | M14 | M15 | M16 | M17 | M18 | M19 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `gorca-i18n` (new), catalogs `i18n/` | | r | | **W** | w | w | w | w | w | w | |
| `gorca-core/present/`, `commands/` | W (M8/M9) | w | w (notices) | **W** (sweep, M12a) | | w | w | w | w | | |
| `gorca-core/browse/`, `doc_actor/` | **W** | w | | w (lang) | | | **W** | | | | |
| `gorca-core` chooser mode (`chooser/`, new) | | | | | | | **W** | | w (tutorial) | w | |
| `gorca-core/terminal.rs`, `extensions/ext-vte` | **W** (port) | | | w | | | | **W** | | | |
| `gorca-speech` (verbalizer, dictionaries, segments) | | | | **W** (lang tables) | **W** (dict, icons) | | | | | | |
| `gorca-speech-voicego`, `-ssip` | | | | w (voice switch) | w | | | | | | |
| `gorca-braille` (new), `gorca-braille-brlapi`, `gorca-output-api` | | **W** | | r | | | | | | | |
| `gorca-config` | | w (schema) | **W** (migration) | w | w | w | | | w (Orca import) | w | |
| `gorca-daemon` (supervisor, watchdog, Control API) | w | w | **W** | w | w | **W** (control) | w | w | w | w | w |
| `gorca-settings` (new app) | | | | | | **W** | | | | | |
| `gorca-ext-api`, `gorca-ext-host`, WIT | W (ext-vte) | | w (state dir) | w (msg ids) | | w (schema) | w (0.2 hooks) | | | **W** | |
| `gorca-connector-accesskit` (new) | | | | | | | | | | | **W** |
| `contrib/systemd`, `debian/`, autostart | | | **W** (units) | | | | | | **W** | | |
| `contrib/gnome-shell-extension` (ShellHelper) | | | | | | | w (clipboard) | | | | |
| `bench/e2e`, `crates/gorca-bench` | w | w | w | w | w | w | w | w | w | w | w |

W = owns, w = small additive edits. Conflict rules (Phase 2 CF-1…CF-5 retired with Phase 2):

- **CF-6** C0 items 1–3 (M8, M9, `ext-vte`) merge before M12a's sweep starts; the sweep is one PR merged
  quickly, and every open branch rebases on it immediately.
- **CF-7** M11 owns `gorca-daemon` supervision/watchdog and `contrib/systemd`; M14 owns the Control API
  module in the daemon; both add one `wire_*` function each (Phase 2 convention).
- **CF-8** `gorca-config` schema changes are serialised: one schema version bump per merge, migration and
  fixture in the same PR (PR-9); the orchestrator merges in arrival order.
- **CF-9** Catalog files are append-mostly; ids are never renamed in Phase 3 (deprecated ids stay until
  Phase 4) so translations never conflict.
- Safe parallel sets: {M11, M12a, M10 backend, V0, P-13/14/15, M17 skeleton}; {M12, M15, M16, M10
  integration, M14}; {M13, M14, M18, M19}; {M17, M18, M19, MP}.

## 7. New documents this plan creates

| Document | Owner | Needed by |
|---|---|---|
| ADR-0009 braille | architect | M10 |
| ADR-0010 localisation, message catalog and language switching | architect | M12a |
| ADR-0011 control API and settings app | architect | M14 |
| ADR-0012 distribution, GNOME integration, Orca coexistence and onboarding | architect | M17 |
| ADR-0013 supervision, crash recovery, diagnostics and config migration | architect | M11 |
| ADR-0014 second connector and Newton readiness | architect | M19 |
| ADR-0015 extension API stability (WIT preview policy) | architect + wasm-engineer | M18 |
| ADR-0005 amendment: `Mode::Chooser`; `docs/design/modes.md` update | architect | M15 |
| `docs/bakeoff/b7-speech-transport/SPEC.md` (only if triggered) | architect | Track V |
| `docs/upstream.md` (filed upstream issues) | linux-platform-engineer | Track U |
| `docs/user-guide/{en,ru}/` | tech-writer | M17 |
| `docs/plan/checklists/M10.md` … `M20.md` (two language columns) | tech-writer | each milestone |

ADR numbers are the next free ones on 2026-10-16; the orchestrator renumbers if any is taken first.

## 8. Cross-cutting risks

- **Scope.** Phase 3 has more breadth than depth. Mitigation: M19 and MP are explicitly droppable; M18 is
  a preview; the release (M20) depends only on M10–M17.
- **Single dev host and load noise.** Unchanged from Phase 2; PR-10 and labelled results. The 72 h soak
  needs a host window without agent benches (orchestrator schedules it).
- **No braille hardware and no alpha testers yet.** Braille is validated virtually until §9 Q1/Q4 are
  answered; RG-5 depends on recruiting testers early (start in Wave 1, not at M20).
- **GNOME integration assumptions** (autostart condition, activation tokens, clipboard) are unverified;
  P-13/P-14/P-15 are the first tasks of their milestones and can change M14/M15/M17 scope.
- **voicego is cross-repo and AGPL.** gorca only talks to it over a socket; release default follows §9 Q6.
- **Hot-path regressions from breadth.** Catalog, language segments, dictionaries and braille all touch
  the presentation path; I1/D1/B1 deltas are gated on every merge, not only at M20.
- **Takeover safety.** A broken gorca with Orca masked would leave a blind user without speech; the
  fallback (H2) and the unmask command are acceptance items of M11/M17, not polish.

## 9. Questions for the user

| # | Question | Options | Recommendation (default if unanswered) |
|---|---|---|---|
| Q1 | Who is the Phase 3 release for, and how is it distributed? | (a) closed alpha: 3–10 blind testers you know, Debian 13 GNOME, signed apt repo; (b) public beta; (c) only you | **(a)**, recruited in Wave 1 so they are ready at M20. Public beta and Debian upstream inclusion are Phase 4. |
| Q2 | Licence (still `TBD` since 2026-10-12; required before anyone else gets a build) | GPL-3.0-or-later / LGPL-2.1-or-later (like Orca) / MPL-2.0 / MIT OR Apache-2.0 | **GPL-3.0-or-later for gorca, MIT OR Apache-2.0 for the WIT files and `gorca-ext-sdk`** so third-party extensions may use any licence. voicego stays AGPL-3.0; it is a separate process, so no conflict. Medium confidence: a legal preference of yours overrides. |
| Q3 | How should gorca become "the" screen reader? | (a) keep its own toggle (Super+Alt+G), Orca stays GNOME's reader; (b) take over GNOME's screen-reader switch, Orca masked per user and started automatically if gorca crash-loops; (c) replace the `orca` binary system-wide | **(b)**. Reversible, per user, no root, and the switch blind users already know works. The GDM login screen keeps Orca in Phase 3. |
| Q4 | Braille: do you or your testers use a display, which model, and which tables (Russian literary, computer braille, English grade 2)? | in Phase 3 / Phase 4 | **In Phase 3** (M10), Russian and English computer braille plus literary tables via liblouis; validated on a virtual display until a real one is available. |
| Q5 | Languages and voice switching | (a) English + Russian, automatic voice switching by `lang` and by script, on; (b) same, switching off by default; (c) more languages | **(a)**. Catalogs for other languages are accepted later from translators; nothing in the design is two-language specific. |
| Q6 | Speech for the release | (a) voicego track in Phase 3; release default = voicego only after it passes the Track V gates, otherwise speech-dispatcher; (b) voicego required for the release; (c) voicego after Phase 3 | **(a)**. `--speech=auto` already prefers voicego when healthy; the package depends on one of the two. |

## 10. Phase 4+ horizon

- **More applications**: LibreOffice (Writer, Calc), Thunderbird (`ext-thunderbird`, ADR-0008 W2), Electron
  apps (VS Code, Element), GTK/libadwaita app-specific polish driven by alpha feedback.
- **Login and system**: GDM greeter and first boot, Debian installer handoff, multi-user Orca masking.
- **Perception**: ADR-0002 phases 3–4 (pixels via `org.gorca.ShellHelper`, OCR eng+rus, MapSurface),
  local image descriptions.
- **Newton live**: a real Wayland-native connector once Newton/Mutter upstream has an access-controlled
  channel and a current AccessKit wire format (N-1, N-2); dual exposure in production.
- **Public release**: public beta, Debian/Ubuntu archive inclusion, extension signing and a catalog,
  more UI languages, braille input (contracted typing), dropping Orca impersonation as upstream fixes land.
- **Beyond GNOME**: only if the core-as-API holds (no core change needed for another compositor's input
  and identity seams); no KDE-specific design until then.
