# ADR-0028: Virtual actions on the fused semantic model

Status: **Accepted** (r3, 2026-10-26: the user-facing model restructured into four layers, §2.0, owner framing;
r2, 2026-10-26: owner answers QV-1..QV-7 = the recommendations, plus the verbosity requirement
§2.2.1; r1 Proposed 2026-10-26). Owner: architect.
Amends: ADR-0026 r7.1 (the 0.10 `actions` interface, AC-4 and the `pointer` permission row; §2.7 permissions table
gains `offer-actions` and `keys`), ADR-0025 MI-1 (the "a pointer effect needs a new ADR" clause: this is that ADR),
ADR-0022 §2.9 (a second add-on side table beside `ExtCats`), ADR-0024 §2.3.3 (act on what was spoken, extended to
virtual actions). Builds on: ADR-0001 (actors, R3), ADR-0003 (browse mirror), ADR-0005 (commands), ADR-0006
(object navigation), ADR-0010 (catalogs), ADR-0016 (chooser list surface, M15a).
Draft WIT (paper only): `wit/drafts/gorca-ext-0.11-vactions/vactions.wit` (parsed with `wit-parser` 0.261 next to
the 0.10 `types` and `host` files bumped to 0.11.0, with the 0.10 `actions` interface removed: `ok gorca:ext@0.11.0
interfaces=11 worlds=1`).

Why a new ADR rather than ADR-0026 r8: the decision changes the model (a new side table and a new `PresIr` field),
the browse and object-navigation surfaces, the activation command semantics, the mouse-emulation invariant MI-1 and
the plugin ABI at once. Native (in-process) extensions, page authors' `aria-actions` and plugins all feed the same
table, so it is not a plugin-only rule. ADR-0026 keeps the ABI mechanics and points here.

## 1. Context

**The owner's idea (2026-10-26).** Plugins add *actions* to the virtual (fused) model, not to the DOM. In Telegram
Web A a message has handlers that only a mouse can reach. The telegram-web plugin declares "this message has the
actions *open message menu* and *reply*" (optionally as a virtual button next to it); the user invokes them with
gorca's keys; gorca performs them through the node's own action, a synthetic click at the node, focus plus keys, or
a short scripted sequence. The page is never modified. "We are heading toward this; let's try it here."

**What Telegram Web A actually does** (source `Ajaxy/telegram-tt` f67a408, 2026-10-16, studied in
`/var/tmp/tg-study/tt`):

| Gesture on `div.Message` | Handler | Effect |
|---|---|---|
| right click (`contextmenu`) | `useOuterHandlers.ts:127 handleContextMenu` (also a 200 ms long tap, `useContextMenuHandlers.ts:12,155`) | opens `MessageContextMenu` (portal; items `div[role=menuitem][tabindex=0].MenuItem`, `ui/MenuItem.tsx:142`; the menu has no role without an aria-label, `ui/Menu.tsx:164`; arrows move a focused index, `useKeyboardListNavigation.ts:24`) |
| double click on the **container** | `useOuterHandlers.ts:140 handleContainerDoubleClick` → `updateDraftReplyInfo({replyToMsgId})` | reply to this message |
| double click on the **content** | `handleContentDoubleClick = stopPropagation` (`useOuterHandlers.ts:220`) | nothing (text selection) |
| click | `handleClick` → `selectMessage` only in select mode | nothing outside select mode |
| keyboard | composer `Ctrl+Up/Down` with an empty composer = `replyToNextMessage(±1)` (`MessageInput.tsx:613-619`) | reply to the *adjacent* message, not a chosen one |

So both wanted actions need a **real pointer event of the right type at the right place**: Gecko's AT-SPI action on
a node without its own listener is `clickAncestor`, and `DoAction` dispatches a click, never `contextmenu` or
`dblclick` (`refs/firefox/accessible/generic/LocalAccessible.cpp:1907-1990`, `GetActionRule` 3484-3500,
`nsCoreUtils::HasClickListener` `base/nsCoreUtils.cpp:63`). "Reply" must land inside the message row and outside its
content bubble.

**What gorca has today.**

- *Pointer*: ADR-0025 via `gorca-helper` v2 (opt-in `pointer` cap in the owner-written allow-list). Core:
  `ReaderEffect::PointerAt(PointerAtReq { node, offset, act: AtAct::{Move, Click(Button)}, if_covered, item })`
  (`crates/gorca-core/src/commands/pointer_req.rs:34`, `commands/mouse.rs:261 mouse_item_click`, `:299 mouse_route`);
  the connector resolver (`BackendOp::Locate`) places a node's visible centre and checks the application's hit test;
  the helper guards window family, occlusion and expiry. **No double click at a node, no aim other than the centre.**
- *Node actions*: browse Enter/Space builds `WebEffect::Act { DocAction::Click | ClickAnnounced { name } }`
  (`browse/commands.rs:2880`), which the AT-SPI doc source runs as `Action.DoAction(0)` after a name/role re-read
  (`gorca-doc-source-atspi/src/source.rs:1935, 2078`). App nodes: ADR-0025 item clicks (`ActionReq::Click`,
  `ContextMenu` by action name). **No named action for documents** (`DoAction(0)` only).
- *Keys to the application*: `gorca-input-inject` is the expectation table plus a stub injector that answers
  `NotAvailable` (`crates/gorca-input-inject/src/lib.rs:1-20`). The helper creates a pointer device only
  (`contrib/gnome-shell-extension/gorca-helper@gorca/helper.js:676`, `POINTER_DEVICE`). **No key injection exists.**
- *Plugins*: ADR-0026 0.10 `actions.{activate, focus, pointer-click}` usable only inside the plugin's own command
  (AC-3), `pointer-click` T2 only (AC-4). Nothing is discoverable: the user must know the plugin's chord.

### 1.1 Prior art

| System | Mechanism | Lesson |
|---|---|---|
| VoiceOver (macOS) | `VO-Command-Space` actions menu; AX custom actions (`NSAccessibilityCustomAction`); "actions available" hint | a discoverable per-element list plus a hint is the accepted UX; the hint must be terse |
| iOS / UIKit | `UIAccessibilityCustomAction` (name + handler); VoiceOver "Actions" rotor, swipe up/down | the *app* runs the handler; a screen reader add-on has no equivalent, so ours must be declarative |
| Android / TalkBack | `AccessibilityNodeInfo.AccessibilityAction` custom actions, TalkBack actions menu | same shape; destructive actions are not special-cased (we do) |
| Orca 48+ | `Orca+Shift+A` lists the AT-SPI actions of the focused object (`refs/orca/src/orca/action_presenter.py`, binding `action_presenter_command_definitions.py:37`) | native actions belong in the same list; reuse Orca's chord in the Orca preset |
| NVDA | no custom actions; app modules script `obj.doAction()` and `mouseHandler` clicks behind add-on gestures | imperative add-on code with full input power: what we avoid (VS-2) |
| ARIA 1.3 `aria-actions` | page names action elements for a host; Firefox maps it to relations `ACTION`/`ACTION_FOR` and always exposes the Action interface (`LocalAccessible.cpp:2619-2625`, `atk/AccessibleWrap.cpp:304`) | the model is the same as ours (an action = a reference to a node that performs it); author `aria-actions` should feed the same table later (VA-7) |
| AccessKit | `Action::CustomAction` + `Node::custom_actions` (`CustomAction { id, description }`, `refs/accesskit/accesskit/src/lib.rs:300, 761`) | Newton sources can supply native custom actions into the same table without plugins |

## 2. Decision

**Virtual actions (VA)** are entries of a per-node side table of the fused model. A plugin (or a native extension,
or later a connector reading `aria-actions`/AccessKit custom actions) **declares** an action as data: label, kind and a
**plan** of at most four steps from a **closed set of host-performed effects**. The user discovers it through a
spoken hint and a list, and invokes it with a core command. The core runs the plan itself; no plugin code runs
between the gesture and the effect.

### 2.0 The user-facing model: four layers (r3)

Owner framing (2026-10-26): virtual actions are an **extension of the interface with ordinary keyboard control**.
Whatever a sighted user does with the mouse on a node (hover, right click, double click, the affordances that only
appear under the pointer) is folded into one keyboard menu; whatever the page opens in answer (a popup, a menu, a
modal) is then used with the keys the user already knows. Four layers, each with one owner and one job:

| Layer | What it is | Owner | Where it lives | Keys |
|---|---|---|---|---|
| **L1 Native actions** | what the accessibility API already exposes for the node: AT-SPI `Action` names, later `aria-actions` relations (Gecko `ACTION`) and AccessKit `custom_actions` | the connector (S0, author intent) | read live from the connector, never copied | Enter/Space (browse), and items of the actions menu |
| **L2 Offered actions** | mouse-only affordances an add-on makes reachable (right-click menu, double-click reply, hover toolbar button) | a plugin or native extension (§2.1, tiers §2.4) | `VActs` side table of the fused model | items of the actions menu; Enter only for a first-party primary action (VA-P1); optional first-party virtual button |
| **L3 Aftermath** | what the page does after any action of L1 or L2: a popup, menu, listbox or modal appears, focus moves, it closes | the **core**, generic for every action and every page | browse / focus machinery that exists today (focus follow, menu entry speech, modal scope, re-anchor) plus one new `ActionOrigin` | the page's own keys: arrows, Enter, Escape |
| **L4 The actions menu** | gorca's own UI that lists L1 + L2 for the current node | the **core** (gorca's UI, SS-4) | a virtual menu in a core-owned virtual document | `gorca+Shift+A` opens; arrows, Home/End, first letter, Enter, Escape |

Separation rules: L2 never re-implements L3 (no plugin step "speak the menu" or "go back"); L4 never runs plugin
code; L1 and L2 meet only in L4, where the de-duplication rules below make them one list.

#### 2.0.1 L4: the actions menu (one menu everywhere)

- **Target.** The node the user is on: the review position in review/object navigation, the browse cursor in
  browse mode, else the focused object (the existing `review_target()` rule, `commands/mouse.rs:320`, so the menu
  and the mouse commands agree on "here"). The same command, the same menu and the same keys in every application,
  web or native.
- **What it is.** A real menu in the model, not a dialog and not a window: a core-owned virtual document (the
  `Mode::Chooser` engine planned for M15, in its *menu style*; ADR-0005 amendment) containing one node of role
  `menu`, name "Actions" / «Действия», items of role `menuitem`, and `group` nodes for the sections. It is composed
  and rendered by the normal pipeline (`compose` → `PresIr` → render), so it sounds exactly like a page menu: entry
  says "Actions menu" and the first item with its position ("Reply, 1 of 3"), braille shows it like any menu.
  Nothing is opened on screen (Wayland: a background process should not open a window; M15 rationale), zero IPC.
- **Keys** (grabbed while the menu is open, released on close; one `SetKeyGrabs` generation each way, ADR-0005):
  Up/Down move with wrap, Home/End, a letter jumps to the next item starting with it (cycling, NVDA/Orca menus),
  Enter or Space runs the item, Escape closes and re-presents nothing but the cursor's node name (brief) so the user
  knows where they are, any gorca command key closes the menu first. Tab is Down. No submenus.
- **Order and grouping, no duplicates.** One list, native first:
  1. **L1** native actions of the target, in the connector's order, each spoken with the core word for its action
     class ("Click", "Press", "Expand", "Show context menu", ...);
  2. **L2** offered actions, first-party owners before third-party, then by owner id, each under a `group` named
     after the add-on (the group name is spoken only when the cursor crosses into the group, as for page menus;
     with one owner and no native actions the group is flat).
  De-duplication: (a) an L2 action whose plan is exactly `Invoke(host, c)` for a class `c` that L1 already lists is
  dropped (counted at admission, `explain` names it); (b) Gecko's `clickAncestor` is shown as "Click" only when the
  node has no L2 action and no other L1 action (it is the ancestor's click, not the node's); (c) a virtual button
  is the same action as its L2 item, so it never adds a row; (d) two L2 actions of different owners with the same
  kind on the same host both stay, each in its owner's group (the user hears whose it is).
- **Item text.** L1: the core word. L2 first-party: the label. L2 third-party: `label, <kind word>: <target role>
  <target observed name>` (VS-3; security text, not a report field). An item whose plan needs an absent capability
  (pointer service, key injector) is present, says "unavailable" after its text and answers Enter with the reason.
- **Empty and single.** No actions: "No actions" and the menu does not open. One action: the menu opens anyway
  (no surprise activation; the user can press Enter at once).
- **After Enter** the menu closes first, then the action runs (§2.3); L3 takes over from there.

#### 2.0.2 L3: what happens after an action (generic, core-owned)

Before any L1 or L2 action runs (from the menu, Enter in browse mode, a virtual button, `vactions.primary` or an
ADR-0025 item click), the Reader records an **`ActionOrigin`**: the document, the cursor node, its item anchor
(the existing `cursor_item` DOM-id tracking of `BrowseState`, `browse/mod.rs`, so a re-rendered row is still found)
and the mode. Then the existing machinery applies, unchanged:

| The page… | gorca does (existing behaviour reused) |
|---|---|
| moves focus into a menu / menu item / listbox / dialog | focus follows; a menu item focus enters focus mode `Auto` (`browse/forms.rs:160-171`), the menu is announced on entry (`web_menu_entered`, `browse/commands.rs:1385`), a modal dialog restricts the browse scope (`Scope::Modal`, `browse/mod.rs:37`) and its name is announced (`lead_dialog`) |
| opens a popup without moving focus | the L2 plan may end with `await-node` + `focus`/`cursor` on the popup's first item (first-party), and the plugin should patch the popup container's role to `menu`/`dialog` with a name through ordinary facts (S1) so the generic rules above recognise it; L1 actions get the generic follow below |
| closes the popup (Escape, or an item ran) | if focus landed somewhere meaningful (not the document root, not inside the removed subtree), follow it (e.g. Reply puts focus in the composer); otherwise **return to the origin**: browse mode re-anchors the cursor at `ActionOrigin` (item anchor first, then the existing `cursor_chain` re-anchor), says the node briefly, and the mode returns to what it was |
| does nothing visible | outcome speech of §2.2 (label once, under `verbosity.confirmations`) |

Generic follow for popups that appear without focus (any action, any page): a new subtree that appears within 1 s
of the action in the same document, is a `menu`, `listbox`, `dialog`/`alertdialog`, or contains focusable
`menuitem`s, and was not there before, becomes the **popup of the action**: the browse cursor moves to its first
item and it is presented like a focused menu or dialog (entry speech, position). Escape while the cursor is in that
popup goes to the page first (`popup_open` rule, `browse/mod.rs:626`), and when the popup is gone the origin rule
applies. One origin at a time; a new action replaces it; it expires 60 s after the popup closed or when the user
moves the cursor by their own command.

### 2.1 Model

```rust
// gorca-model (neutral; paper only)
pub struct VAction {
    pub owner: VActOwner,        // SourceSlot of the plugin / extension / connector (provenance, tier)
    pub id: u16,                 // owner-local, unique per host node
    pub label: ExtMsg,           // catalog reference (ns, key, args), resolved at render, ru/en
    pub kind: VActKind,          // registry code; >= 64 destructive
    pub primary: bool,           // Enter candidate (rules §2.2)
    pub show: ShowAs,            // Attached | Button
    pub plan: Plan,              // 1..=4 steps
    pub basis: RevToken,         // ADR-0026 §2.6.1 token of the admitting call
}
pub struct Plan(SmallVec<[Step; 4]>);
pub enum Target { Host, Sub(NodeKey), SubMatch(SelectorRef), Awaited }
pub enum Aim { Centre, At { x_milli: u16, y_milli: u16 }, Outside(Target) }
pub enum Step {
    Invoke(Target, ActionClass),          // connector action of this class
    Focus(Target),
    ScrollIntoView(Target),
    Pointer { target: Target, button: Button, clicks: Clicks /* One | Two */, aim: Aim },
    Keys(SmallVec<[KeyChord; 4]>),
    Await { sel: SelectorRef, within: Duration /* 50..=1500 ms */ },
    Cursor(Target),
}
pub struct VActs(SmallVec<[VAction; 2]>);   // per host node, <= 8
```

- **Storage.** `VActs` lives beside the entry like `ExtCats` (ADR-0022 §2.9), written only by the node's writer
  inside `FUSE` (SM-8), keyed by `NodeKey`. It is **not** a `Group`: the 24 of 32 group slots in use, the ≤ 64 B
  `Semantics` budget and the fixed-size x32 entry (ADR-0024 §2.12) cannot carry variable lists. The published
  entry view gets one derived bit, `has_vactions`, so composing a node without actions costs one test.
- **Provenance and tier.** Each `VAction` carries its owner slot; tier (T2, T3-trusted, T3) comes from the slot
  (ADR-0026 §2.7). A third-party action is never presented as author intent (§2.2).
- **Kinds** (`VActKind`, append-only registry, codes in the WIT): `other, activate, open-menu, reply, open, expand,
  toggle, copy, react, forward, edit, select`; destructive (≥ 64): `delete, leave, block, send, report`. The core has
  its own word for each kind (catalog `vactions.msg`), used for T3 attribution and confirmation.
- **Admission** (in `FUSE`, same path and same PF-7 stale-on-arrival rule as facts): the host node exists in the
  observed view, is not secret (AC-1) and not in a system scope for T3 (SS-1..SS-4); every `Sub` is in the host's
  observed subtree (depth ≤ 8); every step is permitted for the owner's tier and grants (§2.4); ≤ 8 actions per
  node, ≤ 4 steps, `primary` at most once per node and never on a destructive kind; the label resolves in the
  owner's catalog. A violation drops the offer and counts as a malformed effect (ADR-0026 §2.5.4: 16 in one call
  is a fault).
- **Lifetime.** An action is a fact of its owner's source: it disappears when the owner `withdraw`s it, the host
  node is removed, the source epoch bumps (hot reload, settings change, re-derive), the slot is masked, backs off or
  is disabled, the scope closes, or (T3 `inferred` facts) its TTL lapses (300 s default). A pending activation
  whose action disappears is cancelled with "Not available" (§2.5).
- **Native actions are not copied.** The node's own AT-SPI/AccessKit actions (L1) stay where they are; the actions
  menu (L4, §2.0.1) reads them live (as Orca does) and shows them first.

### 2.2 Presentation

- **Hint.** `PresIr` gets core field kind `ACTIONS` (code 15, appended to the v1 registry; ADR-0022 §2.5) composed
  when the node has at least one *available* virtual action (an action whose plan needs an absent capability, e.g.
  the pointer service, is listed but not counted). Content: `Msg(vactions.hint)` → en "has actions" / ru
  «есть действия» (owner QV-2; no count). Order: after states, before description. Presented on
  focus, browse line entry, object navigation, quick nav and where-am-I; **not** in say-all and not on
  re-presentation of the same node within one browse line. Governed by `report.actions_hint` (§2.2.1).
  Braille: abbreviation `act` (later, with braille M-slices).
- **Actions menu.** Core command `vactions.menu` (default chord `gorca+Shift+A` in every preset, owner QV-3,
  conflict-checked in VA-1 against ADR-0005's preset tables) opens the L4 menu of §2.0.1: native actions first,
  then offered actions grouped by owner, no duplicates.
- **Enter / Space in browse mode** (rule VA-P1): (1) a natively activatable node (link, form control, focusable
  widget, or an AT-SPI action other than `clickAncestor`) keeps today's behaviour; (2) otherwise, a **T2** `primary`
  virtual action runs; (3) otherwise today's `DoAction(0)` (including `clickAncestor`). A T3 action is never run by
  Enter, Space or any unmodified key. Focus mode is unchanged (keys go to the application).
- **Dedicated key.** `vactions.primary` runs the node's primary virtual action (any tier, T3 included, because the
  chord is a core chord the user pressed knowingly and the T3 attribution is spoken first). Unbound by default.
- **Virtual button** (`show = button`, **T2 only**, owner QV-4; T3 degrades to `attached`). Spoken exactly like a
  native button (no "virtual" word). Governed by `report.virtual_buttons` (§2.2.1). The browse line builder inserts a
  synthetic item after the host's own items: role `button`, name = label, key `NodeKey::virtual(owner, host, id)`
  in a reserved key space. It is part of the published view only: never in the observed view, never matched by any
  selector (PF-2 holds), never sent to a connector. Browse arrows, quick nav `B`, the elements list and object
  navigation (last child of the host) reach it; Enter on it runs the action. `explain` names the owner.
- **Outcome speech.** On success the core says nothing when the plan moved focus or the cursor (the new place is
  spoken by the normal pipeline); otherwise it speaks the label once ("Reply") as a confirmation under the existing
  `verbosity.confirmations` (`brief` drops it). Failures speak one sentence (§2.5) and are never silenced.

#### 2.2.1 Verbosity (owner requirement, r2)

Every announcement this ADR adds is a **report field** of the verbosity system: ADR-0016 `report.*` keys, compiled
once per effective snapshot into `ReportFlags` (`crates/gorca-config/src/profiles/resolve.rs:35`, u64) and consumed by
`compose` through `ComposeCtx.flags` (`crates/gorca-present/src/compose.rs:31`, an interim u32). Today no `report.*`
key exists and both values are 0; M22d was to introduce them. VA-1 therefore adds the first keys and the plumbing
(config u64 → `ComposeCtx`), and M22d adopts it unchanged.

| Key | Values (default) | Bits | Silences | Never silenced |
|---|---|---|---|---|
| `report.actions_hint` | `word` \| `cue` \| `off` (`word`) | 2 | the "has actions" field (`cue` = the core `actions` earcon, no words) | Enter rule VA-P1, `vactions.menu`, `vactions.primary` |
| `report.virtual_buttons` | `true` \| `false` (`true`) | 1 | the virtual button as a browse / object-nav / quick-nav stop | the action itself (list, Enter primary) |
| `report.destructive_warning` | `full` \| `brief` \| `cue` (`full`) | 2 | the confirmation wording: `full` "{label}: this cannot be undone. Press again to confirm", `brief` "Again: {label}", `cue` the core `confirm` earcon only | the second press itself (VS-8). There is no `off`: a silently pending confirmation makes the first press sound like a failure |

- **Scope and layering.** `report.*` are Focus-scope keys (ADR-0016 §2.3 table): settable globally, per profile,
  per app and per site, with the usual origin explain. Not privacy keys.
- **Presets.** The verbosity presets (owner QA-2) set them as one bundle: `verbosity.level = normal` → `word` /
  `true` / `full`; `brief` → `cue` / `true` / `brief`. An explicit `report.*` key beats the preset; `verbosity.cycle`
  therefore also switches the hint.
- **Quick toggle.** Command `report.actions_hint_cycle` (word → cue → off → word, speaks the new value; context ANY,
  `max_press` 1), proposed default chord `gorca+Ctrl+Shift+A` subject to the VA-1 conflict check, and
  `report.virtual_buttons_toggle` (unbound). Both behave like the existing `*_toggle` commands: they change the
  session value of the active layer, kept only by the explicit save (ADR-0005, ADR-0016 touched set).
- **Not report fields** (security text, VS-3/VS-4): the T3 attribution and target description in the actions menu,
  "Changed, not done", and every failure sentence.

### 2.3 Execution: the closed effect set

| Step | Performs (existing path) | New work |
|---|---|---|
| `Invoke(t, class)` | web: `WebEffect::Act` with a named action; app: ADR-0025 `ActionReq` by name | **G-4** `DocAction::Invoke(ActionClass)` (today `Click` = `DoAction(0)`), lookup by `Action.GetName(i)` as MO-5b |
| `Focus(t)` | `DocAction::Focus` / `Component.GrabFocus` | — |
| `ScrollIntoView(t)` | `DocAction::ScrollIntoView` | — |
| `Pointer{..}` | `ReaderEffect::PointerAt { act: Click(b), if_covered: Refuse, item: None }` → resolver → `PointerSink` (helper v2) | **G-1** `AtAct::DoubleClick(b)` (helper v2 already has double click at the pointer); **G-2** `Aim` in `BackendOp::Locate` (fraction of the visible rect; "outside a descendant"); **G-3** hit-test acceptance for an aimed point: the answer must be the target or a descendant that is not the excluded node (Blink keeps its `blink-hit-test` exemption) |
| `Keys(chords)` | `KeyInjector` | **G-6 gap**: no injector exists (stub `NotAvailable`; helper has no keyboard device). Plans with `Keys` are admitted for T2 but always "Not available" until a backend lands: helper v3 virtual keyboard behind a new opt-in `keyboard` cap (same allow-list model as `pointer`) or RemoteDesktop/libei (portal consent). Chosen by measurement in VA-5 (QV-6) |
| `Await{sel, within}` | — | **G-5** the node writer (doc actor / app actor) answers `AwaitReq { sel, after: RevToken, within }` with the first node that **appeared after** `after` and matches `sel` on the observed view, or `Timeout`; native selector evaluation, no plugin code |
| `Cursor(t)` | browse/review cursor move (ADR-0026 `move-cursor` path) | — |

**Executor.** One `PlanRun` at a time, owned by the Reader (where commands live); every step is an effect whose
result returns as a message (R3); no step blocks the Reader or the key→speech path. Per-step deadlines: invoke /
focus / scroll 150 ms, pointer 150 ms per ADR-0025 call (plus one bounded scroll-and-relocate for an off-screen
target, as item clicks do), await ≤ 1500 ms, whole plan ≤ 2 s.

```rust
// gorca-core::vact (paper only)
pub enum RunState {
    Idle,
    Confirming { run: RunId, until: Instant },          // destructive kinds only
    Validating { run: RunId },                          // signature + availability re-check (VS-4)
    Stepping { run: RunId, i: u8, wait: Wait },
    Done(RunId), Failed(RunId, VaError), Cancelled(RunId, CancelWhy),
}
pub enum Wait { Reply(ReqId), Await { req: ReqId, deadline: Instant } }
pub enum CancelWhy { NewGesture, AnyKey, FocusLeftScope, ActionWithdrawn, ScopeClosed, Timeout }
```

| State | Event | Next | Effect |
|---|---|---|---|
| Idle | gesture (actions-menu Enter, Enter rule VA-P1, `vactions.primary`, virtual button) | Confirming if destructive, else Validating | destructive: the warning per `report.destructive_warning` |
| Confirming | same gesture within 2 s | Validating | — |
| Confirming | timeout / any other key | Cancelled | none spoken on other key; "Cancelled" on timeout |
| Validating | signature matches, capabilities available, rate ok | Stepping{0} | emit step 0 |
| Validating | mismatch / unavailable / rate | Failed | one sentence (§2.5) |
| Stepping{i} | reply ok, i+1 < n | Stepping{i+1} | emit step i+1 (targets re-resolved and re-checked) |
| Stepping{i} | reply ok, last | Done | outcome speech (§2.2), audit line |
| Stepping{i} | reply error / await timeout | Failed | one sentence, audit line; no retry (a repeated click can toggle twice) |
| any active | new gesture | Cancelled(NewGesture) → new run | late replies of the old run are dropped by `RunId` |
| any active | user key (not the confirming gesture), focus leaves the scope, action withdrawn, scope closed | Cancelled | nothing further injected; an in-flight helper call expires by its `not-after` (ADR-0025 C-1) |
| Done / Failed / Cancelled | — | Idle | — |

Backpressure: at most one run; no queue (a new gesture cancels, like speech). A second identical gesture within
300 ms of the first is ignored (key bounce / double Enter). Rate limits: ≤ 2 runs/s per scope, pointer steps ≤ 2/s
(AC-4), and the helper's own buckets.

### 2.4 Tier and capability rules

New manifest permissions: `offer-actions { kinds, effects }` (to offer; `effects` lists the step kinds the package
may use, spoken on install/enable) and `keys`. Existing `act`, `navigate`, `pointer` keep their meaning.

| Step | T2 first-party | T3-trusted, T3 | Extra gates (all tiers) |
|---|---|---|---|
| `Invoke` | `act` | `act` + per-plugin user consent | target not secret, not in a system scope for T3 |
| `Focus` | `act` | `act` + consent | same |
| `ScrollIntoView`, `Cursor` | `navigate` | `navigate` | — |
| `Pointer` | `pointer` | **never** (QP-5 unchanged) | helper `pointer` cap (MI-9: the owner writes it), `mouse.enabled`, `[vactions] pointer = true` (default true for T2, owner QV-7; every run is in the VS-9 audit line), `if_covered` forced to `Refuse` whatever `mouse.if_covered` says |
| `Keys` | `keys` | **never** | focus must be inside the host's application and document at run time; printable keysyms without Ctrl/Alt/Super refused at admission (no text entry); never while focus is editable or secret |
| `Await` | with any of the above | only after `Invoke` | ≤ 1 per plan for T3 |
| `show = button`, `primary` on Enter | yes | no (degrade to attached / menu-only) | — |

Trust affects facts (ADR-0026 PF-5), **not input power**: T3-trusted gets no effect T3 lacks.

### 2.5 Security: spoofing, gesture, scope

- **VS-1 Gesture binding.** A run starts only from a core command key event of this session (actions-menu Enter, the
  VA-P1 Enter rule, `vactions.primary`, Enter on a virtual button). No timer, event, fact, derive, reactor or
  plugin command can start a virtual action. ADR-0025 MI-1 holds: every injection traces to a user command.
- **VS-2 No code at activation.** The plan is data admitted earlier; the plugin is not called during the run and
  learns nothing about it (no reply, no outcome, no timing). Plugin commands use `actions.perform` with the same
  executor and checks, under AC-3.
- **VS-3 Announce what will be touched.** For T3, the actions-menu item speaks the host-built description of the first
  effect's target (role and observed name from S0) after the label and the add-on name, so a label "Reply" on a
  plan that presses "Delete" is audible as "…: button Delete". T2 labels are trusted.
- **VS-4 Act on what was announced** (ADR-0024 §2.3.3 extended). When the hint or actions-menu item is spoken, the core
  records the signature of each `Host`/`Sub` target (role, observed name hash, generation) and the action's
  `basis`. Validation refuses with "Changed, not done" when a signature or the action changed after the
  announcement. Web targets use the existing `ClickAnnounced` re-read in the doc source.
- **VS-5 Confinement.** Targets are the host, its observed subtree, or the node the plan's own `Await` found (new
  since the previous step, same scope). Pointer points come only from the host's resolver over the target's own
  extents and must pass the application hit test and the helper's window and occlusion guards. A plan can neither
  name coordinates nor reach another application, document or scope (AC-2).
- **VS-6 Secrets and system scopes.** No action on, inside, or targeting a secret node; no T3 action in SS-1..SS-4;
  `Keys` never while focus is editable or secret.
- **VS-7 No typing.** `Keys` carries chords, not text, and refuses printable keys without Ctrl/Alt/Super.
- **VS-8 Destructive kinds** are confirmed by a second press within 2 s after a spoken warning (owner QV-5; wording
  per `report.destructive_warning`), are never `primary`, and are never run by Enter.
- **VS-9 Audit.** One INFO line per run: `vaction run owner=<id> action=<id> kind=<code> steps=<kinds>
  outcome=<code> ms=<n>`; no label, no name, no text (ADR-0026 logging rule).
- **VS-10 Fail open, fail alone.** A failing plan costs only that run; repeated failures do not strike the slot
  (the page, not the plugin, may be at fault); malformed offers do (§2.1).

Failure sentences (catalog `vactions.msg`, en/ru): "Not available", "Changed, not done", "Mouse not available"
(+ ADR-0025 permission hint), "Covered", "No location", "Busy", "Cancelled". The hint and the destructive warning
are report fields (§2.2.1).

### 2.6 WIT 0.11 and the one-ABI convergence

Minimal additions in the 0.11 bump (no compatibility path, owner rule 2026-10-25), draft
`wit/drafts/gorca-ext-0.11-vactions/vactions.wit`. **One slice, one bump** (orchestrator 2026-10-26): the 0.11 bump
carries both the required `warmup` export (ADR-0026 §2.4.8, draft `wit/drafts/gorca-ext-0.11-warmup/`) and the
interfaces below in a single slice **WIT-11** (M18a-4w's bump extended): package renamed to `wit/gorca-ext-0.11/`,
host world, SDK, telegram-web, the host test guest and all fixtures ported once. VA-1 then implements the host side
against the frozen 0.11 package; there is no intermediate 0.11.x.

- `vtypes`: `vaction-kind` (u8 registry code), `step-target`, `aim`, `button`, `pointer-step`, `key-chord`,
  `await-step`, the closed `step` variant, `show-as`, the `vaction` record.
- `vactions` (import, **W lane**, buffered like `facts`): `offer(host: fact-target, a: vaction)`,
  `withdraw(host, id)`. Offers are derived from the observed view in `derive`, so they follow the facts' trigger,
  token and lifetime rules unchanged.
- `actions` (R lane, command only) is **replaced**: `perform(n, steps)` with the same `step` type; 0.10
  `activate`, `focus` and `pointer-click` are deleted. `move-cursor`, `request-mode`, `pass-key` stay.
- Manifest data form (PM-2, data before code): `[[vaction]]` rows with `on = { watch = N }` or a selector, steps
  using `host`, `sub-match`, `awaited` — evaluated natively; a K0 package can offer actions without any code.

**One ABI.** The converged M18a world (ADR-0026 §2.4.8.8) carries `vactions` in `app-module` and `site`. Native
in-process extensions offer through the same host-side trait (`FactSink::offer_vaction(host, VAction)`), so
`ext-gnome-shell` and `ext-vte` reach the same table when ported; the 0.4 world is not extended. A later connector
feature (VA-7) fills the table from `aria-actions` relations (`ACTION`) and AccessKit `custom_actions` with the
connector as owner (S0, author intent, tier "page").

### 2.7 First slice: telegram-web (VA-TG1)

Pure manifest data in `plugins/telegram-web/manifest.toml` (no new guest code), catalog keys in `i18n/{en,ru}.msg`:

```toml
[permissions]
grants = ["read-names", "read-attributes", "contribute-facts", "offer-actions", "act", "pointer"]
[permissions.offer-actions]
kinds = ["open-menu", "reply"]
effects = ["pointer", "await-node", "focus"]

[[selector]]           # the message bubble inside div.Message
id = 10
class = "message-content-wrapper"
[[selector]]           # a menu item; `await-node` only accepts one that appeared after the right click
id = 11
class = "MenuItem"

[[vaction]]            # "Message menu" / "Меню сообщения"
on = { watch = 3 }     # div.Message
id = 1
label = "vact.menu"
kind = "open-menu"
primary = true
steps = [
  { pointer = { target = { sub-match = 10 }, button = "right", clicks = 1, aim = "centre" } },
  { await-node = { selector = 11, within_ms = 800 } },
  { focus = "awaited" },
]

[[vaction]]            # "Reply" / "Ответить"
on = { watch = 3 }
id = 2
label = "vact.reply"
kind = "reply"
steps = [
  { pointer = { target = "host", button = "left", clicks = 2, aim = { outside = { sub-match = 10 } } } },
]
```

Prerequisite **G-7**: the plugin host implements only `[[watch]]` (tag, one class, aria-role;
`crates/gorca-plugin-host/src/package.rs:110-135`); `[[selector]]` rows with ids (ADR-0019 §4.4) and
`[[vaction]]` rows are new manifest parsing in VA-1, with the same native matcher.

The plugin also patches the portal menu container (the parent of the `.MenuItem` nodes, inside
`.MessageContextMenu`) to role `menu` with the name "Message menu" through an ordinary S1 fact, so the generic L3
rules announce it like any page menu.

Behaviour (the four layers): browse cursor on a message → "…, has actions" (unless `report.actions_hint` is off or
`cue`). **Enter** (no native activation, first-party primary, VA-P1) or `gorca+Shift+A` → "Actions menu, Message
menu 1 of 2" → Enter → right click → Telegram's menu opens; focus lands on its first item → "Message menu, Reply 1 of
N" (L3: focus mode `Auto`, menu entry speech); arrows, first letters and Enter are Telegram's own; **Escape** closes
it and the cursor is back on the message (origin re-anchor); choosing "Reply" puts focus in the composer and gorca
follows it. "Reply" from the actions menu double-clicks and gorca follows the composer focus the same way. Without
the helper `pointer` cap the hint is silent and both items say "unavailable" with the permission hint. Telegram K
(`bubble[role=article]`) is a follow-up after a live check of its handlers.

Slice order (each ≤ 2 days), after **WIT-11** (above): **VA-1** model side table, admission, `PresIr ACTIONS`, the L4 actions menu (the `Mode::Chooser` core in
menu style, built here and reused by M15's elements list with its filter added there), the L3 `ActionOrigin` and
generic popup follow, Enter rule,
executor with `Invoke`/`Focus`/`ScrollIntoView`/`Cursor`, manifest parsing, the `report.*` keys, presets,
`ReportFlags` plumbing and the toggle commands of §2.2.1 (behind `[vactions] enabled`, default off); **VA-2** pointer steps (G-1..G-3) and `Await` (G-5); **VA-TG1** the rows above + a fixture test on
the recorded Telegram tree and one live run by the owner; **VA-3** virtual buttons; **VA-4** named `Invoke` for
documents (G-4); **VA-7** `aria-actions` / AccessKit custom actions; **VA-5** `Keys`: the slice first measures, on the owner's
Wayland / GNOME 48 session, a helper virtual keyboard device (Clutter `KEYBOARD_DEVICE` behind a new opt-in
`keyboard` cap) against RemoteDesktop/libei (portal consent, `gorca-input-inject`), ships whichever works (owner
QV-6) and records the choice in decisions.md.

## 3. Alternatives considered

| Alternative | Verdict |
|---|---|
| Modify the page (browser extension adds buttons/ARIA) | rejected by the owner: the DOM is the page's; also needs the browser bus (ADR-0024) and breaks on re-render |
| Plugin callback at activation (`on-activate(id)` returns or performs effects) | rejected: code with input power at the moment of the gesture, timing side channel, harder to audit; the declarative plan gives the same reach for the closed effect set |
| A `Group` slot "actions" in `SemEntry` | rejected: variable-length, would break the x32 layout and the ≤ 64 B `Semantics` budget |
| Keep only plugin commands (0.10 `actions.*`) | rejected as the only path: not discoverable, invisible in browse/object nav; kept as `actions.perform` over the same executor |
| Virtual nodes for every tier | rejected: a third party could plant convincing fake buttons; T3 gets menu-only actions with attribution |
| Open-ended scripting (loops, waits on arbitrary conditions) | rejected: four steps from a closed set cover the Telegram cases; anything more is a new step kind in a major bump |
| Pointer via RemoteDesktop/libei directly | out of scope (ADR-0025 §8 verdict unchanged); the plan is backend-neutral, a new `PointerSink` needs no ADR change here |

No bake-off: nothing here is a performance choice with competing designs; the contested points are UX (QV-1..QV-3)
and are settled by the owner and the VA-TG1 live run.

## 4. Consequences

- Plugins and extensions can make mouse-only widgets usable without touching the page, and every surface (focus,
  browse, object nav, quick nav, elements list) sees the same actions.
- The effect set is closed: adding an effect is a WIT major bump and an ADR amendment, by design.
- Third-party add-ons gain discoverable actions but no pointer, no keys, no Enter, no virtual buttons.
- New core work: side table, field kind, list surface reuse, executor, `AtAct::DoubleClick`, `Aim`, `Await`, named
  document actions. Key injection stays an explicit gap.
- Telegram's message actions depend on the helper `pointer` cap the owner grants; without it they are listed as
  unavailable.

## 5. Metrics to validate

| Id | Metric | Target |
|---|---|---|
| VM-1 | gorca-side time from the gesture key event to the first step leaving the Reader | p99 < 2 ms (inside the 15 ms budget) |
| VM-2 | composing a node without actions vs today (`has_vactions` test) | no measurable change (< 1 % of compose p50) |
| VM-3 | side-table memory | ≤ 96 B per action, ≤ 1 MiB for a 10k-message history with 2 actions each |
| VM-4 | VA-TG1 live: first-try success of "Message menu" and "Reply" on Firefox ESR, scale 1 | ≥ 19/20 each; failures labelled (covered, no location, Gecko drop) |
| VM-5 | adversarial set VA-A1..VA-A10 (label/target mismatch, target outside subtree, forged handle, offer on secret node, T3 pointer/keys/Enter/button, action withdrawn mid-run, target renamed between announce and run, run without gesture, rate flood) | 0 effects performed |
| VM-6 | cancellation: key pressed during an `Await` | no further step emitted; ≤ 1 helper call in flight expires unused |

## 6. Owner answers (2026-10-26)

All seven recommendations accepted:

| Id | Answer |
|---|---|
| QV-1 | native activation first, then a first-party primary action, never third-party (VA-P1) |
| QV-2 | hint en "has actions", ru «есть действия»; a report field (§2.2.1) |
| QV-3 | `gorca+Shift+A` lists actions (all presets; conflict check in VA-1); `vactions.primary` unbound |
| QV-4 | virtual buttons first-party only, spoken like a normal button |
| QV-5 | destructive kinds: second press after a spoken warning |
| QV-6 | key backend chosen by measurement in VA-5 (helper keyboard device vs libei) |
| QV-7 | first-party pointer use allowed by default, with the VS-9 audit line |

Requirement added with the answers: every new announcement is a per-field verbosity setting, global and per
profile, quick to toggle, and silencing it never removes the behaviour (§2.2.1, VA-11). Target user: a fast
listener (about 1000 words/min) who knows the flow and turns the hints off.

Unverified: the exact AT-SPI relation name Firefox uses for `aria-actions` on ATK (`ATSPI_RELATION_ACTION` was not
found in `refs/at-spi2-core/atspi/atspi-constants.h`); that a right click on the bubble centre never lands on a link
or media element whose own handler wins (VA-TG1 live run); that Telegram's portal menu is reachable as a new node
within 800 ms on a loaded host; that `div.Message` answers `clickAncestor` rather than `click` (if Telegram's
renderer attaches its own click listener to the element, Gecko reports `click`, VA-P1 step 1 applies and Enter keeps
`DoAction(0)`; the menu is then reached through `vactions.menu` or `vactions.primary` unless QV-1 picks the
alternative).

## 7. Invariants

- **VA-1** Virtual actions live only in gorca's model; nothing is written to the application or page.
- **VA-2** No owner code runs between a gesture and its effects; plans are data admitted earlier.
- **VA-3** Every run starts from one user command key event; one gesture runs at most one plan.
- **VA-4** Every step's target is the host, its observed subtree, or the plan's own awaited node, re-validated at run.
- **VA-5** T3 owners never inject pointer or keys, never bind Enter, never create virtual nodes.
- **VA-6** A virtual node is never in the observed view, a selector result or a connector request.
- **VA-7** An action vanishes with its owner's source state (mask, backoff, disable, reload, scope close, TTL).
- **VA-8** At most one plan runs; a newer gesture or any user key cancels it; late replies are dropped by `RunId`.
- **VA-9** Destructive kinds are confirmed and never primary.
- **VA-10** Every run leaves one audit line without content text.
- **VA-11** Silencing a report field (§2.2.1) never changes what a gesture does; failure and security sentences are
  not report fields.
- **VA-12** One actions menu per node: L1 first, then L2 by owner; an action appears at most once (§2.0.1 de-duplication).
- **VA-13** The actions menu is gorca's model only (no window, no IPC to open it) and behaves the same in every
  application and mode.
- **VA-14** After an action, a popup the page opened is presented by the generic core rules; when it closes without
  a meaningful focus, the cursor returns to the action's origin. Plugins never script the return.

## 8. Changelog

**r3 (2026-10-26)** — the user-facing model separated into four layers (§2.0): L1 native actions, L2 offered
actions, L3 aftermath (generic core: focus follow, menu/dialog entry, `ActionOrigin` return, popup follow), L4 the
actions menu (a real virtual menu, `Mode::Chooser` menu style, one list native first, de-duplication rules). The
"list" became the actions menu (`vactions.menu`, `gorca+Shift+A`); VA-TG1 relies on L3 and a role patch for
Telegram's menu; VA-12..VA-14 added. Model, steps, tiers, security, WIT and verbosity unchanged.

**r2 (2026-10-26)** — Accepted. Owner answers QV-1..QV-7 (§6); hint "has actions" / «есть действия»; §2.2.1
verbosity (`report.actions_hint`, `report.virtual_buttons`, `report.destructive_warning`, presets, quick toggle,
VA-11); one WIT-11 slice for the warm hook and virtual actions; VA-5 key backend by measurement.

**r1 (2026-10-26)** — initial proposal.
