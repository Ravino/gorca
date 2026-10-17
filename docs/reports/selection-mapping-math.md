# Browse-mode selection mapping: formal model, algorithms, copy spec (2026-10-17)

Author: math-researcher. Scope: read-only analysis of the selection mirror on branch
`worktree-agent-aa24d6c83ee085224` (head 435d8a13 plus the uncommitted diff), and a reference
model in `tools/research/selection_mapping.py`.

Reproduce:

```
. ./env.sh
python tools/research/selection_mapping.py            # property tests + statistics
python tools/research/selection_mapping.py --brute    # minimality / impossibility vs exhaustive search
python tools/research/selection_mapping.py --vectors  # test vectors V1..V9
```

## 0. Verdict

* Every flat range has an **exact preimage in atom space**: the DOM-selectable units (text
  scalars, images) it contains form one contiguous interval. Only separators (block breaks, cell
  gaps) and markers of empty controls are lost, and they have no DOM counterpart.
* **Chromium can take that interval exactly in one call.** `org.a11y.atspi.Document.SetTextSelections`
  (signature `a((so)i(so)ib)`) takes a start object and offset and an end object and offset.
  Chromium implements it when ATK is at least 2.52 (`refs/chromium/ui/accessibility/platform/ax_platform_node_auralinux.cc:176,4758`),
  and the host has ATK 2.56. In the model it is exact on 100 % of ranges. The current
  single "snap" range on the common ancestor is exact on only 48 %.
* **Firefox cannot, in general.** Its ATK Document interface lacks that method
  (`refs/firefox/accessible/atk/nsMaiInterfaceDocument.cpp:28-37`). `Text.SetSelection` ranges are
  confined to one container, and an end offset that names a hypertext child takes the whole child
  (`HyperTextAccessibleBase::ToTextLeafPoint`, `:317-337`). From this we prove two things.
  First, a selection that ends inside a hypertext child, or continues into one, right after a text
  character has **no exact representation by any set of ranges** (Proposition G2).
  Second, the browser joins several ranges with no separator. On random documents the best
  possible Gecko plan copies exactly 54 % of ranges, and 72 % of whole-line ranges.
* **Mode B (gorca renders the text) is exact by construction** and is the only copy semantics that
  is the same on both engines. Recommendation: B is the copy truth. Use A on Firefox only behind
  the exactness certificate of section 4.4, and use A on Chromium through `Document.SetTextSelections`.

## 1. Model

### 1.1 Tree and hypertext

Here *T* is the mirror tree (`WebDoc`) after the TX-1 run layer
(`crates/gorca-doc-source-atspi/src/runs.rs`). It has three kinds of nodes:

* **leaf** ℓ: `Role::StaticText` with text t(ℓ), one run of a container's hypertext;
* **hypertext container** *a*: every other node that exposes `Text`. Its children are leaves and
  embedded objects;
* **object** *o*: an embedded child that is not hypertext in Gecko (an image). Gecko maps it to
  `TextLeafPoint(o, 0|1)`.

The hypertext of *a* is H(a) = concat over the children x of: t(x) if x is a leaf, else U+FFFC.
The offset unit depends on the engine (verified in the sources):

| engine | AT-SPI text offset unit | source |
|---|---|---|
| Firefox | UTF-16 units (an astral scalar is followed by a phantom U+FEFF, so the ATK character count equals UTF-16 length) | `runs.rs` header, TU-2 |
| Chromium | Unicode scalars; Blink converts with `UnicodeToUTF16OffsetInText` | `ax_platform_node_auralinux.cc:1041,4948` |

The notation is u(x) = units of x in its parent's hypertext, and pos_a(x) = Σ u(y) over the
siblings y before x.

### 1.2 Flat projection P

The flat buffer F = P(T) is the preorder concatenation of the node contributions
lead(v) · body(v) (`doc.rs` `own_chars`, `translate.rs` `assign_breaks`, `marker_role`). Each
flat character has one class:

| class | produced by | DOM counterpart |
|---|---|---|
| **T** | a leaf scalar | one atom (a text scalar) |
| **O** | U+FFFC marker of an image | one atom (the object) |
| **O0** | U+FFFC marker of an empty control (button, entry, checkbox, ...) | none (zero DOM text) |
| **NL** | lead `\n` (`BREAK_BEFORE`: a block, or the node after one) | none (a block boundary) |
| **CS** | lead ` ` (`CELL_SEP`) | none (a cell boundary) |
| **R** | role or state words | not present in this branch's flat text (the presenter speaks them); if ever added, treat as O0 |

List bullets ("• ", "1. ") reach the mirror as part of the `li` hypertext, so they are class T.
They are accessibility text with no DOM text node, which matters for mode A (section 7).

**Atoms** A = the T and O characters in flat order, numbered 0..|A|-1. For atom k,
`flat(k)` is its flat offset and `cont(k)` is the container whose hypertext holds it.

**Lemma P1 (order).** `flat` is strictly increasing. P is a preorder concatenation, and a
container's hypertext lists its children in the same order. *Tested:* `atom_flat == sorted(atom_flat)`
holds on 200 random documents.

### 1.3 Inverse of a flat range

For a flat range r = [lo, hi), P⁻¹(r) = { k : lo ≤ flat(k) < hi }. By P1 this is a contiguous
interval [p, q), or empty when r contains only NL, CS or O0 characters.

* **Exact preimage.** Every r with a non-empty P⁻¹ has an exact preimage in atom space.
  Container boundaries do not matter at this level. Only the engine API can lose precision
  (section 3).
* **Rounding at the flat level.** hull(p, q) = [flat(p), flat(q-1)+1) ⊆ r. The difference
  r \ hull consists only of NL, CS and O0 characters at the two ends (property P2, tested).
  Define r ≡ r' iff P⁻¹(r) = P⁻¹(r'). The canonical representative of a class is its hull.
* **What is provably lost by any DOM selection:** NL and CS (they are re-created only by the
  engine's serializer, and only inside one range), O0 markers, and R words. A selection made only
  of such characters selects nothing in the page.

## 2. Engine semantics of one range

Each resolver maps an AT-SPI range (a, s, e) to an atom interval. All three are implemented in the
prototype.

* **Gecko** (`ToTextLeafPoint`, `HyperTextAccessibleBase.cpp:317`; child lookup `:14-62`). The
  child at offset o is the one whose span holds o, and o = len selects the last child.
  - Start point: at a hypertext child x it descends to the start of x; at a leaf or object it is
    exact.
  - End point (`aDescendToEnd`): at a hypertext child x it descends to **the end of x**, so x
    comes along.
  - Measured (`ctrl-c-passthrough.md`): end = len with a trailing embedded child is refused at
    ATK level, so len-1 must be used.
* **Blink `Text.SetSelection`**: one range per document, and ends are exclusive at child
  boundaries. Offsets are scalars.
* **Blink `Document.SetTextSelections`**: the endpoints may sit in different containers and are
  resolved with upstream affinity (`ax_platform_node_auralinux.cc:4758-4810`). There is one
  range, `start_is_active` gives its direction, and objects must be descendants of the document.

## 3. Expressibility (Gecko)

Every leg is a range on a single container, and its endpoints resolve as in section 2.

**G1. Start points are always expressible.** A start before atom k is (cont(k), offset of k).
Gecko's start resolution at a leaf or object offset is exact.

**G2. Proposition (impossibility).** Let τ be the last atom before a hypertext child x with at
least one atom, inside the same container a = parent(x) (τ is a leaf scalar or an image of a).
If τ ∈ S and S does not contain every atom of x, then S has no exact representation.

*Proof.* A range that covers τ lives on a container b that is a or an ancestor of a, because
ranges cover only atoms of their own subtree. On b = a, its end offset must exceed τ's offset, so
it is ≥ pos_a(x). Offset pos_a(x) resolves to the end of x (descent), and larger offsets resolve
to boundaries at or after the end of x. In both cases every atom of x is covered. On a proper
ancestor b, the child of b that contains a also contains x, and any end offset in b resolves to
the start or the end of a whole child of b. Covering τ needs an end at or after the end of that
child, so all of x is covered again. ∎

*Verified exhaustively:* on 246 random selections that the planner flags as G2, an exhaustive
search over every Gecko range of the document found no exact cover. That search found 0
counterexamples.

Cases that occur in practice:
- (i) the selection ends inside a link right after "See " (V2);
- (ii) the selection runs from list-item text into a nested list (V3: `<li>Item<ul>...`);
- (iii) the selection ends right before a link at a letter.

The available roundings are **up** (all of x comes along, so the selection grows to the end of
x) and **down** (τ is lost: one character, or one image).

**G3 (trailing object, measured, not proven from code).** The end of a container whose last child
is an image cannot be named in that container, because of the measured refusal. It can be named in
the parent only when the whole container is selected. The prototype assumes the refusal. The
source code (`GetChildIndexAtOffset` returns the last child at o = len) suggests that Gecko might
accept it. **Unverified: probe.**

**Single-range criterion.** Let c = LCA(cont(p), cont(q-1)). The interval [p, q) is one exact
Gecko range iff both of these hold:
- the start lifts to c, or already lies in c;
- the end lifts to c, or already lies in c, and its end offset is not a G2 boundary.

Lifting is defined in section 4.2. The current `whole_start`/`whole_end` tests in selmirror are a
stricter version: they lift only from the first-leaf chain, and only all the way to c.

**Separators.** Firefox joins several ranges with **no** separator (measured: `delta.Echo`). A
multi-range copy is therefore string-exact iff no junction between consecutive ranges is a block
or cell boundary.

## 4. Algorithms

### 4.1 Flat range → atom interval

`P⁻¹(lo, hi)`: find the first atom with `flat ≥ lo` and the last with `flat < hi`. With the
`WebDoc` treap (`at_offset` is O(log n)), scan forward from `at_offset(lo)` to the first T or O
character, and backward from `at_offset(hi-1)`. Each scan is O(log n + s), where s is the length
of the separator run (bounded by the tree depth). Empty → nothing to select.

### 4.2 Atom interval → minimal Gecko legs (multi-range engine)

```
plan_lifted(p, q):
  (a, s) := (cont(p), hyper offset before p)
  (b, e) := (cont(q-1), hyper offset after q-1)
  c := LCA(a, b)
  while a != c and p == first_atom(a):  s := pos(a); a := parent(a)          # lift start
  while b != c and q == end_atom(b):    e := pos(b)+1; b := parent(b)        # lift end
  while a == b == c and p == first_atom(c) and q == end_atom(c) and c has parent:
        s := pos(c); e := s+1; a := b := c := parent(c)                      # whole container
  if a == c and b == c: legs := [(c, s, e)]
  else:
     legs := [(a, s, len(a))] ++ [(x, pos(child)+1, len(x)) for x on a..c exclusive]
             ++ [(c, mid_s, mid_e)]
             ++ [(x, 0, pos(child)) for x on c..b exclusive, top-down] ++ [(b, 0, e)]
  drop legs whose atom interval is empty
  each end offset e' of a leg on container n goes through gecko_end(n, e'):
     child before e' is hypertext        -> e'-1  (descends to that child's end: exact)
     e' == len and last child is a leaf  -> e'
     e' == len and last child is an image -> G3 (inexact)
     child at e' is a non-empty hypertext -> G2 (inexact; round up = e', down = e'-1)
     otherwise                          -> e'
```

Lifting uses **atom equivalence** (`p == first_atom(a)`), not `offset == 0`. Empty siblings
(an empty link, an empty cell, an O0 control) would otherwise block a lift.

**Proposition L (minimality), empirical.** When no leg is inexact, `plan_lifted` uses the
fewest ranges of any exact Gecko cover. The exhaustive check enumerates every (container, s, e)
of the document, resolves each to atoms, and runs an optimal greedy segment cover. *Result:*
1464 of 1464 agree (`--brute`). Fewer ranges means fewer junctions, so fewer separators can be
lost.

Complexity: O(h · f) per plan, with h the tree depth and f the largest fan-out on the two chains
(`pos` sums sibling units). The number of legs is at most 2h + 1. A password check over the range
must not walk every node: see bug S-7.

### 4.3 Single range

* **Chromium, exact:** `Document.SetTextSelections([((obj(cont(p)), off_p), (obj(cont(q-1)), off_q), active_is_start)])`,
  with offsets in **scalars**. O(h · f). It is exact in 100 % of cases (property P3: the resolved
  interval equals [p, q) on every sampled range).
* **Fallback** (ATK < 2.52, or the call returns false): the snap range on c, rounded out to
  whole children of c. This is a cover, exact only under the single-range criterion of section 3.
* **Gecko, single:** the same snap, with `gecko_end` applied to its end.

### 4.4 Exactness certificate (decides A vs B on Firefox)

`certify(plan)` is true iff both hold:
- no leg is G2 or G3 (no inexact end);
- for each pair of consecutive ranges, the flat text between the last atom of one and the first
  atom of the next contains no NL or CS.

It is computed from the plan in O(legs · s).

*Sound (tested, 0 violations):* if `certify` holds, the browser copy equals mode B. It is not
complete: a junction next to an image with empty alt is rejected, although nothing would be lost.

## 5. selmirror.rs: findings

Each finding was checked against the Python port `sm_plan`, `sm_apply_*`, and is reproduced by
the vector named in brackets.

| # | sev | finding | fix |
|---|---|---|---|
| S-1 | high | **Chromium offsets are scalars, selmirror sends UTF-16.** On text with an astral character (emoji), the range is shifted by one per astral, or refused past the end. [V8] With astral text in the model, Blink-exact drops from 48 % to 37 % and not-mirrored rises from 0.4 % to 8.7 %. | Per-engine unit (`EngineQuirks`): Gecko = UTF-16, Blink = scalars. Better: make the span engine-neutral in scalars and let the connector convert. |
| S-2 | high | **Chromium snap rounds to whole blocks** although `Document.SetTextSelections` is exact. [V3, V4, V5] Over-selection on 47.6 % of random ranges. | Call `Document.SetTextSelections` first, and keep the snap as the fallback. Probe in the private session (focus side effects unverified). |
| S-3 | high | **`trim_ws` on a junction leg loses a real space.** A leg that ends before a hypertext child the selection continues into is trimmed, and Firefox glues the legs: "See li" is copied as "Seeli". [V2] | Trim only on the leg that holds the selection's final end. On a junction G2 has no exact answer: return "inexact" and let mode B copy. |
| S-4 | med | **The mid leg ends before x_e when a text leaf precedes x_e.** Gecko then takes all of x_e, and the head leg inside x_e duplicates it: over-selection to the end of the block. [V3: nested lists, `<li>text<ul>`] | Same as S-3 (it is G2): flag it, and do not send it as exact. |
| S-5 | med | **`end_on_child` on a non-hypertext child (image).** `gecko_range` sends end-1, and Gecko resolves that to before the image, so the image is excluded. [V6] | `end_on_child` must distinguish hypertext children from objects. For an object, send e, not e-1. |
| S-6 | med | **Images (and other O atoms) at the range ends are dropped.** `leaves()` considers text leaves only, so a selection of just an image plans nothing, and the page selection is then cleared. [V7] | Plan in atom space (section 4.1), so that O atoms count. |
| S-7 | low | **Password guard bypass.** `leaves()` breaks on a `PasswordEntry` met in preorder, but a range whose start lies inside the password field's runs starts below the entry and never meets it. It is mitigated by the focusable-start guard unless the field is focused. Also, for Ctrl+A, the forward scan is O(n) per plan. | Check the ancestors of the first leaf. Better: add a "secret" category count to the treap and test the range in O(log n). |
| S-8 | low | **No lifting through intermediate levels.** A start at the first atom of an inner container (a list item) is emitted as a separate leg, which adds a junction and loses a `\n`. [V5: 3 legs vs 2] | §4.2 lifting. |
| S-9 | low | `whole_start`/`whole_end` use `first_leaf`/`last_leaf` (first-child chains). An empty or O0 first child blocks them. | Atom-equivalence tests (`p == first_atom(x)`). |
| S-10 | info | The uncommitted `clear_selection` says "never `SetSelection(0,0,0)`" but still calls `range_call(path, true, 0..0)` after `remove_ranges(path, 1)`. R2-1 is not fixed yet in the working tree. | `remove_ranges(path, 0)` and no `SetSelection`. |
| S-11 | info | The hypertext model assumes that the number of embedded children equals the number of U+FFFC characters. `runs.rs` pairs them by position when they disagree (Chromium), and the offsets are then silently wrong. | Keep the source's `CharacterCount` per container. Refuse to mirror (and fall back to B) when Σ u(x) ≠ CharacterCount. |

Aggregate, random documents (300 docs × 40 ranges of char, word or line granularity; no astral):

| plan | exact copy | exact atoms, glued | over | under | not mirrored |
|---|---|---|---|---|---|
| selmirror, Gecko | 49.3 % | 26.1 % | 5.5 % | 18.1 % | 0.5 % |
| selmirror snap only, Gecko | 46.4 % | 0 | 48.1 % | 4.1 % | 0.5 % |
| lifted (§4.2), Gecko, round up | 54.5 % | 28.5 % | 14.8 % | 1.7 % | 0.1 % |
| lifted, Gecko, line-granular ranges | 72.3 % | 19.5 % | 8.2 % | 0 | 0 |
| selmirror snap, Blink | 48.1 % | 0 | 47.6 % | 3.1 % | 0.4 % |
| `Document.SetTextSelections`, Blink | **100 %** | 0 | 0 | 0 | 0 |
| mode B | **100 %** (by definition) | | | | |

"Exact copy" compares the modelled browser plain text against the mode-B text of the same atoms.
The generator is synthetic: paragraphs with links, spans and images, headings, lists with nested
lists, tables, and mixed `div` content. The numbers show the shape of the trade-off, not real-page
frequencies.

## 6. Round trip and fixpoint

After the mirror is set, the engine fires `object:text-selection-changed`, and it may fire
`caret-moved`. Browse mode in this branch ignores both, so it cannot oscillate today. It also
cannot adopt a mouse selection. If either is added, the following holds.

Notation:
- M(r): the page selection that gorca sets for flat range r;
- σ(S): the atom set of a page selection S. Read it back with `Text.GetNSelections` /
  `GetSelection` on the **document**, or with `Document.GetTextSelections` on Chromium;
- P̂(S) = hull(σ(S)) is the flat range that S denotes. When S has gaps, its hull is used.

**Fixpoint condition.** The reader state r is a fixpoint of the loop
"event → r := P̂(S) → mirror" iff σ(M(P̂(M(r)))) = σ(M(r)). For the lifted planner with up
rounding, M ∘ P̂ ∘ M = M on atoms. *Tested (P5), 0 violations.* The reason: P̂(M(r)) is a hull
whose ends are atoms that M covered, and re-planning that hull reproduces the same legs. Up
rounding is extensive and idempotent.

The loop must still not be closed naively. Up rounding would make the user's selection grow to
the rounded one, so an echo must never rewrite r. The protocol is a state machine in the Reader:

```
enum MirrorEcho { Idle, InFlight { gen: u64, expect: AtomSet, deadline: Instant } }
on mirror(r):        state = InFlight { gen+1, σ(M(r)), now + 500 ms }; send M(r)
on sel-changed(S):   match state {
   InFlight{expect,..} if σ(S) == expect      => state = Idle            // echo: say nothing
   InFlight{deadline,..} if now < deadline     => ignore                  // partial echoes of multi-range sets
   _ if σ(S) == σ(M(r))                        => ignore                  // late echo
   _                                           => external change: r := P̂(S); announce per §8 }
```

`σ` comparisons need atom identity: (node id, scalar index), and O atoms by node id. Gecko emits
one event per range touched, so the intermediate states of a multi-range set must not count as
external. That is why the deadline exists.

Invariants for property tests (prototype names in brackets):

* **I1** P⁻¹(r) is contiguous, and hull(P⁻¹(r)) ⊆ r with only NL, CS or O0 in the difference [P2].
* **I2** Exact engine: σ(M(r)) = P⁻¹(r) [P3, Chromium document selection].
* **I3** Gecko up: σ(M(r)) ⊇ P⁻¹(r) unless G3 [P4]. Gecko down: σ(M(r)) ⊆ P⁻¹(r).
* **I4** If no leg is inexact, then σ(M(r)) = P⁻¹(r) [P4].
* **I5** Idempotence: σ(M(P̂(M(r)))) = σ(M(r)) [P5].
* **I6** Soundness: certify(M(r)) ⇒ copy(M(r)) = B(r) [P7].
* **I7** B(r) contains no U+FFFC, and does not start or end with `\n` or `\t` [P6].
* **I8** Never mirror a password atom: σ(M(r)) ∩ secret = ∅ (needs the S-7 fix).
* **I9** Minimality: legs(M(r)) = min exact cover, when one exists [`--brute`].

## 7. Mode B: copy text as a function of the flat range

B(r) is defined over F[lo, hi). Walk the characters in order with a pending separator
`pend ∈ {none, TAB, LF}`:

| class | rule |
|---|---|
| T | emit the scalar. **Exception:** inside a `PasswordEntry` subtree, emit nothing, ever. |
| T list bullet | emit it (it is accessibility text; NVDA copies it because it is in the IA2 text). |
| O (image) | emit the accessible name (alt). Empty alt: nothing. No name: nothing (NVDA would derive a file name; we do not). |
| O0 button with no text | emit its name (NVDA: `nameIsContent`). |
| O0 other controls | nothing. The value of a combo box, slider or spin button is emitted only if a later spec adds it (NVDA emits `value`). |
| R (role/state words) | never. |
| NL | pend := LF |
| CS | pend := TAB unless pend = LF |
| — | before emitting text: if output is non-empty and pend ≠ none, emit pend; then pend := none |

Consequences:
- runs of separators collapse to one;
- leading and trailing separators are dropped;
- table cells are joined by TAB and rows by LF, so a pasted table goes into spreadsheet columns
  (V9: `a1\tb1\na2\tb2`);
- line ending is LF; the receiving application converts if it wants CRLF.

Compared with NVDA (`refs/nvda/source/virtualBuffers/__init__.py:496`: paragraphs
`strip("\r\n")`, joined by CRLF; Gecko backend `nvdaHelper/vbufBackends/gecko_ia2/gecko_ia2.cpp:686-707,1257-1330`):

| content | NVDA copy | gorca B | note |
|---|---|---|---|
| block boundary | CRLF | LF | Linux convention |
| empty block | an empty line | collapsed | NVDA keeps empty paragraphs |
| table cell | CRLF (a cell is a block, `display: table-cell`) | TAB | deliberate: rows stay rows |
| image with alt | alt | alt | same |
| alt="" | nothing | nothing | same |
| unlabelled image in a link | file name from URL | nothing | NVDA heuristic, not adopted |
| empty cell | a space | nothing between TABs | columns keep alignment |
| button / link | name or text | text (name if no text) | same |
| form field value | value | entry text (runs) | same |
| password | bullets as exposed | nothing | stricter |
| list bullet | yes (IA2 text) | yes | same |
| role words | no | no | same |

B is O(hi - lo) using `WebDoc::text_in` plus per-node class lookup. Text is never logged
(the `ClipText` rule).

## 8. Switching models with an active selection

The virtual flat range r is the source of truth in browse mode under both copy models. A "switch"
changes which side is authoritative.

* **B → A** (the user turns the native mirror on, or the reader moves from browse to focus mode
  and hands the selection to the page): send M(r).
  - Let Δ+ = σ(M(r)) \ P⁻¹(r) and Δ- = P⁻¹(r) \ σ(M(r)).
  - Announce only when the copy would differ, that is when certify(M(r)) fails.
  - Message: "native selection differs: N more" (Δ+) and/or "N fewer" (Δ-), counted in scalars.
    For glue, say "line breaks lost".
  - Never announce for differences that are separators only (r ≡ hull).
* **A → B** (the page has a selection made by mouse or page script, or in focus mode, and the
  user enters browse mode or turns the mirror off): r := P̂(S).
  - Exact when S is one range.
  - When S has several ranges with atoms in the gaps, the hull adds those atoms. Announce
    "N ranges merged".
  - Atoms not in the mirror (hidden, not yet fetched) are clamped to the nearest mirrored atom.
    Announce "selection partly outside the document view".
* **Mode toggle while a mirror is in flight:** cancel by generation (section 6). The new mode
  applies to the next selection change, and the clipboard is never touched by the toggle itself.

## 9. Porting notes (Rust)

* Plan in **atom space**: `AtomPos = (DocNodeId, u32 scalar)`. Keep scalar offsets engine-neutral
  in `SelSpan`, and let the connector convert units (Gecko UTF-16, Blink scalars).
* `SelSpan` gains `end_kind: { Exact, DescendBefore /*send e-1*/, Inexpressible(G2|G3) }`, computed
  in core from the child kind (hypertext or object). Core never needs the engine name, only
  whether the child is an embedded hypertext.
* `Plan` gains `certified: bool` (§4.4) and `atoms: (AtomPos, AtomPos)` for echo comparison.
* `DocAction::SetSelection` gains a `DocumentRange { start: (node, off), end: (node, off), forward: bool }`
  variant. The AT-SPI source tries `org.a11y.atspi.Document.SetTextSelections` (`a((so)i(so)ib)`)
  and falls back to the spans on `false` or an error.
* Add the secret-category count to the treap summaries (S-7).
* Port the prototype's property tests as `proptest`. The test vectors V1..V9 become unit tests
  with expected spans per engine.

## 10. Unverified / open

* G3 (refusal of end = len after an image) is measured for a hypertext child only. Probe it for an
  image.
* `Document.SetTextSelections` in Chromium 154 on this host: return value, focus side effects
  (BM-2 analog), event shape. Code-read only.
* Firefox plain-text copy of list bullets and image alt text: the model assumes it matches B
  inside one range. Measure with `tests/web/selcopy`.
* Statistics come from a synthetic generator. Real-page frequencies of G2 (nested lists, partial
  links) are unknown. Running the planner over `research/web-graph/real-trees` would need text
  content those dumps do not have.
