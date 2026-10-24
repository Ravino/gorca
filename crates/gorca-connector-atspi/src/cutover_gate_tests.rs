#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate (`cargo xtask cutover <surface>`, ADR-0022 section 8.9).
//!
//! `cutover_emit` replays every recorded corpus family of one surface through the legacy presenter
//! and through `gorca-present`, and writes one canonical record per node to the file named by
//! `GORCA_CUTOVER_OUT`. It does not judge: `xtask` classifies the records against the dated
//! `deviations.toml` rows. Without `GORCA_CUTOVER_OUT` the test is a no-op (the xtask runs it).
//!
//! Environment: `GORCA_CUTOVER_SURFACE` (`focus` or `change`), `GORCA_CUTOVER_OUT` (file),
//! `GORCA_CUTOVER_SHUFFLE` (non-zero seed: feed every page to the fusion writer in a seeded
//! shuffled order, which also moves nodes between its 512-node batches; the records must not
//! change, because fused values are a function of the final state, not of arrival order).
//!
//! Record format, one line per node, tab separated and sorted:
//! `corpus  node  case  surface  outcome  deviation  legacy  new` with `\`, tab and newline escaped.
//! `case` is `focus` or the state mutation applied for `change`. `outcome` is one of `equal`,
//! `silent` (both presenters say nothing), `order-only`, `allowed` (with the row id) or `different`.

use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, WebEngine};
use gorca_present::{
    ComposeCtx, Deviations, DiffCtx, DiffOutcome, LegacyToken, Reason, RenderOpts, ReportFlags,
    compose, compose_change, diff, render,
};

use crate::decode::RawNode;
use crate::legacy_agreement_tests::{page, page_with, pages_of, root};

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
];

/// One output line.
struct Record {
    corpus: String,
    node: u64,
    case: String,
    surface: &'static str,
    outcome: &'static str,
    deviation: &'static str,
    legacy: String,
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
    let mut slot: std::collections::BTreeMap<Option<gorca_model::NodeKey>, usize> =
        std::collections::BTreeMap::new();
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

fn classify(
    outcome: &DiffOutcome,
    legacy_empty: bool,
    new_empty: bool,
) -> (&'static str, &'static str) {
    match outcome {
        DiffOutcome::Equal if legacy_empty && new_empty => ("silent", ""),
        DiffOutcome::Equal => ("equal", ""),
        DiffOutcome::OrderOnly => ("order-only", ""),
        DiffOutcome::Allowed(id) => ("allowed", id),
        DiffOutcome::Different { .. } => ("different", ""),
    }
}

fn focus_records(family: &Family, seed: u64, dev: &Deviations, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let Some(snaps) = page(family.dir, &name) else {
            continue;
        };
        let label = format!("{}/{name}", family.dir);
        let fused = fuse(tables, &label, &snaps, seed);
        for s in &fused {
            if is_web(s) != web_run {
                continue;
            }
            let Some(entry) = s.sem.entry.as_ref() else {
                continue;
            };
            let u = gorca_core::present_focus(s, gorca_model::UttId(1), gorca_model::Epoch(1));
            let legacy: Vec<LegacyToken> = u
                .segments
                .iter()
                .map(|g| LegacyToken {
                    kind: g.kind,
                    text: Arc::clone(&g.text),
                })
                .collect();
            let ctx = ComposeCtx {
                flags: ReportFlags::default(),
                may_refine: false,
                web: web_run,
                skip_selected: false,
            };
            let Ok(ir) = compose(entry, s.key, Reason::Focus, ctx) else {
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case: "focus".to_owned(),
                    surface: if web_run { "focus-web" } else { "focus" },
                    outcome: "different",
                    deviation: "",
                    legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                    new: "<no rule row>".to_owned(),
                });
                continue;
            };
            let tokens = render(
                &ir,
                RenderOpts {
                    description_pause: web_run,
                },
            );
            let surface = if web_run { "focus-web" } else { "focus" };
            let d = diff(&legacy, &tokens, dev, DiffCtx::for_entry(surface, entry));
            let (outcome, deviation) = classify(&d, legacy.is_empty(), tokens.is_empty());
            out.push(Record {
                corpus: label.clone(),
                node: s.key.local.0,
                case: "focus".to_owned(),
                surface,
                outcome,
                deviation,
                legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
            });
        }
    }
}

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
];

fn change_records(family: &Family, seed: u64, dev: &Deviations, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let Some(base) = page(family.dir, &name) else {
            continue;
        };
        let label = format!("{}/{name}", family.dir);
        let old = fuse(tables, &label, &base, seed);
        for (case, mutate) in MUTATIONS {
            // Which nodes the mutation applies to is a property of the raw node, so decide it in
            // the same pass that edits it.
            let touched = std::sync::Mutex::new(std::collections::BTreeSet::<usize>::new());
            let idx = std::sync::atomic::AtomicUsize::new(0);
            let Some(mutated) = page_with(family.dir, &name, &|raw| {
                let i = idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // A change utterance is spoken for the focused node: only focusable nodes can
                // have one.
                if has(&raw.state_words, st::FOCUSABLE) && mutate(raw) {
                    touched.lock().unwrap().insert(i);
                }
            }) else {
                continue;
            };
            let touched = touched.into_inner().unwrap();
            if touched.is_empty() {
                continue;
            }
            let new = fuse(tables, &format!("{label}#{case}"), &mutated, seed);
            for i in touched {
                let (o, n) = (&old[i], &new[i]);
                if is_web(n) != web_run {
                    continue;
                }
                compare_change(o, n, &label, case, web_run, dev, out);
            }
        }
    }
}

/// Both presenters on one `old -> new` change of a node; pushes one record.
fn compare_change(
    o: &Arc<NodeSnapshot>,
    n: &Arc<NodeSnapshot>,
    label: &str,
    case: &str,
    web_run: bool,
    dev: &Deviations,
    out: &mut Vec<Record>,
) {
    let (Some(oe), Some(ne)) = (o.sem.entry.as_ref(), n.sem.entry.as_ref()) else {
        return;
    };
    let legacy_pairs = gorca_core::legacy_change_tokens(o, n, false);
    let legacy: Vec<LegacyToken> = legacy_pairs
        .iter()
        .map(|(kind, text)| LegacyToken {
            kind: *kind,
            text: Arc::clone(text),
        })
        .collect();
    let ctx = ComposeCtx {
        flags: ReportFlags::default(),
        may_refine: false,
        web: web_run,
        skip_selected: false,
    };
    let ir = compose_change(oe, ne, n.key, ctx);
    let tokens = render(
        &ir,
        RenderOpts {
            description_pause: false,
        },
    );
    let surface = if web_run { "change-web" } else { "change" };
    let d = diff(&legacy, &tokens, dev, DiffCtx::for_entry(surface, ne));
    let (outcome, deviation) = classify(&d, legacy.is_empty(), tokens.is_empty());
    out.push(Record {
        corpus: label.to_owned(),
        node: n.key.local.0,
        case: case.to_owned(),
        surface,
        outcome,
        deviation,
        legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
        new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
    });
}

/// The recorded state-change corpus (`tests/fixtures/sem-change/<family>/<case>.before|after`):
/// the toolkit or the page script made the change, the recorder saw the tree before and after.
/// Every node whose spoken facts differ (states other than FOCUSED, name) is one change; the
/// trigger buttons (`do:*`) are not.
fn real_change_records(family: &Family, seed: u64, dev: &Deviations, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    let dir = format!("../sem-change/{}", family.dir);
    for name in pages_of(&dir) {
        let Some(case) = name.strip_suffix(".before") else {
            continue;
        };
        let (Some(before), Some(after)) = (page(&dir, &name), page(&dir, &format!("{case}.after")))
        else {
            continue;
        };
        if before.len() != after.len() {
            continue; // the tree changed shape: not a state change
        }
        let label = format!("{}/change:{case}", family.dir);
        let old = fuse(tables, &format!("{label}.before"), &before, seed);
        let new = fuse(tables, &format!("{label}.after"), &after, seed);
        for (o, n) in old.iter().zip(&new) {
            let states = (o.states ^ n.states) & !gorca_model::StateSet::FOCUSED;
            let renamed = o.name != n.name;
            let trigger = n.name.as_deref().is_some_and(|x| x.starts_with("do:"));
            if trigger || is_web(n) != web_run || (states.is_empty() && !renamed) {
                continue;
            }
            compare_change(o, n, &label, &format!("real-{case}"), web_run, dev, out);
        }
    }
}

/// Replay `surface` over every family present in the fixture tree; records sorted for byte
/// comparison. `None` for a surface that has no new presenter yet.
fn emit(surface: &str, seed: u64) -> Option<String> {
    // Hidden inputs of `render` are pinned: the catalog language is English on this thread whatever
    // the process language is, and no extension registry is passed (compose and render take none,
    // so add-on kinds cannot reach the output). The replay calls compose/render/diff
    // synchronously, the same functions `ShadowActor::process` runs, so no sample is ever dropped
    // by a bounded queue.
    let _lang = gorca_i18n::scoped(gorca_i18n::Lang::En);
    let dev = Deviations::builtin();
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        match surface {
            "focus" => focus_records(family, seed, &dev, &mut recs),
            "change" => {
                change_records(family, seed, &dev, &mut recs);
                real_change_records(family, seed, &dev, &mut recs);
            }
            _ => return None,
        }
    }
    recs.sort_by(|a, b| {
        (&a.corpus, a.node, &a.case, a.surface).cmp(&(&b.corpus, b.node, &b.case, b.surface))
    });
    let mut text = String::new();
    for r in &recs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.corpus,
            r.node,
            r.case,
            r.surface,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new)
        );
    }
    Some(text)
}

/// The gate's replay entry (run by `cargo xtask cutover`; a no-op otherwise).
#[test]
fn cutover_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let text = emit(&surface, seed).expect("surface has a replay");
    std::fs::write(path, text).expect("write records");
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    // The same contract the gate checks, kept as a plain test so a regression shows without xtask.
    for surface in ["focus", "change"] {
        let a = emit(surface, 0).unwrap();
        assert!(!a.is_empty(), "{surface}: empty replay");
        assert_eq!(a, emit(surface, 0).unwrap(), "{surface}: two runs differ");
        assert_eq!(
            a,
            emit(surface, 0x5eed).unwrap(),
            "{surface}: shuffled arrival changed the output"
        );
    }
}
