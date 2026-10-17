# Browse-mode selection and copy: two models, one Ctrl+C (2026-10-17)

Author: architect. Status: design consultation, read-only. Companion draft: `docs/adr/0021-selection-and-copy-models.md`
(Proposed). ADR-0020 is reserved by ADR-0019 for packaging/signing/store, so this takes 0021.

Inputs: branch `worktree-agent-aa24d6c83ee085224` (head 435d8a13) and its reports
`docs/reports/ctrl-c-passthrough.md`, `docs/reports/ctrl-c-verify.md` (in that worktree);
`docs/plan/phase-3-features.md` §5.4 (K1/K2/K3); `docs/plan/phase-3.md` M15, P-14;
`docs/platform/org.gorca.ShellHelper.md`; ADR-0001 R3, ADR-0003 §21, ADR-0005, ADR-0008, ADR-0019.

---

## 1. Prior art (read in `refs/`)

### 1.1 NVDA: exactly the owner's switch, but asymmetric

- **Two models, one key.** `cursorManager.py:679-688` `script_copyToClipboard` is bound to `kb:control+c`
  (`cursorManager.py:725`) in browse mode. If `_nativeAppSelectionMode` it calls `gesture.send()` (the
  application copies, rich); otherwise it copies NVDA's own buffer text through
  `textInfos/__init__.py:631-641` (`clipboardText` = `convertToCrlf(self.text)`, `api.copyToClip(..., notify=True)`
  speaks "copied to clipboard"), or says "No selection" when collapsed.
  NVDA **always intercepts Ctrl+C in browse mode** and re-sends it in native mode. gorca cannot re-send a
  swallowed key without injection (section 3), which is the root of our design difference.
- **Toggle.** `browseMode.py:2951-2988` `script_toggleNativeAppSelectionMode` (`NVDA+shift+f10`): turning on
  calls `clearAppSelection()` then `updateAppSelection()`; failure says "Native selection mode unsupported in this
  document" and stays off; turning off calls `collapseAppSelection()` (the app caret lands at the browse cursor).
  Messages: "Native app selection mode enabled/disabled". State is **per browse-mode document** (instance
  attribute), not persisted.
- **Default.** Off. Feature flag `virtualBuffers.nativeSelectionMode` (`config/configSpec.py:231`, default
  disabled) turns it on automatically per new document (`browseMode.py:1863`,
  `virtualBuffers/gecko_ia2.py:722-735` probes clear+update and enables only if both succeed). Word/Outlook:
  always native, cannot be turned off (`UIAHandler/browseMode.py:507`, `NVDAObjects/window/winword.py:1491`).
- **Sync points.** `gecko_ia2.py:49-56`: every browse selection change pushes to the app (eager, like our 40 ms
  mirror) and a collapsed selection **clears** the app selection; only in browse mode, never in focus mode.
  `browseMode.py:2942-2949`: switching to focus mode with native mode on syncs the app selection (caret
  continuity; NVDA fixed a "caret stuck" bug there, `user_docs/en/changes.md:216`).
- **Mechanism.** One cross-object range through `IAccessibleTextSelectionContainer::SetSelections`
  (`gecko_ia2.py:737-797`), start object/offset to end object/offset. Clear = `SetSelections(0, ...)`.
- **UX caveats documented by NVDA** (`user_docs/en/userGuide.md:1218-1245`): native copy loses NVDA-generated
  labels; the native selection "may not always be completely accurate".

### 1.2 Orca: native only, no virtual model

- Orca has no virtual buffer; caret navigation moves the real caret, so selection is always the page's.
  `text_selection_manager.py:813-870`: tries `Document.SetTextSelections` first
  (`ax_document.py:77-118`), then `Text.SetSelection` within one object, then a multi-object fallback.
- Clipboard writes (flat review copy/append) go through a clipboard manager over D-Bus when one runs
  (GPaste2/3, Klipper) else in-process `Gtk.Clipboard` (`clipboard.py:107-158`, 470-560). GTK3's Wayland
  clipboard needs an input serial on a client surface; Orca's fallback is unreliable on Wayland (not measured
  here). Lesson: on GNOME the clipboard writer must live with the compositor, not in the reader.

### 1.3 Platform APIs found while reading

| API | Who implements | Use |
|---|---|---|
| `org.a11y.atspi.Document.SetTextSelections a((so)i(so)ib)` (`refs/at-spi2-core/xml/Document.xml:88-103`) | **Chromium** (`ax_platform_node_auralinux.cc:4758-4810`, needs ATK >= 2.52, exactly one range, cross-object, exact ends); **not Firefox** (no ATK document selection hook in `refs/firefox/accessible/atk/`) | Blink native model: replaces the "single exclusive range on the common ancestor" quirk and probably the Ctrl+A-before-`<input>` failure (7/9) |
| `Text.SetSelection/AddSelection/RemoveSelection` on a hypertext | Firefox (`HyperTextAccessibleBase::SetSelectionBoundsAt`, `TextLeafRange::SetSelection` with `aSetFocus`) | Gecko native model (current branch); clear with `RemoveSelection` only (R2-1) |
| `EditableText.CopyText` | Firefox only on editors (`generic/HyperTextAccessible.cpp:798-805`) | No document copy via AT-SPI |
| AccessKit `Action::SetTextSelection` + `TextSelection{anchor, focus}` (`refs/accesskit/accesskit/src/lib.rs:784`) | Newton apps | Newton connector can do native selection natively |
| `St.Clipboard.set_text / set_content(one mimetype)` (`refs/gnome-shell/src/st/st-clipboard.h:80-88`, `st-clipboard.c:311-320` → `MetaSelectionSourceMemory`) | in-shell | K1 ShellHelper writer |
| `org.gnome.Mutter.RemoteDesktop.Session.EnableClipboard/SetSelection/SelectionWrite` | Mutter; in main the clipboard methods check only the sender (`meta-clipboard-session.c:98-104`, `meta-remote-desktop-session.c:1089-1105`), while the remote-access handle (the Shell indicator) is created only by `Start` (`meta-remote-desktop-session.c:296-313`) | K4 candidate: clipboard-only RD session, **never Start**. Unverified on 48.7 |
| `org.gnome.Mutter.Clipboard` | Mutter main, but only as a sub-object of RemoteDesktop/InputCapture sessions (`meta-remote-desktop-session.c:1644`, `meta-input-capture-session.c:1194`) | not a standalone service |
| ext/wlr data-control | **absent** in Mutter (grep of `refs/mutter/src` main, 2026-10-07: no hits) | rejected on GNOME |
| xdg-desktop-portal Clipboard | needs a started RemoteDesktop portal session: consent dialog + indicator | rejected |

---

## 2. Options for the selection/copy model

Definitions. **Virtual selection** = a range in gorca's flat document (`VRange{anchor, active}`), always kept.
**Native (page) selection** = the engine's DOM selection set over the connector. **Model** = which one Ctrl+C
copies, and therefore whether gorca writes the page's selection at all.

| # | Option | What Ctrl+C copies | Page side effects | Needs clipboard writer | Verdict |
|---|---|---|---|---|---|
| O1 | Native only (current branch, mirror on) | browser's rich copy of the mirrored range | `selectionchange` per burst, focus risk (R2-1/R2-2), links unmirrorable, Gecko glues partial cross-block ranges | no | ship now as model A |
| O2 | Virtual only (NVDA default) | gorca's text as heard | none | yes | needs K1/K4 |
| **O3** | **Switch A/B (owner's idea), gorca+Shift+F10** | A: browser; B: gorca | A: as O1; B: none | B only | **recommended target** |
| O4 | O3 + `auto` value: Native while the mirror is exact, gorca copy when the mirror is impossible or inexact (link start, background tab, unsupported engine, refused call) | rich when possible, text otherwise, announced | as O1 | yes | phase 3; better than NVDA |
| O5 | O3 + a one-shot "copy as text" command (works in both models) | gorca text | none | yes | cheap escape hatch; recommended with O3 |
| O6 | Lazy native: mirror only at Ctrl+C time | rich | minimal | no, but needs Ctrl+C re-injection | rejected: no injection without an indicator (RD) or a ShellHelper virtual keyboard that re-enters our own grab; ordering is fragile |
| O7 | Virtual copy with **two flavours** (text/plain as heard + text/html built from the mirror: headings, lists, tables, links with href) | both, receiver picks | none | yes, multi-mime | phase 4 idea; St.Clipboard is single-mime, needs a custom `MetaSelectionSource` in the helper or the RD backend (multi-mime native) |

Where gorca can beat NVDA:
1. **Copy confirmation in both models.** NVDA is silent after a native copy. gorca watches the unswallowed
   Ctrl+C (KeyboardMonitor reports all keys, grabs only decide swallowing) and listens to clipboard owner
   changes (ShellHelper `Meta.Selection::owner-changed` relay, or RD `SelectionOwnerChanged`); "copied" is
   spoken only when the owner actually changed within 500 ms, "nothing copied" otherwise.
2. **Honest mirror fidelity.** The connector reports `Exact | Approximate | NotMirrored(reason)`. In model A an
   inexact or impossible mirror is announced once per selection burst ("page selection incomplete"), and with
   O4 Ctrl+C silently routes to the gorca copy instead.
3. **A defined copy projection** for model B (section 2.2), not "whatever the buffer text is".
4. **Chromium exactness** via `Document.SetTextSelections` (NVDA uses the IA2 equivalent; Orca already uses it).
5. **Persistence by profile layer and site** (ADR-0016 stack, Site trigger), not just a global flag.

### 2.1 UX for a blind user

- **Words, not beeps** for the state (Q-W1 precedent): toggle says "page selection" / "virtual selection"
  (ru: "выделение на странице" / "виртуальное выделение"); unsupported: "page selection not available here",
  model stays. The current model is also part of the browse status report (`where am I` / mode report).
- **Scope of the toggle:** the current browser application for this run (all its tabs), not per document.
  NVDA's per-document state silently resets on every new tab or reload, which a blind user cannot see. Saving is
  explicit (ADR-0005 rule): `profile.save` writes `browse.selection_model` to the active layer, so a Site layer
  gives per-site defaults ("gmail: page", "docs site: virtual"). Owner question QS-1.
- **Copy feedback:** model B: "copied" / "no selection" / "copy failed" (+ reason class: "clipboard service not
  available"). Model A: "copied" after owner change, nothing if no clipboard watcher exists (NVDA parity);
  `NotMirrored` at Ctrl+C time: "page selection not set". Text is never spoken back or logged (`ClipText`).
- **Mode switch Browse to Focus in model A:** collapse the native selection to the virtual cursor (NVDA
  `browseMode.py:2942-2949`), so the focus-mode caret starts where the user was. In model B the existing
  caret-placement rule applies.
- **Default:** owner call (QS-2). Recommendation: `native` while no clipboard writer exists (it is the only model
  in which Ctrl+C works), `virtual` once K1/K4 is verified, because B has no page side effects, works on links,
  background tabs and every engine, and copies what was heard; formatting users flip to `page` per site.

### 2.2 Copy projection for model B (core, engine-neutral)

`CopyProjection` turns `VRange` over the mirror into text, deterministically, from data the mirror already has:
block break = `\n`, paragraph gap = `\n\n` (no blank-line runs); list item = marker as spoken ("•", "1.") + space;
image/graphic = its name (alt) in place; table: cells joined by `\t`, rows by `\n`; form control = its value
(never a password: ranges stop before `PasswordEntry`, F-7); link = its text (href only in the O7 HTML flavour);
U+FFFC object markers never leak; trailing whitespace trimmed per line. Verbosity labels ("heading level 2",
"link", "list with 3 items") are **not** copied: they are presentation, not content (NVDA also copies buffer text,
not speech). A site extension may adjust the projection through data (ADR-0019 data-only world), not code.

---

## 3. The hard problem: how Ctrl+C reaches gorca in model B, and how gorca writes the clipboard

### 3.1 Getting the key

KeyboardMonitor (`org.gnome.Orca.KeyboardMonitor` ownership) already gives gorca every key event; grabbing
decides swallowing. Rule (state-derived, not selection-derived):

> Ctrl+C (all layouts, both Shift levels, as the existing compile test enumerates) is in the grab set **iff**
> `mode == Browse ∧ model == Virtual ∧ focused object is a browse document`. Never in Focus/Review, never in model A.

Grabbing for the whole browse/virtual state (not only "while a virtual range exists") removes the reviewer's F-4
race (grab lagging selection by up to 150 ms) because the grab set changes only on mode/model transitions, which
already push `SetGrabSet` immediately. Cost: a Ctrl+C with no virtual range says "no selection" (NVDA identical) and
cannot copy a mouse selection; pass-next-key or focus mode does that. The existing regression test changes from
"never grabbed" to "grabbed only in exactly this state" (one predicate, still exhaustive over presets/layouts).

### 3.2 Writing the clipboard (ranked)

| # | Path | Indicator / prompt | Install friction | Verdict |
|---|---|---|---|---|
| K1 | `org.gorca.ShellHelper.SetClipboardText(s)` → `St.Clipboard.set_text(CLIPBOARD, s)`; plus signal `ClipboardOwnerChanged` relayed from `global.display.get_selection()` `owner-changed` | none | extension exists (`contrib/gnome-shell-extension/gorca-helper@gorca`, pidfd+exe+cgroup allow-list), but on this host `disable-user-extensions` is true and new extensions load only at next login | **primary** |
| K4 | Mutter RD session used for clipboard only: `CreateSession`, `EnableClipboard`, `SetSelection{mime-types}`, serve `SelectionTransfer` with a non-blocking pipe (F-3 fix); **never `Start`** | none if 48.7 behaves like main (no handle without Start). Unverified | none | **fallback, probe first**; relies on an undocumented looseness that GNOME may close |
| K5 | Xwayland X11 `CLIPBOARD` ownership bridged by Mutter | none | starts Xwayland on demand, X11 dependency, focus rules unverified | not pursued |
| — | RD session with `Start` (round 1) | "Stop Screen Sharing" indicator | — | rejected (F-1) |
| — | portal Clipboard | consent dialog + indicator | — | rejected |
| — | ext/wlr data-control | not in Mutter | — | rejected on GNOME; a generic Wayland backend could use it elsewhere (not designed now) |
| — | own Wayland surface + `wl_data_device.set_selection` | needs a focused surface and input serial: focus stealing | — | rejected |
| — | through the page (hidden DOM range, `EditableText.CopyText`) | — | CopyText is editor-only; projection text is not in the DOM | rejected |

Security note: any same-UID process can already create Mutter RD sessions on the session bus, so K1's allow-list
is stricter than the platform baseline. The helper must refuse clipboard writes while the session is locked and
rate-limit them (existing limits). Clipboard text never touches logs or disk.

### 3.3 What must be probed in a private session (no live-session writes)

Use `contrib/gnome-shell-extension/headless-shell.sh` (private bus, headless shell 48.7, extension preloaded) and
the selcopy rig (`tests/web/selcopy/`, reviewer memory: read SPEAK lines from `out/ssip.log`).

| Probe | Pass criterion |
|---|---|
| P-CB1 K1 write | helper `SetClipboardText("ü…\n")` from an allow-listed caller; `wl-paste` in the private display returns the exact bytes; denied caller gets `AccessDenied` |
| P-CB2 K1 owner signal | after a Firefox Ctrl+C (model A) the helper emits `ClipboardOwnerChanged` within 50 ms; after our own write it reports ourselves (so gorca can tell its own write from the page's) |
| P-CB3 K4 without Start | on 48.7: `EnableClipboard` + `SetSelection` + `SelectionTransfer` work **without** `Start`; the remote-access controller sees no new handle (watch Shell's AT-SPI tree for the "screen sharing" indicator, and `org.gnome.Mutter.RemoteDesktop` handle signals); paste equals copy |
| P-CB4 K4 lifetime | ownership survives 10 min idle; another app's copy ends it (`SelectionOwnerChanged`); gorca exit drops it without leaks |
| P-CB5 Blink `Document.SetTextSelections` | Chromium 154: one cross-object range with exact ends; re-run the 9 selcopy rows, expect 9/9 (today 7/9) |
| P-CB6 Gecko clear | `RemoveSelection` clear never moves focus on a page starting with a skip link (R2-1 regression) |
| P-CB7 key path | model B, Shift+End then Ctrl+C within 5 ms: clipboard holds the range (grab is state-wide, so no race); model A: Ctrl+C never grabbed |
| P-CB8 W6 gate | virtual copy of a 10k-character range → clipboard owned ≤ 50 ms wall (`docs/plan/phase-3.md` W6) |

---

## 4. Invariants and placement

| Part | Lives in | Why |
|---|---|---|
| `SelectionModel {Native, Virtual, Auto}`, `VRange`, toggle, grab predicate, announcements, `CopyProjection` | `gorca-core` browse model | engine-neutral; the core decides what Ctrl+C means |
| Native selection apply/clear/collapse; strategy by caps: `DocTextSelections` (Blink, ATK ≥ 2.52) > `HypertextRanges` (Gecko) > none; fidelity report | connector `DocSource` (`gorca-doc-source-atspi`; later the Newton connector via AccessKit `SetTextSelection`) | ADR-0008: engine quirk normalisation is connector work; core sees `SourceCaps::native_selection` and a `NativeSelResult` |
| Clipboard writer + owner watcher | **platform service port** `ClipboardSink` (trait in core, impls in a platform crate: `ShellHelper` backend, `MutterRdClipboard` backend, `Null`) | not an a11y source (not a connector), not policy (not an extension); it is an output device like speech |
| Per-site default model, projection tweaks | profile data (ADR-0016 Site layer) and ADR-0019 data-only/site worlds | policy, no code |
| Extensions that want to copy | ADR-0019 effect `clipboard.write` (capability-gated), applied after return through the same `ClipboardSink` | extensions never touch the platform directly |

Invariant check:
- **Backend-agnostic core:** holds. Core imports no AT-SPI or Mutter type; `SetTextSelections` vs hypertext ranges
  is a connector strategy; St.Clipboard vs RD is a sink backend.
- **Never block:** the Reader emits effects and never awaits. Mirror: one in-flight request per document,
  latest-generation wins (older pending requests are dropped, not queued), one action deadline (250 ms) for the whole
  apply; results return as `ReaderMsg::NativeSelApplied{gen, fidelity}` and stale generations are ignored. Clipboard:
  one in-flight write, a newer copy cancels the older; deadline 500 ms; result `ReaderMsg::CopyDone{req, Result}`
  drives "copied"/"copy failed" (fixes F-2's optimistic speech). K4 pipe writes use a non-blocking pipe so a dropped
  future closes the fd (F-3).
- **Crash isolation:** a helper or Mutter failure is a `CopyDone(Err)`; the sink reconnects with backoff; the
  browser actor is unaffected.
- **Extensions boundary:** unchanged; nothing per-browser enters the core.
- **Latency:** Ctrl+C in model B is a grabbed key → projection (≤ 1 ms for typical ranges, W6 for 10k chars) →
  one D-Bus call; the K2 key→speech budget covers the "copied" announcement after `CopyDone`.

Failure modes: helper absent/disabled → K4 → `Null` ("clipboard service not available; press gorca+Shift+F10 for page
selection"); mirror refused → fidelity `NotMirrored`, announced once; page replaces selection (scripts, find bar) in
model A → adopt it as the virtual range only when it is not our own echo (pending-generation match, Orca's
`_PendingSelectionChange` pattern, `text_selection_manager.py:417-445`); document reload → `VRange` dropped, native
not touched.

---

## 5. Recommendation and phases

**S0 (in-flight branch, before merge):**
1. Fix R2-1 (clear with `RemoveSelection` n-1..0, never `SetSelection(0,0,0)`) and R2-2; add the skip-link row to the rig.
2. Replace `BrowseState.native_selection: bool` with `SelectionModel {Native, Virtual}` (no behaviour change) and keep
   `gorca+Shift+F10` as the model switch; rename messages to "page selection" / "virtual selection" now, so B slots in
   without a UX change. Command id `browse.selection_model_toggle` (keep `browse.native_selection_toggle` as alias).
3. Default `Native` until a clipboard writer is verified; record in `docs/decisions.md` (answers R2-3).
4. In `Virtual` with no sink: Ctrl+C grabbed under the predicate of 3.1 and says "virtual selection cannot be copied
   yet; gorca+Shift+F10 switches to page selection" instead of silently copying nothing. Optional; if the owner prefers
   no grab before B exists, leave Ctrl+C ungrabbed and drop this item.

**S1 (next, small):** P-CB5 and, if it passes, the Blink `DocTextSelections` strategy in the connector; fidelity
report (`Exact | Approximate | NotMirrored`) and its one-time announcement; collapse-on-focus-mode in model A.

**S2 (M15 slice, after P-CB1..P-CB4):** `ClipboardSink` port + ShellHelper backend (`SetClipboardText`,
`ClipboardOwnerChanged`) + K4 fallback if P-CB3 passes; `CopyProjection`; model B Ctrl+C; one-shot `browse.copy_as_text`
(O5, binding by the keymap owner); "copied" confirmation in both models; profile key `browse.selection_model`
(`native|virtual|auto`) with Site layer; then flip the default if the owner agrees (QS-2).

**S3 (later):** `auto` (O4); reverse sync of page-originated selections in model A; O7 HTML flavour; upstream a Firefox
bug for ATK `set_text_selections` (Firefox already implements the IA2 container NVDA uses on Windows), which would
remove the Gecko multi-range gluing.

## 6. Risks and open questions

- K4 depends on Mutter 48.7 matching main (no remote-access handle without `Start`); unmeasured. If GNOME later
  requires `Start` for clipboard, K4 dies and K1 is the only path.
- K1 requires the user to enable user extensions and log in once; acceptable for the owner, a packaging question for
  others (ADR-0020).
- Chromium `SetTextSelections` needs ATK ≥ 2.52 at runtime (installed: libatk1.0-0t64 2.56.2, so the runtime gate passes; Chromium bundles no ATK) and fixes
  the `<input>` boundary case only if Blink's `AsUnignoredSelectionPosition` treats it differently: P-CB5.
- Owner questions: QS-1 toggle scope (app run vs document); QS-2 default model after B ships; QS-3 whether model B
  should grab Ctrl+C for the whole browse state (recommended) or only while a range exists (re-opens F-4).
