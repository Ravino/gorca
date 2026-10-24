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
//! `GORCA_CUTOVER_LANG` (`en` or `ru`: the catalog language of both presenters, the diff and the
//! render; the gate runs both), `GORCA_CUTOVER_SHUFFLE` (non-zero seed: feed every page to the
//! fusion writer in a seeded shuffled order, which also moves nodes between its 512-node batches;
//! the records must not change, because fused values are a function of the final state, not of
//! arrival order).
//!
//! Record format, one line per record, tab separated and sorted:
//! `corpus  node  case  surface  outcome  deviation  legacy  new  witness` with `\`, tab and
//! newline escaped. `case` is `focus`, the state mutation applied for `change`, or `real-<case>`.
//! `outcome` is one of:
//! * `equal`, `silent` (both presenters say nothing), `order-only`, `different`;
//! * `allowed` (with the row id in `deviation`). `witness` is empty unless the row's predicate
//!   reads a fused model fact that the source record (the legacy side) contradicts: then the gate
//!   treats the difference as unclassified, because a row must not justify a regression with the
//!   very model fact that causes it;
//! * `dropped`: nodes or cases the replay did not compare. `deviation` is the reason code, `legacy`
//!   the number of drops, `new` the first example. Nothing is skipped silently: every drop is a
//!   record, and `xtask` fails the gate on a reason it has no explanation for.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::sem::{Group, Select, SemRole, SrcRecord};
use gorca_model::{NodeSnapshot, Role, StateSet, WebEngine};
use gorca_present::{
    ComposeCtx, Deviations, DiffCtx, DiffOutcome, LegacyToken, Reason, RenderOpts, ReportFlags,
    compose, compose_change, diff_report, render,
};

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
    witness: String,
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
        outcome: "dropped",
        deviation: reason,
        legacy: n.to_string(),
        new: example,
        witness: String::new(),
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

// ---------------------------------------------------------------------------------------------
// The model-fact check
// ---------------------------------------------------------------------------------------------

/// The AT-SPI role number of a toggle button.
const RAW_TOGGLE_BUTTON: u32 = 62;

/// A deviation row must not justify a regression with the model fact that causes it. Some
/// classifiers apply because of a fused fact (`not_toggle`, `not_selectable`, `avail_na`,
/// `toggle_button`, a fused value). When the source record (the connector's own normalisation of
/// the same node, which the legacy presenter was built on) contradicts that fact, the difference
/// is a model defect and not an intended improvement. Returns the contradictions, empty when the
/// row rests on nothing but evidence both sides share.
fn model_fact_witness(
    fired: &[&'static str],
    ctx: &DiffCtx<'_>,
    rec: Option<&SrcRecord>,
    meta: &NodeMeta,
) -> String {
    let change = ctx.surface.starts_with("change");
    let mut why: Vec<String> = Vec::new();
    for &id in fired {
        let w = match id {
            "select-not-applicable" => rec
                .filter(|r| {
                    ctx.not_selectable
                        && r.known.has(Group::Select)
                        && r.sem.select != Select::NotSelectable
                })
                .map(|r| {
                    format!(
                        "the source record says selectable ({:?}), the fused entry says it cannot be selected",
                        r.sem.select
                    )
                }),
            // Availability: the independent table of gorca-sem-atspi lists the roles a disabled
            // state is meaningful on. The fused profile has its own copy; a role the source table
            // disables and the fused entry calls "not applicable" is the defect, not a deviation.
            "unavailable-static" => rec
                .filter(|r| gorca_sem_atspi::states::is_disableable(r.sem.role))
                .map(|r| {
                    format!(
                        "source role {:?} can be disabled (gorca-sem-atspi is_disableable), the row calls availability not applicable",
                        r.sem.role
                    )
                }),
            "pressed-without-toggle" => (change
                && ctx.not_toggle
                && meta.raw_role == RAW_TOGGLE_BUTTON)
                .then(|| {
                    "the engine role is a toggle button, the fused entry says it has no pressed state"
                        .to_owned()
                }),
            "toggle-state-word" => (ctx.toggle_button && meta.raw_role != RAW_TOGGLE_BUTTON)
                .then(|| {
                    format!(
                        "the fused entry says toggle button, the engine role {} is not one",
                        meta.raw_role
                    )
                }),
            // The legacy connector fetches the Value reading of sliders, progress bars and spin
            // buttons; a node that has one must be spoken by the legacy side as well, so the row
            // cannot be the reason it is silent.
            "value-from-attrs" => (meta.has_value
                && matches!(
                    ctx.role,
                    SemRole::Slider | SemRole::ProgressBar | SemRole::SpinButton
                ))
            .then(|| {
                "the recorded node has a Value reading the legacy connector fetches, so the legacy side must speak it"
                    .to_owned()
            }),
            _ => None,
        };
        if let Some(w) = w {
            why.push(format!("{id}: {w}"));
        }
    }
    why.join("; ")
}

// ---------------------------------------------------------------------------------------------
// Focus
// ---------------------------------------------------------------------------------------------

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

fn focus_records(family: &Family, run: &Run, dev: &Deviations, out: &mut Vec<Record>) {
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
        for ((s, meta), web) in fused.iter().zip(&metas).zip(&webs) {
            if *web != web_run {
                drops.add(kind_mismatch_reason(web_run), || describe(s));
                continue;
            }
            let Some(entry) = s.sem.entry.as_ref() else {
                drops.add(no_entry_reason(s), || describe(s));
                continue;
            };
            compared += 1;
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
                hedge_inferred: false,
                lang: run.lang,
            };
            let Ok(ir) = compose(entry, s.key, Reason::Focus, ctx) else {
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case: "focus".to_owned(),
                    surface,
                    outcome: "different",
                    deviation: "",
                    legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                    new: "<no rule row>".to_owned(),
                    witness: String::new(),
                });
                continue;
            };
            let tokens = render(&ir, render_opts(run, web_run));
            let dctx = DiffCtx::for_entry(surface, entry, run.lang);
            let (d, fired) = diff_report(&legacy, &tokens, dev, dctx);
            let (outcome, deviation) = classify(&d, legacy.is_empty(), tokens.is_empty());
            let witness = if outcome == "allowed" {
                model_fact_witness(&fired, &dctx, s.sem.rec.as_deref(), meta)
            } else {
                String::new()
            };
            out.push(Record {
                corpus: label.clone(),
                node: s.key.local.0,
                case: "focus".to_owned(),
                surface,
                outcome,
                deviation,
                legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
                witness,
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
    // its range (or one step when the range is unknown).
    ("value", |r| {
        let Some(crate::decode::RawValue::Number { current, min, max }) = r.value.as_mut() else {
            return false;
        };
        let to = if min.is_finite() && max.is_finite() && max > min {
            (*min + *max) / 2.0
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

/// Both presenters on one `old -> new` change of a node; pushes one record.
#[allow(clippy::too_many_arguments)]
fn compare_change(
    o: &Arc<NodeSnapshot>,
    n: &Arc<NodeSnapshot>,
    meta: &NodeMeta,
    label: &str,
    case: &str,
    web_run: bool,
    run: &Run,
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
        hedge_inferred: false,
        lang: run.lang,
    };
    let ir = compose_change(oe, ne, n.key, ctx);
    let tokens = render(&ir, render_opts(run, false));
    let surface = if web_run { "change-web" } else { "change" };
    let dctx = DiffCtx::for_entry(surface, ne, run.lang);
    let (d, fired) = diff_report(&legacy, &tokens, dev, dctx);
    let (outcome, deviation) = classify(&d, legacy.is_empty(), tokens.is_empty());
    let witness = if outcome == "allowed" {
        model_fact_witness(&fired, &dctx, n.sem.rec.as_deref(), meta)
    } else {
        String::new()
    };
    out.push(Record {
        corpus: label.to_owned(),
        node: n.key.local.0,
        case: case.to_owned(),
        surface,
        outcome,
        deviation,
        legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
        new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
        witness,
    });
}

fn change_records(family: &Family, run: &Run, dev: &Deviations, out: &mut Vec<Record>) {
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
                compare_change(o, n, &metas[i], &label, case, web_run, run, dev, out);
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
fn real_change_records(family: &Family, run: &Run, dev: &Deviations, out: &mut Vec<Record>) {
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
            compare_change(o, n, &am[ni], &label, &rcase, web_run, run, dev, out);
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

/// Replay `surface` over every family present in the fixture tree; records sorted for byte
/// comparison. `None` for a surface that has no new presenter yet.
fn emit(surface: &str, seed: u64, lang: gorca_present::Lang) -> Option<String> {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is (and `RenderOpts`, `ComposeCtx` and `DiffCtx` carry the same
    // language), and no extension registry is passed (compose and render take none, so add-on
    // kinds cannot reach the output). The replay calls compose/render/diff synchronously, the same
    // functions `ShadowActor::process` runs, so no sample is ever dropped by a bounded queue.
    let _lang = gorca_i18n::scoped(lang);
    let dev = Deviations::builtin();
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        match surface {
            "focus" => focus_records(family, &run, &dev, &mut recs),
            "change" => {
                change_records(family, &run, &dev, &mut recs);
                real_change_records(family, &run, &dev, &mut recs);
            }
            _ => return None,
        }
    }
    recs.sort_by(|a, b| {
        (
            &a.corpus, a.node, &a.case, a.surface, a.outcome, a.deviation, &a.legacy, &a.new,
        )
            .cmp(&(
                &b.corpus, b.node, &b.case, b.surface, b.outcome, b.deviation, &b.legacy, &b.new,
            ))
    });
    let mut text = String::new();
    for r in &recs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.corpus,
            r.node,
            r.case,
            r.surface,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new),
            esc(&r.witness)
        );
    }
    Some(text)
}

fn lang_of(s: &str) -> Option<gorca_present::Lang> {
    match s {
        "en" => Some(gorca_present::Lang::En),
        "ru" => Some(gorca_present::Lang::Ru),
        _ => None,
    }
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
    let lang_name = std::env::var("GORCA_CUTOVER_LANG").unwrap_or_else(|_| "en".to_owned());
    let lang = lang_of(&lang_name).expect("GORCA_CUTOVER_LANG is en or ru");
    let text = emit(&surface, seed, lang).expect("surface has a replay");
    std::fs::write(path, text).expect("write records");
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    // The same contract the gate checks, kept as a plain test so a regression shows without xtask.
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        for surface in ["focus", "change"] {
            let a = emit(surface, 0, lang).unwrap();
            assert!(!a.is_empty(), "{surface}: empty replay");
            assert_eq!(
                a,
                emit(surface, 0, lang).unwrap(),
                "{surface}: two runs differ"
            );
            assert_eq!(
                a,
                emit(surface, 0x5eed, lang).unwrap(),
                "{surface}: shuffled arrival changed the output"
            );
        }
    }
}

#[test]
fn every_new_family_has_recorded_pages_and_every_page_compares_something() {
    // A family or page that decodes to nothing must not count as coverage (the empty
    // firefox/exotic-date page did, once). The replay reports `empty-corpus`; this keeps the same
    // rule visible without xtask.
    let text = emit("focus", 0, gorca_present::Lang::En).unwrap();
    for family in FAMILIES {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("{}/", family.dir))
                    && !l.split('\t').nth(4).is_some_and(|o| o == "dropped")),
            "family {} compared no node",
            family.dir
        );
    }
    assert!(
        !text
            .lines()
            .any(|l| l.split('\t').nth(5) == Some("empty-corpus")
                || l.split('\t').nth(5) == Some("page-unparsable")),
        "a recorded page is empty or unparsable:\n{}",
        text.lines()
            .filter(|l| l.contains("\tempty-corpus\t") || l.contains("\tpage-unparsable\t"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[cfg(test)]
mod witness_tests {
    use super::*;
    use gorca_model::sem::{Avail, GroupMask, Press, Semantics};

    fn meta(raw_role: u32, has_value: bool) -> NodeMeta {
        NodeMeta {
            path_key: String::new(),
            fixture_id: None,
            raw_role,
            has_value,
        }
    }

    fn rec(role: SemRole, select: Select) -> SrcRecord {
        SrcRecord {
            sem: Semantics {
                role,
                select,
                ..Semantics::default()
            },
            known: GroupMask::EMPTY.with(Group::Select),
            ..SrcRecord::default()
        }
    }

    fn ctx(surface: &str) -> DiffCtx<'_> {
        DiffCtx {
            surface,
            role: SemRole::MenuItem,
            name_derived: false,
            untoggled: false,
            not_toggle: false,
            not_selectable: false,
            toggle_button: false,
            avail_na: false,
            lang: gorca_present::Lang::En,
        }
    }

    #[test]
    fn a_selectable_source_node_the_model_calls_not_selectable_is_flagged() {
        let c = DiffCtx {
            not_selectable: true,
            ..ctx("change-web")
        };
        let r = rec(SemRole::MenuItem, Select::On);
        let w = model_fact_witness(&["select-not-applicable"], &c, Some(&r), &meta(35, false));
        assert!(w.contains("select-not-applicable"), "{w}");
        // The source agrees that it cannot be selected: the row stands on shared evidence.
        let r2 = rec(SemRole::Button, Select::NotSelectable);
        assert!(
            model_fact_witness(&["select-not-applicable"], &c, Some(&r2), &meta(43, false))
                .is_empty()
        );
    }

    #[test]
    fn a_disableable_source_role_the_model_calls_not_applicable_is_flagged() {
        let c = DiffCtx {
            avail_na: true,
            ..ctx("change")
        };
        let tree = rec(SemRole::Tree, Select::NotSelectable);
        assert!(
            !model_fact_witness(&["unavailable-static"], &c, Some(&tree), &meta(65, false))
                .is_empty(),
            "a tree can be disabled"
        );
        let doc = rec(SemRole::Document, Select::NotSelectable);
        assert!(
            model_fact_witness(&["unavailable-static"], &c, Some(&doc), &meta(95, false))
                .is_empty(),
            "a document cannot"
        );
        // The focus row, gated by role, is held to the same table.
        let group = rec(SemRole::Group, Select::NotSelectable);
        let f = DiffCtx {
            role: SemRole::Group,
            ..ctx("focus-web")
        };
        assert!(
            !model_fact_witness(&["unavailable-static"], &f, Some(&group), &meta(87, false))
                .is_empty(),
            "a disabled fieldset is a group that can be disabled"
        );
    }

    #[test]
    fn toggle_facts_are_checked_against_the_engine_role() {
        let c = DiffCtx {
            not_toggle: true,
            ..ctx("change")
        };
        let r = SrcRecord {
            sem: Semantics {
                press: Press::Off,
                avail: Avail::Enabled,
                ..Semantics::default()
            },
            ..SrcRecord::default()
        };
        assert!(
            !model_fact_witness(&["pressed-without-toggle"], &c, Some(&r), &meta(62, false))
                .is_empty(),
            "the engine says toggle button"
        );
        assert!(
            model_fact_witness(&["pressed-without-toggle"], &c, Some(&r), &meta(4, false))
                .is_empty(),
            "a check box flipped to pressed is not a toggle"
        );
        let t = DiffCtx {
            toggle_button: true,
            ..ctx("change")
        };
        assert!(
            !model_fact_witness(&["toggle-state-word"], &t, Some(&r), &meta(43, false)).is_empty()
        );
        assert!(
            model_fact_witness(&["toggle-state-word"], &t, Some(&r), &meta(62, false)).is_empty()
        );
    }

    #[test]
    fn a_value_the_recorded_node_has_cannot_be_the_reason_the_legacy_side_is_silent() {
        let c = DiffCtx {
            role: SemRole::Slider,
            ..ctx("focus-web")
        };
        assert!(!model_fact_witness(&["value-from-attrs"], &c, None, &meta(51, true)).is_empty());
        assert!(model_fact_witness(&["value-from-attrs"], &c, None, &meta(51, false)).is_empty());
        // A row the check knows nothing about is not touched.
        assert!(model_fact_witness(&["role-from-model"], &c, None, &meta(51, true)).is_empty());
    }

    #[test]
    fn nodes_are_paired_by_identity_when_the_tree_changes_shape() {
        let m = |p: &str, id: Option<&str>| NodeMeta {
            path_key: p.to_owned(),
            fixture_id: id.map(str::to_owned),
            raw_role: 0,
            has_value: false,
        };
        let before = vec![m("a|1", None), m("a|2", Some("x")), m("a|3", None)];
        // Node 2 was re-created under a new path but keeps its id; node 3 vanished; a new one
        // appeared between.
        let after = vec![m("a|1", None), m("a|9", None), m("a|7", Some("x"))];
        let (pairs, removed, added) = match_nodes(&before, &after);
        assert_eq!(pairs, vec![(0, 0), (1, 2)]);
        assert_eq!(removed, vec![2]);
        assert_eq!(added, vec![1]);
    }
}
