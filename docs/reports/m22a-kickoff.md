# M22a kickoff: open fact-source contract, profiles interaction, plan rationale

Date: 2026-10-19. Author: architect. Main at `d87409fb`. Design only, no product code.

Deliverables: ADR-0024 `docs/adr/0024-fact-sources-contract.md` (Proposed), ADR-0022 r3 note,
`docs/plan/m22a-spec.md` (task breakdown, first tasks), `docs/plan/phase-3.md` §M22a, §M23–M25.

## 1. Why an addendum ADR, not ADR-0022 r3 in full

ADR-0022 decides the *model, fusion and presentation*; its reviewed core (registers, strata, SM-8,
`explain`, migration) does not change. What the owner added on 2026-10-19 is *who may contribute
and under which rules*: taxonomy, identity across sources, confidence, trust, privacy, latency,
WIT exposure. That is a contract with its own lifecycle (each later source milestone amends it), so
it lives in ADR-0024; ADR-0022 r3 only points to it and records the two amendments to §2.2 (new
stratum S4 inferred below derived; open source-slot list).

## 2. Prior art and what it teaches

| System | How extra facts enter | Lesson |
|---|---|---|
| Chromium image descriptions | appended to the accessible name, status strings included (`refs/chromium/ui/accessibility/platform/ax_platform_node_base.cc:235-262`) | provenance lost; "getting description" becomes a name; reader cannot hedge. Keep inferred text out of observed groups (ADR-0024 alt. C) |
| NVDA content recognition | user-invoked; result is a separate virtual document `RecogResultNVDAObject` (`refs/nvda/source/contentRecog/recogUi.py:95,291`) | safe but cannot name an unlabeled button in place. Keep as a review surface for full-window OCR (M24) |
| ADR-0002 §9.2 naming cascade | hints → label memory → OCR → SigLIP with calibrated abstention, "probably/maybe" wording, never delays step 0 (NC-1) | already the right shape: static order + threshold + hedge. ADR-0024 S4 is this cascade as data |
| Math report §1.3 | numeric max-confidence rejected (uncalibrated, unexplainable, global scale) | confidence admits and hedges, never ranks (FS-4) |
| ADR-0001 §9.3 IR-1..IR-8, `JoinQuality` in `gorca-doc-source-api/src/join.rs` | exact vs heuristic join; heuristic joins confirmed before actions; WebExtension data untrusted (IR-8) | generalised to `BindQuality` with a minimum per group class |
| Browser engines | both expose the HTML `id` as an AT-SPI attribute: Chromium `ax_platform_node_base.cc:1447-1450` (also `class`, `:1440`), Firefox `accessible/generic/LocalAccessible.cpp:1248` | `DomId` anchor gives `Unique` bindings without any engine change; elements without ids remain TBD-S3 |
| AccessKit | node ids are exact keys in the AT-SPI path (`refs/accesskit/adapters/unix/src/atspi/object_id.rs:14-42`) | `AnchorKey::Exact` for dual exposure (A8 unchanged) |

## 3. Worked examples (how each future source plugs in)

1. **Icon-only button in a web app, browser bus on.** AT-SPI: role push button, name `Absent`.
   Add-on: `hint.icon_class = "fa-trash"` and `hint.title_attr = "Delete"` on `DomId("del-btn")`
   (`Unique`). S3 rule "title names an unnamed control" → name "Delete", layer Derived, `Asserted`,
   spoken plainly. Without the title, S4 rule icon-class table → "Delete", Inferred, `Probable` →
   "probably Delete". The first focus utterance never waits: if the fact arrives within 500 ms with
   the epoch unchanged, one refine tail; otherwise next visit.
2. **Unlabeled GTK toolbar button, inference on.** Focus speaks "button" at once (NC-1). After
   speech start the core asks the inference source for the masked crop (LB-3, epoch-tagged).
   OCR returns "Save", `p_milli = 950`, `Basis::Capture(id, fp)`, `Region` anchor → `Geometric`
   binding, allowed for `visual.*` evidence; S4 → name "Save", `Probable`. The window re-renders:
   `vis_rev` bumps, the fact is stale, nothing is spoken about it, a new capture happens only on the
   next visit (NC-3 caching per fingerprint).
3. **Third-party add-on knows a chat app.** A patch (S1) renames message rows deterministically.
   A `facts.assert` of "unread" as an evidence tag from a heuristic is Inferred/T3 by default and
   hedged; it cannot override AT-SPI states.
4. **Newton app also visible through AT-SPI** (M19). Per-app dedupe; the AccessKit primary wins;
   node fusion only on `Exact` keys.
5. **Page floods the add-on** (10k mutations/s). The add-on's `FactPort` coalesces by (subject,
   group); the token bucket throttles; the source goes `Throttled`; the writer's interactive lane
   and AT-SPI ingestion are untouched (M-15).

## 4. New testable properties (extend math report §5)

| Id | Property | Oracle |
|---|---|---|
| F11 | **Confidence gate**: a fact below its grant floor never wins; moving `p_milli` across the floor changes only that (x, g) winner and hedge | value + hedge diff |
| F12 | **Class safety (monotonicity)**: adding any set of Secondary/Inferred facts never changes a group whose winner is User, Patch or Observed-primary non-empty, except where a declared `PI` row ranks a secondary first | equality on protected groups |
| F13 | **Basis staleness**: after a `vis_rev` bump past an inferred winner's basis, the group falls to the next candidate (or default) and no change utterance is produced | value + ST output count = 0 |
| F14 | **Revocation equivalence**: masking or revoking source s yields the state of the run without s's facts | equality |
| F15 | **Binding safety**: no fact with binding below its group-class minimum ever wins; ambiguous `DomId` never binds `Unique` | predicate |
| F16 | **Isolation under flood**: a flood from source s changes no other source's registers and no primary-derived entry; drops counted | equality + counters |
| F17 | **Hot-path independence**: removing every non-primary fact at keypress time gives the same first utterance for that key (non-primary facts reach speech only as refine tails) | transcript equality |

## 5. Profiles (M14a, ADR-0016) and the model

Owner follow-up 2026-10-19: per-app variability (browse/focus defaults per app/site and similar
presets) lives in the profiles system. ADR-0024 §2.13 fixes the interaction:

- Two planes. **Fact plane** (sources → `FUSE` → `SemEntry`, owned by node writers) and **settings
  plane** (layer stack L0..L8 → `Arc<Settings>` per `ProfileKey`, owned by the Reader). Profiles
  never write facts; facts never write settings (PS-1).
- **Mode defaults are profile keys** (`browse.enabled`, `browse.mode_default`,
  `browse.auto_focus_mode`, `browse.sticky_focus`); the Reader's mode machine owns the decision
  from settings + document semantics + the user's toggle (PS-2). This matches DP-3 (NVDA-style
  default, Orca-style sticky focus toggle, per-app presets in add-ons).
- **Add-ons contribute profile rows as data** (vendor profiles, L4, presentation and `browse.*`
  keys only); they never activate profiles and never enable data flows (PS-3, PS-5).
- **CHR-0 interim per-app mode table**: shape each row as `(AppMatcher, {browse.* keys})` with the
  PS-2 names, keep it as first-party data (not code), and M14a moves it verbatim into a vendor
  profile of `ext-chromium`. No user-facing persistence in CHR-0 (that is M14a PE-1).
- **Triggers read the model**: `Context` triggers match the published `SemEntry` role class, so
  "what profile is active" and "what was spoken" cannot disagree; add-on context tags (PF-3,
  Phase 4) become evidence facts on the document node (PS-4).
- **Source opt-ins are settings, grants are per scope** (App/Site triggers only, so grants do not
  flap with focus); vendor profiles may not set them (PS-5).
- **User semantic rules** ("on this site `.icon-x` is Close") are S2 user patches whose activation
  reuses the profiles' `Trigger` matcher (PS-6).
- **Terminology fix**: ADR-0022's `PI[app profile]` is a *source profile* (engine/toolkit class),
  not an ADR-0016 user profile.

### Should M14a move earlier? Recommendation: yes, M14a-core in parallel with M22a

Today M14a sits in Wave 2 after M11/M12a, and M22d, the CHR-0 presets migration, DP-3's
"save mode for this app/site", M18a vendor profiles and the ADR-0024 source opt-ins all wait on it.

- **M14a-core** = ADR-0016 + `gorca-config` layer stack, `Trigger`/`AppMatcher` matcher, fold and
  `ProfileKey` LRU in the Reader, origin-layer edits (PE-1/PE-2), vendor-profile loading for
  first-party extensions, `browse.*` and `report.*` keys, the CHR-0 table migration. **No GUI**
  (M14) and no Control API.
- Files: `gorca-config`, Reader settings cell, `gorca-daemon` wiring (one `wire_profiles`). Disjoint
  from every M22a file (§1 of the spec) and from CHR-0 after its interim table merges.
- Prerequisites: a config schema version bump with its migration (CF-8) is enough; full M11
  supervision is not required. `AppInfo.desktop_id` (ShellHelper) can start as exe/app-name
  matching. M12a catalog ids for profile display names can be literal until M12a.
- Cost: ADR-0016 (architect, 1 day, mostly written in `phase-3-features.md` §3) + ≈ 6–8 implementer
  days. It uses the free fourth implementer slot of M22a waves A/B.
- Risk: a second milestone touching the Reader while M22a wave C touches the actors. Mitigation:
  M14a-core merges before M22a wave C starts or rebases after C1; the Reader settings cell is not
  touched by M22a.

Confidence: medium-high. Order becomes `P2-ARIA-0 ∥ CHR-0 → M22a (A, B) ∥ M14a-core → M22a C →
M18a ∥ M22b1`.

## 6. Decisions needing owner input

| # | Question | Options | Recommendation |
|---|---|---|---|
| D-1 | Meaning of "confidence-weighted" | (a) confidence gates admission and sets a spoken hedge; ranking stays a static per-group order; corroboration later; (b) numeric weighted fusion across all sources | **(a)**, high confidence: explainable, order-independent, a wrong confident model cannot override an author's label |
| D-2 | Default state of new sources | (a) all off, opt-in per browser/app/site; (b) browser bus on when its add-on is installed; inference off | **(a)**: installing the add-on asks once, then per-site deny list |
| D-3 | Hedged speech | (a) "probably X"/"maybe X" (ADR-0002 wording), refine tail only for Name within 500 ms; (b) plain text, hedge only in where-am-I; (c) a short earcon instead of words | **(a)** default, (c) as a verbosity option once cues exist (M13) |
| D-4 | May a browser add-on override engine values? | (a) only groups the engine cannot express (primary `Unknown`) + evidence; (b) per-group data rows may rank it first after evidence from `observed-disagree` counters | **(a)** now, (b) case by case with a decision row each |
| D-5 | M22a start without P2-ARIA-0 | (a) waves A/B now, wave C (integration, X1 replacement) after P2-ARIA-0 and CHR-0 merge; (b) wait | **(a)**: A/B touch only new crates and `gorca-model` |
| D-6 | Move M14a earlier | (a) M14a-core in parallel with M22a A/B; (b) keep Wave 2 | **(a)** (§5) |

## 7. Coordination with CHR-0

CHR-0 owns `source.rs`, `doc_registry.rs`, `doc_route.rs`, `toolkit.rs`, the loading-grace/re-join
code in `browse/commands.rs`, `gorca-dbus/src/drain/map.rs` and the interim mode table. M22a waves
A/B touch none of them. Items the CHR-0 review deferred to M22a (`docs/reports/chromium-base-design-review.md`
§5: Blink S-1 rows, SM-8 writer handoff, `Unknown` for adopted nodes, `repair:<gen>` provenance)
are in tasks B5, C2 and the `Basis::DocRev` / repair epoch of B2. The M2 cursor-identity rung stays
with M7b; M22a only provides `BindQuality` and provenance it can use.

## 8. Unverified

- `SemEntry` size with `hedge` (the current r2 layout may already exceed 112 B with two
  `Option<Arc<str>>`); A1 measures and packs masks if needed.
- Exact cross-source keys for elements without an HTML id (TBD-S3); M23's first task.
- Capture latency and window mapping through the gorca-helper `CaptureWindowArea`
  (`contrib/gnome-shell-extension/gorca-helper@gorca/helper.js:43,286`); M24.
- Cost estimates for admission and side-table lookups are design targets until B6 runs.
