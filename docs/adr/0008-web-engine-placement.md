# ADR-0008: Where web support lives — core engine, connector quirk tables, extensions for policy

Status: **Accepted** (2026-10-15). Owner: architect. Milestones: M5 (extension host, B-4), M8 (Firefox
NVDA parity), M9 (Chromium). Amends ADR-0001 §10 and `docs/plan/phase-2.md` §M5, §4 (B-4) on acceptance.

Inputs: `CLAUDE.md`; ADR-0001 §5.6, §10, §11; ADR-0003 §6, §14, §15, §21; `docs/design/browse-mode.md`;
`docs/plan/phase-2.md` §M5–M9; measurements in `docs/reports/wasm-hook-overhead.md`
(`bench/wasm-hook/`, `bench/results/wasm-hook/2026-10-15-*.tsv`); code as of 2026-10-15:
`crates/gorca-doc-source-atspi/src/{factory.rs,source.rs,warm.rs}`, `crates/gorca-core/src/builtin_ext.rs`,
`crates/gorca-ext-api/src/api.rs`.

## 1. Context

The owner asked whether improving web support through plugins is useful or harmful, and whether
Firefox and Chromium should instead go into the core with everything else as extension plugins.

What exists today:

- **Generic web engine, already engine-neutral:** `gorca-doc` (arena, treap order index, flat text,
  tables, live-region registry), `gorca-doc-source-atspi` (AT-SPI `DocSource`), the doc actor and
  translate step, browse mode and quick navigation in `gorca-core`. Everything keys on AT-SPI document
  roles and `SourceCaps`, not on browser names.
- **Per-engine quirks found so far:** Gecko counts text offsets in UTF-16 units; Gecko needs a cache
  warm-up before its tree is complete; Blink emits `caret(0)` as scroll-to-anchor; Blink reports static
  text through `children-changed`; Blink reorders keyed lists silently. They sit in
  `gorca-doc-source-atspi` today as ad-hoc booleans (`factory.rs` `is_gecko`/`is_blink`,
  `SourceConfig::browse(text_unit, chromium)`, `warm.rs`).
- **The plan:** ADR-0001 §10 lists `ext-firefox` and `ext-chromium` as first-party WASM extensions;
  ADR-0003 §15 already splits "browser quirk normalisation" (connector) from "browser-specific policy"
  (extensions). B-4 was to decide "WASM only vs dual native + WASM" for first-party extensions, judged on
  X1, `ext-vte` under storm and the Shell walk.

The web engine also serves Thunderbird (Gecko), Electron apps and Chromium-embedded apps (Blink),
WebKitGTK apps (Epiphany, Evolution's mail view, GNOME Help, Yelp) and GTK WebView. A cut by product name
("Firefox", "Chrome") does not match the code that is actually shared.

### 1.1 Prior art

| Reader | Generic web engine | Per-engine quirks | Per-product / per-app polish |
|---|---|---|---|
| NVDA | in core: `source/browseMode.py`, `source/virtualBuffers/__init__.py` | in core, per engine: `nvdaHelper/vbufBackends/{gecko_ia2,webKit,mshtml}` (C++, injected), `source/virtualBuffers/gecko_ia2.py` (809 lines), `source/NVDAObjects/IAccessible/{mozilla.py,chromium.py}` (299 / 205 lines) | `source/appModules/` (Python, in-process, no isolation): `thunderbird.py`, `msedgewebview2.py`; no Firefox or Chrome app module |
| Orca | in tree: `scripts/web/{script.py,script_utilities.py,speech_generator.py}` (≈ 6,600 lines) | in tree, per toolkit: `scripts/toolkits/Gecko/script.py` (73 lines), `scripts/toolkits/Chromium/script.py` (128 lines, all per-event handlers: caret, children-added/removed, focus, selection) | `scripts/apps/` (Python, in-process) |
| VoiceOver | WebKit support inside the reader and WebKit itself | n/a (one engine) | none exposed |

Both mature open readers converge on: a large generic engine in the core, a thin per-engine layer that
runs on every event (hot path), and app modules for product polish. Neither isolates any of it; NVDA's
and Orca's crash and hang history (`docs/reports/2026-10-12-architect-prior-art.md`) comes from that, not
from where the browser code sits.

## 2. Measurements (summary of `docs/reports/wasm-hook-overhead.md`)

wasmtime 49.0.3, Cranelift, synchronous component calls, P-core of an Intel Core Ultra 9 285H, the same
Rust quirk logic in both arms:

| Hook | WASM p50 / p99 | Native |
|---|---|---|
| empty call | 35 / 41 ns | — |
| per-key `on-key(node) -> option<bool>` | 68 / 75 ns | 11 ns |
| per-key `present(node) -> option<list<segment>>` | 143 / 164 ns | — |
| batch normalisation, per delta (marginal) | `classify` (verdict bytes) 38 ns; `normalize` (full rewrite) 80 ns | 1.9 ns in place |
| batch of 32 deltas | `classify` 1.1 µs; `normalize` 2.2 µs | 47 ns |
| host import `get-node` per call | ≈ 66 ns (walk of 1,000: 66 µs p50, 69 µs p99) | < 1 ns (stand-in) |
| trap → fresh instance → first call | 31 µs p99 (8.9 µs pooled) | — |
| infinite loop with a 2 ms budget → trap | 2.17 ms p50, 2.81 ms p99 | no protection |

Against the budgets: X1 (≤ 100 µs) passes by ~600×; one hook per quick-nav key is < 0.01 % of W1
(5 ms); 10k events/s with one call per event costs 0.15 % of a core (0.035 % batched by 32); trap
recovery is 0.06 % of the 50 ms gate. **The call boundary is not the problem.** Two things are:

- **Chatty host access scales with the page.** A per-key algorithm that walks nodes through `get-node`
  costs ≈ 66 ns × nodes: 6.6 ms on a 100k-node page, which alone breaks W1. Index queries must be native
  and in-process with the mirror.
- **Interface reach.** Quirk normalisation runs inside the connector's IPC planning (warm-up, P2P fetch,
  revalidation) and over connector-level data (raw offsets, `children-changed` shapes). Exposing that to a
  WASM world means connector types in WIT and an IPC capability for extensions; ADR-0001 §10 forbids
  both, and CLAUDE.md forbids the core importing connector types.

## 3. Decision

Adopt **option C (tiered)**, refined: the cut is **engine-neutral core / per-(connector, engine) data in
the connector / per-product policy in extensions**, not "browsers in core".

### 3.1 Tiers

| Tier | What | Where | Runs | Isolation |
|---|---|---|---|---|
| W0 core web engine | document mirror, treap index, flat text, lines, tables, live-region policy engine, SPA burst classification, route-swap inference, position restore, browse/focus SM, pass-through default predicate, structural navigation, elements list, find | `gorca-doc`, `gorca-core` (as now, ADR-0003 §15) | per key and per event, native, lock-free reads | actor supervision (ADR-0001), no special case |
| W1 engine quirks | Gecko / Blink / WebKit normalisation of one connector | `gorca-doc-source-atspi` as an **engine profile table** (§3.2); a future Newton source has its own (likely near-empty) table | per event, inside the source, before translate | doc actor supervision; table rows are data, so a bad row cannot hang |
| W2 product policy | Firefox UI noise filters (autocomplete, page-switch stale state, Mozilla 1867044 / 1882794 / 1879750), PDF.js viewer rules, Electron detection, Chrome omnibox popup, Thunderbird message list, site rules | `ext-firefox`, `ext-chromium`, `ext-thunderbird`, data files for site rules | at apply time (nav units, ADR-0003 §15), on `on-event` verdicts, `present`, `pass-through-verdict`, `live-politeness`, commands | extension host (§3.3) |
| W3 app polish | GNOME Shell, VTE policy (T-2 classification), LibreOffice, third-party | `ext-*` | same hooks | extension host (§3.3) |

Rule (keeps ADR-0001 §10): a workaround needed by two W2 extensions moves down to W1 if it is an engine
artefact, or to W0 if it is behaviour.

### 3.2 Engine profiles: hot-path normalisation as data

`gorca-doc-source-atspi` replaces the `is_gecko`/`is_blink` booleans with one table:

```rust
/// Selected once per app from AppInfo (toolkit name + ToolkitVersion + app name), never per event.
pub enum Engine { Gecko, Blink, WebKit, Unknown }

pub struct EngineProfile {
    pub engine: Engine,
    pub versions: VersionRange,        // toolkit version gate; empty = all
    pub caps: SourceCaps,              // text_unit, silent_reorders, settled_markers, ...
    pub warm: WarmPolicy,              // None | GeckoCacheWarm { deadline }
    pub rules: &'static [QuirkRule],   // evaluated in order, first match wins
}

pub struct QuirkRule {
    pub id: &'static str,              // stable id, e.g. "blink.caret0-scroll-anchor"; logged, testable, disableable
    pub on: RawPattern,                // event kind + predicates over flags/role/offsets (closed enum)
    pub action: QuirkAction,           // closed enum below
}

pub enum QuirkAction {
    Drop,                              // e.g. Blink caret(0) on a non-focused node
    ConvertOffsets(TextUnit),          // e.g. Gecko UTF-16 → scalar, needs the payload or the run cache
    RetargetToParentText,              // e.g. Blink static text via children-changed
    MarkParentRevalidate,              // e.g. Blink silent reorder
    TreatRemoveAs(RemoveSemantics),
    Code(fn(&mut RawDelta, &SourceState) -> Verdict),  // escape hatch, reviewed per rule
}
```

- Every `match` on `QuirkAction` and `Engine` is exhaustive (CLAUDE.md).
- `Code` rows are ordinary Rust in the connector; a rule needs `Code` only when no data form fits, and
  the reviewer checks each one.
- A user-level `disabled_quirks = ["blink.caret0-scroll-anchor"]` setting turns a row off without a
  rebuild when a browser update fixes the bug. Rows cannot be added from config (no code from data).
- WebKitGTK gets a profile in M9 or later; `is_web` stops being "Gecko or Blink" and becomes "a profile
  exists or the document role is present", so Epiphany and Yelp use W0 with `Engine::Unknown` defaults.

### 3.3 Extensions: one API, two builds of the same source

The extension API stays `gorca-ext-api` (WIT world + Rust trait mirror). First-party extensions are
written **once** against the Rust trait and built **two ways**:

- `wasm32-wasip2` component, run by `gorca-ext-host` (default for shipping): fuel or epoch limit,
  32 MiB, trap → reset, three resets in 60 s → disabled.
- native, linked in-process (tests, debugging, and an emergency fallback per extension): hook called in
  the app actor under `catch_unwind`; a post-hoc deadline check counts overruns as faults with the same
  three-strikes rule. Native cannot pre-empt a hang; the app actor's watchdog (ADR-0001) restarts the
  actor.

```rust
pub enum ExtBackend { Wasm, Native }   // per extension, from the manifest; Native only for in-tree first-party
```

Health stays the existing `InstanceGuard` / `InstanceHealth { Active, Disabled }` in
`gorca-ext-host/src/health.rs`: every fault resets the instance (WASM: new `Store` from `InstancePre`;
native: the extension value is dropped and rebuilt) and is recorded; the third in 60 s makes `Disabled`
sticky for that app. Both backends feed the same guard. A fault is a trap, a panic, an epoch/fuel
interrupt, or a native call over its CPU limit.

A thin adapter macro in `gorca-ext-api` (`export_extension!(MyExt)`) maps WIT exports onto the trait
inside the guest and the trait onto the host for the native build, so one crate compiles both ways.
Measured overhead makes WASM the default without a latency cost; native exists for debuggability and
fault triage, not speed.

### 3.4 Invariants

- **WE-1** The core never branches on an application, product or engine name. It sees `SourceCaps`,
  document roles and extension verdicts only. (`builtin_ext.rs` already follows this; keep it.)
- **WE-2** Per-event normalisation runs in the source as a table lookup plus at most one `Code` call per
  delta; no extension hook runs between the raw event and the dirty mark.
- **WE-3** No extension hook runs per key over many nodes. Per key, at most one hook call per extension
  (`pass-through-verdict`, `present`, `command`); extension nav units are computed at apply time
  (ADR-0003 §15) into reserved category bits, so quick nav stays an index query.
- **WE-4** Batch hooks use the patch form (verdict per item, `list<u8>` or `list<tuple<u32, verdict>>`), not
  full rewrites; measured 2× cheaper.
- **WE-5** Every extension call has a CPU limit (2 ms hooks, 20 ms commands) enforced by epoch for WASM
  and detected after the fact for native.
- **WE-6** Quirk rule ids are stable and appear in the presentation log when a rule fires (debug level), so a
  wrong drop is traceable to one row.

### 3.5 Failure modes

| Failure | Effect | Containment |
|---|---|---|
| A quirk row matches too much (e.g. drops real caret moves) | missed speech in one engine | rule id in the log; user `disabled_quirks`; corpus test per row |
| `Code` quirk panics | doc actor panics | supervisor restarts the doc actor, rebuild-as-resync (ADR-0003 §9.5) |
| W2 extension loops | its hook trapped after ≈ 2–3 ms; event passes with `Pass` | reset; three strikes → disabled, one announcement |
| W2 extension returns garbage | verdicts are a closed variant; text goes through the verbalizer | WIT lifting rejects malformed values (trap → reset) |
| Native-build extension hangs | app actor stalls | app-actor watchdog restart; extension marked faulted; why native is not the default |
| Browser update changes a quirk | new mis-behaviour or a now-redundant fix | version gate in `EngineProfile`; parity corpus per browser version in CI |

## 4. Alternatives

### A. Everything per app in WASM (engine, quirks, policy as `ext-firefox`, `ext-chromium`)

- Safety: best fault isolation for browser code.
- Latency: calls are cheap (§2), but the engine would live in guest linear memory, so the Reader's
  per-key index queries become host↔guest round trips or a second copy of the mirror; per-key walks at
  66 ns/node break W1 on large pages. The lock-free left-right publication (ADR-0003 §4.8) cannot be read
  across the boundary.
- Interface: WIT must carry connector-level deltas and IPC (warm-up, P2P, revalidation); breaks ADR-0001 §10
  and the core/connector separation; every NVDA-parity change becomes a WIT version bump.
- Duplication: Thunderbird, Electron, WebKitGTK would each need their own copy or a shared WASM "web
  library" — the core engine again, behind a slower ABI.
- Rejected.

### B. Web engine and per-browser quirks in core

- Latency: native, best possible (but §2 shows the gain over C is ≈ 0.1 µs per call).
- Breaks WE-1: core branches on "Firefox"/"Chrome"; Newton/AccessKit sources would inherit AT-SPI
  artefacts or need per-connector branches in core; product policy (PDF.js, omnibox, site rules) lands in
  the core, the exact Orca pattern CLAUDE.md wants to avoid.
- Development speed: highest at first, then core churn on every browser release.
- Partly right: the engine **does** belong in core (that is W0 in C). Rejected as stated.

### C. Tiered (chosen, §3)

Engine in core, quirks as connector data, policy in extensions with WASM as the default build. Costs a
quirk-table refactor (small) and the dual-build adapter (M5).

### C′. Tiered with first-party extensions native only (WASM for third party)

- Faster debugging; no hang pre-emption for first-party code that runs on every event (`ext-firefox`
  `on-event`, `ext-vte`), which is exactly where Orca's hangs came from. Measured gain ≈ 0.1 µs/call.
- Kept as a per-extension fallback (`ExtBackend::Native`), not as the default.

## 5. Does a Newton / AccessKit connector change the answer?

It strengthens it. AccessKit trees carry explicit child order, UTF-8 `character_lengths` and atomic
`TreeUpdate`s (`refs/accesskit/accesskit/src/lib.rs`; consumed in
`refs/accesskit/accesskit_consumer/src/text.rs`), so silent reorders, `children-changed` text and
UTF-16 offsets are AT-SPI artefacts that a Newton source will not have. The quirks are a property of
(connector, engine), which is why they live in the connector's profile table and not in the core (B) or in
a product extension (A). W0 is unchanged and serves both connectors; W2 product policy (Firefox UI noise,
PDF.js) is connector-independent and stays in extensions.

## 6. Consequences

- M8 and M9 work goes into W0 (`gorca-doc`, browse code in `gorca-core`) and W1 (profile rows); `ext-firefox`
  and `ext-chromium` start small (policy only) and can stay native until M5 lands.
- `gorca-doc-source-atspi`: replace `is_gecko`/`is_blink`/`SourceConfig::browse(_, chromium)` with
  `EngineProfile` (one PR, behaviour-preserving, M8 prep). Add `ToolkitVersion` to `AppInfo` for version
  gates.
- WIT 0.2 (`gorca:ext@0.2.0`): add `pass-through-verdict`, `live-politeness`, apply-time `nav-match`
  (ADR-0003 §15), patch-form batch verdicts (WE-4). No connector types, no IPC.
- `gorca-ext-api` gains the dual-build adapter; `gorca-ext-host` runs both backends.
- Plan text to amend on acceptance: `docs/plan/phase-2.md` §M5 scope and §4 (B-4), ADR-0001 §10 tier
  table (native first-party becomes a build of the same source, default WASM).

### 6.1 Impact on M5

M5 scope becomes: wasmtime host (one `Store` per extension × app, epoch, pooling allocator, 32 MiB,
trap → reset, three strikes), manifest matcher, keymap merge, settings KV, **dual-build adapter**, WIT 0.2
hooks above, `ext-gnome-shell` ported, `ext-vte` skeleton, a faulty test extension. Out of M5: anything
in W0/W1.

### 6.2 B-4: no longer a performance bake-off

B-4's open question was whether component-model overhead on hot paths forces native first-party
extensions. §2 answers it: ≤ 0.2 µs per hook call, 38 ns per batched delta, 31 µs trap recovery. With the
dual build, WASM vs native is a manifest flag over the same source, so two competing implementations
would measure the same code twice. Recommendation:

- **Retire B-4 as a bake-off.** Replace it with an M5 acceptance benchmark run on both builds of the same
  extensions: X1, `ext-vte` `on-event` under the V2 storm, `present` on the M4 Shell walk, plus the fault
  suite. WASM is the default unless it misses a gate that native meets.
- If the owner still wants a bake-off in M5, the contested choice left is **WIT batch-hook shape and
  host-access model** (patch verdicts with read-only snapshot views vs per-call `get-node`), judged on
  `ext-vte` under V2 and a 10k-node apply-time `nav-match` pass. That is a narrower, cheaper bake-off and
  is only worth running if the first `ext-vte` port misses V1/V2.

## 7. Metrics to validate

| ID | Metric | Target | Source |
|---|---|---|---|
| WM-E1 | hook call overhead, WASM, p99 | ≤ 1 µs (X1 gate 100 µs) | measured 0.16 µs; re-measure with WIT 0.2 in `gorca-ext-host` |
| WM-E2 | quirk-table cost per raw event in `AtspiDocSource` | ≤ 100 ns p99, ≤ 5 % of the 2 µs ingest budget | MB bench on the profile table |
| WM-E3 | L3-50k storm with `ext-firefox` and `ext-chromium` loaded | `lost = 0`, `stale = 0`, extra CPU < 1 % of a core | PR-2 e2e |
| WM-E4 | W1 at 100k nodes with extension nav units loaded | unchanged vs none (≤ 5 ms p99) | M8 e2e |
| WM-E5 | trap recovery / hang interruption | < 50 ms / ≤ budget + 1 ms | measured 31 µs / 2.8 ms p99; fault suite in M5 |
| WM-E6 | core diffs needed to add a quirk row or a product rule | 0 lines in `gorca-core` / `gorca-doc` | PR audit over M8/M9 |
| WM-E7 | every quirk row covered by a corpus test that fails without it | 100 % | `tests/web/` |
| WM-E8 | WebKitGTK page in Epiphany usable with W0 + `Engine::Unknown` | browse, headings, links work | M9 smoke |

## 8. Open questions

- Q-E1 (owner): accept retiring B-4 as a bake-off (§6.2)?
- Q-E2: `call_async` with epoch yield instead of trap for long `command` hooks — fiber switch cost not
  measured.
- Q-E3: memory per WASM instance across ~30 apps × 4 extensions with the pooling allocator (not measured).
- Q-E4: whether `ConvertOffsets` for Gecko needs the run cache (TX-1) or the event payload suffices for
  every event kind; web-a11y-engineer to confirm on the corpus.
  **Answered 2026-10-15 (M3b): neither is needed, because nothing is converted.** Gecko's ATK text is the
  UTF-16 string with a phantom U+FEFF after each astral scalar (`DOMtoATK.h`), so every offset it sends
  (caret events, text-changed offsets and lengths, `GetStringAtOffset` ranges, selections, `GetCharacterCount`)
  is the index of a character of that string, verified on the private Firefox with `a😀b😀😀c` and Cyrillic
  (`tests/web/m3b/probe_fields.py`; gate FC1h: 144 keys, 0 lost). The Reader's window keeps that string and the
  unit rules keep a phantom with its scalar. Only the insert text of `text-changed` lacks phantoms (its length
  has them): the Reader puts them back from the length. Measured alternatives (`crates/gorca-text/tests/qe4.rs`,
  host load 13, governor performance): converting a caret offset with the text at hand costs 0.4 µs at 1k
  scalars, 5.8 µs at 16k, 37 µs at 100k and 379 µs at 1M, and needs the text in front of the caret, i.e. an IPC
  read proportional to the caret's position; a run cache would be a sorted list of phantom offsets (lookup
  5 to 10 ns, build 12 µs to 0.5 ms, an edit shifts every later entry) that must follow every edit. The chosen
  path costs 6 ns per character unit without emoji and about 0.2 µs with emoji in the window. The browse
  mirror's text runs are rebuilt whole from `GetText` on each change (never spliced by offset), so they need no
  conversion either. A Newton source has no phantoms, so its profile row is empty. The connector's quirk table
  keeps `Gecko => Utf16Bom` (`TextUnit::window_follows`), plus the attribute-name table of `info.formatting`.


## 9. Amendment 1 (2026-10-15, M5 implementation)

M5 shipped the host as designed in section 3.3, with these deliberate differences:

- **WIT stays 0.1 in M5.** The 0.2 hooks of section 6.1 (`pass-through-verdict`, `live-politeness`,
  apply-time `nav-match`, patch-form batch verdicts) move to M7, with the first extension that needs
  them (phase-2 plan: "TBD-WEB-7 hooks stay for M7"). 0.1 gained only additive pieces needed by the first
  ports: `get-setting`, the `document-web` role, and the property mask on `properties-changed`.
- **Fuel is the CPU limit, epoch is the wall-clock backstop** (10x). Epoch alone measured wrong under
  load: a descheduled healthy hook is interrupted and counted as a fault (WE-5 is a CPU limit).
  `FUEL_PER_MS` is calibrated on the dev host. Native keeps the post-hoc check.
- **The hook traits live in `gorca-core`** (`ExtSource`, `AppExt`, `ExtSink`, `ExtCommands`); the host
  implements them. The core still never links the host or wasmtime (rules 1, 3; new xtask rule 9).
- **No `wasmtime-wasi`.** Pure WASI clock/random imports are linked by the host; the rest traps.
- Q-E3 (memory per instance) is measured in `docs/reports/m5-impl.md`.

## 10. Amendment 2 (2026-10-17, ext-vte port)

`ext-vte` follows the `ext-gnome-shell` precedent of amendment 1: a component by default, the same source built natively as the exclusive fallback (`--terminal-policy=native`).

- **WIT 0.2.0** adds the `terminal` export (`configure`, `step`, `snapshot`, `counters`; the interface existed since M6 but was not in the world), `raw-key` and `term-input::raw`. The web hooks of section 6.1 move to a later version: the editing polish batch took 0.3.0 for the terminal additions (`term-config.word-level`, `term-config.boundary`, `term-action.cue`; pre-1.0 versions carry no ABI promise).
- **No hook-shape bake-off.** This ADR said one is warranted only if `ext-vte` missed V1/V2: the measured WASM V1 p99 is 32.8 to 33.4 ms (gate 35) and V2 reader CPU 1.6 to 2.2 % (gate 60), indistinguishable from native under the same load (docs/reports/ext-vte-wasm.md). The per-input host cost is 0.4 us and the largest screen step 0.46 ms.
- **The extension is a pure function** (`step(now, input) -> actions`), so `host::fetch`/`text` stay unsupported: the screen is an action (`fetch-screen`) the core executes and answers as data.
- **Product rules stay in the extension, safety rules stay in the core**: readline bindings are the extension's, but the "never log or lose the sensitive marking of a password" rule is enforced by the core independently of the extension (fail closed).
