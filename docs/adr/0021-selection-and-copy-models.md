# ADR-0021: Browse-mode selection and copy models (page vs virtual, one Ctrl+C)

Status: **Proposed** (draft, 2026-10-17). Owner: architect. Analysis, prior art and probe plan:
`docs/reports/selection-modes-architecture.md`. ADR-0020 is reserved by ADR-0019 (packaging, signing, store).
Relates to: ADR-0001 R3 (results as messages), ADR-0003 §21 (browse mode), ADR-0005 (keymap, explicit save),
ADR-0008 (engine quirks in the connector), ADR-0019 (effects, worlds), `docs/plan/phase-3-features.md` §5.4
(K1/K2/K3, superseded in part), `docs/plan/phase-3.md` M15, P-14. Milestones: S0 now, S1, S2 = M15 slice, S3 later.

## 1. Context

In browse mode gorca selects inside its virtual document (Shift+arrows, Ctrl+A are grabbed). The browser's own
Ctrl+C then copied nothing. Branch `worktree-agent-aa24d6c83ee085224` (435d8a13) mirrors the virtual selection into
the page with AT-SPI `Text.SetSelection` 40 ms after the last change, so the browser copies it; `gorca+Shift+F10`
turns mirroring off. Review found side effects: clearing via `SetSelection(0,0,0)` moves focus (R2-1), a start in
text directly under the document blurs focus (R2-2), page `selectionchange` fires on every burst (R2-3), Gecko
glues multi-range copies without separators, Blink keeps one exclusive range, links are not mirrored.
A clipboard writer through a started Mutter RemoteDesktop session works but lights the "screen sharing" indicator
(rejected). Mutter 48 has no data-control protocol.

The owner asks for `gorca+Shift+F10` to switch between two models in which Ctrl+C "just works": (A) page selection,
the browser copies with formatting; (B) virtual selection, gorca copies what the user heard. This is NVDA's design
(`refs/nvda/source/cursorManager.py:679-688`, `browseMode.py:2951-2988`), except that NVDA intercepts Ctrl+C in
browse mode always and re-sends it in native mode; gorca cannot re-send a swallowed key without injection.

## 2. Decision

### 2.1 State

```rust
/// Which selection Ctrl+C copies in browse mode. Profile key `browse.selection_model`.
pub enum SelectionModel { Native, Virtual, Auto /* S3 */ }

/// Connector's answer for one mirror generation.
pub enum MirrorFidelity { Exact, Approximate, NotMirrored(NotMirroredReason) }
pub enum NotMirroredReason { FocusableStart, BackgroundDocument, Unsupported, Refused, Deadline }

/// Per browse document; the Reader owns it.
pub enum NativeSync {
    Idle,                                   // no native selection set by us
    Pending { gen: u64 },                   // one request in flight
    Applied { gen: u64, fidelity: MirrorFidelity },
}
```

`VRange` (virtual selection) always exists in both models. In `Virtual`, `NativeSync` stays `Idle` and the page is
never written.

### 2.2 Transitions

| From | Event | To / effect |
|---|---|---|
| any model | `VRange` changes (settled 40 ms) in `Native` | `Pending{gen+1}`; `DocAction::SetSelection{gen}`; older pending dropped |
| `Pending{g}` | `NativeSelApplied{g, f}` | `Applied{g, f}`; `f != Exact` announced once per burst ("page selection incomplete") |
| `Pending{g}` | result for `g' < g` | ignored |
| `Applied`/`Pending` | plain move (range collapses) | `DocAction::ClearSelection` (`RemoveSelection` only), `Idle` |
| `Native` | `gorca+Shift+F10` | clear native if we set it; model `Virtual`; say "virtual selection" |
| `Virtual` | `gorca+Shift+F10`, connector caps lack native selection | stay; say "page selection not available here" |
| `Virtual` | `gorca+Shift+F10` | model `Native`; mirror current `VRange` now; say "page selection" |
| `Native`, Browse | switch to Focus mode | collapse native selection to the virtual cursor |
| any | document reload / leave | `VRange` dropped, `Idle`, nothing written |

Toggle scope: the current browser application for this run. Persisting is explicit through the profile save
command (ADR-0005), so the ADR-0016 Site layer gives per-site defaults. (Owner QS-1.)

### 2.3 Ctrl+C routing

Grab predicate (one function, exhaustively tested over presets, layouts, keysyms):
`grab(Ctrl+C) ⇔ mode == Browse ∧ model == Virtual ∧ focus is a browse document`.

- `Native`: Ctrl+C is never grabbed; the browser copies. gorca sees the unswallowed key and, if a clipboard owner
  watcher exists, says "copied" when the owner changes within 500 ms; `Applied{NotMirrored}` says "page selection not set".
- `Virtual`: gorca copies `CopyProjection(VRange)` through `ClipboardSink`; "no selection" when collapsed; "copied"
  or "copy failed" from `CopyDone`. Without a sink: "virtual selection cannot be copied yet; gorca+Shift+F10 switches to
  page selection".
- `browse.copy_as_text` (binding TBD by the keymap owner) runs the `Virtual` copy in either model.

### 2.4 Placement

| Part | Crate / layer |
|---|---|
| `SelectionModel`, `VRange`, `NativeSync`, grab predicate, messages, `CopyProjection` | `gorca-core` browse |
| apply/clear/collapse strategies: `DocTextSelections` (AT-SPI `Document.SetTextSelections`, Blink with ATK ≥ 2.52), `HypertextRanges` (Gecko `Text.Set/Add/RemoveSelection`), Newton via AccessKit `SetTextSelection`; fidelity report | connector `DocSource` |
| `ClipboardSink` port (`set_text(req, ClipText)`, owner-changed stream); backends `ShellHelper` (St.Clipboard), `MutterRdClipboard` (no `Start`), `Null` | trait in core, impls in a platform crate |
| per-site default, projection tweaks | profile data / ADR-0019 data-only and site worlds |
| extension copy | ADR-0019 effect `clipboard.write`, capability-gated, via the same sink |

### 2.5 Invariants

- I-1 The core never imports AT-SPI, Mutter or Shell types; strategies and backends are behind `SourceCaps` and the sink trait.
- I-2 The Reader never awaits: mirror and copy are effects; results return as `ReaderMsg::NativeSelApplied{gen,..}`
  and `ReaderMsg::CopyDone{req, Result}`.
- I-3 At most one mirror request in flight per document and one clipboard write in flight; newer supersedes older
  (latest-generation wins, no queue growth).
- I-4 Deadlines: mirror apply 250 ms total; clipboard write 500 ms; pipe writes non-blocking so a dropped future
  closes the fd.
- I-5 gorca never clears a native selection it did not set; clearing never uses `SetSelection(0,0,0)`.
- I-6 Ctrl+C is grabbed only under the 2.3 predicate; Ctrl+X/V/Z/Y/Insert/Delete never.
- I-7 Copied and clipboard text is never logged or written to disk; password entries are never in a projection.
- I-8 No path that shows a screen-sharing indicator or a consent dialog is used to write the clipboard.

## 3. Alternatives

- Native only (branch as is): no clipboard writer needed, but page side effects, links and Gecko gluing remain, and
  the user cannot copy what was heard.
- Virtual only (NVDA default): clean, but loses formatting; blocked on a writer.
- Lazy native (mirror at Ctrl+C time): needs Ctrl+C re-injection; RD injection shows the indicator, a ShellHelper
  virtual keyboard re-enters our own grab. Rejected.
- Grab Ctrl+C only while a virtual range exists: re-opens the grab-lag race (review F-4). Rejected.
- Clipboard via portal, started RD session, data-control, own surface, Xwayland: see report §3.2. Rejected or not pursued.
- Per-document toggle (NVDA): resets invisibly on new tabs. Rejected in favour of app-run scope + explicit save.

## 4. Consequences

- The in-flight branch merges as model A after R2-1/R2-2 and the `bool` → `SelectionModel` rename; the regression
  test changes from "Ctrl+C never grabbed" to the 2.3 predicate once model B ships (or once the S0 stop-gap lands).
- Model B depends on K1 (ShellHelper `SetClipboardText` + `ClipboardOwnerChanged`, needs user extensions enabled and
  one login) or K4 (RD clipboard without `Start`, unverified on 48.7). Without either, B announces that copy is unavailable.
- `docs/plan/phase-3-features.md` §5.4 stays valid for K1; K4 is added as fallback; the "K3 toggle = native selection
  on/off" wording becomes "model switch".

## 5. Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| SM-1 | selcopy rig rows (Firefox / Chromium) in model A | 9/9 and 9/9 (Chromium today 7/9; expects `DocTextSelections`, probe P-CB5) |
| SM-2 | focus changes caused by mirror/clear on a skip-link page | 0 |
| SM-3 | model B: Shift+End then Ctrl+C at 5 ms spacing, 200 trials | clipboard equals projection in 200/200 |
| SM-4 | virtual copy of 10k characters → clipboard owned (W6) | ≤ 50 ms wall |
| SM-5 | Ctrl+C key → "copied" SPEAK, model B | p99 ≤ 15 ms gorca-side + sink round trip reported separately |
| SM-6 | Shell indicator / prompt appearances during 100 copies (private shell, AT-SPI tree watch) | 0 |
| SM-7 | grab predicate test over presets × layouts × modes × models | exhaustive, passes |

## 6. Open questions

- QS-1 toggle scope (app run, recommended, vs per document as NVDA).
- QS-2 default after model B ships (recommended: `virtual`; `native` until then).
- QS-3 confirm grabbing Ctrl+C for the whole virtual browse state (recommended) vs only with a range.
- Probes P-CB1..P-CB8 (report §3.3); K4 decided by P-CB3/P-CB4.
