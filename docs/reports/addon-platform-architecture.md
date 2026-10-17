# Add-on platform architecture: gap analysis and design

Date: 2026-10-17. Author role: architect. Status: input to ADR-0019 (Proposed,
`docs/adr/0019-extension-platform.md`). Security and formal analysis are done in parallel by the
reviewer and the math researcher; this report covers capability, architecture, versioning,
lifecycle, packaging and the phased plan, and only names the security boundaries.

Sources read: `crates/gorca-ext-api/wit/gorca-ext.wit` (0.2.0), `crates/gorca-ext-host/src/*`
(manifest, health, wasm limits, catalog loading), `extensions/ext-{vte,gnome-shell,test}`, the
Telegram Web demo on branch `worktree-agent-aa50a60757c1de84c` (commit 98cb234: WIT 0.3.0 `web`,
`extensions/ext-telegram-web`, ADR-0008 amendment 3, `docs/design/extensions.md` §8a), ADR-0001 §10,
ADR-0008 §6/§9-11, ADR-0010 §7-8, `docs/plan/phase-3.md` (M18, ADR-0015 slot),
`docs/plan/phase-3-features.md` (§3.7 vendor profiles, §5 chooser/elements list), `docs/decisions.md`
(2026-10-17 licence, Telegram demo, M18-first). NVDA: `refs/nvda/source/{appModuleHandler.py,
globalPluginHandler.py, baseObject.py, scriptHandler.py, inputCore.py, extensionPoints/,
speech/extensions.py, braille/extensions.py, browseMode.py, addonHandler/, addonAPIVersion.py,
addonStore/, _bridge/}`. Wasmtime 49 (`refs/wasmtime/crates/wasmtime/src/runtime/component/linker.rs:26-60`).

---

## 1. What we have today (facts)

| Area | Today | Where |
|---|---|---|
| WIT | one package `gorca:ext@0.2.0`, one world `extension` (import `host`, export `hooks` + `terminal`); 0.3.0 adds `web` on the demo branch only | `crates/gorca-ext-api/wit/gorca-ext.wit` |
| Hooks | `on-event` (9 event kinds, verdict pass/suppress/rewrite/re-evaluate), `present`, `policy` (4 moments), `nav-units`/`nav-match` (no caller), `command(id)`, `terminal.step`, branch: `web.open`/`web.describe` | same |
| Host imports | `get-node` (mirror), `fetch`/`text` (declared, return `not-supported`), `speak`, `get/set-setting` (KV), `log` | same, `docs/design/extensions.md` §4 |
| Manifest | `ext.toml`: name, backend wasm/native, module, hooks, capabilities (4 bits: speak, fetch:prefetch, fetch:background, settings), matcher (app_name, toolkit, connector, document_role, verified_shell; branch: document_url), keymap (command, id, help, contexts focus/global/browse/review, chord, max_press, interrupts; branch: nav_category/nav_dir) | `crates/gorca-ext-host/src/manifest{,_file}.rs` |
| No manifest fields for | id, version, author, licence, API range, permissions rationale, settings schema, data contributions | — |
| Isolation | one wasmtime `Store` per (extension, app); fuel + CPU-time epoch backstop; 32 MiB; pooling allocator 512 instances; trap → reset from `InstancePre`; 3 resets / 60 s → disabled for that app with one notice; host-work meter on every host call | `wasm.rs:1-110,235-245`, `health.rs` |
| Limits | 2 ms CPU `on-event`/`present`/`policy`/`describe`, 20 ms `command`/`web.open`, 16 ms `terminal.step`; max 64 extensions | `wasm.rs`, `host.rs:33` |
| Localisation | `i18n/<lang>.msg` per extension; message references encoded in strings (`U+001E ns:key ...`) | ADR-0010 §7-8 |
| Discovery | system/build dirs; `--ext-dir` replaces the search; no user dir yet (M18 scope) | `host.rs`, `docs/plan/phase-3.md` M18 |
| Design lessons already learned | (1) **pure function + actions** beats host callbacks (`terminal.step(now,input)->actions`, ADR-0008 amendment 2); (2) **data first**: a site add-on answers a *profile of selectors* the core evaluates natively, code runs only for claimed nodes (describe p99 8.5 µs on 3000 chats, zero cost when not loaded; ADR-0008 amendment 3); (3) safety rules stay in the core (secret fail-closed) | ADR-0008 §10-11 |

Bottom line: the host's **isolation machinery is ahead of NVDA** (NVDA add-ons run in-process
Python with no sandbox; a hung add-on freezes the reader). The **contract surface is far behind**: one
world, closed enums, nine events, `command(id)` with no context and no effects beyond speech, no async
host access, no data contributions besides keymaps/catalogs, and no package identity or versioning.

---

## 2. What NVDA add-ons can do (with sources)

| # | NVDA mechanism | Source |
|---|---|---|
| N1 | **App modules**: per-process class, `event_appModule_gainFocus/loseFocus`, `sleepMode` (NVDA goes quiet in self-voicing apps), app-specific scripts, status bar text, UIA quirk switches | `appModuleHandler.py:412,456,461,494,786-872` |
| N2 | **Global plugins**: singleton, sees every event and object, own scripts | `globalPluginHandler.py:62` |
| N3 | **Overlay classes**: `chooseNVDAObjectOverlayClasses(obj, clsList)` lets an app module or global plugin insert classes that override name, role, states, events and scripts per object | `appModuleHandler.py:646`, `globalPluginHandler.py:81` |
| N4 | **Events** `event_<name>(obj, nextHandler)`: chain, may swallow | `eventHandler.py` |
| N5 | **Scripts and gestures**: `__gestures` dicts, `bindGesture`, `@script` with category/description; user-remappable in Input Gestures | `baseObject.py:186-276`, `scriptHandler.py:370` |
| N6 | **Extension points** (47 in core): `Action`, `Filter`, `Decider`, `AccumulatingDecider`, `Chain`; e.g. `filter_speechSequence`, `pre_speech`, `decide_executeGesture`, `decide_handleRawKey`, `post_appSwitch`, `post_configProfileSwitch`, `decide_beep`, braille `filter_displaySize`, `pre_writeCells` | `extensionPoints/__init__.py:25-262`, `speech/extensions.py:13-50`, `inputCore.py:488,506`, `braille/extensions.py` |
| N7 | **Synth drivers** (in-process; 32-bit SAPI now hosted out of process via `_bridge/runtimes/synthDriverHost`) | `synthDrivers/`, `_bridge/` |
| N8 | **Braille display drivers**, device detection `bdDetect.scanForDevices` | `brailleDisplayDrivers/`, `bdDetect.py` |
| N9 | **Vision enhancement providers** (highlighter, screen curtain) | `visionEnhancementProviders/`, `vision/providerBase.py` |
| N10 | **Settings panels** (`SettingsPanel` subclasses appended to `categoryClasses`) and **config spec** (`config.conf.spec[...]`), profile-aware | `gui/settingsDialogs.py:515-522` |
| N11 | **Browse mode**: `addQuickNav` for new quick-nav types, custom tree interceptors (virtual buffers for non-web apps), Elements List types are a fixed tuple (links, headings, form fields, buttons, landmarks) | `browseMode.py:586,1454-1475` |
| N12 | **TextInfo** subclasses for broken text models | `textInfos/` |
| N13 | **Data in manifest**: braille tables, symbol dictionaries, speech dictionaries | `addonHandler/__init__.py:1138-1158` |
| N14 | **Install tasks**: arbitrary `installTasks.onInstall/onUninstall` code | `addonHandler/__init__.py:552-556` |
| N15 | **Manifest**: name, summary, description, author, version, changelog, `minimumNVDAVersion`, `lastTestedNVDAVersion`, url, docFileName; bundle `.nvda-addon` (`application/x-nvda-addon`) | `addonHandler/__init__.py:97,1095-1135` |
| N16 | **API compatibility**: `BACK_COMPAT_TO` is bumped almost every year (2019.3, 2021.1, 2022.1, 2023.1, 2024.1, 2025.1, 2026.1, 2027.1), each time silently invalidating add-ons whose `lastTestedNVDAVersion` is older; users can override per add-on | `addonAPIVersion.py:17-37`, `addonVersionCheck.py:20-37` |
| N17 | **Store**: `addonStore.nvaccess.org`, channels stable/beta/dev/external, update channel per add-on, model fields `publisher`, `license`, `licenseURL`, `sourceURL`, `sha256` (checked after download), `reviewURL`, `submissionTime`, VirusTotal scan results; no code signing | `addonStore/network.py:43,62,370-395`, `addonStore/models/channel.py:12-140`, `addonStore/models/addon.py:115-135`, `models/scanResults.py:13` |

Root cause of NVDA's yearly breakage: **the whole Python namespace is the API** (add-ons import
`speech`, `controlTypes`, `NVDAObjects`, monkey-patch functions). Any refactor is a break. gorca's
WIT boundary makes the API surface exactly the WIT text, which is the structural fix, provided we
version it correctly (section 5).

Prior art without an add-on ecosystem: Orca has per-app Python scripts compiled into Orca
(`refs/orca/src/orca/scripts/apps/`: evolution, gajim, gnome-shell, soffice, Thunderbird, ...) and an
unsandboxed `orca-customizations.py`; no store, no isolation. VoiceOver has no third-party plugins
(AppleScript only). JAWS scripts are the closest commercial analogue (per-app, in-process).

---

## 3. Gap analysis: capability matrix

Priority for store viability: **P0** = a credible first wave of third-party add-ons cannot exist
without it (target: M18 preview); **P1** = needed within the first year of the store; **P2** = later;
**X** = deliberately never (replaced by something safer or kept in core).

| Capability | NVDA | gorca today | Needed | Prio |
|---|---|---|---|---|
| Per-app modules | N1 | matcher (app/toolkit/connector/verified_shell); per (ext,app) store | match by `desktop_id`/exe and window class/title glob; app lifecycle hooks (`app-focused`, `app-blurred`); **sleep mode** as a declared verdict | P0 |
| Global plugins | N2 | none (an `app_name=["*"]` ext is N per-app instances with no shared state) | `global` world: one instance, own lane, bounded coalescing mailbox, declared subscriptions | P0 |
| Per-object behaviour (overlay classes) | N3 | `policy` facts→verdict; branch: web claim selectors + `describe` | one **node selector** language for all sources (role, state mask, name/id/class/tag globs, ancestor chain, toolkit attrs) evaluated natively; **node patches as data** (rename, re-role, label-by relation/sibling, hide/flatten, mark live, mark category, mark sleep); claims → `describe`; claim-scoped commands | P0 |
| Event handling | N4 | 9 kinds, verdict chain | open event vocabulary (state, name, description, value, attributes, window create/destroy/activate, notification, live text), subscription mask in manifest so the host never calls for undeclared kinds; batched delivery for global | P0 |
| Commands and gestures | N5 | keymap entries + `command(id)` returning speak/handled | command **context** (focus key, mode, press count, doc id, claim id), **effects** (speak, cue, braille flash, move browse/review cursor, open chooser/prompt, do-action, set setting, arm timer); gesture categories and catalog help (exists via M12a); **command layers** (prefix-key tables as data); user remap through the core keymap (exists, ADR-0005) | P0 |
| Raw key interception | `decide_handleRawKey` | terminal `raw` keys only | stays core; only the bounded terminal exception | X |
| Async object queries | free (synchronous COM) | `fetch`/`text` return `not-supported` | `request(query)→id` + `deliver(id, reply)`; queries: node, children (bounded), text range, attributes, relations, actions list, document URL; host deadline per budget class | P0 |
| Act on objects | `doAction`, `setFocus` | none | `act` effect: do-action(name), focus, set caret/selection, scroll-to, set value; capability-gated, scoped to matched apps | P0 |
| Timers / background work | threads | terminal `arm-timer` only | general timers (min period 50 ms, ≤ 16 armed), `tick(token)`; no threads | P0 |
| Settings with UI | N10 | string KV, own namespace | **typed settings schema in the manifest** (bool, int range, enum, string with max, keymap ref) rendered by the settings app (M14) and the in-reader chooser (M15a); values profile-aware as `ext.<id>.*` (ADR-0016 L2); `settings-changed` hook | P0 |
| Profiles | trigger API | vendor profiles designed (phase-3-features §3.7, PF-1..4) | ship as manifest data; later a **context tag** effect (PF-3) | P0 data / P1 tag |
| Speech and symbol dictionaries | N13 | ext layer in the dict chain designed (ADR-0017) | manifest `[[dictionary]]`, `[[symbols]]` data | P0 |
| Localisation | gettext per add-on | catalogs + message refs as magic strings | typed `msg-ref` in WIT; localised manifest strings, docs per language | P0 |
| Quick-nav categories | `addQuickNav` | branch: 4 reserved bits, core-run keys | named category registry (≥ 32 per document, allocated per doc), `nav_category` keys, categories exported to the elements list | P0 |
| Elements-list categories | fixed tuple | M15 not built | extension categories are sources of `Mode::Chooser` (CH-*), including desktop apps (e.g. "messages" in a mail client) | P0 |
| Custom chooser menus | wx dialogs | none | `open-chooser(items)` effect + `chosen(id)` hook (spoken list, zero IPC); `prompt` for text input | P1 |
| Review / object-nav hooks | TreeInterceptor, TextInfo | terminal review fed by ext-vte | patches `hide`/`flatten` affect object nav; **review providers** (line model from an ext for a claimed node, as ext-vte does for terminals) | P1 |
| Custom browse documents for desktop subtrees | tree interceptors | none | selector-declared "treat subtree as document" → core DocSource over desktop mirror | P2 |
| TextInfo fixes | N12 | `AppInfo.text_unit`/quirk tables in core | text fixups as data (offset unit, line-break rules) | P1 data / X code |
| Speech filter | `filter_speechSequence` | none | data first (dictionaries, regex rules compiled by core); code filter only for `global` add-ons, inline budget, fail-open to unfiltered | P1 |
| Cues / earcons | `nvwave`, `tones`, `decide_beep` | CueSink designed (M13, via voicego per QF-4) | cue themes as data; `cue(id)` effect; mapping role/state→cue in profile data | P1 |
| Braille filters / regions | N6 braille | M10 not built | `braille-flash` effect P1; region filters P2 | P1/P2 |
| Synth drivers | N7 | separate speech service by design | not WASM: an external process speaking SSIP or voicego protocol, registered as a speech backend (native tier, never in core process) | P2 |
| Braille display drivers | N8 | brltty/brlapi | use brltty | X |
| Vision providers | N9 | none | Shell-side highlighter (Phase 4); not an add-on API in 1.0 | P2 |
| Persistent storage | free filesystem | settings KV (bounded) | per-add-on store 1 MiB (KV + blobs), write-coalesced in the state dir (M11) | P1 |
| Network | free | none | host-mediated HTTPS `request` to declared origins, deadline, size cap, user-visible permission (AI describers, translators, site data) | P1 |
| Clipboard | free | none | `clipboard-write` effect P1; `clipboard-read` request with per-use confirmation P1 | P1 |
| Screen capture / OCR | free | none | capture of a node's rectangle through the platform with per-use consent | P2 |
| Process launch, raw D-Bus, files | free | none | never (raw D-Bus maybe as a reviewed allow-list per well-known name, Phase 5) | X |
| Install tasks | N14 arbitrary code | none | declarative settings migrations only | X (code) |
| Inter-add-on API | Python import | none | none in 1.0; later `provides`/`requires` of data contributions only | P2 |
| Manifest identity and compat | N15/N16 | name only | id, semver, API range **computed from the component's imports**, `min-gorca` for behaviour, permissions with reasons | P0 |
| Package format | `.nvda-addon` zip | directory | `.gorca-addon`, checksummed, signed (store) | P0 format / Phase 4 signing |
| Store | N17 | none | Phase 4 (ADR-0020) | Phase 4 |
| Crash isolation | none (in-process) | sandbox, reset, 3 strikes, safe mode (M11) | + global quarantine across apps, automatic rollback after a bad update | P0 |

P0 count is large but most of it is **data and plumbing around the existing host**, not new isolation
technology. The one new runtime concept is the async request/deliver loop plus the global lane.

---

## 4. Proposed platform architecture

### 4.1 Principles (all from evidence in this repo)

- **PA-1 Data first, code for the remainder.** Every capability that can be declared is declared
  (keymaps, layers, selectors, node patches, categories, live rules, profiles, dictionaries, symbols,
  cue themes, settings schema, catalogs) and evaluated by the core natively. A large share of
  add-ons needs **no component at all** (data-only add-ons). Evidence: the Telegram demo costs nothing
  per node because selectors are core-evaluated (ADR-0008 amendment 3).
- **PA-2 Hooks are pure functions; effects are buffered.** A hook receives facts and returns a
  verdict; anything it *does* goes through effect imports that only append to a per-call bounded
  buffer. The host applies the buffer after the hook returns and **discards it on trap or timeout**
  (transactional effects). This is the `terminal.step` lesson generalised, and it is also the
  versioning escape hatch (section 5: adding an import function is compatible, adding a variant case
  is not).
- **PA-3 Nothing blocks.** Host imports touch only in-memory state. Anything needing IPC is a
  `request` with a budget class and a host deadline; the reply arrives later as `deliver`. Matches
  ADR-0001 R3.
- **PA-4 Placement by lane.** Inline (Reader) only for `present`, `policy`, `describe`, and a future
  speech filter; everything else on extension lanes. Aggregate inline budget per presentation.
- **PA-5 The core decides; extensions advise.** Modes, focus bookkeeping, secrets, speech
  scheduling, cancellation and user settings sovereignty remain core (section 4.9).

### 4.2 Worlds

One WIT package family `gorca:ext@0.9.0` with shared interfaces and several worlds. A package
(`.gorca-addon`) holds zero or more components, each targeting exactly one world.

| World | Instance topology | Lane | Exports | Typical add-on |
|---|---|---|---|---|
| *(none: data-only)* | none | none | — | dictionaries, symbol sets, vendor profiles, keymap layers, cue themes, site profiles with no code, translations of other add-ons' catalogs |
| `app-module` | one Store per (add-on, app), lazy at first match, evicted after idle | app actor (events), Reader (inline), ext worker (commands, deliver, tick) | `lifecycle`, `events`, `present`, `policy`, `commands`, `reactor` | per-app fixes and features (LibreOffice, Thunderbird, Telegram Desktop) |
| `site` | one Store per (add-on, browser app); state keyed by `doc-id` | document actor (`open`), Reader (`describe`), worker | `site` (open, describe, route-changed), `commands`, `reactor` | web apps (Telegram Web, Gmail, GitHub) |
| `terminal-policy` | per (add-on, terminal app) | Reader-adjacent, as today | `terminal` | shell/editor policies (ext-vte) |
| `global` | **one Store** per add-on | dedicated `ext-global` lane (worker pool), never Reader | `global-events`, `commands`, `reactor`, optional `speech-filter` (P1) | clipboard history, translators, AI describers, productivity tools |

Rationale for singleton globals: N per-app copies of a global plugin cannot share state and multiply
memory (32 MiB each, pool of 512). Isolation is preserved because a global add-on sees events only
through its mailbox and can be quarantined independently.

### 4.3 Interface sketch (0.9 "preview")

Sketch, not final syntax. `@unstable(feature = ...)` gates experimental parts (supported by
wit-parser and wasmtime bindgen: `refs/wasmtime/crates/component-macro/tests/codegen/unstable-features.wit`).

```wit
package gorca:ext@0.9.0;

interface types {
  type node-key = tuple<u64, u64>;
  type doc-id = u64;
  /// Open vocabularies: numeric codes with a published registry (gorca-ext-api constants).
  /// New codes are added in minor releases; an unknown code must be treated as "unknown".
  type role = u16;
  type event-kind = u16;
  type state-set = u64;        // bit registry, append-only
  type prop-mask = u32;

  /// Localised text: a message of the add-on's own catalog (ADR-0010), or a literal (deprecated at 1.0).
  record msg-ref { key: string, args: list<tuple<string, string>> }
  variant text { msg(msg-ref), literal(string) }
  type seg-kind = u8;          // registry: text, role, state, value, name, description, ...
  record segment { text: text, kind: seg-kind }

  record node-facts {          // frozen per major; growing data is read via host accessors
    key: node-key, role: role, states: state-set,
    name: option<string>, description: option<string>, value: option<string>,
  }
  /// The context every command receives.
  record cmd-ctx { focus: option<node-key>, doc: option<doc-id>, claim: option<u32>,
                   mode: u16, press-count: u8, epoch: u64 }
  type req-id = u32;
  enum budget { interactive, prefetch, background }
}

/// Read-only, in-memory, never blocks. Grows by added functions in minors (compatible).
interface mirror {
  use types.{node-key, node-facts};
  node: func(k: node-key) -> option<node-facts>;
  parent: func(k: node-key) -> option<node-key>;
  attr: func(k: node-key, name: string) -> option<string>;   // toolkit/web attributes, clipped
  setting: func(key: string) -> option<string>;              // own namespace, typed by schema
  now-ms: func() -> u64;
}

/// Effects: append to the per-call buffer; applied after return; dropped on fault.
interface effects {
  use types.{segment, node-key, req-id, budget};
  enum prio { interrupt, message, text, progress, notification }
  say: func(s: list<segment>, p: prio);                // epoch-tagged; interrupt only inside an owned command
  cue: func(id: string);                               // own cue theme or core cue names
  braille-flash: func(s: list<segment>);
  move-cursor: func(target: node-key);                 // browse/review cursor, core validates
  act: func(target: node-key, action: string);         // capability `act`
  open-chooser: func(title: msg-ref, items: list<tuple<u32, list<segment>>>);  // P1
  arm-timer: func(token: u32, after-ms: u32);
  set-setting: func(key: string, value: string);       // own namespace, schema-validated
  request: func(q: query, b: budget) -> result<req-id, request-error>;  // see `queries`
  cancel: func(id: req-id);
  log: func(level: u8, msg: string);
}

interface queries {
  variant query { node(node-key), children(tuple<node-key, u16>), text(tuple<node-key, u32, u32>),
                  relations(node-key), actions(node-key), doc-url(doc-id), http(http-get) /* P1, `net:` */ }
  variant reply { node(node-facts), nodes(list<node-facts>), text(string), ..., failed(query-error) }
}

interface reactor {   // every async completion lands here, off the Reader
  use queries.{reply};
  deliver: func(id: req-id, r: reply);
  tick: func(token: u32);
  settings-changed: func(keys: list<string>);
}

interface app-hooks {
  app-focused: func(); app-blurred: func();
  on-event: func(kind: event-kind, node: node-key, mask: prop-mask) -> event-verdict;
  present: func(n: node-facts, reason: u8) -> option<list<segment>>;
  policy: func(f: policy-facts) -> policy-verdict;
}
interface commands { use types.{cmd-ctx}; run: func(id: u32, ctx: cmd-ctx); chosen: func(chooser: u32, item: u32); }
interface site { open: func(d: doc-info) -> option<profile>; describe: func(claim: u32, nodes: list<web-node>) -> option<list<segment>>; route-changed: func(doc: doc-id, url: string) -> option<profile>; }
interface global-events { on-batch: func(evs: list<global-event>, dropped: u32); }

world app-module { import mirror; import effects; export app-hooks; export commands; export reactor; }
world site       { import mirror; import effects; export site; export commands; export reactor; }
world global     { import mirror; import effects; export global-events; export commands; export reactor; }
world terminal-policy { import mirror; import effects; export terminal; }
```

Notes on the sketch:

- The `node` record of 0.2 becomes `node-facts` (frozen) plus `mirror.attr` for anything else; the
  `role` enum becomes an open `u16` code. Both are forced by the versioning rules of section 5.
- `command(id) -> command-result` becomes `commands.run(id, ctx)` with effects; `handled` is implied
  unless the command calls `effects.pass-through()` (an added import) to give the key back.
- The web interface of 0.3.0 becomes the `site` world with `route-changed` (fixes the SPA limitation
  recorded in ADR-0008 amendment 3) and its selectors generalised (4.4).
- `fetch`/`text` (blocking-shaped, never implemented) are removed in favour of `request`/`deliver`.

### 4.4 Declarative contributions (manifest data, core-evaluated)

| Contribution | Shape | Evaluated by |
|---|---|---|
| `[[selector]]` | role codes, state mask (all/none), name/id/class/tag globs, toolkit attribute equals, ancestor selector (depth ≤ 4) | core matcher, compiled once per add-on; both desktop mirror and web mirror |
| `[[patch]]` | `selector` + one of: `name = msg`/`name-from = "label-by" \| "sibling-prev" \| "child-text"`, `role = code`, `hide`, `flatten`, `live = polite\|assertive`, `category = name`, `sleep`, `claim` | core, at upsert time (as `SiteProfile::patch` on the branch) |
| `[[category]]` | name (msg id), selector; usable by quick-nav keys and the elements list | core category registry (per document, dynamic bit allocation, ≥ 32) |
| `[[keymap]]`, `[[layer]]` | command entries (exists) + prefix layers: a chord enters a one-shot key table | core keymap (ADR-0005) |
| `[[profile]]` | vendor profile (phase-3-features §3.7, PF-1..4) | `gorca-config` |
| `[[dictionary]]`, `[[symbols]]` | NVDA-compatible `.dic` subset and symbol tables per language | verbalizer chain (ADR-0017) |
| `[cues]` | theme files (ogg/flac, size-capped) and role/state → cue map | CueSink (M13) |
| `[settings]` | typed schema, defaults, msg ids for labels/help, `profileable` flag | settings app (M14), chooser (M15a) |
| `[[migration]]` | settings renames/defaults between add-on versions | installer |

Desktop and web share **one selector and patch language**; the branch's web `selector` (tag, class,
role, dom-id) is the web subset. This is gorca's answer to NVDA overlay classes: 80 % of overlay
uses in NVDA add-ons fix a name, a role or a label (estimate from experience, not measured) and need
no code.

### 4.5 Lanes, budgets and backpressure

| Path | Where | Budget | On overrun / overload |
|---|---|---|---|
| `present`, `policy`, `describe` | Reader inline | 2 ms CPU per call (exists); **new: 3 ms aggregate per presentation across all add-ons** | remaining add-ons skipped, core presentation (fail-open), strike for the one that overran |
| `on-event` (app-module) | app actor | 2 ms CPU | verdict `pass`, strike |
| `global-events.on-batch` | `ext-global` lane | 5 ms CPU per batch, batch ≤ 64 events | mailbox: bounded 256 per add-on, coalesce by (kind, node), drop oldest, `dropped` count delivered; the producer (hub) never waits |
| `commands.run`, `deliver`, `tick` | ext worker | 20 ms CPU | strike; effects dropped |
| `site.open`, `route-changed` | document actor's blocking pool | 20 ms CPU | `none` (core presentation) |
| `request` | host executes on connector actors | interactive 150 ms, prefetch 500 ms, background 2 s, http 10 s | `failed(timeout)`; ≤ 8 in flight per instance, the ninth returns `busy` immediately |
| timers | ext worker | ≥ 50 ms period, ≤ 16 armed | `arm-timer` beyond the cap is dropped with a log line |
| effects buffer | per call | ≤ 32 effects, ≤ 8 KiB text | excess dropped, counted |
| speech from add-ons | Speech lane | epoch-tagged; `interrupt` only inside an owned command; rate ≤ 10 utterances/s per add-on outside commands | stale epoch cancelled with the rest; excess coalesced to the latest |

Cancellation: every effect and request carries the epoch current when the hook ran. A newer epoch
cancels pending speech (as today) and marks outstanding replies stale; `deliver` still runs (state
updates) but `say` from a stale delivery is dropped unless it is `notification` priority.

### 4.6 Lifecycle state machines

Package level (persisted in the state dir):

```rust
enum AddonState {
    Discovered,                                   // found in a search dir or a downloaded file
    Rejected { why: LoadDiagnostic },             // manifest/package/API invalid; terminal until replaced
    Installed { enabled: bool },
    Staged { next: Version, prev: Version },      // update validated + precompiled, waiting for a switch point
    Active,                                       // enabled and loaded
    Quarantined { reason: QuarantineReason, since: SystemTime },
    PendingRemoval,
}
enum QuarantineReason { CrashLoopAcrossApps, GlobalCrashLoop, UserSafeMode, IncompatibleHost, Revoked }
```

| From | Event | To | Action |
|---|---|---|---|
| Discovered | validate ok | Installed{enabled: false for sideload, true for first-party} | precompile `.cwasm` (keyed by wasmtime version + engine config hash) |
| Discovered | validate fails | Rejected | diagnostic in settings app and log, never a crash |
| Installed{true} | daemon start / enable | Active | register catalogs, keymaps, data contributions; components stay cold until a match |
| Active | update downloaded and validated | Staged | old version keeps running |
| Staged | switch point (no slot mid-call; next app focus change or idle) | Active(next) | atomic swap of the `LoadedExt`; instances rebuilt lazily; prev kept on disk |
| Active(next) | quarantined within 24 h of the swap | Active(prev) | **automatic rollback**, one spoken notice |
| Active | ≥ 3 apps disabled the add-on within 10 min, or global world disabled | Quarantined | one notice; user can re-enable (clears strikes) |
| any | safe mode (M11: second restart in 60 s) | Quarantined{UserSafeMode} for every non-core add-on | — |
| Active | host upgrade breaks the API range | Quarantined{IncompatibleHost} | diagnostic names the missing interface |
| any | user removes | PendingRemoval → gone | settings kept for 30 days unless purged |

Slot level (one add-on × one app or the global singleton): `Cold → Instantiating → Active ↔
Resetting → DisabledForApp` (the existing `InstanceGuard`, 3 resets / 60 s), plus `Idle-evicted →
Cold` after 10 min without a call (frees pool slots; re-instantiation from the precompiled module is
tens of µs, ADR-0008 §2: 31 µs trap recovery).

### 4.7 Permissions (deny by default)

Declared in the manifest with a reason (msg id) each; spoken on install; shown in the settings app.
Detailed threat analysis is the reviewer's.

| Permission | Grants | Default for store add-ons |
|---|---|---|
| `speak`, `cue`, `braille` | output effects | granted with install |
| `read-tree` | `request` node/children/text/relations beyond the facts passed to hooks | granted, scoped to matched apps |
| `act` | `act` effect on nodes of matched apps | prompt on install |
| `observe-all-apps` | `global` world events from every app | prompt on install, re-confirmed on update if added |
| `observe-typing` | typed characters in events | prompt; **never** inside secret fields (core redaction, I-5) |
| `storage` | 1 MiB per add-on | granted |
| `net:<origin>` | `http` queries to listed HTTPS origins | prompt on install, listed origins only |
| `clipboard-write` / `clipboard-read` | effect / request | write: install prompt; read: per-use confirmation |
| `capture` (P2) | image of a node rectangle | per-use confirmation |

### 4.8 Versioning and compatibility (avoid NVDA's yearly breakage)

Facts that drive the rules:
- Component-model types are structural and **closed**: adding a field to a record or a case to a
  variant/enum is a different type, so an old component no longer type-checks against a host that
  changed it.
- Instance subtyping does allow a host to provide **more functions** than a component imports.
- Wasmtime's linker resolves semver-compatible import names automatically (`a:b/c@0.2.0` import
  resolves to a defined `0.2.1`) and lets a host define several versions at once
  (`refs/wasmtime/crates/wasmtime/src/runtime/component/linker.rs:26-60`), on the condition that
  "interfaces once defined are never changed".

Rules:
- **V-1** Package `gorca:ext@MAJOR.MINOR.PATCH`. 0.x is the preview (minor may break, with a
  migration note); 1.0 freezes.
- **V-2** Within a major: add interfaces, add functions to imported interfaces, add *optional*
  exported interfaces (the host probes whether a component exports them). Never change an existing
  type or function signature.
- **V-3** Growing vocabularies (roles, states, event kinds, segment kinds, prop bits, cue names) are
  open numeric codes with an append-only registry; unknown values mean "unknown", never a trap.
- **V-4** Inputs that will grow are read through accessor imports (`mirror.attr`) instead of record
  fields; records are frozen snapshots.
- **V-5** The host links the current major **and the previous major** for at least 24 months after
  the new one ships, through host-side adapters (old bindings → new core calls). Golden components
  built against every released minor are kept in the repo and run in CI forever.
- **V-6** Experimental surface uses `@unstable(feature = …)` and is enabled only with the dev channel
  or `--experimental`; experiments never consume a version number.
- **V-7** API compatibility is **computed, not declared**: the installer and the store read the
  component's imports/exports and check them against the host's linker. No `lastTested` field gates
  loading; `min-gorca` remains only for behaviour the add-on depends on.
- **V-8** Deprecation: announced one minor ahead with a load diagnostic naming the replacement;
  removal only at a major.

### 4.9 What stays in the core (non-negotiable)

- Focus bookkeeping, mode machine (`Mode::*`), document/caret/review cursors: extensions request
  moves (`move-cursor`), the core validates.
- Speech scheduling, epochs and cancellation; the keypress → speech p99 15 ms budget. Extension
  inline time counts against it and is capped (4.5).
- Secrets: password/secret nodes and typed text in them never reach an extension (fail closed,
  ADR-0008 amendment 2).
- Input grabs, the gorca modifier, raw key classification (terminal raw keys excepted), and the
  **reserved chords** that no add-on may bind or claim: stop speech, toggle speech, safe mode /
  disable all add-ons, the input help toggle.
- User settings outside `ext.<id>.*`; profile activation (PF-3).
- All IPC, connector types and D-Bus.

### 4.10 Packaging and installation

- File: `<id>-<version>.gorca-addon`, a zip (stored or deflate) with a fixed layout:
  `addon.toml`, `components/*.wasm`, `i18n/<lang>.msg`, `data/` (dictionaries, symbols, profiles,
  cue sounds), `docs/<lang>/README.md`, `LICENSE`, `CHECKSUMS` (SHA-256 per file),
  `CHECKSUMS.sig` (store signature, Phase 4), optional `publisher.sig`.
- Caps: package 16 MiB, component 8 MiB, sounds 4 MiB, entries ≤ 512, no symlinks, no absolute or
  `..` paths, decompression ratio bounded (zip-bomb guard).
- Install root: user `~/.local/share/gorca/addons/<id>/<version>/` with a `current` pointer; system
  `/usr/share/gorca/addons/`; user wins over system for the same id only if its version is newer or
  the user pinned it. Development: `--ext-dir` (exists) and `gorca addon dev <dir>` (hot reload).
- Precompiled code cache in `$XDG_CACHE_HOME/gorca/cwasm/` keyed by package SHA-256 + wasmtime
  version + engine config; never trusted across hosts.

---

## 5. Store groundwork (Phase 4, ADR-0020)

**Manifest fields (`addon.toml`)**

```toml
[addon]
id = "org.example.telegram-web"     # reverse-DNS, immutable, unique in the store
version = "1.4.2"                   # semver; ordering for updates
name = "addon-name"                 # catalog ids (i18n/<lang>.msg), English required
summary = "addon-summary"
description = "addon-description"
authors = ["Jane Doe <jane@example.org>"]
license = "MIT OR Apache-2.0"       # SPDX; store requires it
homepage = "https://…"
source = "https://…"                # required for store; tag must match version
changelog = "docs/en/CHANGELOG.md"
min-gorca = "0.9"                   # behaviour floor only (API compat is computed, V-7)
categories = ["web-apps", "productivity"]
[[component]]
world = "site"                      # app-module | site | global | terminal-policy
path = "components/telegram.wasm"
[permissions]
read-tree = { why = "perm-read-tree" }
net = { origins = ["https://api.example.org"], why = "perm-net" }
```

**Trust and signing (outline; reviewer owns the threat model)**
- Store index signed with an offline-rooted ed25519 key (minisign/signify-style detached signature)
  over a static JSON index; each entry pins the package SHA-256 (NVDA pins sha256 per entry too,
  `addonStore/network.py:392-395`, but has no signature over the index).
- Publisher key per add-on id, trust-on-first-publish; updates must be signed by the same key or
  carry a rotation statement signed by the old key.
- Tiers: first-party (shipped), store-reviewed (stable), store-unreviewed (beta/dev), sideloaded
  (explicit confirmation, permissions read aloud).
- **Reproducible build** badge: store CI builds the component from `source` at the tag and compares
  hashes (cargo `--locked`, pinned toolchain); a mismatch blocks "reviewed".

**Channels**: stable, beta, dev (as NVDA, `models/channel.py:12-46`), per-add-on update channel,
plus `external` for sideloads. Updates go through `Staged` with automatic rollback (4.6).

**Compatibility metadata**: derived from the component (imported/exported WIT names and versions),
recomputed by store CI against every supported host major; the index lists the host range per
version. No human "last tested" gate.

**Review requirements (store-reviewed tier)**: source available and licence SPDX-valid; reproducible
build; manifest lint (permissions minimal for the declared features, every permission has a reason,
English catalog complete, docs present); automated run of the add-on's own recorded-trace tests in
the `gorca-ext-testkit` harness; fuzzed inputs against its hooks (no traps in 10^5 calls, CPU within
budget); a human review for `act`, `observe-all-apps`, `observe-typing`, `net`, `clipboard-read`.
Index carries `reviewURL`-style feedback link and submission time as NVDA does.

**Ratings/feedback**: out of scope for ADR-0020's first cut; a per-add-on issue link suffices.

---

## 6. Phased plan

| Step | Content | Milestone | Owner agents | Depends on |
|---|---|---|---|---|
| A | ADR-0019 accepted (this design), ADR-0008 amendment 4 (web hooks superseded by the `site` world), ADR-0001 §10 amended; decide ADR-0015's fate (fold into ADR-0019 §versioning) | before M18 code | architect, reviewer | — |
| B | **M18a platform core**: WIT 0.9 worlds + open vocabularies + `msg-ref` + effects/reactor + request/deliver (node, children, text, doc-url) + timers + command ctx; selectors/patches/categories generalised from the Telegram branch to desktop + web; `route-changed`; manifest v1 (id, version, permissions, settings schema, data contributions); global world with mailbox; precompile cache; lifecycle state machine incl. staged update and rollback; user add-on dir; golden-component CI; port `ext-vte`, `ext-gnome-shell`, `ext-telegram-web` | M18 (first in Phase 3, per 2026-10-17 decision) | wasm-engineer (lead), rust-engineer, test-engineer, reviewer | M11 state dir (or a temporary path), M12a (done) |
| C | **M18b SDK**: `gorca-ext-sdk` (MIT OR Apache-2.0), `cargo xtask new-ext` templates per world, `gorca addon pack/validate/lint`, `gorca-ext-testkit` (replays recorded event traces and fake mirrors against a component, no desktop needed), guide + reference, one example per world | M18 | wasm-engineer, tech-writer | B |
| D | Wire contributions into Phase 3 features as they land: categories → elements list (M15/M15a chooser sources), settings schema → settings app (M14), vendor profiles (M14a), dictionaries/symbols/cue themes (M13), braille flash (M10) | M13-M15 | owners of those milestones | B |
| E | P1 capabilities: `net:` HTTP queries, clipboard, storage 1 MiB, chooser/prompt effects, speech filter for `global`, context tags (PF-3), review providers | Phase 3 stretch (M18c) or Phase 4 | wasm-engineer, linux-platform-engineer | B, M15a |
| F | **Store**: ADR-0020 (packaging, signing, index, channels, review pipeline, reproducible builds), `.gorca-addon` installer UI in settings app and in-reader, store index repo with CI | Phase 4 | architect, go-engineer or rust-engineer, reviewer | B, C |
| G | **WIT 1.0 freeze**: after ≥ 3 third-party add-ons in at least two worlds have shipped against 0.9.x for one release cycle without needing a breaking change | Phase 4 | architect | F |

ADRs to write: **ADR-0019** extension platform (draft in this change, Proposed); **ADR-0020** add-on
packaging, signing and store (Phase 4); ADR-0008 amendment 4; ADR-0001 §10 amendment. ADR-0015
("extension API stability", reserved in `docs/plan/phase-3.md`) is covered by ADR-0019 §versioning;
recommendation: retire the 0015 number in the plan rather than write a second document.

---

## 7. Contested choices and validation

**C-1 Node data model across the boundary** (contested; small bake-off or spike):
(A) frozen `node-facts` records + accessor imports (sketched above), (B) a `node` resource handle
with lazy getter methods backed by the mirror, (C) A for inline hooks and B for reactor hooks.
Criteria, measured on the existing benches: `present` p99 on the M4 Shell walk and `describe` p99 on
Telegram 3000 chats (gate: within 1.2x of 0.2/0.3 numbers, describe ≤ 20 µs p99), guest allocations
per call, X1 gate unchanged, and a compatibility test (add a node property in a minor; the old golden
component must still link). Recommendation: run a two-day spike on (A) vs (B); escalate to a
`/bakeoff` only if the spike's p99 differ by more than 2x or (A) fails the compat test design.

**C-2 Global add-ons: singleton vs per-app** — decided (singleton) on memory and shared-state grounds;
validate with the storm gate below.

**Metrics** (to land in ADR-0019 §Metrics):

| ID | Metric | Target |
|---|---|---|
| XP-1 | keypress → speech p99 with 10 matching app-module add-ons loaded | unchanged vs none (within 0.5 ms) |
| XP-2 | aggregate inline extension CPU per presentation p99 | ≤ 1 ms (cap 3 ms) |
| XP-3 | first focus of an app with a matching cold add-on (precompiled) | instantiate p99 ≤ 2 ms, off the Reader |
| XP-4 | 10k ev/s storm with 3 global add-ons subscribed to everything | Reader p99 unchanged, hub never blocked, drops counted not lost silently |
| XP-5 | golden components of every released minor link and pass on the current host | 100 % in CI |
| XP-6 | staged update + automatic rollback after a crash-looping version | rollback ≤ 1 s, one notice, settings intact |
| XP-7 | fuzzed manifests and packages (zip bombs, path traversal, huge catalogs) | 0 host crashes, bounded memory |
| XP-8 | data-only add-on (100 patches, 32 categories) on a 50k-node page | patch cost ≤ 100 ns per upserted node p99 |

---

## 8. Risks and open questions

- `@unstable` feature gates and multi-version linking are verified in the wasmtime 49 sources, not
  yet exercised in gorca's host; a spike must confirm `bindgen!` with features and two majors linked.
- The open-numeric vocabulary weakens Rust type safety for guests; the SDK must wrap codes in
  non-exhaustive enums with an `Unknown(u16)` arm.
- The selector/patch language is a new mini-language; its cost model on 50k-node pages (XP-8) and the
  ancestor-chain evaluation need measurement (math researcher may bound it).
- `observe-all-apps` + `net` together is an exfiltration channel by design (a translator needs
  both); the reviewer's threat model decides whether that pair needs a stronger tier.
- Speech backends as add-ons (native processes) are out of the WASM model; a native tier needs its own
  trust rules (Phase 5 at the earliest).
- Owner questions: (Q-A) are sideloaded add-ons allowed without the store, with a spoken warning?
  (recommended: yes); (Q-B) should store add-ons default to the beta channel during the 0.9 preview?
  (recommended: dev/beta only until WIT 1.0); (Q-C) is "reproducible build" mandatory for the
  reviewed tier from day one? (recommended: yes).
