# Phase 2 plan: from a focus reader to a daily-driver screen reader

Status: **Proposed** (2026-10-13). Owner: architect. Decisions taken from this plan are recorded by the
orchestrator in `docs/decisions.md`; the user questions in §8 block the milestones named there.

Inputs: `CLAUDE.md`; ADR-0001 (core architecture, §7 state machines, §10 extensions, §14 metrics,
§15 bake-offs, §16.3 open questions); ADR-0002 (perception layer, §13 phased rollout); ADR-0003
(web document mirror, §11 browse mode, §12 structural navigation, §14 budgets, §16 B-6); ADR-0004
(ingress); `docs/reports/2026-10-13-rust-engineer-phase1-slice.md`;
`docs/reports/2026-10-13-phase1-slice-review.md`; `docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`;
`docs/platform/vte-terminal-accessibility.md`; `bench/e2e/README.md`.

## 1. Starting point

Phase 1 delivered: AT-SPI connector over the ADR-0004 drain and ingress, a focus Reader (name, role,
states), speech through voicego with SSIP failover, the daemon, and the e2e harness. Measured
gorca-side p99: L1 focus→SPEAK 2.0 ms, L2 stale cancel 0.9 ms, L3 under 50k ev/s 3.1 ms, `lost = 0`.

Not in Phase 1, and therefore in scope here:

| Gap | ADR anchor | Milestone |
|---|---|---|
| gorca modifier, command keys, grab-set SM | ADR-0001 §7.3 (K-1…K-7) | M1 |
| Settings and keymap as data | ADR-0001 §11 (`gorca-daemon`: config) | M1 |
| "Where am I", window title, repeat, speech controls | — (new, ADR-0005) | M1 |
| Mirror caching, invalidation, P2P (every focus refetches today) | ADR-0001 §5.2, §5.4, D-7; B-1 | M2 |
| Caret/text echo, typing echo, selection | ADR-0001 §4.3 (TU), ADR-0006 (new) | M3 |
| Flat review and object navigation | ADR-0001 §7.2 `Mode::Review` | M4 |
| Window activation, dialogs, notifications, Shell polish | ADR-0001 §10 `ext-gnome-shell` | M4 |
| Extension host (wasmtime) | ADR-0001 §10; B-4 | M5 |
| Terminal flawless | ADR-0001 §5.6 T-1…T-4, D-5 | M6 |
| Firefox browse mode and navigation | ADR-0003; B-6 | M7, M8 |
| Chromium | ADR-0003 WF-4, W-6 | M9 |
| Braille | ADR-0001 §11 (stub until braille ADR) | M10 (parallel track) |

Alt+Tab switcher speech and the key-echo policy are being fixed separately; this plan assumes they
land before M1 merges (both touch the input lane, see §6).

## 2. Rules that apply to every milestone

- **PR-1 User-testable.** Each milestone ends with a short live checklist the user runs on the real
  GNOME 48 desktop (`tools/gorca-toggle.sh`, Super+Alt+G). A milestone is not done until the user has
  run it. The checklist lives in `docs/plan/checklists/Mn.md` (written by tech-writer with the
  milestone's engineer).
- **PR-2 Latency gates are e2e scenarios.** Every new user-facing path gets an e2e scenario in
  `bench/e2e` (perf-engineer), gated on the median-of-interleaved-repeats rule already adopted. All gates
  are gorca-side unless stated. Every scenario also runs under the L3-50k storm and must keep
  `lost = 0` and `stale = 0`.
- **PR-3 Gates tighten to the targets already recorded** (`docs/decisions.md`, 2026-10-13): L1 ≤ 5 ms,
  L2 ≤ 2 ms from M2 on; the CLAUDE.md 15 ms remains the hard ceiling for anything that needs IPC.
- **PR-4 State machines first.** Every milestone that adds a state machine lands its `enum`, transition
  table and pure `step()` tests before wiring (ADR-0001 §7).
- **PR-5 Review.** Each milestone merge goes through the reviewer (adversarial review, same flow as
  Phase 1). Bake-offs use `/bakeoff`.
- **PR-6 No core special cases.** App-specific behaviour goes to `ext-*` behind the `gorca-ext-api`
  trait from day one, even before the WASM host exists (M5 moves it into WASM or keeps it native,
  per B-4).

### 2.1 New e2e scenarios (all p99, gorca-side, median over repeats)

| ID | Stimulus → measured end | Gate (ceiling / target) | Milestone |
|---|---|---|---|
| K2 | command chord `KeyEvent` → SPEAK, zero-IPC command (repeat, time, speech rate) | 5 ms / 2 ms | M1 |
| K3 | "where am I" → SPEAK, fake app replying in 1 ms, cache cold | 15 ms / 5 ms | M1 |
| K4 | 10k randomized mode/grab-set switches during synthetic typing (ADR-0001 M-16) | 0 lost, 0 doubled keys | M1 |
| L1h | focus → SPEAK, mirror hit | 2 ms / 1 ms | M2 |
| S1 | state change on focused node (checkbox toggled) → SPEAK | 5 ms / 2 ms | M2 |
| C1 | arrow key + `text-caret-moved` → SPEAK (char/word/line), one text fetch | 15 ms / 5 ms | M3 |
| C2 | held arrow (30 Hz autorepeat, 3 s) | 0 stale utterances, last position always spoken | M3 |
| E1 | printable `KeyEvent` → SPEAK of the typed character (typing echo) | 3 ms / 1 ms | M3 |
| R1 | review/object-nav step → SPEAK, mirror hit | 3 ms / 1 ms | M4 |
| A1 | window activation → SPEAK of title + focus | 15 ms / 5 ms | M4 |
| X1 | extension `present` hook overhead (ADR-0001 M-7) | 100 µs; trap recovery < 50 ms | M5 |
| V1 | last VTE output frame → SPEAK (after the T-1 settle) | settle ≤ 30 ms wall + 5 ms gorca | M6 |
| V2 | `cat` of 1 GB / `yes` in the focused terminal for 10 min (ADR-0001 M-11) | no broker disconnect, SIOCINQ p99 < 1 MiB, drain < 60 % core | M6 |
| V3 | Ctrl+C during V2 → silence | ≤ 2 ms (L2 path) and next prompt spoken | M6 |
| W1 | quick-nav key → SPEAK (ADR-0003 §14, SN-5) | 5 ms / 2 ms | M7/M8 |
| W2 | 10k-node page load-complete → first line speakable (ADR-0001 M-9) | 150 ms | M7 |
| W3 | tab switch → title + cursor line (ADR-0003 wake W0) | 15 ms | M7 |

## 3. Milestones

### P0 — Prep track (not user-facing; days, starts now, in parallel with M1/M2)

**Scope.**
1. *Core seams* (rust-engineer, first, ~1–2 days): split `crates/gorca-core/src/reader.rs` and
   `presenter.rs` into a command dispatcher (`commands/`), a presenter registry (`present/` keyed by
   role/context), and empty `review/`, `text/` modules, each with its own message enum arm. This is what
   lets M1–M4 run in parallel without editing the same file (§6).
2. Phase 1 follow-ups: path-table RSS (shrink on clear, drop empty buckets, RSS test), name-slot
   reservation in `resolve`, exempt each owner's newest path from eviction.
3. **B-5 runtime topology A/B** (perf-engineer): config switch over the same code, arms (a) one tokio
   multi-thread runtime, (b) dedicated hot lanes + workers + rayon, (c) (b) + rtkit `SCHED_FIFO` and
   pinning. Judged on L1/L2/K1 while L3-50k runs **and** all cores are saturated (synthetic M-15 load);
   ADR-0001 M-14. Needs P-12 (rtkit limits on Debian 13 RT kernel; linux-platform-engineer).
4. **MB-1** mirror store microbenchmark (`papaya` vs `scc` vs `left-right` vs sharded `ArcSwap`),
   input to M2.
5. e2e harness skeletons for K2–K4, S1, C1, E1 (test-engineer + perf-engineer), so each milestone
   starts with a red gate.

**Decision settled:** B-5 (threading layout) and MS-3 (mirror store implementation).
**Acceptance:** seams merged with no behaviour change (Phase 1 e2e numbers within noise); B-5
judgement report; MB-1 report; follow-up RSS test passes.
**Risks:** host load noise (~10) makes B-5 inconclusive → authoritative runs on a quiet host with the
performance governor and pinned cores, as already decided for T7.

### M1 — Command layer: gorca modifier, keymap, "where am I", settings v0

**User-testable increment.** The user presses the gorca modifier plus keys anywhere on the desktop:
where am I (basic; double press = detailed), read window title, repeat last utterance, stop/pause,
speech rate/pitch/volume up/down, toggle key echo/verbosity, time and date, learn mode (InputHelp:
keys described, not executed), gorca settings reload. Double-tapping the modifier passes it to the
app (Mutter's `is_a11y_modifier_first_click` rule). With Orca running, gorca announces once that
keyboard commands are unavailable (ID-1) and keeps reading focus.

**Scope.**
- `MonitorState` + `GrabSetState` state machines and key classification exactly as ADR-0001 §7.3
  (`Command`/`PassToApp`/`Ambiguous`/`Injected`), `SetKeyGrabs(modifiers, keystrokes)` with the ×4
  NumLock/CapsLock mask variants (K-6), 5 ms settle (P-7 measures whether that is enough).
- Command registry in `gorca-core/commands/`: `CmdId` enum, context (`Mode`), keymap lookup O(1) on the
  input lane; command execution is a Reader message, results are speech intents (R3).
- Keymap as data (TOML), with presets (§8 Q1/Q2). Extension keymaps merge later (M5).
- Settings v0: new crate `gorca-config` (pure, no tokio): `~/.config/gorca/gorca.toml`, schema with
  versioning, defaults, validation that never fails startup (bad values are reported once and
  replaced by defaults), hot reload (inotify on the daemon, swap via `ArcSwap`), and spoken
  confirmation. No GUI in Phase 2.
- "Where am I" v1 uses the focus ancestry already fetched (one Interactive fetch on a miss): app,
  window, container chain (dialog, list "3 of 12", tab "2 of 5"), role, name, state, shortcut,
  description. Detailed variant adds description and position info.
- `docs/design/modes.md` (open since ADR-0001 §7.2): the mode table and full keymap, written in this
  milestone.

**Agents:** architect (ADR-0005 commands, keymap and settings; `docs/design/modes.md`),
linux-platform-engineer (`gorca-input-mutter` grabs; P-7, P-10, P-8 measurements),
rust-engineer (command registry, Reader wiring, `gorca-config`), test-engineer (fake Mutter grab
semantics in `tests/harness/fake_mutter.py`: modifier double-tap, exact-mask matching), perf-engineer
(K2–K4), reviewer.
**Depends on:** P0 seams; the in-flight key-echo fix merged first (same files).
**Bake-off:** none (the grab design is fixed by ADR-0001 K-1/K-2).
**Decision settled:** gorca modifier and keymap family (user, §8 Q1–Q2); settings format (ADR-0005).
**Acceptance.**
- K2 ≤ 5 ms, K3 ≤ 15 ms (target 5), K4 0 lost/doubled keys; all under L3-50k with Δp99 ≤ 1 ms.
- The modifier alone never reaches the app; double-tap reaches it; CapsLock state is unchanged when
  CapsLock is the modifier.
- Manager owner loss (Shell restart in the private headless gnome-shell session, or `fake_mutter.py`
  restart) → `Lost` → re-authorised and grabs restored within 2 s, announced once.
- A malformed `gorca.toml` never prevents startup.
- Live checklist on Settings, Files, Text Editor, top bar, overview.
**Risks.** P-7: Mutter applies grabs asynchronously, so a fast chord right after a mode switch may be
`Ambiguous` (counted, never re-executed). P-8: pass-next-key needs re-injection through EIS; if P-8
fails, pass-next-key ships later and double-tap is the only pass-through. The modifier conflicts with
layout switching if the user's XKB options use CapsLock (`grp:caps_toggle`) — Q1.

### M2 — Native mirror and caching (bake-off B-1)

**User-testable increment.** Tabbing through Settings, Files and the Shell is audibly faster on revisits,
toggled checkboxes, expanded/collapsed rows, slider values and renamed items are spoken immediately
and never stale, and "where am I" answers from cache.

**Scope.**
- P2P connections (D-7): lazy per app, `Startup` deadline, fallback to the bus call connection.
- Event-driven invalidation in the connector: `PropertyChange` (name, description, value, role),
  `StateChanged`, `ChildrenChanged`, `BoundsChanged` ignored; generation bump on `defunct` (I-1).
- `gorca-mirror` implementation chosen by MB-1; `may_refine` (focus spoken from cache, corrected within
  the refine window if the revalidation disagrees, ADR-0001 M-6).
- Focused-object state-change speech (S1): checked/unchecked, expanded/collapsed, selected, value.
- **B-1 bake-off** (three arms, all on P2P and the ADR-0004 drain): (a) lazy on demand + invalidation;
  (b) eager `Cache.GetItems` per app; (c) hybrid: eager focus-window ancestry and siblings, lazy
  elsewhere. Judged on L1h hit rate on a scripted Shell + Settings + Files walk, first-focus latency
  per app, D-Bus messages/s at idle and under L3, RSS (M-8), M-6 refine-correction rate.
- P-9 (GTK4 `AddAccessible`/`RemoveAccessible` consistency) is answered before arm (b) is judged.

**Agents:** architect (B-1 spec), rust-engineer ×2–3 (bake-off arms), linux-platform-engineer (P-9,
P2P reachability), perf-engineer (benchmark), reviewer (judge), test-engineer (invalidation
conformance tests in `gorca-testkit`: rename, state flip, subtree replace, app restart).
**Depends on:** P0 (MB-1, B-5). Independent of M1 (different crates; see §6).
**Bake-off:** **B-1** → settles native mirror fill strategy.
**Acceptance.** L1h p99 ≤ 2 ms; L1 (cold) unchanged; S1 ≤ 5 ms; stale presentations = 0 on the
conformance suite (each invalidation case has a test); M-6 corrections < 1 %; RSS growth bounded over
a 1 h scripted walk; L4 hung-app isolation still green.
**Risks.** GTK4 and St (Shell) emit incomplete property-change events → a hybrid TTL or revalidate-on-read
for the focused node only; caching must never trade correctness for speed (the user requirement "read
everything" implies "read it correctly").

### M3 — Text, caret and typing echo

**User-testable increment.** In Text Editor, a GTK entry, the Shell search entry and a Settings field:
left/right speak the character, Ctrl+left/right the word, up/down the line, Home/End and PgUp/PgDn
the line; Shift+arrows announce "selected"/"unselected" text; Backspace/Delete speak the removed
character; typing echoes characters (and words, per setting); gorca commands say current
character/word/line, spell word, say all from caret.

**Scope.**
- Caret state machine per focused text object: `NoText | Tracking { node, offset, sel } |
  Correlating { key, deadline }` — the input lane tags the last navigation key so the Reader chooses
  the unit (char/word/line) from the key, not from offset deltas alone (Orca's heuristic; ADR-0006
  specifies it with a transition table).
- Text units per ADR-0001 §4.3 (TU-2 offset conversion for Firefox/VTE stays in the connector's
  normalisation tables).
- Typing echo from `KeyEvent` + single-character insert signature (no wait for the caret event),
  deduplicated against the text-changed event; the key-echo policy from the in-flight fix is reused.
- Selection: diff old vs new selection ranges; speak the delta, not the whole selection.
- Say-all for native text (chunked by sentence, epoch-cancelled, resumes cursor tracking).
- Verbalizer (`gorca-speech`): punctuation levels, symbol names, capitals (pitch or "cap"), spelling.

**Agents:** architect (ADR-0006 text/caret/review presentation), rust-engineer (Reader text module,
verbalizer), linux-platform-engineer (GTK4/St caret and text-changed event characterisation), test-engineer
(fake editable app scripts, property tests for selection diff), perf-engineer (C1, C2, E1).
**Depends on:** M1 (commands, key context on the input lane); M2 soft (text cached by the mirror; M3 can
start on the uncached path).
**Bake-off:** none.
**Acceptance.** C1 ≤ 15 ms (target 5), C2 0 stale, E1 ≤ 3 ms; password entries echo like any other
field (decision 2026-10-13) and are never logged; no double echo (key echo + text-changed) in a
10k-keystroke randomized test.
**Risks.** St entries (Shell) and GTK4 differ in caret/text-changed ordering; autorepeat bursts;
input methods (IBus, Russian layout) produce committed text without matching key events → typing echo
must fall back to text-changed when no key correlates.

### M4 — Desktop and Shell polish, flat review and object navigation

**User-testable increment.** Window switches speak title and focus; dialogs and alerts read their
content on open; Shell notifications and OSD (volume, brightness) are announced; menus, popovers,
quick settings, overview search results and the app grid read well. The user can review the screen:
next/previous object, parent/first child, flat review by line/word/character of the active window,
review cursor to focus and back, read the status bar.

**Scope.**
- `Mode::Review { origin, cursor }`: object navigation over the mirror tree; flat review builds a
  per-window line model from `Component.GetExtents` (geometry, Background budget, cached), not from
  per-key IPC. Review commands are mirror-only (R1 zero IPC on hit).
- Window-activate, dialog-open ("alert" role and `dialog` children read), notification and
  `Announcement` speech with priorities (`Prio::Notification` never interrupts focus speech).
- `ext-gnome-shell` (native trait now, WASM after M5): Alt+Tab switcher (lands from the in-flight fix),
  overview, notifications, OSD, quick settings, lock screen and polkit dialogs.
- Live regions on the desktop (`notification` signal, ADR-0001 W-3).

**Agents:** rust-engineer (review mode, window/dialog presenters), linux-platform-engineer (Shell/St
quirks, extents reliability on Wayland), architect (review model inside ADR-0006), test-engineer (Shell
scenario scripts in a private headless gnome-shell), perf-engineer (R1, A1).
**Depends on:** M1, M2.
**Bake-off:** none.
**Acceptance.** R1 ≤ 3 ms, A1 ≤ 15 ms; a 25-item Shell checklist passes with the user (including lock
screen and polkit); notifications never cut off focus speech; flat review works in Settings, Files,
Text Editor.
**Risks.** Wayland gives no global coordinates: extents are window-relative, so flat review is per
window and cannot span windows (acceptable, matches the platform). St objects sometimes lack names
(the ADR-0002 naming cascade is out of Phase 2 scope, Q6).

### M5 — Extension host (bake-off B-4)

**User-testable increment.** `ext-gnome-shell` (and `ext-vte` skeleton) run as WASM components; a
deliberately crashing or looping test extension is reset within 50 ms while gorca keeps speaking, and
after three resets in 60 s it is disabled with one announcement.

**Scope.** `gorca-ext-host` on wasmtime: one `Store` per (extension, app), fuel/epoch interruption,
32 MiB limit, trap → instance reset, manifest matcher, keymap merge into M1's registry, settings KV
namespace; WIT 0.1 from ADR-0001 §10 (TBD-WEB-7 hooks for ADR-0003 §15 stay for M7).
**B-4 bake-off:** (a) WASM only vs (b) dual native + WASM, judged on X1, `ext-vte` `on-event` under the
T-1 storm (V2 load), and `present` cost on the M4 Shell walk.
**Agents:** wasm-engineer (host, WIT), architect (B-4 spec), rust-engineer (core hook points), perf-engineer,
reviewer (judge).
**Depends on:** P0 seams (hook points in the presenter registry); M4's `ext-gnome-shell` native code is
the port target. Can start in parallel with M3/M4.
**Decision settled:** B-4 (first-party extension execution).
**Acceptance.** X1 ≤ 100 µs p99 hook overhead; trap/loop recovery < 50 ms; no core panic from any
extension input (fuzzed WIT inputs); M1 latency gates unchanged with extensions loaded.
**Risks.** Component-model call overhead on the T-1 path; if B-4 picks dual, native extensions lose hang
protection (ADR-0001 §10), which must be stated in the manifest.

### M6 — GNOME Terminal (VTE), flawless

**User-testable increment.** In GNOME Terminal (bash/zsh, multiple tabs): command output is read
once, in order, without echoing typed characters twice; `cat` of a huge file, `yes`, and `make -j`
never stall gorca and Ctrl+C silences immediately; Up/Down in shell history reads the recalled line;
Left/Right read characters; Tab completion reads the completed text; `less`, `man`, `git log`, `nano`
are usable; review commands read the screen by line/word/character; switching terminal tabs reads the
new tab's title and cursor line.

**Scope.**
- T-1 per-frame collapse (ADR-0001 R-TEXT, already in ingress), T-2 screen-diff presenter (line-hash diff on
  `cpu`, in the app actor's job), T-3 offset clamping everywhere text is requested from VTE plus a fuzz
  target, T-4 background terminals → `Flooded`.
- D-5 per-app `text-changed` deregistration for non-focused terminals, if P-11 confirms atk-bridge honours
  the `app` argument; otherwise rely on T-4.
- `ext-vte` policy: prompt detection (heuristic + user-configurable regex, Q5), "new output" vs repaint
  classification (replacing Orca's 1 s repaint window, `ax_utilities_event.py`), typed-character
  suppression, cursor-line reading after history recall.
- Terminal review uses the M4 review mode over the terminal snapshot (no IPC per key).

**Agents:** rust-engineer (screen diff, presenter), linux-platform-engineer (P-11, VTE event
characterisation under load, real-VTE private session for V2), test-engineer (scripted terminal
sessions, fuzz T-3), perf-engineer (V1–V3), wasm-engineer (ext-vte in WASM if B-4 picks it), reviewer.
**Depends on:** M3 (caret/typing echo), M4 (review mode), M5 (ext host; `ext-vte` can start native).
**Bake-off:** none (B-4 already ran with `ext-vte` as its workload).
**Acceptance.** V1–V3 gates; zero gnome-terminal-server aborts in the fuzz run and in an 8 h soak;
typed characters echoed exactly once in 10k-keystroke randomized sessions; a 30-item terminal
checklist passes with the user.
**Risks.** VTE rebuilds its snapshot lazily so follow-up `GetText` sees a different screen than the
event (use event payload + offsets, per the platform report); full-screen TUIs (vim, htop, mc) are
best-effort in Phase 2 (review mode works; automatic reading of changes is heuristic) — Q5.

### M7 — Firefox browse mode: mirror, reading, say-all (bake-off B-6)

**User-testable increment.** Opening a page in Firefox reads title and starts in browse mode; arrows
read lines/words/characters from the virtual cursor; say-all reads the page and stops on any key;
Tab and Enter on form fields switch to focus mode with an earcon and Escape returns; switching tabs
reads the title and the line where the cursor was.

**Scope.** ADR-0003 §4 (arena, treap index, flat text, lines), §5 identity variant C, §6.2
`AtspiDocSource` with pipelined P2P, §8 viewport-first build, §9 incremental pipeline, §11 browse-mode
SM, BM-1…BM-3, §6.6 dormancy (wake W0/W1). **B-6 bake-off** per ADR-0003 §16: (a) `ATSPI-TUNED` vs
(b) `HYBRID-AGP-FF`, decision rule unchanged (b wins only with ≥ 5× on metrics 1–3 and passing
robustness, coverage and operational cost).
**Agents:** web-a11y-engineer (lead: source, normalisation, corpus), rust-engineer (doc actor, Reader
browse SM), math-researcher (TBD-WEB-6, treap/flat-text index validation, W-7), architect (B-6 spec,
TBD-B6-* closure), perf-engineer (W2, W3, M-15), test-engineer (`tests/web/` corpus automation in the
private headless gnome-shell), reviewer (judge).
**Depends on:** M1 (grab sets, browse keymap), M2 (P2P, mirror conventions), M3 (text units,
verbalizer). The `gorca-doc` data-structure work and the B-6 harness have no dependency and can start
during M1/M2 (§7).
**Decision settled:** B-6 → Firefox default `DocSource` (TBD-B6-1…7).
**Acceptance.** W2 ≤ 150 ms (10k nodes), W3 ≤ 15 ms, arrow/line nav p99 ≤ 5 ms, M-13 identity
join ≥ 99.9 % with 0 wrong-node presentations on the corpus, 100k-node page full mirror < 5 s in
the background, no stale speech after SPA route swaps in the corpus.
**Risks.** Firefox lazy cache domains (empty text treated as `Stale`); hidden-event drops; B-6 arm (b)
needs privileged Firefox bootstrap (Marionette/AutoConfig) — opt-in only, as already decided.

### M8 — Firefox navigation flawless (NVDA browse-mode parity)

**User-testable increment.** Quick navigation (H/1–6, K, U/V, F/E/B/C/X/R, D, T/L/I, G/Q/S/M/O/A, W,
N), table navigation (Ctrl+Alt+arrows with header changes), elements list (gorca+F7), find in page,
live regions (polite/assertive, throttled), modal dialogs confined (ADR-0003 §11 Modal scope), SPA
position restore — on the corpus and on the user's real sites, side by side with NVDA behaviour.
**Scope.** ADR-0003 §10 (re-render matching, route swap, position restore), §12 structural navigation,
§13 live-region policy, `ext-firefox` for remaining Firefox-only quirks.
**Agents:** web-a11y-engineer (lead), rust-engineer, test-engineer (NVDA-parity scripted comparisons;
expected utterances recorded from NVDA's rules, not from a Windows run), perf-engineer (W1), reviewer.
**Depends on:** M7.
**Bake-off:** none.
**Acceptance.** W1 ≤ 5 ms at 100k nodes; parity checklist (≥ 60 scripted cases from
`refs/nvda/source/browseMode.py` behaviours) passes; live-region last mutation → speech ≤ 65 ms;
cursor restore success ≥ 85 % on re-rendered corpus pages (GM §3.2 baseline).
**Risks.** Virtualised lists (TBD-WEB-3) and `aria-posinset` exposure (TBD-WEB-2) may block "N of M"
announcements in some frameworks.

**Prep status (2026-10-15, web-a11y-engineer).** Done: `docs/plan/m8-spec.md` (NVDA parity matrix, 10 parity deltas D-1..D-10,
implementation order), corpus `tests/web/m8/` (17 pages incl. news, encyclopedia, code forge, web app and SPA shaped ones, 5 frame pages, generated 100k-node and 12k-cell pages), gate harness
`e2e-m8` (318 scripted steps, 176 P0, expected-fail marks, ADR-0003 14 thresholds) and the baseline against `main`
(`docs/reports/m8-baseline.md`: 108 of 176 P0 steps pass on the merged M3b+M7b main).

### M9 — Chromium/Chrome

**User-testable increment.** The same browse mode and quick navigation as M7/M8 in Chrome launched
normally (comm=orca detection) and with `--force-renderer-accessibility`.
**Scope.** W-6 (does comm=orca enable `kAXModeComplete | kScreenReader`), Chromium normalisation rows
(caret(0) = scroll-to-anchor, static text via `children-changed`, silent reorders → parent revalidation),
`DocDelta::Reset` on tree rebuild, `ext-chromium`.
**Agents:** web-a11y-engineer, linux-platform-engineer (W-6), test-engineer, perf-engineer.
**Depends on:** M8. **Bake-off:** none (AT-SPI only per ADR-0003 WF-4).
**Acceptance.** Same gates as M7/M8 with Chromium bounded by tree arrival; the same parity checklist
passes or each failure is attributed to a Chromium bug with a filed reproduction.

### M10 — Braille (parallel track)

**User-testable increment.** With a BrlAPI display (or `brltty -b xw`/a virtual display for testing):
focus, caret and review are shown; panning and routing keys move the caret or review cursor.
**Scope.** Braille ADR (ADR-0007: cell model, contraction via liblouis FFI, routing, cursor shapes,
output scheduling separate from speech epochs but sharing them), `gorca-braille-brlapi` backend over
the `BrailleBackend` trait in `gorca-output-api`, BrlAPI connection with deadlines and reconnect.
**Agents:** architect (ADR-0007), c-cpp-engineer (liblouis/BrlAPI FFI or pure-Rust protocol client),
rust-engineer, test-engineer (virtual display).
**Depends on:** M2 (mirror), M3 (caret) for useful content; the backend itself has no dependency.
**Bake-off:** none. **Acceptance:** focus/caret → braille write p99 ≤ 15 ms gorca-side; display
unplug/replug never affects speech. **Priority:** Q4.

### Out of Phase 2

ADR-0002 perception layer (semantic find, label memory, pixels, MapSurface) beyond its Phase 0
scaffolding; Newton connector (R5); LibreOffice; a settings GUI; speech dictionaries UI. See Q6.

## 4. Bake-off schedule

| Bake-off | Milestone | Starts when | Decision | Judge on |
|---|---|---|---|---|
| B-5 runtime topology (config A/B, one codebase) | P0 | now | threading layout; rtkit/pinning on or off | L1/L2/K1 under L3-50k + saturated cores; M-14 |
| B-1 native mirror fill | M2 | after MB-1 and P-9 | lazy / eager `GetItems` / hybrid | L1h hit rate, first-focus latency, msgs/s, RSS, M-6 |
| B-4 extension execution | M5 | host skeleton + hook points ready | WASM only vs dual native+WASM | X1, ext-vte under storm, Shell walk |
| B-6 web SourceLayer | M7 | `gorca-doc` core structures merged | Firefox default `DocSource` | ADR-0003 §16 rule; M-9, M-13, M-15 |

B-5 runs first because every later latency gate is measured on its layout. B-1 serves the top two
polish targets. B-4 must finish before `ext-vte` is final. B-6 is last but its harness is prepared early.

## 5. Order and the recommendation

```
P0 ─┬─► M1 ──┬─► M3 ──┬─► M6 (needs M4, M5)
    │        │        │
    └─► M2 ──┴─► M4 ──┘
P0 ─────► M5 (parallel with M3/M4) ──► M6
web prep (gorca-doc, B-6 harness) ──► M7 (needs M1, M2, M3) ──► M8 ──► M9
M10 braille: any time after M2 (ADR-0007 first)
```

**Start first: M1 and M2 together, with P0 running alongside.** P0's core-seams step (1–2 days)
merges before M1/M2 write into `gorca-core`. Rationale: M1 is the largest visible gap (no commands at
all) and gates M3, M4 and browse mode; M2 removes the per-focus refetch that everything else would
otherwise be built and measured on, and its bake-off (B-1) is on the critical path for Shell and
Terminal. They touch disjoint crates (§6).

Second wave: M3 and M5 together (plus web prep), then M4, then M6. Terminal (priority 2) lands
before Firefox (priority 3) is finished, but Firefox work starts early on the data-structure side so
it does not serialise behind the terminal.

## 6. Parallelism and file ownership

| Area (crate / path) | P0 | M1 | M2 | M3 | M4 | M5 | M6 | M7–M9 | M10 |
|---|---|---|---|---|---|---|---|---|---|
| `gorca-core/src/reader.rs` (thin after P0) | **W** | r | r | r | r | r | r | r | r |
| `gorca-core/commands/` | W | **W** | | w (cmds) | w (cmds) | w (ext keymap) | w | w | w |
| `gorca-core/present/` | W | | w (state) | **W** (text) | **W** (window) | w (hooks) | w | w | |
| `gorca-core/text/`, `review/` | W | | | **W** | **W** (review) | | w | | |
| `gorca-core/doc_actor.rs`, browse SM | | | | | | | | **W** | |
| `gorca-input-mutter`, `gorca-input-api` | | **W** | | w (key ctx) | | | | w (grab sets) | |
| `gorca-config` (new) | | **W** | | w | w | w | w | w | w |
| `gorca-connector-atspi` (split into `events/`, `fetch/`, `text/`, `doc_source/`) | W (split) | | **W** (events, fetch) | **W** (text) | | | w (vte rows) | **W** (doc_source) | |
| `gorca-mirror` | | | **W** | | | | | | |
| `gorca-dbus` (P2P pool) | | | **W** | | | | | w | |
| `gorca-doc`, `gorca-doc-source-*` | | | | | | | | **W** | |
| `gorca-speech` (verbalizer) | | | | **W** | | | | | |
| `gorca-ext-api`, `gorca-ext-host`, `extensions/` | | | | | w (ext-gnome-shell) | **W** | **W** (ext-vte) | w | |
| `gorca-braille-brlapi`, `gorca-output-api` braille | | | | | | | | | **W** |
| `gorca-daemon` wiring | w | w | w | w | w | w | w | w | w |
| `bench/e2e`, `crates/gorca-bench/src/e2e` | **W** (skeletons) | w | w | w | w | w | w | w | |

W = owns, w = small additive edits. Conflict rules:

- **CF-1** P0 seams merge first; after that, M1 (commands/, input) and M2 (connector events/fetch,
  mirror, dbus) do not share files except `gorca-daemon` wiring.
- **CF-2** `gorca-daemon` wiring edits are additive (one `wire_*` function per subsystem); the
  orchestrator merges in order M2 → M1 when both are ready.
- **CF-3** M3 and M4 both add presenters; P0's registry gives each its own module. M3 owns
  `present/text.rs`, M4 owns `present/window.rs`, `present/notification.rs`.
- **CF-4** The connector split (P0) separates M2 (`events/`, `fetch/`), M3 (`text/`) and M7
  (`doc_source/`).
- **CF-5** The in-flight key-echo and Alt+Tab fixes touch `gorca-input-mutter` and the presenter;
  they merge before M1 and before P0 seams, or P0 rebases on them.
- Safe parallel sets: {M1, M2, P0-bench/B-5/MB-1, web prep}; {M3, M5, M10, web prep}; {M4, M5,
  M7 non-Reader parts}; {M6, M7}; {M8, M10}.

## 7. Web prep track (no milestone of its own; starts with M1/M2)

`gorca-doc` arena, treap index and flat-text map with property tests and MB benches
(math-researcher + rust-engineer); `AtspiDocSource` block fetch over pipelined P2P against the corpus
(web-a11y-engineer); B-6 harness and AGP arm skeleton; TBD-WEB-1/2/3 research. None of this touches
`gorca-core` until M7.

**Status 2026-10-14 (web-a11y-engineer, `docs/reports/2026-10-14-web-prep.md`).** Done: `AtspiDocSource` in
`crates/gorca-doc-source-atspi` (block fetch 100k nodes 1.54 s, 10k 148 ms; progressive first block 10.7 ms / 80 ms; events to deltas;
identity variant C; `Revalidate`; deadlines everywhere; 45 tests against a fake peer, ingestion into `gorca_doc::DocumentMirror` checked);
B-6 harness skeleton `crates/gorca-bench-web` + `tests/web/b6` (arm (a) complete, arm (b) skeleton); TBD-WEB-1/2/3 closed in ADR-0003 §20.1.
Open for M7: wire the source in the daemon and warm the Firefox cache domains at application attach; doc-actor ownership of the dirty set
(report D1); mirror-side removal grace (IL-R4); fallback walk for nodes over 65,536 children; event-storm rate sweep; coverage matrix run;
arm (b) (AGP) itself.

## 8. Questions for the user

| # | Question | Options | Recommendation |
|---|---|---|---|
| Q1 | Which key is the gorca modifier? | (a) Insert + KP_Insert (NVDA and Orca desktop default); (b) CapsLock (Orca laptop layout); (c) both | **(c) both by default**, with CapsLock dropped automatically if the XKB options use it for layout switching (`grp:caps_toggle` etc.). Mutter's double-tap passes the key through either way. Medium confidence: depends on how you switch between Russian and English layouts. |
| Q2 | Keymap family for desktop and review commands | (a) Orca-compatible (desktop and laptop layouts, numpad flat review); (b) NVDA-like (NVDA+Tab where am I, NVDA+T title, NVDA+numpad / laptop review) | **Keymap as data with both presets; default = the one in your muscle memory.** Browse-mode quick-nav letters are the same in both. My lean: NVDA-like, because Firefox parity is benchmarked against NVDA and one family across desktop and web is easier to learn. Medium confidence. |
| Q3 | Do you use a laptop without a numpad? | yes / no | Decides whether review commands default to the laptop layout. |
| Q4 | Braille in Phase 2? | (a) parallel track now; (b) after M6 | **(a) only if you use a display daily**, otherwise (b). |
| Q5 | Terminal: which shell and prompt, and are full-screen TUIs (vim, htop, mc) required "flawless" in Phase 2? | line-oriented flawless + TUIs best-effort / both flawless | **Line-oriented flawless, TUIs best-effort (review mode works) in Phase 2.** Optional: a shell snippet that marks prompts (OSC 133) to make prompt detection exact. |
| Q6 | Perception layer (ADR-0002 semantic find, label memory) in Phase 2? | in / out | **Out**, except keeping its scaffolding compiling; it competes for the same engineers as M7. |
| Q7 | Settings: TOML file + spoken commands, no GUI, no Orca settings import in Phase 2? | yes / want GUI / want import | **Yes.** |
| Q8 | Typing echo default | characters / words / both / off | **Characters on, words off** (both toggleable by command). |

## 9. Cross-cutting risks

- **Host noise.** The dev host runs at load ~10; gates use median-of-repeats and authoritative runs on
  a quiet host. A gate that only passes on a quiet host is reported as such, not waived.
- **Mutter key path.** Grab application is asynchronous (P-7); re-injection may be unavailable (P-8);
  shortcut-inhibiting windows deliver no keys (K-7). Each degrades commands only, never reading.
- **Shell restarts and Orca.** gorca must survive Shell restarts (KeyboardMonitor `Lost` →
  `Authorizing`) and never fight Orca (A1, ID-3).
- **Scope creep in the web track.** M7/M8 are the biggest milestones; B-6 must not block M7 —
  arm (a) is the baseline that ships regardless of the B-6 outcome.
- **Single-file hotspots.** Without P0 seams, M1–M4 would serialise on `reader.rs`; P0 step 1 is not
  optional.

## 10. New documents this plan creates

| Document | Owner | Needed by |
|---|---|---|
| ADR-0005 commands, keymap, settings | architect | M1 |
| `docs/design/modes.md` (mode table + keymap) | architect + tech-writer | M1 (desktop), M7 (browse) |
| ADR-0006 text, caret and review presentation | architect | M3, M4 |
| ADR-0007 braille | architect | M10 |
| B-1, B-4, B-5 specs under `docs/bakeoff/` | architect | P0, M2, M5 |
| `docs/plan/checklists/M1.md` … `M9.md` | tech-writer | each milestone |
