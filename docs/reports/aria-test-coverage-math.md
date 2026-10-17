# ARIA presentation test coverage: combinatorial design and metamorphic oracles

Date: 2026-10-18. Author role: math-researcher. Tool: `tools/research/aria_cover.py` (pure Python;
`selftest`, `stats`, `gen`, `faults`, `mutants`). All numbers below come from that tool with
`--seed 1 --cands 12` unless stated otherwise.

## 0. Problem statement

The presentation function under test is

    present : Node x Context x Engine x Verbosity x Lang -> Utterance (sequence of kinded segments)

with Context in {focus, browse line, quick-nav, review (object), state-change event}. Today it is
implemented by several code paths (`crates/gorca-core/src/present/utterance.rs::present_web`,
`crates/gorca-core/src/browse/present.rs`, `crates/gorca-core/src/quick.rs`), and the bugs we see are
*divergences between paths* on narrow input combinations (ARIA `role=button` + `aria-label` + icon
content: skipped by browse arrows, role spoken twice on Tab, correct on quick-nav). Two questions:

1. Which inputs to test (the space is ~10^13 raw): combinatorial test design.
2. What to check without golden strings: metamorphic / consistency properties plus an abstract
   model oracle.

## 1. Coverage model

### 1.1 Factors

Split the factors in two classes; this split is the key design decision.

**Page factors** (properties of one DOM node, covered combinatorially): 23 factors.

| factor | values |
|---|---|
| role | 30 roles: button, link, checkbox, radio, switch, textbox, searchbox, combobox, spinbutton, slider, progressbar, heading, img, listitem, tab, option, menuitem, menuitemcheckbox, menuitemradio, treeitem, gridcell, row, columnheader, dialog, alert, status, region, navigation, generic, none |
| impl | native element / ARIA role on div/span |
| name_src | contents, aria-label, aria-labelledby, title, label-for, alt, none |
| content | text, icon (aria-hidden svg), text+icon, split-text (inline spans), empty |
| desc | absent, aria-describedby |
| focus | default, tabindex=0, tabindex=-1 |
| expanded / selected | absent, true, false |
| checked / pressed | absent, true, false, mixed |
| level | absent, 1, 3, 6 (boundaries) |
| posinset (+setsize=5) | absent, first, mid, last |
| current | absent, page, true |
| invalid | absent, true, spelling |
| required / readonly / busy | absent, true |
| disabled | absent, aria-disabled, native disabled |
| haspopup | absent, true, menu, dialog |
| live | absent, polite, assertive |
| hidden | absent, aria-hidden, ancestor aria-hidden, sr-only (visually hidden) |
| container | body, list, table, form, landmark, link |
| value | absent, min, mid, max, valuetext |

**Run factors** (not covered combinatorially; crossed exhaustively): Context (4 navigation contexts +
the event context per state flip), Engine (2), Verbosity (3), Lang (2) = 48 presentations per node.
Reason: every metamorphic property *compares* contexts / languages / engines with each other, so a
node must be visited in all of them anyway, and visiting is cheap (one page load, many traversals; in
the fake-tree tier it is a function call). Covering run factors pairwise would destroy the oracle.

### 1.2 Constraints (ARIA 1.2 / HTML-AAM), in a normal form

Constraints are expressed as

- (a) **unary domains conditioned on (role, impl)**: `ALLOWED[(role, impl)][factor]`, e.g. aria-pressed
  only on button; checked=mixed only on checkbox/menuitemcheckbox/treeitem; level required on native
  `<h1..h6>`; label-for only on labelable native elements; alt only on native img; void natives
  (`input`, `img`, `progress`, `select`) have empty content; aria-label/labelledby prohibited on
  generic/none; no interactive content inside a link; native `disabled` only on native form controls;
  no native element for switch/tab/menuitem/option/treeitem/gridcell/alert;
- (b) a short list of **forbidden pairs** between non-role factors (name_src=contents with content=empty
  is a duplicate of name_src=none, etc.);
- (c) every factor has a safe value (`-` = absent) that no pair constraint forbids.

**Proposition (exactness).** Under (a)-(c), a partial assignment is extendable to a valid full
assignment iff a DFS over (role, impl) and the factors that occur in (b) finds a consistent
assignment; all other factors are completed with any allowed value. Proof sketch: after fixing
(role, impl), remaining constraints are unary or (b); factors not in (b) are independent of each
other and have a non-empty domain. So tuple validity is exact and costs O(|roles| x |impl|) in the
worst case (memoised). This matters: constrained CA generators that only check forbidden tuples
syntactically generate *implicitly invalid* rows; here that cannot happen (asserted in `selftest`).

Invalid ARIA (aria-checked on a link, role=button without tabindex) is real-world content. It is in
the model where it is common (unfocusable ARIA widget = `impl=aria, focus=default`; focusable
aria-hidden = `hidden=aria-hidden, focus=tabindex0`) and otherwise left to a separate robustness
suite with the rule "unsupported attributes are ignored identically in every context".

### 1.3 Size of the space and of the arrays (measured)

| quantity | value |
|---|---|
| raw cartesian page space | 9.0 x 10^12 |
| valid page configurations (exact count) | 6.9 x 10^7 |
| valid (role, impl) pairs | 49 |
| x run factors | 48 presentations per node |
| strength 1 (each value once) | 32 rows (lower bound 30) |
| strength 2 (all valid pairs: 4,088 tuples) | 210 rows (lower bound 157) |
| **mixed**: 2-wise all + 3-wise in two groups (10,165 tuples) | **835 rows** (lower bound 704) |
| strength 3, all 23 factors (85,544 tuples) | 1,358 rows (lower bound 807), 25 s with 3 candidates/row |
| Stein-Lovasz-Johnson upper bound CAN(3, 23, v=7) / (v=31) | 4,561 / 529,713 |

3-way groups (where the observed bugs live):
- G1 naming/reachability: role, impl, name_src, content, focus, hidden;
- G2 state interplay: role, impl, disabled, expanded, checked, pressed, selected.

The lower bound is the number of valid tuples of the largest single interaction (e.g. role x name_src
x content); greedy AETG lands at 1.2-1.7x of it, the usual ratio. Larger role sets scale roughly
linearly in |roles| for t=2 and in |roles| x max-other-domain for t=3.

Generator: weighted, constrained AETG (seed each row with the heaviest uncovered tuple; fill the
remaining factors greedily by weighted gain subject to `feasible`; best of 12 candidates). Complexity
per row O(C x K x v x |interactions touching f|) with memoised feasibility; the full 3-wise array
over 23 factors builds in 25 s in CPython, so a Rust port is not needed for speed.

## 2. Properties (oracle without golden strings)

Notation: for node n and run configuration r = (ctx, eng, verb, lang), `U_r(n)` is the utterance,
`S_r(n)` the multiset of (kind, token) with kind in {Name, Role, State, Level, Value, Description} and
token a **catalog id** (`MsgId`) for Role/State, the literal text for Name. `R_ctx` is the set of
nodes reachable in a context. `M(n)` is the abstract model expectation computed from the generator row
(`expected()` in the tool): accessible name per accname 1.2, role token, required state tokens,
reachability flags. No string is golden; only tokens.

| id | property | catches |
|---|---|---|
| P0 | **Spec oracle**: State tokens of S_r(n) = M(n).states; Name token = M(n).name; Role = M(n).role (at normal+ verbosity) | uniform omissions that all paths share (only property that can) |
| P1 | Name once: for every r with n in R_ctx, count(Name = M(n).name) = 1 | doubled / missing name |
| P2 | Role once, state once: at most one Role segment, each State token at most once | "button button", repeated "current page" |
| P3 | Cross-context consistency: for contexts c, c' where n is reachable, S_c(n) triangle S_c'(n) is a subset of Allowed(c, c', role) (declared table, initially empty except value/description policy) | path divergence (the observed bug class) |
| P4 | Reachability: R_quicknav(key) subset of R_browse; tab-reachable perceivable nodes subset of R_browse; M(n).quicknav = k implies n in R_quicknav(k) | "skipped by browse arrows" |
| P5 | Hidden (metamorphic): the twin page with aria-hidden=true on n removes n and its subtree from R_browse, R_quicknav, R_review **and leaves every other node's utterances unchanged**. Declared exception: a focus event on a focusable hidden node is still spoken | aria-hidden leaks, collateral damage |
| P6 | Flip once: a state change of attribute a from v to v' yields exactly one announcement in the event context; it contains the token for v' and not v; a no-op set (v to v) yields zero; the next focus/browse presentation reflects v' | double announcements, stale states |
| P7 | Language: the sequence of (kind, MsgId) is identical for en and ru; every catalog segment of the ru output comes from the ru catalog (no fallback) | missing translations, order bugs in one language |
| P8 | Verbosity monotone: S_low subset of S_normal subset of S_high, Name always present | verbosity dropping the name |
| P9 | Engine invariance: S over Firefox = S over Chromium for the same HTML, modulo a declared engine table | xml-roles/mapping differences (switch exposed as toggle button) |
| P10 | Determinism: presenting twice yields the same U | caching / epoch bugs |

P3 with Allowed = empty is too strict for real policy (e.g. browse line includes neighbouring inline
text; focus may omit "focusable"); the Allowed table is part of the presentation spec the architect is
designing and should be a data table, not code, so the property and the presenter share it.

P5 and P6 are true metamorphic relations (a transformation of the input with a predicted relation
between outputs); P1-P4, P7-P9 are consistency relations across run factors; P0 is a model oracle.

### 2.1 Fault-detection power (measured, with stated caveats)

**Input coverage vs interaction faults.** A fault is a valid t-tuple of factor values that triggers a
failure; a suite detects it if some row contains it (oracle assumed perfect). 2,000 faults sampled
uniformly (u) or by real-world weight (w):

| suite (N rows) | t=1 | t=2 (u / w) | t=3 in G1 (u / w) | t=3 any triple (u) |
|---|---|---|---|---|
| one row per (role, impl) (49) | 0.54 | 0.23 / 0.55 | 0.09 / 0.30 | 0.06 |
| random valid rows (210) | 1.00 | 0.88 / 0.93 | 0.44 / 0.45 | 0.74 |
| CA strength 2 (210) | 1.00 | **1.00** / 1.00 | 0.51 / 0.62 | 0.84 |
| random valid rows (835) | 1.00 | 0.99 / 0.99 | 0.81 / 0.82 | 0.92 |
| CA mixed (835) | 1.00 | **1.00** | **1.00** | 0.95 |
| CA strength 3 (1,358) | 1.00 | 1.00 | 1.00 | **1.00** (by construction) |

"One example per role", the typical hand-written suite, detects 6-30% of 3-way faults.

**Oracle power (mutation analysis on a toy presenter).** `present()` in the tool is a reference
presenter with the same contexts; 16 mutants model real bug classes, each conditioned on an
interaction (M1 = the observed doubled role: role=button x impl=aria x name_src=aria-label x
content in {icon, text+icon} x focusable, a **5-way** trigger; M2 = the observed browse skip; M13 = a
uniform omission of aria-readonly in every path). Mutation score (fraction of mutants killed by any
property on any row):

| suite | properties incl. P0 | metamorphic only (no P0) | M1 first killed at row |
|---|---|---|---|
| one row per (role, impl), 49 | 0.06 | 0.06 | never |
| random 210 (mean of 20) | 0.92 | - | killed in 30% of suites |
| random 835 (mean of 20) | 0.97 | - | killed in 45% of suites |
| CA-2, 210 | 1.00 | 0.94 (misses M13) | 116 |
| CA-mixed, 835, weighted order | 1.00 | 0.94 (misses M13) | **22** |

Reading: (1) the properties catch every divergence mutant without golden text; the only miss is the
one that is *consistent across all paths*, which by definition needs P0 (or a golden). (2) Mutation
score saturates because most mutants trigger on many rows; the discriminating case is the narrow
5-way M1, which random testing misses 55-70% of the time and the weighted mixed array hits at row 22
because the APG icon-button pattern is boosted. That is prioritisation working, not a guarantee:
5-way coverage is not promised by a 3-way array. (3) The fix for that gap is cheap: **seed the array
with APG pattern rows** (each `PATTERNS` entry completed into a full row, plus every bug that reaches
the tracker as a pinned regression row) before the greedy fill.

## 3. Generated pages

`aria_cover.py gen --strength mixed --out DIR` renders the array (835 cases, 30 per page, 28 pages
for mixed) and a `manifest.json`:

- each case is `<p class="m">Case cN.</p><section class="case">...</section>`: the marker paragraph
  gives the e2e runner a browse-mode anchor; required parents (radiogroup, tablist, listbox, menu,
  tree, row/table, ul) are generated automatically; labels/descriptions are hidden spans referenced by
  id; tri-state native checkboxes get `indeterminate` via script;
- `window.gorcaFlip(id, attr, value)` performs P6 flips from the harness;
- manifest per case: the row, `expected` (name, role token, state tokens, tab/browse reachability,
  quick-nav key), `flips` (attr, old, new), and the P5 twin directive (aria-hidden variant).

Integration, two tiers:

1. **In-process model tier (fast, every `cargo test`)**: convert each manifest case to a fake web node
   (gorca-testkit `fake_doc_source` / `fake_source`): AT-SPI role per engine profile (Core-AAM:
   e.g. switch -> toggle button with `xml-roles=switch` in both engines; button with aria-pressed ->
   toggle button), name, description, state set, object attributes (`xml-roles`, `level`,
   `posinset`, `setsize`, `current`, `haspopup`). Run all 48 run configurations through the real
   presentation functions and check P0-P10 on `Segment` kinds. Full 3-wise (1,358 cases x 48) is
   ~65k presentations: seconds. Requires one testability hook: `Segment` should carry the catalog
   `MsgId` (behind a test feature) so P7 compares ids, not words.
2. **E2E tier (tests/web, `crates/gorca-bench` e2e rig)**: serve the pages to a private Firefox /
   Chromium, walk each page in browse mode (Down to each marker), quick-nav per key, Tab, and the flip
   hook; collect the mock speech server transcript per case and apply the same property checker. Run
   the weighted-order prefix: 15 rows give 50% and 169 rows 90% of the weighted mixed-tuple mass (1
   and 6 pages of 30 cases); nightly runs the full array. The engine mapping table used by the fake
   tier must be validated against tier 2 (P9 failures in tier 2 that tier 1 does not show mean the fake
   engine profile is wrong).

## 4. Prioritisation

Tuple weight = product of per-value weights x max pattern boost. Value weights are heuristic
real-world frequencies (role: link 10, button 8, heading/img/listitem 6, textbox/generic 4, ...;
absent state 4 vs any set value 1; native 3 vs ARIA 1; aria-hidden on a control 1 vs absent 10).
Pattern boosts (x3 to x6) come from APG widgets and common authoring patterns: icon button,
toggle button, menu button, nav link with aria-current=page, tri-state checkbox, tab with
selected+posinset, combobox, tree item expanded with level, labelled required invalid textbox,
heading inside a card link, clickable div with tabindex=0, polite live region, icon img.

Greedy AETG on weighted gain yields an order where the weighted coverage curve is front-loaded:

| strength | rows to 50% / 90% / 99% weighted coverage | rows to 90% unweighted |
|---|---|---|
| 2 | 7 / 57 / 149 | 125 |
| mixed | 15 / 169 / 502 | 502 |
| 3 | 21 / 210 / 712 | 646 |

The first case of the mixed array is exactly the reported bug pattern (`<button aria-label>` with an
icon). The weights are guesses; they change the order, not the coverage guarantee. Replace them with
measured frequencies (HTTP Archive / WebAIM Million ARIA usage) when available.

## 5. Proven vs heuristic

Proven / exact: tuple-validity exactness under the constraint normal form; every listed valid t-tuple
is covered (asserted for t=2 in `selftest`, by construction of the generator loop for the others);
exact counts of the valid space.
Measured but model-dependent: array sizes (greedy, seed-dependent, +/- 5%); fault detection numbers
(depend on the fault distribution, and the random baseline samples each factor uniformly, which
over-represents set states and so flatters random); mutation scores (toy presenter, hand-written
mutants; they show which property class catches which bug class, not real gorca defect rates).
Heuristic: weights, pattern boosts, choice of 3-way groups, the role subset.

## 6. Porting notes (Rust)

- The generator stays in Python as a build-time tool; check the generated manifest + pages into
  `tests/web/aria_cover/` (deterministic seed) and regenerate on spec changes. The array is data.
- The property checker should be Rust (gorca-testkit) so both tiers share it: input
  `Vec<(CaseId, RunConfig, Option<Utterance>)>` plus the manifest; output violations with the property
  id. Keep the Allowed(c, c', role) and engine tables as data shared with the presentation layer.
- Feasibility: port `ALLOWED` as a `[(Role, Impl)] -> [u64 bitmask per factor]` table; validity is a
  bitmask AND plus a tiny DFS over the 4 coupled factors.
- Seeds: pinned regression rows (from bug reports) and APG pattern rows are listed in the manifest
  before the greedy rows and never reordered.
