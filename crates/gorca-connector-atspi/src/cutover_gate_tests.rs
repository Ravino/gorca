#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for the desktop surfaces (`cargo xtask cutover window|
//! notification`, ADR-0022 section 8.9, M22b4).
//!
//! `cutover_emit` replays every recorded corpus family through the legacy lead-in and notice
//! builders (`gorca_core::lead_segments`, `notice_segments`, which read the snapshot's role word
//! and raw name) and through `gorca-present` (`Reason::Window`, `Reason::Notice` over the fused
//! entry), and writes one canonical record per node to the file named by `GORCA_CUTOVER_OUT`. It
//! does not judge: `xtask` classifies the records. Without `GORCA_CUTOVER_OUT` the test is a no-op.
//!
//! Every node of every page is compared: the window lead-in is total (a plain node leads with its
//! name), and the notice is built for every role, so the whole role table is exercised, not only
//! the roles a notification has today. A notice is replayed twice per node: `bare` (name and
//! description only) and `lines` (four static-text lines that repeat the name, the description and
//! each other, to exercise the drop of repeated pieces).
//!
//! Environment: `GORCA_CUTOVER_SURFACE` (`window` or `notification`), `GORCA_CUTOVER_OUT`,
//! `GORCA_CUTOVER_LANG` (`en` or `ru`), `GORCA_CUTOVER_SHUFFLE` (non-zero seed: the arrival order
//! of the nodes at the fusion writer is shuffled; the records must not change).
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the same as the focus and browse gates).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, Role, Segment, WebEngine};
use gorca_present::{
    ComposeCtx, Deviations, DiffCtx, DiffOutcome, LegacyToken, Reason, RenderOpts, ReportFlags,
    compose, compose_notice, diff_report, render,
};

use crate::legacy_agreement_tests::{page, pages_of, root};

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
// The replay
// ---------------------------------------------------------------------------------------------

struct Run {
    lang: gorca_present::Lang,
    seed: u64,
}

/// The static-text lines of the `lines` notice case: they repeat the node's own name and
/// description (with padding) and one another, so the drop of repeated pieces is compared too.
fn lines_of(s: &NodeSnapshot) -> Vec<Arc<str>> {
    let mut v: Vec<Arc<str>> = vec![Arc::from("First line")];
    if let Some(n) = &s.name {
        v.push(Arc::from(format!(" {n} ")));
    }
    if let Some(d) = &s.description {
        v.push(Arc::clone(d));
    }
    v.push(Arc::from("First line"));
    v.push(Arc::from("Second line"));
    v
}

/// Whether a node can reach the surface in the Reader. A window lead-in is built for the activated
/// window (a window, a dialog, an alert or the application's top level); a notice is read for an
/// alert and, when the application's policy says so, a dialog (ADR-0008: the Shell's dialogs).
/// Both the legacy role and the fused role count, so a node only one side puts in the set shows
/// up as a difference instead of being skipped.
fn reachable(surface: &str, s: &NodeSnapshot, entry: &gorca_model::sem::SemEntry) -> bool {
    use gorca_model::sem::SemRole as R;
    let fused = entry.sem.role;
    if surface == "window" {
        matches!(
            s.role,
            Role::Window | Role::Dialog | Role::Alert | Role::Application
        ) || matches!(
            fused,
            R::Window | R::Dialog | R::AlertDialog | R::Alert | R::Application
        )
    } else {
        matches!(s.role, Role::Dialog | Role::Alert)
            || matches!(fused, R::Dialog | R::AlertDialog | R::Alert)
    }
}

fn legacy_of(segs: &[Segment]) -> Vec<LegacyToken> {
    segs.iter()
        .map(|g| LegacyToken {
            kind: g.kind,
            text: Arc::clone(&g.text),
        })
        .collect()
}

fn records(family: &Family, surface: &'static str, run: &Run, out: &mut Vec<Record>) {
    let dev = Deviations::builtin();
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let Some(snaps) = page(family.dir, &name) else {
            out.push(dropped(
                &label,
                "all",
                surface,
                "page-unparsable",
                1,
                String::new(),
            ));
            continue;
        };
        let fused = fuse(tables, &label, &snaps, run.seed);
        let webs = web_flags(&fused);
        let mut drops = Drops::default();
        let mut seen = 0u64;
        for (s, web) in fused.iter().zip(&webs) {
            let Some(entry) = s.sem.entry.as_ref() else {
                drops.add(no_entry_reason(s), || describe(s));
                continue;
            };
            seen += 1;
            if !reachable(surface, s, entry) {
                drops.add(
                    if surface == "window" {
                        "not-a-window-role"
                    } else {
                        "not-a-notice-role"
                    },
                    || describe(s),
                );
                continue;
            }
            let ctx = ComposeCtx {
                flags: ReportFlags::default(),
                may_refine: false,
                web: *web,
                skip_selected: false,
                hedge_inferred: false,
                lang: run.lang,
            };
            let cases: Vec<(&'static str, Vec<LegacyToken>, gorca_present::PresIr)> =
                if surface == "window" {
                    let legacy = legacy_of(&gorca_core::lead_segments(s));
                    let ir = compose(entry, s.key, Reason::Window, ctx).expect("window row");
                    vec![("lead", legacy, ir)]
                } else {
                    let mut v = Vec::new();
                    for (case, lines) in [("bare", Vec::new()), ("lines", lines_of(s))] {
                        let legacy = legacy_of(&gorca_core::notice_segments(s, &lines));
                        let ir = compose_notice(entry, s.key, ctx, &lines);
                        v.push((case, legacy, ir));
                    }
                    v
                };
            for (case, legacy, ir) in cases {
                let tokens = render(
                    &ir,
                    RenderOpts {
                        description_pause: false,
                        lang: run.lang,
                        ..RenderOpts::default()
                    },
                );
                let dctx = DiffCtx::for_entry(surface, entry, run.lang);
                let (d, _fired) = diff_report(&legacy, &tokens, &dev, dctx);
                let (outcome, deviation) = classify(&d, legacy.is_empty(), tokens.is_empty());
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case: case.to_owned(),
                    surface,
                    outcome,
                    deviation,
                    legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                    new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
                    witness: String::new(),
                });
            }
        }
        drops.flush(&label, "all", surface, out);
        // A page with no window or alert in it compares nothing and is fine; a page that decodes to
        // no fused node at all is a broken corpus.
        if seen == 0 {
            out.push(dropped(
                &label,
                "all",
                surface,
                "empty-corpus",
                1,
                "no node of the page has a fused entry".to_owned(),
            ));
        }
    }
}

fn emit(surface: &str, seed: u64, lang: gorca_present::Lang) -> Option<String> {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is, and no extension registry is passed.
    let _lang = gorca_i18n::scoped(lang);
    let surface: &'static str = match surface {
        "window" => "window",
        "notification" => "notification",
        _ => return None,
    };
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        records(family, surface, &run, &mut recs);
    }
    recs.sort_by(|a, b| {
        (
            &a.corpus,
            a.node,
            &a.case,
            a.surface,
            a.outcome,
            a.deviation,
            &a.legacy,
            &a.new,
        )
            .cmp(&(
                &b.corpus,
                b.node,
                &b.case,
                b.surface,
                b.outcome,
                b.deviation,
                &b.legacy,
                &b.new,
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
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        for surface in ["window", "notification"] {
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
fn every_family_has_recorded_pages_and_every_page_compares_something() {
    let text = emit("window", 0, gorca_present::Lang::En).unwrap();
    let mut seen = BTreeSet::new();
    for f in FAMILIES {
        seen.insert(f.dir);
        assert!(
            text.lines().any(|l| l.starts_with(&format!("{}/", f.dir))
                && l.split('\t').nth(4).is_none_or(|o| o != "dropped")),
            "family {} compared no node",
            f.dir
        );
    }
    assert!(
        !text
            .lines()
            .any(|l| l.split('\t').nth(5) == Some("empty-corpus")
                || l.split('\t').nth(5) == Some("page-unparsable")),
        "a recorded page is empty or unparsable"
    );
    assert_eq!(seen.len(), FAMILIES.len());
}
