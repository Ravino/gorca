# Navigation over virtualized message lists: browse mirror, focus following, detector, identity

Date 2026-10-19, math-researcher. Simulation: `tools/research/virtual_list_nav.py`
(`python tools/research/virtual_list_nav.py all`, about 45 s; `selftest` holds the test vectors).
Inputs are synthetic. Every parameter marked ASSUMED below was not measured on Signal, Telegram Web or Slack.
Related: `chromium-mirror-freshness-math.md` (staleness, M2 identity, the displaced-cursor rule),
`chromium-electron-a11y-research.md` and `chr0-implementation.md` (why browse mode never engaged in Signal),
`signal-orca-vs-gorca.md` (web-a11y-engineer, in progress in parallel; it was not in the tree when this was written).

## 0. Answer in one paragraph

The owner's symptom ("arrows almost don't work in Signal, Orca is fine") was mostly CHR-0. The mirror stayed at
4 nodes and focus never joined it, so this model does not cover that case. Orca is fine because it does no
browse mode in Electron apps at all: `refs/orca/src/orca/document_presenter.py:717` turns on sticky focus mode, and
Signal's own roving focus does the navigation. Once the mirror is fresh, browse mode over a virtualized list fails
in three specific ways. The model separates them, and each has a cheap generic fix:
(F1) a **trailing scroll debounce**, so under key autorepeat the rendered window never follows the cursor (9-27 dead
arrows per 100);
(F2) **node/offset identity** on lists that relabel rows (index-from-bottom keys, conversation lists that reorder),
giving 2-6 wrong lines per 100 at 3 msg/min and 15-37 at 30 msg/min;
(F3) **DOM order ≠ visual order** in slot-pool recyclers, giving a false top or bottom in 33-68 of 100 arrows.
An identity cursor with list-scoped resync and edge prefetch (A2) brings all three down to ≤ 0.3 wrong + ≤ 3 "stale but
right" lines per 100. Its extra latency is under 1 ms per arrow on average. Focus following is correct only when the
app has a roving item focus. Even then, the app's own focus latency (ASSUMED median 15-30 ms, p99 100-270 ms) is 10-100x
gorca's 2 ms mirror path. So it should stay what it is today: triggered by focus on a focusable item (rule D-1), plus
an Electron profile default. A detector should not switch modes on its own.

## 1. Model

Logical list of messages `0..N-1` (0 = oldest), with `L ∈ {1,2,3}` lines each (P = .5/.3/.2). The viewport holds V = 10
rows, with overscan o on each side, so W = V + 2o rows are rendered (W = 20, 40; 60 behaves like 40). Arrivals are
Poisson with rate λ. The app auto-scrolls only when the viewport is at the bottom. gorca's browse cursor moves one line
per arrow. Inter-key time τ is lognormal (median 0.6 s reading, 0.25 s skimming, 0.04 s autorepeat). Each cursor move
schedules `ScrollIntoView` after the 150 ms trailing debounce (`SCROLL_DEBOUNCE`,
`crates/gorca-core/src/browse/commands.rs:77`). The app renders (ASSUMED median 30 ms) and the window shifts minimally.

Virtualizer classes (row key → which message a DOM/AT-SPI object shows):

| class | key of message i | what a window shift / arrival does to objects | examples (ASSUMED, verify) |
|---|---|---|---|
| KEYED | message id | edge rows created/destroyed; others untouched | Signal timeline (keyed windowed loading), Slack |
| INVERTED | N-1-i (index from bottom, or index from top of a list that reorders) | **every arrival relabels every row**; edges created/destroyed | react-virtualized lists keyed by index whose order changes (Signal's conversation list reorders on each new message) |
| POOL | i mod W (slot reuse) | every shift relabels rows; DOM order = slot order (CSS transforms place them) | recycler-style lists (Telegram Web-like) |

Mirror. A DOM change becomes visible after an event lag (ASSUMED median 20 ms). This applies to structural changes
(Chromium fires `children-changed` at subtree roots) and to renames with probability p_ev (0.9 default, 0.5 and 0.0
tested). A silent rename stays invisible until a resync. In A0 that means an unrelated repair (exp, mean 5 s). In
A1/A2 it means the list-scoped resync below.

Policies:
- **A0** (today). The cursor is (node, line). A detached node relocates to the same linear offset
  (`cursor::cursor_at(last_off)`, `commands.rs:2063-2085`). A relabelled node keeps the cursor, which now reads another
  message. The next line follows DOM order. The trailing debounce applies.
- **A0T**. A0 with the debounce replaced by a throttle (leading edge, at most one scroll per 150 ms, one trailing).
- **A1**. The cursor is (message identity, line). It is relocated after every mirror change to the node now showing that
  identity, and moves in the identity's order key (timestamp / posinset), not in DOM order. Any evented structural change
  in the container, and our own `ScrollIntoView`, trigger a **list-scoped resync**: a GetTree of the container, about
  W × 10 nodes (ASSUMED 6 ms).
- **A2**. A1 plus edge prefetch: within E = 2 items of the mirror's edge, scroll the edge item into view at once (no
  debounce). At the edge it does a bounded wait: scroll, then wait ≤ 250 ms for the resync, then retry.

Outcome per arrow, against the logical successor of what the user last heard:
- **ok**;
- **stale_ok**: the right text, but the node already shows another message, so an Enter would hit another item; the
  click guard catches it;
- **jump**: another message (skip, repeat or backwards);
- **stale_jump**;
- **dead**: "top"/"bottom" or nothing, although the list continues.
`disp` is how often A0's displaced flag would be raised (node detached or its content changed since the last speech).

### 1.1 Closed form: probability that the cursor's object is relabelled or replaced between two keys

With inter-key time τ, scroll-shift rate s (rows/s) and arrival rate λ:
- KEYED: P = 0 unless the drift moves the cursor's row out of the window: `P = 1[s τ + λ τ·1(at bottom) ≥ o + V/2]`.
  Under normal reading it is 0.
- INVERTED: `P = 1 - exp(-(λ + s) τ)` for any position. At λ = 0.5/s and E[τ] = 0.68 s, P = 0.29, against a simulated
  `disp` of 27/100.
- POOL: `P = 1 - exp(-(s + λ·1(at bottom)) τ)`. In addition, the DOM order of the pool is a rotation of the visual order,
  so a DOM-order walk meets a false end every W rows. This holds even with a perfectly fresh mirror.

## 2. Results: browse mirror (per 100 arrows, reading upwards, 20 sessions per cell, p_ev = 0.9 unless stated)

| class | W | τ | λ /s | A0 ok / stale_ok / jump+stale_jump / dead | A0 disp | A0T dead | A1 ok / stale / bad | A2 ok / stale / bad / dead | A2 wait ms/arrow |
|---|---|---|---|---|---|---|---|---|---|
| keyed | 20-40 | 0.25-0.6 | 0-0.5 | 100 / 0 / ≤0.1 / 0 | 0 | — | 100 / 0 / 0 | 100 / 0 / 0 / 0 | 0 |
| keyed | 20 | 0.04 (autorepeat) | 0 | 71 / 0 / 1.9 / **27** | 0 | **0.4** | 73 / 0 / 0.8 / 26 dead | 99.7 / 0 / 0.3 / 0 | 0.7 |
| keyed | 40 | 0.04 | 0 | 89.5 / 0 / 0.8 / **9.7** | 0 | **0** | 91 / 0 / 0.2 / 8.4 dead | 100 / 0 / 0 / 0 | 0 |
| inverted | 40 | 0.6 | 0.05 | 92.7 / 1.5 / 5.7 / 0 | 3.5 | — | 99.9 / 0.1 / 0.1 | 99.9 / 0.1 / 0.1 / 0 | 0 |
| inverted | 40 | 0.25 | 0.5 | 77.0 / 4.7 / **18.3** / 0 | 12.2 | — | 99.0 / 0.9 / 0.1 | same as A1 | 0 |
| inverted | 40 | 0.6 | 0.5 | 64.6 / 5.3 / **30.0** / 0 | 25.6 | — | 98.8 / 1.1 / 0.1 | same | 0 |
| inverted | 40 | 0.25 | 0.5, p_ev 0 | 22.4 / 40.9 / **36.7** / 0 | 6.2 | — | 97.2 / 2.9 / 0 | same | 0 |
| inverted | 40 | 0.04 | 0.05 | 90.3 / 0.2 / 0.9 / 8.6 | 0.1 | 0 | 91.7 / 0 / 0.3 / 8.0 dead | 99.9 / 0 / 0.1 / 0 | 0.2 |
| pool | 20 | 0.25-0.6 | 0-0.5 | 31-34 / 0 / ≤0.2 / **66-68** | ≤3.6 | — | 100 / 0 / 0 | 100 / 0 / 0 / 0 | 0 |
| pool | 40 | 0.25-0.6 | 0-0.5 | 65-69 / 0 / ≤0.2 / **31-35** | ≤1.2 | — | 100 / 0 / ≤0.1 | 100 / 0 / ≤0.1 / 0 | 0 |
| pool | 20 | 0.04 | 0 | 33.8 / 0 / 0.1 / 66 | 0 | 66 | 73.5 / 0 / 1.0 / 25.5 dead | 99.8 / 0 / 0.1 / 0.1 | 0.4 |

Reading downwards towards new arrivals (λ = 0.05 and 0.5, τ = 0.6, W = 40) gives the same picture.
- KEYED: 99-100 ok for all policies.
- INVERTED, A0: 91 ok / 5.5 bad at λ = 0.05, and 60 ok / 31 bad at λ = 0.5.
- INVERTED, A1/A2: 99-99.5 ok.
- POOL, A0: 67 ok / 33 dead.
- POOL, A1/A2: 99.3-100 ok.

Readings:
1. **Keyed lists with a fresh mirror are fine for A0 at reading speeds.** The only failure is autorepeat. The trailing
   debounce never fires while the key repeats, so the rendered window stops and the cursor hits its edge. A throttle
   (A0T) removes it: 27 → 0.4 dead per 100 at W = 20, 9.7 → 0 at W = 40. This is a generic one-constant core change.
2. **Relabelling lists break node identity in proportion to λτ** (closed form 1.1). The current displaced rule only
   protects Enter. Reading itself jumps: 18-37 skipped or repeated messages per 100 arrows at 30 msg/min, and 3-6 at
   3 msg/min. Identity cursoring (A1) brings it to ≤ 0.3, plus 1-3 "stale_ok". Those are right for reading, and the
   existing click guard catches them on Enter.
3. **Silent renames** (p_ev = 0) make A0 speak stale text on 40 of 100 arrows. A1's list-scoped resync on any
   structural event or own scroll removes all jumps. What remains is 2.9 stale_ok.
4. **POOL DOM order** is the worst case for any DOM-order walk, and A0T does not help. Only order-key navigation (A1)
   fixes it. A2's prefetch is needed in addition under autorepeat.
5. A2's latency cost is a mean of 0-0.7 ms per arrow. The bounded wait is rare. Each wait is ≤ 250 ms, and only at a
   window edge.

## 3. Focus-following mode: latency and failure modes

Key → speech = app (key handling + React render + AX serialization → focus event) + bus hop + AT-SPI fetch + gorca.
ASSUMED app medians: Signal 22 ms, Slack 30 ms, Telegram Web 15 ms (σ = 0.5-0.6), plus a 4-8 % tail of 60-120 ms (GC,
list re-render). AT-SPI call: median 0.3 ms, with 5 % queued behind a busy Chrome UI thread (+4 ms median).

| app | fetch | p50 ms | p90 | p99 | gorca-only p99 | browse (mirror hit) p99 |
|---|---|---|---|---|---|---|
| Signal | cached / 3 calls / row GetTree / 15 calls | 24 / 26 / 27 / 34 | 51-62 | 166-178 | 2.0 | 2.0 |
| Slack | same | 33 / 35 / 36 / 43 | 91-103 | 247-267 | 2.0 | 2.0 |
| Telegram Web | same | 17 / 18 / 19 / 26 | 34-46 | 104-114 | 2.0 | 2.0 |

The gorca share stays within the 15 ms budget in every case. End to end, though, focus following is 10-100x slower than
a mirror hit, and the app owns the tail. Fetch strategy matters little. A row GetTree (4 calls) is as good as cached;
walking 15 children costs +7 ms p50.

Failure modes, per 100 arrows (model; the rates are ASSUMED, not measured):

| failure | when | rate | mitigation |
|---|---|---|---|
| container focus (arrows scroll pixels, no item focus events) | app has no roving item focus | **100** (total silence) | do not enter focus following unless an item has received focus (D-1 already requires this) |
| relabel without a focus event | INVERTED/POOL with roving focus: the focused element keeps focus, shows another message | 26 at λ = 0.5 (INVERTED), 2.5 POOL | present `accessible-name`/content change of the focused object, as Orca does (`refs/orca/src/orca/scripts/default.py:625-640`). Residual about 10 % of that, when relabels coalesce |
| focus lost to document on re-render | keyed row remounted; pool wrap | 0.3 keyed, 3 pool | existing "focus parked on document" rule (`commands.rs:1015-1045`): stay put, re-anchor to the identity, do not announce the document |
| activedescendant lists | focus stays on the container | — | treat `active-descendant-changed` as focus (verify gorca handles it in Chromium) |

## 4. Virtual-list detector (hybrid policy)

Signals available over AT-SPI on a container with role list / listbox / grid / table / feed / log:
- **declared**: `setsize` > rendered items, `posinset` of the first rendered item > 1, or role feed;
- **churn**: after a scroll episode (our ScrollIntoView, or a scroll/visible-data change in the container), turnover
  `(removed + relabelled)/n ≥ 3 %` at a steady child count (`|Δn| ≤ max(1, 10 % n)`).

**Rule D**: virtual ⇔ declared, or at least 2 churn episodes, counting only episodes attributed to a scroll.

Simulated population (ASSUMED mix): 16 % virtual (keyed / pool / inverted), with 35 % of virtual lists declaring
setsize; the rest are static lists, growing logs, lazy-append infinite lists, paginated tables (30 % declare
aria-rowcount), carousels and filtered lists. 4000 containers per row.

| scroll episodes seen | FPR | FNR | FPR without the scroll attribution |
|---|---|---|---|
| 0 | 2.2 % | 61.6 % | 2.6 % |
| 1 | 2.5 % | 60.5 % | 2.4 % |
| 2 | 2.9 % | 8.6 % | 4.0 % (carousel 24 %) |
| 3 | 2.4 % | 1.4 % | 4.6 % (carousel 37 %) |
| 5 | 2.3 % | 0.1 % | 6.5 % |
| 10 | 2.8 % | 0.0 % | 10.7 % (paginated 59 %, carousel 57 %, filtered 46 %) |

- The residual FPR is the paginated tables that declare a row count. That is an irreducible "declared" false positive.
- Scroll attribution is what keeps carousels and filters out.
- FNR at 0-1 episodes is the undeclared virtual lists. Evidence arrives within the first 2-3 window shifts, which is
  one to two screens of reading.

**What the detector should switch.** The cost of a mistake decides the design:
- **R1 (low regret): within the container, use the A2 cursor** (identity, list resync, prefetch, bounded edge wait).
  - FP cost: one ≤ 250 ms wait at a non-extendable edge. Then mark the container "not extendable" when a scroll
    produced no change, and say "end of list". Also about 6 ms of CPU per structural event in that container.
  - FN cost: the A0 numbers in section 2.
  - Given that cost structure, R1 can even be applied to every list-like container in Chromium documents; the detector
    then only tunes prefetch.
- **R2 (mode switch to focus following)**:
  - FP cost: total silence (100/100) in any list without roving focus, plus 10-100x latency.
  - Use it only when there is positive evidence of a roving item focus: an item in the container received focus
    (D-1). Or by profile: Electron apps default to sticky focus, like Orca, as a per-app/toolkit profile row with the
    owner's override.
  - Do not let the detector alone flip to focus mode.

## 5. Message identity across recycling and "next/previous message"

Keys available over AT-SPI:
- K1: item text;
- K2: + author;
- K3: + displayed time (minute resolution);
- K4: K3 plus the K3 of both neighbours;
- K5: the window **sequence**, i.e. the best shift aligning the old and new windows.

Twin share (messages whose key is not unique), synthetic chat, window 40 / history 1000:

| chat | K1 | K2 | K3 | K4 | K5 alignment ambiguous (K3 / text only) |
|---|---|---|---|---|---|
| group, distinct (6 authors, 10 % short) | 2.5 % / 9.1 % | 1.0 / 6.1 | 0 / 0 | 0 / 0 | 0 / 0 |
| two people, chatty (35 % short) | 21 / 35 | 16 / 34 | 1.3 / 1.2 | 0 / 0 | 0 / 0 |
| two people, "ok/thanks" (60 % short) | 40 / 60 | 33 / 60 | 5.5 / 5.3 | 0 / 0 | 0 / 0 |

- The closed form from the earlier report still applies: P(twin among M) = 1-(1-q)^(M-1), q = Σp(k)².
- K3 alone leaves 1-6 % twins in chatty chats. Neighbour context (K4) or sequence alignment (K5) resolves all of them in
  the simulation. Alignment is ambiguous only if the overlap is periodic in the key, which takes a window made entirely
  of repeated short messages.
- The DOM `id` attribute, which Chromium exposes as an object attribute, would make identity exact if Signal or Slack
  set a stable one. Unverified.
- Twins left after K4/K5 must be **flagged**, not guessed (relocation class "Flagged" of DP-5).

**Algorithm "next/previous message" (proposed core command, generic):**
1. Item = the nearest ancestor of the cursor that is a child of the list-like container (listitem / row / article).
2. Identity = `(K3 hash, prev K3, next K3)`. The order key is `posinset` when present, else the parsed time + DOM
   order, else DOM order (KEYED/INVERTED lists only).
3. next(dir):
   - resolve the current identity in the container index, O(1) expected through a hash map;
   - if an item exists beyond it in dir, move there and speak its first line;
   - else, if the container is extendable (declared, or churn seen, or unknown), `ScrollIntoView(edge item)` now, wait
     ≤ 250 ms for the list resync, then retry;
   - else, if `posinset == setsize`, or a scroll produced no change twice, say "end of list";
   - else say "loading" and keep the identity, so the next press continues from it.
4. Prefetch: when within 2 items of the edge in the moving direction, scroll the edge item immediately (throttled at
   ≤ 1 per 100 ms).
5. On a mirror change: re-index the container in O(W) (hash the items), re-align with the previous window by K5 (shift
   search O(W·D), D ≤ 15, or O(W) with a hash of the anchor), and relocate the cursor by identity. The relocation class
   (Exact / Confident / Flagged / Lost) feeds the displaced rule.

Cost: O(1) per keystroke; O(W·s) per resync (s = nodes per item, about 10, so about 400 nodes, a few ms); O(W)
memory.

## 6. Decision points

| # | Decision | Options | Recommendation (confidence) | Layer |
|---|---|---|---|---|
| VL-1 | Scroll-into-view scheduling | trailing debounce (today) / throttle (leading + 1 trailing, 150 ms) | **Throttle** (high). Removes 9-27 dead arrows per 100 under autorepeat; no downside found | generic core |
| VL-2 | Browse cursor identity inside list-like containers | node + offset (today) / item identity K4 + order key | **Item identity** (high for relabelling lists, neutral for keyed). Ties into DP-4/DP-5 of the freshness report | generic core (identity), connector supplies order attrs |
| VL-3 | Mirror freshness for list containers | event-only + global repairs / list-scoped GetTree resync on structural event or own scroll | **List-scoped resync** (medium-high). Removes stale reading under silent renames; cost about 6 ms per event burst | connector (Chromium quirk row), core triggers |
| VL-4 | Edge behaviour | say top/bottom / prefetch + bounded wait + "loading" | **Prefetch + wait ≤ 250 ms** (medium). Needs a "not extendable" memory per container to avoid repeated waits | generic core |
| VL-5 | Auto-switch to focus following by detector | yes / no; profile default for Electron | **No detector-driven switch** (medium-high). Focus mode only via D-1 (focus on an item) and a profile row "Electron app → sticky focus default, user can override", matching Orca | profile/app-extension row |
| VL-6 | Focus-following correctness | as is / present name/content change of the focused object | **Present it** (high). Without it, relabelling lists go silent on about 26 % of arrows at 30 msg/min | generic core |
| VL-7 | next/previous message command | none / generic item navigation with identity | **Generic** (medium). Per-app add-ons only provide item selectors when the container heuristic fails | core + optional add-on selector |

## 7. Proven vs heuristic

- **Proven** (under the model): the closed forms in 1.1; the twin formula; the POOL false-end argument (a DOM walk
  meets a slot boundary once per W rows unless the DOM order is visual); the debounce starvation argument (a trailing
  debounce never fires if inter-key time < 150 ms).
- **Heuristic or assumed**: all app latencies; the virtualizer class of each app; p_ev; the detector population and
  thresholds (3 %, 10 %, 2 episodes); the resync cost; the focus failure rates; the chat generator.

## 8. Measurements needed (read-only, private Signal profile)

1. **Class of each Signal list.** Record (path, name) of rows before and after a ScrollIntoView and after an arrival
   (`tools/electron-probe/`):
   - paths persist with the same text → KEYED;
   - paths persist with changed text → INVERTED/POOL;
   - compare the DOM order with the extents order (POOL).
2. **p_ev**: count `accessible-name` / `text-changed` events against GetTree diffs for relabelled rows.
3. **App focus latency**: timestamp of the key (KeyboardMonitor) to the focus event, in Signal's timeline and its
   conversation list.
4. Whether Signal/Slack rows carry `setsize`/`posinset` or a stable DOM `id`.
