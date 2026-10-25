#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests of the window lead-in and the notice (`Reason::Window`, `Reason::Notice`;
//! M22b4 cut-over): the replay half of the `window` and `notification` gates, kept after the
//! legacy builders were deleted. Every recorded corpus family is fused the way the app actor fuses
//! it and run through `gorca-present`, in English and in Russian; the records must equal the
//! checked-in files `tests/fixtures/sem/golden/{window,notification}.{en,ru}.tsv`.
//!
//! The replay selects the nodes the gate compared: a window lead-in for a window, dialog, alert or
//! application top level (`reachable`), a notice for an alert or a dialog. A notice is replayed
//! twice per node: `bare` (name and description) and `lines` (static-text lines that repeat the
//! name, the description and each other, to exercise the drop of repeated pieces). Every node it
//! does not compare is counted with a reason, and the counts are pinned.
//!
//! `GORCA_BLESS=1 cargo test -p gorca-connector-atspi --lib desktop_golden` rewrites the files
//! (review the diff: every changed line is a change of what a screen reader user hears). The
//! replay is repeatable and independent of the arrival order of the nodes.
//!
//! Record format, one line per compared node, tab separated and sorted:
//! `corpus  node  case  surface  tokens` with `\`, tab and newline escaped.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, Role, WebEngine};
use gorca_present::{ComposeCtx, Reason, RenderOpts, ReportFlags, compose, compose_notice, render};

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

// ---------------------------------------------------------------------------------------------
// The replay
// ---------------------------------------------------------------------------------------------

/// One output line: a compared node, or the drops of one page.
struct Record {
    corpus: String,
    node: u64,
    case: &'static str,
    surface: &'static str,
    new: String,
}

/// Nodes the replay did not compare: reason and count, over all families.
type DropCounts = BTreeMap<&'static str, u64>;

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

fn describe(s: &NodeSnapshot) -> String {
    let name: String = s.name.as_deref().unwrap_or("").chars().take(30).collect();
    format!("{:?} {name:?}", s.role)
}

/// Why a node has no fused entry. A text leaf is merged into its parent's content name; any
/// other role without an entry is a defect the replay must not hide.
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
/// that order is data (the child index), not arrival noise.
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
/// web ancestor. The page is in document order (parents first).
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

fn records(
    family: &Family,
    surface: &'static str,
    run: &Run,
    out: &mut Vec<Record>,
    drops: &mut DropCounts,
) {
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let Some(snaps) = page(family.dir, &name) else {
            *drops.entry("page-unparsable").or_default() += 1;
            continue;
        };
        let fused = fuse(tables, &label, &snaps, run.seed);
        let webs = web_flags(&fused);
        let mut seen = 0u64;
        for (s, web) in fused.iter().zip(&webs) {
            let Some(entry) = s.sem.entry.as_ref() else {
                *drops.entry(no_entry_reason(s)).or_default() += 1;
                continue;
            };
            seen += 1;
            if !reachable(surface, s, entry) {
                let why = if surface == "window" {
                    "not-a-window-role"
                } else {
                    "not-a-notice-role"
                };
                *drops.entry(why).or_default() += 1;
                let _ = describe(s);
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
            let mut cases = Vec::new();
            if surface == "window" {
                let ir = compose(entry, s.key, Reason::Window, ctx).expect("window row");
                cases.push(("lead", ir));
            } else {
                for (case, lines) in [("bare", Vec::new()), ("lines", lines_of(s))] {
                    cases.push((case, compose_notice(entry, s.key, ctx, &lines)));
                }
            }
            for (case, ir) in cases {
                let tokens = render(
                    &ir,
                    RenderOpts {
                        description_pause: false,
                        lang: run.lang,
                        ..RenderOpts::default()
                    },
                );
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case,
                    surface,
                    new: join(tokens.iter().map(|t| (t.kind, t.text.as_str()))),
                });
            }
        }
        if seen == 0 {
            *drops.entry("empty-corpus").or_default() += 1;
        }
    }
}

/// The records of a surface (sorted lines), the number of records and the drops by reason.
fn emit(surface: &str, seed: u64, lang: gorca_present::Lang) -> (String, usize, DropCounts) {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is, and no extension registry is passed.
    let _lang = gorca_i18n::scoped(lang);
    let surface: &'static str = if surface == "window" {
        "window"
    } else {
        "notification"
    };
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    let mut drops = DropCounts::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() || !applies(surface, family.dir) {
            continue;
        }
        records(family, surface, &run, &mut recs, &mut drops);
    }
    recs.sort_by(|a, b| {
        (&a.corpus, a.node, a.case, &a.new).cmp(&(&b.corpus, b.node, b.case, &b.new))
    });
    let mut text = String::new();
    for r in &recs {
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
    (text, recs.len(), drops)
}

/// The families each surface applies to: keep in step with `WINDOW_FAMILIES` and
/// `NOTICE_FAMILIES` in `xtask/src/cutover.rs` (the recordings that hold the roles).
fn applies(surface: &str, dir: &str) -> bool {
    if surface == "window" {
        dir != "telegram"
    } else {
        matches!(dir, "gtk4" | "firefox" | "chromium" | "gnome-shell")
    }
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
fn the_window_lead_in_of_every_recorded_window_is_golden() {
    check("window", "en", gorca_present::Lang::En);
    check("window", "ru", gorca_present::Lang::Ru);
}

#[test]
fn the_notice_of_every_recorded_alert_and_dialog_is_golden() {
    check("notification", "en", gorca_present::Lang::En);
    check("notification", "ru", gorca_present::Lang::Ru);
}

/// The records the goldens hold per language. The gates compared 106 window nodes and 66 notice
/// records (33 nodes, two cases) when they passed (`cargo xtask cutover window` and
/// `notification`, 7 and 4 families, en and ru); a golden file that holds fewer has lost coverage.
const GATE_WINDOW_RECORDS: usize = 106;
const GATE_NOTICE_RECORDS: usize = 66;

#[test]
fn the_goldens_hold_every_record_the_gates_compared() {
    for (lang_name, lang) in [
        ("en", gorca_present::Lang::En),
        ("ru", gorca_present::Lang::Ru),
    ] {
        for (surface, want) in [
            ("window", GATE_WINDOW_RECORDS),
            ("notification", GATE_NOTICE_RECORDS),
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
        for surface in ["window", "notification"] {
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

#[test]
fn every_family_has_a_window_and_no_page_is_empty_or_unparsable() {
    let (text, _, drops) = emit("window", 0, gorca_present::Lang::En);
    for family in FAMILIES.iter().filter(|f| applies("window", f.dir)) {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("{}/", family.dir))),
            "family {} compared no node",
            family.dir
        );
    }
    assert!(
        !drops.contains_key("empty-corpus") && !drops.contains_key("page-unparsable"),
        "a recorded page is empty or unparsable: {drops:?}"
    );
    let (text, _, _) = emit("notification", 0, gorca_present::Lang::En);
    for dir in ["gtk4", "firefox", "chromium", "gnome-shell"] {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("{dir}/"))),
            "family {dir} compared no notice"
        );
    }
}

/// A notification banner of the Shell and an alert dialog are in the notice goldens, and the
/// window goldens hold a dialog with its role word.
#[test]
fn the_goldens_hold_the_roles_that_matter() {
    let (n, _, _) = emit("notification", 0, gorca_present::Lang::En);
    assert!(
        n.lines()
            .any(|l| l.starts_with("gnome-shell/notification-banner\t") && l.contains("Role:alert")),
        "the Shell's notification banner"
    );
    let (w, _, _) = emit("window", 0, gorca_present::Lang::En);
    assert!(
        w.lines().any(|l| l.contains("Role:dialog")),
        "a dialog leads with its role word"
    );
}
