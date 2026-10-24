#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests of `compose` (M22b1 cut-over): the replay half of the cut-over gate, kept after the
//! legacy presenters were deleted. Every recorded corpus family is fused the way the app actor
//! fuses it and run through `gorca-present`, in English and in Russian: the focus utterance of
//! every node, the utterance of every synthetic state flip and of every recorded real change
//! (nodes paired by path or fixture id, value changes included) must equal the checked-in
//! records `tests/fixtures/sem/golden/{focus,change}.{en,ru}.tsv`.
//!
//! The replay selects exactly the nodes and cases the gate compared (`web_flags`, `match_nodes`,
//! `spoken_facts_differ`), and `the_goldens_hold_every_record_the_gate_compared` pins the counts
//! the gate reported when it passed (6497 focus and 3750 change records per language). Every
//! difference to the legacy presenter was zero or a dated, reasoned deviation row at that point.
//!
//! `GORCA_BLESS=1 cargo test -p gorca-connector-atspi --lib golden` rewrites the files (review the
//! diff: every changed line is a change of what a screen reader user hears). The replay is
//! repeatable and independent of the arrival order of the nodes (a seeded shuffle gives the same
//! records).
//!
//! Record format, one line per compared node, tab separated and sorted:
//! `corpus  node  case  surface  tokens` with `\`, tab and newline escaped; `case` is `focus`, the
//! state mutation applied for `change`, or `real-<case>`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, Role, StateSet, WebEngine};
use gorca_present::{ComposeCtx, Reason, RenderOpts, ReportFlags, compose, compose_change, render};

use crate::decode::RawNode;
use crate::legacy_agreement_tests::{NodeMeta, page, page_meta, page_with, pages_of, root};

/// A corpus family: directory under `tests/fixtures/sem`, toolkit name for the profile tables,
/// and the web engine (`None` for native applications).
struct Family {
    dir: &'static str,
    toolkit: &'static str,
    web: WebEngine,
}

/// Keep in step with `FAMILIES` in `xtask/src/cutover.rs`.
const FAMILIES: &[Family] = &[
    Family {
        dir: "gtk3",
        toolkit: "GTK",
        web: WebEngine::None,
    },
    Family {
        dir: "gtk4",
        toolkit: "GTK",
        web: WebEngine::None,
    },
    Family {
        dir: "firefox",
        toolkit: "",
        web: WebEngine::Gecko,
    },
    Family {
        dir: "chromium",
        toolkit: "",
        web: WebEngine::Blink,
    },
    Family {
        dir: "electron",
        toolkit: "",
        web: WebEngine::Blink,
    },
    Family {
        dir: "telegram",
        toolkit: "",
        web: WebEngine::Blink,
    },
    // GNOME Shell: Clutter/St actors through cally; no toolkit attribute the profile table knows.
    Family {
        dir: "gnome-shell",
        toolkit: "",
        web: WebEngine::None,
    },
    // gnome-terminal: GTK 3 widgets around a VTE terminal.
    Family {
        dir: "vte",
        toolkit: "GTK",
        web: WebEngine::None,
    },
];

/// One output line: a compared node, or the drops of one (corpus, case).
struct Record {
    corpus: String,
    node: u64,
    case: String,
    surface: &'static str,
    /// `Some((reason, count))` for dropped nodes or cases: they are not golden lines.
    dropped: Option<(&'static str, u64)>,
    new: String,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn join<'a>(it: impl Iterator<Item = (gorca_model::SegmentKind, &'a str)>) -> String {
    it.map(|(k, t)| format!("{k:?}:{t}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

// ---------------------------------------------------------------------------------------------
// Drops: nothing the replay does not compare is skipped silently
// ---------------------------------------------------------------------------------------------

/// Drops of one (corpus, case): per reason a count and the first example.
#[derive(Default)]
struct Drops(BTreeMap<&'static str, (u64, String)>);

impl Drops {
    fn add_n(&mut self, reason: &'static str, n: u64, example: impl FnOnce() -> String) {
        if n == 0 {
            return;
        }
        let e = self.0.entry(reason).or_insert_with(|| (0, example()));
        e.0 += n;
    }

    fn add(&mut self, reason: &'static str, example: impl FnOnce() -> String) {
        self.add_n(reason, 1, example);
    }

    fn flush(self, corpus: &str, case: &str, surface: &'static str, out: &mut Vec<Record>) {
        for (reason, (n, example)) in self.0 {
            out.push(dropped(corpus, case, surface, reason, n, example));
        }
    }
}

fn dropped(
    corpus: &str,
    case: &str,
    surface: &'static str,
    reason: &'static str,
    n: u64,
    example: String,
) -> Record {
    Record {
        corpus: corpus.to_owned(),
        node: 0,
        case: case.to_owned(),
        surface,
        dropped: Some((reason, n)),
        new: example,
    }
}

fn describe(s: &NodeSnapshot) -> String {
    let name: String = s.name.as_deref().unwrap_or("").chars().take(30).collect();
    format!("{:?} {name:?}", s.role)
}

/// Why a node has no fused entry. A text leaf is merged into its parent's content name and is
/// never a focus target; any other role without an entry is a defect the gate must not hide.
fn no_entry_reason(s: &NodeSnapshot) -> &'static str {
    if matches!(s.role, Role::StaticText | Role::Label) {
        "static-leaf-no-entry"
    } else {
        "no-fused-entry"
    }
}

/// The reason a node of the wrong kind for this family's run is not compared.
fn kind_mismatch_reason(web_run: bool) -> &'static str {
    if web_run {
        "native-node-in-web-family"
    } else {
        "web-node-in-native-family"
    }
}

/// SplitMix64: a tiny deterministic generator (the shuffle must not depend on a crate version).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A seeded arrival order that interleaves the nodes of different parents (and so moves them
/// between batches, children before parents included) but keeps the relative order of siblings:
/// that order is data (the child index), not arrival noise, so no contract may ignore it.
fn arrival_order(snaps: &[Arc<NodeSnapshot>], seed: u64) -> Vec<usize> {
    let mut queues: Vec<std::collections::VecDeque<usize>> = Vec::new();
    let mut slot: BTreeMap<Option<gorca_model::NodeKey>, usize> = BTreeMap::new();
    for (i, s) in snaps.iter().enumerate() {
        let q = *slot.entry(s.parent).or_insert_with(|| {
            queues.push(std::collections::VecDeque::new());
            queues.len() - 1
        });
        queues[q].push_back(i);
    }
    let mut rng = Rng(seed);
    let mut order = Vec::with_capacity(snaps.len());
    while !queues.is_empty() {
        let q = usize::try_from(rng.next() % queues.len() as u64).unwrap_or(0);
        if let Some(i) = queues[q].pop_front() {
            order.push(i);
        }
        if queues[q].is_empty() {
            queues.swap_remove(q);
        }
    }
    order
}

/// Fuse `snaps` the way the app actor does. With a non-zero `seed` the nodes arrive in a seeded
/// shuffled order (and so in other batches); the result is returned in the original order.
fn fuse(
    tables: Option<gorca_model::SemTables>,
    label: &str,
    snaps: &[Arc<NodeSnapshot>],
    seed: u64,
) -> Vec<Arc<NodeSnapshot>> {
    if seed == 0 {
        return gorca_core::fuse_snapshots(tables, snaps);
    }
    let order = arrival_order(snaps, seed ^ fnv(label));
    let shuffled: Vec<Arc<NodeSnapshot>> = order.iter().map(|&i| Arc::clone(&snaps[i])).collect();
    let fused = gorca_core::fuse_snapshots(tables, &shuffled);
    let mut back: Vec<Option<Arc<NodeSnapshot>>> = vec![None; snaps.len()];
    for (pos, &i) in order.iter().enumerate() {
        back[i] = Some(Arc::clone(&fused[pos]));
    }
    back.into_iter()
        .zip(snaps)
        .map(|(f, orig)| f.unwrap_or_else(|| Arc::clone(orig)))
        .collect()
}

fn is_web(s: &NodeSnapshot) -> bool {
    s.attrs
        .iter()
        .any(|(k, _)| *k == gorca_doc_source_api::web_attrs::TAG)
}

/// Whether each node is web content: it carries the engine's DOM tag, is a web document, or has a
/// web ancestor. Chromium exports no tag on its text leaves, so the tag alone would call them
/// native and drop them. The page is in document order (parents first).
fn web_flags(snaps: &[Arc<NodeSnapshot>]) -> Vec<bool> {
    let index: BTreeMap<gorca_model::NodeKey, usize> =
        snaps.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
    let mut out = vec![false; snaps.len()];
    for (i, s) in snaps.iter().enumerate() {
        let parent_web = s
            .parent
            .and_then(|p| index.get(&p))
            .is_some_and(|&j| j < i && out[j]);
        out[i] = is_web(s) || s.role == Role::DocumentWeb || parent_web;
    }
    out
}

struct Run {
    lang: gorca_present::Lang,
    seed: u64,
}

fn render_opts(run: &Run, description_pause: bool) -> RenderOpts<'static> {
    RenderOpts {
        description_pause,
        lang: run.lang,
        ..RenderOpts::default()
    }
}

fn focus_records(family: &Family, run: &Run, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let surface = if web_run { "focus-web" } else { "focus" };
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let (Some(snaps), Some((metas, unreadable))) =
            (page(family.dir, &name), page_meta(family.dir, &name))
        else {
            out.push(dropped(
                &label,
                "focus",
                surface,
                "page-unparsable",
                1,
                String::new(),
            ));
            continue;
        };
        if snaps.len() != metas.len() {
            out.push(dropped(
                &label,
                "focus",
                surface,
                "page-unparsable",
                1,
                "decoded and raw node lists differ".to_owned(),
            ));
            continue;
        }
        let mut drops = Drops::default();
        drops.add_n("unreadable-node", unreadable.len() as u64, || {
            format!("node {}", unreadable[0])
        });
        let fused = fuse(tables, &label, &snaps, run.seed);
        let mut compared = 0u64;
        let webs = web_flags(&fused);
        for (s, web) in fused.iter().zip(&webs) {
            if *web != web_run {
                drops.add(kind_mismatch_reason(web_run), || describe(s));
                continue;
            }
            let Some(entry) = s.sem.entry.as_ref() else {
                drops.add(no_entry_reason(s), || describe(s));
                continue;
            };
            compared += 1;
            let ctx = ComposeCtx {
                flags: ReportFlags::default(),
                may_refine: false,
                web: web_run,
                skip_selected: false,
                hedge_inferred: false,
                lang: run.lang,
            };
            let new = match compose(entry, s.key, Reason::Focus, ctx) {
                Ok(ir) => join(
                    render(&ir, render_opts(run, web_run))
                        .iter()
                        .map(|t| (t.kind, t.text.as_str())),
                ),
                Err(_) => "<no rule row>".to_owned(),
            };
            out.push(Record {
                corpus: label.clone(),
                node: s.key.local.0,
                case: "focus".to_owned(),
                surface,
                dropped: None,
                new,
            });
        }
        drops.flush(&label, "focus", surface, out);
        if compared == 0 {
            out.push(dropped(
                &label,
                "focus",
                surface,
                "empty-corpus",
                1,
                "no node of the page was compared".to_owned(),
            ));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// State changes
// ---------------------------------------------------------------------------------------------

/// AT-SPI state numbers (`AtspiStateType`) the state mutations flip.
mod st {
    pub const CHECKED: usize = 4;
    pub const COLLAPSED: usize = 5;
    pub const EDITABLE: usize = 7;
    pub const ENABLED: usize = 8;
    pub const EXPANDABLE: usize = 9;
    pub const EXPANDED: usize = 10;
    pub const FOCUSABLE: usize = 11;
    pub const PRESSED: usize = 20;
    pub const SELECTABLE: usize = 22;
    pub const SELECTED: usize = 23;
    pub const SENSITIVE: usize = 24;
    pub const INDETERMINATE: usize = 32;
    pub const REQUIRED: usize = 33;
    pub const INVALID_ENTRY: usize = 36;
    pub const CHECKABLE: usize = 41;
}

fn has(w: &[u32], bit: usize) -> bool {
    w.get(bit / 32).is_some_and(|x| x & (1 << (bit % 32)) != 0)
}

fn put(w: &mut Vec<u32>, bit: usize, on: bool) {
    if w.len() <= bit / 32 {
        w.resize(bit / 32 + 1, 0);
    }
    if on {
        w[bit / 32] |= 1 << (bit % 32);
    } else {
        w[bit / 32] &= !(1 << (bit % 32));
    }
}

/// The state changes the replay applies, each to the nodes where such an event can happen.
/// Returns whether the node was changed.
type Mutation = fn(&mut RawNode) -> bool;

const MUTATIONS: &[(&str, Mutation)] = &[
    ("check", |r| {
        let w = &mut r.state_words;
        if !(has(w, st::CHECKED) || has(w, st::CHECKABLE)) {
            return false;
        }
        let on = has(w, st::CHECKED);
        put(w, st::CHECKED, !on);
        put(w, st::INDETERMINATE, false);
        true
    }),
    ("mixed", |r| {
        let w = &mut r.state_words;
        if !(has(w, st::CHECKED) || has(w, st::CHECKABLE)) || has(w, st::INDETERMINATE) {
            return false;
        }
        put(w, st::INDETERMINATE, true);
        true
    }),
    ("expand", |r| {
        let w = &mut r.state_words;
        if !(has(w, st::EXPANDABLE) || has(w, st::EXPANDED) || has(w, st::COLLAPSED)) {
            return false;
        }
        let open = has(w, st::EXPANDED);
        put(w, st::EXPANDED, !open);
        put(w, st::COLLAPSED, open);
        true
    }),
    ("select", |r| {
        let w = &mut r.state_words;
        if !(has(w, st::SELECTABLE) || has(w, st::SELECTED)) {
            return false;
        }
        let on = has(w, st::SELECTED);
        put(w, st::SELECTED, !on);
        true
    }),
    ("press", |r| {
        let w = &mut r.state_words;
        if !has(w, st::PRESSED) && !has(w, st::CHECKABLE) {
            return false;
        }
        let on = has(w, st::PRESSED);
        put(w, st::PRESSED, !on);
        true
    }),
    ("disable", |r| {
        let w = &mut r.state_words;
        if !has(w, st::SENSITIVE) {
            return false;
        }
        put(w, st::SENSITIVE, false);
        put(w, st::ENABLED, false);
        true
    }),
    ("invalid", |r| {
        let w = &mut r.state_words;
        if !has(w, st::EDITABLE) {
            return false;
        }
        let on = has(w, st::INVALID_ENTRY);
        put(w, st::INVALID_ENTRY, !on);
        true
    }),
    ("required", |r| {
        let w = &mut r.state_words;
        if !has(w, st::EDITABLE) {
            return false;
        }
        let on = has(w, st::REQUIRED);
        put(w, st::REQUIRED, !on);
        true
    }),
    ("rename", |r| {
        let Some(n) = r.name.as_mut().filter(|n| !n.is_empty()) else {
            return false;
        };
        n.push_str(" renamed");
        true
    }),
    // A value change: the reading of a slider, spin button or progress bar moves to the middle of
    // its range (or one step when the range is unknown). A node with an `aria-valuetext` has a
    // value text that does not follow the number, so a lone number flip would be incoherent there:
    // the recorded real cases cover those.
    ("value", |r| {
        if r.sem_attrs.iter().any(|(k, _)| &**k == "valuetext") {
            return false;
        }
        let Some(crate::decode::RawValue::Number { current, min, max }) = r.value.as_mut() else {
            return false;
        };
        let to = if min.is_finite() && max.is_finite() && max > min {
            f64::midpoint(*min, *max)
        } else {
            *current + 1.0
        };
        let to = if (to - *current).abs() < f64::EPSILON {
            *current + 1.0
        } else {
            to
        };
        *current = to;
        true
    }),
];

/// The change utterance of one `old -> new` change of a node; pushes one record.
fn compare_change(
    o: &Arc<NodeSnapshot>,
    n: &Arc<NodeSnapshot>,
    label: &str,
    case: &str,
    web_run: bool,
    run: &Run,
    out: &mut Vec<Record>,
) {
    let (Some(oe), Some(ne)) = (o.sem.entry.as_ref(), n.sem.entry.as_ref()) else {
        return;
    };
    let ctx = ComposeCtx {
        flags: ReportFlags::default(),
        may_refine: false,
        web: web_run,
        skip_selected: false,
        hedge_inferred: false,
        lang: run.lang,
    };
    let ir = compose_change(oe, ne, n.key, ctx);
    let tokens = render(&ir, render_opts(run, false));
    out.push(Record {
        corpus: label.to_owned(),
        node: n.key.local.0,
        case: case.to_owned(),
        surface: if web_run { "change-web" } else { "change" },
        dropped: None,
        new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
    });
}

fn change_records(family: &Family, run: &Run, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let surface = if web_run { "change-web" } else { "change" };
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let (Some(base), Some((metas, _))) =
            (page(family.dir, &name), page_meta(family.dir, &name))
        else {
            out.push(dropped(
                &label,
                "mutations",
                surface,
                "page-unparsable",
                1,
                String::new(),
            ));
            continue;
        };
        if base.len() != metas.len() {
            out.push(dropped(
                &label,
                "mutations",
                surface,
                "page-unparsable",
                1,
                "decoded and raw node lists differ".to_owned(),
            ));
            continue;
        }
        let old = fuse(tables, &label, &base, run.seed);
        let old_webs = web_flags(&old);
        for (case, mutate) in MUTATIONS {
            // Which nodes the mutation applies to is a property of the raw node, so decide it in
            // the same pass that edits it. A change utterance is spoken for the focused node:
            // only focusable nodes can have one; the others are counted, not forgotten.
            let touched = std::sync::Mutex::new(BTreeSet::<usize>::new());
            let not_focusable = std::sync::atomic::AtomicU64::new(0);
            let idx = std::sync::atomic::AtomicUsize::new(0);
            let Some(mutated) = page_with(family.dir, &name, &|raw| {
                let i = idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if has(&raw.state_words, st::FOCUSABLE) {
                    if mutate(raw) {
                        touched.lock().unwrap().insert(i);
                    }
                } else if mutate(&mut raw.clone()) {
                    not_focusable.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }) else {
                out.push(dropped(
                    &label,
                    case,
                    surface,
                    "page-unparsable",
                    1,
                    String::new(),
                ));
                continue;
            };
            let touched = touched.into_inner().unwrap();
            let mut drops = Drops::default();
            drops.add_n(
                "mutation-not-focusable",
                not_focusable.load(std::sync::atomic::Ordering::Relaxed),
                || format!("a node of {label}"),
            );
            if touched.is_empty() {
                drops.add("mutation-no-target", || {
                    format!("no focusable node of {label} takes the `{case}` mutation")
                });
                drops.flush(&label, case, surface, out);
                continue;
            }
            let new = fuse(tables, &format!("{label}#{case}"), &mutated, run.seed);
            for i in touched {
                let (o, n) = (&old[i], &new[i]);
                if old_webs[i] != web_run {
                    drops.add(kind_mismatch_reason(web_run), || describe(n));
                    continue;
                }
                if o.sem.entry.is_none() || n.sem.entry.is_none() {
                    drops.add(no_entry_reason(n), || describe(n));
                    continue;
                }
                compare_change(o, n, &label, case, web_run, run, out);
            }
            drops.flush(&label, case, surface, out);
        }
    }
}

/// The facts a change utterance can be about: states other than FOCUSED, the name, the value.
fn spoken_facts_differ(o: &NodeSnapshot, n: &NodeSnapshot) -> bool {
    let value_key = |s: &NodeSnapshot| {
        (
            format!("{:?}", s.value),
            s.sem
                .entry
                .as_ref()
                .and_then(|e| e.sem.text.as_ref())
                .and_then(|t| t.value_text.clone()),
        )
    };
    !((o.states ^ n.states) & !StateSet::FOCUSED).is_empty()
        || o.name != n.name
        || value_key(o) != value_key(n)
}

/// Pair the nodes of two recordings of one session: by `bus|path` (stable while the object lives),
/// then by a unique fixture id among the rest. Returns the pairs (before, after) and the indices
/// that are only in `before` (removed) and only in `after` (added).
fn match_nodes(
    before: &[NodeMeta],
    after: &[NodeMeta],
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut by_path: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, m) in after.iter().enumerate() {
        by_path.insert(&m.path_key, i);
    }
    let mut pairs = Vec::new();
    let mut taken = vec![false; after.len()];
    let mut left = Vec::new();
    for (i, m) in before.iter().enumerate() {
        match by_path.get(m.path_key.as_str()) {
            Some(&j) if !taken[j] => {
                taken[j] = true;
                pairs.push((i, j));
            }
            _ => left.push(i),
        }
    }
    // Second pass: the rest by fixture id, only when the id is unique on both sides.
    let ids_after: Vec<(usize, &str)> = after
        .iter()
        .enumerate()
        .filter(|(j, _)| !taken[*j])
        .filter_map(|(j, m)| m.fixture_id.as_deref().map(|id| (j, id)))
        .collect();
    let mut removed = Vec::new();
    for i in left {
        let hit = before[i].fixture_id.as_deref().and_then(|id| {
            let same_after: Vec<usize> = ids_after
                .iter()
                .filter(|(_, x)| *x == id)
                .map(|(j, _)| *j)
                .collect();
            let same_before = before
                .iter()
                .enumerate()
                .filter(|(k, m)| {
                    m.fixture_id.as_deref() == Some(id) && !pairs.iter().any(|(b, _)| b == k)
                })
                .count();
            (same_after.len() == 1 && same_before == 1 && !taken[same_after[0]])
                .then(|| same_after[0])
        });
        match hit {
            Some(j) => {
                taken[j] = true;
                pairs.push((i, j));
            }
            None => removed.push(i),
        }
    }
    let added: Vec<usize> = (0..after.len()).filter(|j| !taken[*j]).collect();
    pairs.sort_unstable();
    (pairs, removed, added)
}

/// The recorded state-change corpus (`tests/fixtures/sem-change/<family>/<case>.before|after`):
/// the toolkit or the page script made the change, the recorder saw the tree before and after.
/// Nodes are paired by identity, so a change that adds or removes nodes (an expanded tree item,
/// a popup menu) is diffed like any other: every node whose spoken facts differ (states other than
/// FOCUSED, name, value) is one change; the trigger buttons (`do:*`) are not, and the nodes that
/// appeared or vanished are counted as drops, never skipped.
fn real_change_records(family: &Family, run: &Run, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let surface = if web_run { "change-web" } else { "change" };
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    let dir = format!("../sem-change/{}", family.dir);
    for name in pages_of(&dir) {
        let Some(case) = name.strip_suffix(".before") else {
            continue;
        };
        let label = format!("{}/change:{case}", family.dir);
        let rcase = format!("real-{case}");
        let after_name = format!("{case}.after");
        let (Some(before), Some(after), Some((bm, _)), Some((am, _))) = (
            page(&dir, &name),
            page(&dir, &after_name),
            page_meta(&dir, &name),
            page_meta(&dir, &after_name),
        ) else {
            out.push(dropped(
                &label,
                &rcase,
                surface,
                "page-unparsable",
                1,
                String::new(),
            ));
            continue;
        };
        if before.len() != bm.len() || after.len() != am.len() {
            out.push(dropped(
                &label,
                &rcase,
                surface,
                "page-unparsable",
                1,
                "decoded and raw node lists differ".to_owned(),
            ));
            continue;
        }
        let old = fuse(tables, &format!("{label}.before"), &before, run.seed);
        let new = fuse(tables, &format!("{label}.after"), &after, run.seed);
        let new_webs = web_flags(&new);
        let (pairs, removed, added) = match_nodes(&bm, &am);
        let mut drops = Drops::default();
        let mut compared = 0u64;
        for (oi, ni) in pairs {
            let (o, n) = (&old[oi], &new[ni]);
            if n.name.as_deref().is_some_and(|x| x.starts_with("do:")) {
                drops.add("trigger-button", || describe(n));
                continue;
            }
            if !spoken_facts_differ(o, n) {
                continue;
            }
            if new_webs[ni] != web_run {
                drops.add(kind_mismatch_reason(web_run), || describe(n));
                continue;
            }
            if o.sem.entry.is_none() || n.sem.entry.is_none() {
                drops.add(no_entry_reason(n), || describe(n));
                continue;
            }
            compared += 1;
            compare_change(o, n, &label, &rcase, web_run, run, out);
        }
        for i in removed {
            drops.add("node-removed", || describe(&old[i]));
        }
        for j in added {
            drops.add("node-added", || describe(&new[j]));
        }
        drops.flush(&label, &rcase, surface, out);
        if compared == 0 {
            out.push(dropped(
                &label,
                &rcase,
                surface,
                "case-without-change",
                1,
                "no matched node changed a spoken fact".to_owned(),
            ));
        }
    }
}

/// A dropped (corpus, case, reason, count).
type Drop = (String, String, &'static str, u64);

/// Replay `surface` over every family present in the fixture tree: the golden text (compared
/// records only, sorted for byte comparison) and the dropped (corpus, case, reason, count) list.
fn emit(surface: &str, seed: u64, lang: gorca_present::Lang) -> (String, usize, Vec<Drop>) {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is, and no extension registry is passed (compose and render take none,
    // so add-on kinds cannot reach the output).
    let _lang = gorca_i18n::scoped(lang);
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        if surface == "focus" {
            focus_records(family, &run, &mut recs);
        } else {
            change_records(family, &run, &mut recs);
            real_change_records(family, &run, &mut recs);
        }
    }
    let drops: Vec<_> = recs
        .iter()
        .filter_map(|r| {
            r.dropped
                .map(|(why, n)| (r.corpus.clone(), r.case.clone(), why, n))
        })
        .collect();
    let mut kept: Vec<&Record> = recs.iter().filter(|r| r.dropped.is_none()).collect();
    kept.sort_by(|a, b| {
        (&a.corpus, a.node, &a.case, a.surface, &a.new)
            .cmp(&(&b.corpus, b.node, &b.case, b.surface, &b.new))
    });
    let mut text = String::new();
    for r in &kept {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}",
            r.corpus,
            r.node,
            r.case,
            r.surface,
            esc(&r.new)
        );
    }
    (text, kept.len(), drops)
}

fn golden_path(surface: &str, lang: &str) -> std::path::PathBuf {
    root().join("golden").join(format!("{surface}.{lang}.tsv"))
}

fn check(surface: &str, lang_name: &str, lang: gorca_present::Lang) {
    let (got, _, _) = emit(surface, 0, lang);
    let path = golden_path(surface, lang_name);
    if std::env::var("GORCA_BLESS").is_ok_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if want != got {
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
        let first = w
            .iter()
            .zip(&g)
            .position(|(a, b)| a != b)
            .unwrap_or(w.len().min(g.len()));
        panic!(
            "{surface}.{lang_name}: the golden records differ ({} lines recorded, {} now); first difference at line {}:\n recorded: {:?}\n now:      {:?}\n(GORCA_BLESS=1 rewrites them)",
            w.len(),
            g.len(),
            first + 1,
            w.get(first),
            g.get(first)
        );
    }
}

#[test]
fn the_focus_utterance_of_every_recorded_node_is_golden() {
    check("focus", "en", gorca_present::Lang::En);
    check("focus", "ru", gorca_present::Lang::Ru);
}

#[test]
fn the_change_utterance_of_every_recorded_state_flip_is_golden() {
    check("change", "en", gorca_present::Lang::En);
    check("change", "ru", gorca_present::Lang::Ru);
}

/// The records the cut-over gate compared when it passed (`cargo xtask cutover focus` / `change`,
/// 8 of 8 families, en and ru): a golden file that holds fewer has lost coverage.
const GATE_FOCUS_RECORDS: usize = 6497;
const GATE_CHANGE_RECORDS: usize = 3750;

#[test]
fn the_goldens_hold_every_record_the_gate_compared() {
    for (lang_name, lang) in [
        ("en", gorca_present::Lang::En),
        ("ru", gorca_present::Lang::Ru),
    ] {
        for (surface, want) in [
            ("focus", GATE_FOCUS_RECORDS),
            ("change", GATE_CHANGE_RECORDS),
        ] {
            let (_, n, _) = emit(surface, 0, lang);
            assert_eq!(
                n, want,
                "{surface}.{lang_name}: records the replay compares"
            );
            let lines = std::fs::read_to_string(golden_path(surface, lang_name))
                .unwrap_or_default()
                .lines()
                .count();
            assert_eq!(
                lines, want,
                "{surface}.{lang_name}: lines in the golden file"
            );
        }
    }
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        for surface in ["focus", "change"] {
            let a = emit(surface, 0, lang).0;
            assert!(!a.is_empty(), "{surface}: empty replay");
            assert_eq!(a, emit(surface, 0, lang).0, "{surface}: two runs differ");
            assert_eq!(
                a,
                emit(surface, 0x5eed, lang).0,
                "{surface}: shuffled arrival changed the output"
            );
        }
    }
}

/// Real expand and value cases are in the goldens (the pairing by path or fixture id compares
/// them even when the tree changed shape).
#[test]
fn the_real_expand_and_value_cases_are_golden() {
    let (text, _, _) = emit("change", 0, gorca_present::Lang::En);
    for case in ["real-expand", "real-expand-tree", "real-value-progress"] {
        assert!(
            text.lines().any(|l| l.split('\t').nth(2) == Some(case)),
            "no golden record for {case}"
        );
    }
    let value = text
        .lines()
        .filter(|l| {
            l.split('\t')
                .nth(2)
                .is_some_and(|c| c.starts_with("real-value"))
        })
        .count();
    assert!(value >= 3, "real value cases: {value}");
}

#[test]
fn every_family_has_recorded_pages_and_every_page_compares_something() {
    // A family or page that decodes to nothing must not count as coverage.
    let (text, _, drops) = emit("focus", 0, gorca_present::Lang::En);
    for family in FAMILIES {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("{}/", family.dir))),
            "family {} compared no node",
            family.dir
        );
    }
    assert!(
        !drops
            .iter()
            .any(|(_, _, why, _)| *why == "empty-corpus" || *why == "page-unparsable"),
        "a recorded page is empty or unparsable: {drops:?}"
    );
}
