#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests of "where am I" (`commands::info`, M22b4 cut-over): the replay half of the
//! `whereami` gate, kept after the legacy answer was deleted. Every recorded corpus family is fused
//! the way the app actor fuses it; every node is the focus once, its ancestry is the chain of
//! parents the page holds (at most 24, leaf first), and the answer of `Reason::WhereAmI` over the
//! entries (basic, detailed, spelled name, brief) must equal the checked-in files
//! `tests/fixtures/sem/golden/whereami.{en,ru}.tsv`, in English and in Russian. The gate compared
//! 28296 records per language against the legacy answer, 0 unclassified.
//!
//! `GORCA_BLESS=1 cargo test -p gorca-connector-atspi --lib info_golden` rewrites the files (review
//! the diff: every changed line is a change of what a screen reader user hears). The replay is
//! repeatable and independent of the arrival order of the nodes.
//!
//! Record format, one line per compared node and case, tab separated and sorted:
//! `corpus  node  case  surface  parts` with the parts of the answer joined by ` | `.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, Role, WebEngine};

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

/// The longest ancestry the answer walks (`commands::info::MAX_DEPTH`).
const MAX_DEPTH: usize = 24;

fn records(family: &Family, run: &Run, out: &mut Vec<Record>, drops: &mut DropCounts) {
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let Some(snaps) = page(family.dir, &name) else {
            *drops.entry("page-unparsable").or_default() += 1;
            continue;
        };
        let fused = fuse(tables, &label, &snaps, run.seed);
        let webs = web_flags(&fused);
        let index: BTreeMap<gorca_model::NodeKey, usize> =
            fused.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
        let mut seen = 0u64;
        for (i, s) in fused.iter().enumerate() {
            if s.sem.entry.is_none() {
                *drops.entry(no_entry_reason(s)).or_default() += 1;
                continue;
            }
            seen += 1;
            // The ancestry as the mirror holds it: leaf first, up to the known parent.
            let mut chain: Vec<Arc<NodeSnapshot>> = vec![Arc::clone(s)];
            let mut chain_web = vec![webs[i]];
            let mut cur = s.parent;
            while let Some(p) = cur {
                if chain.len() >= MAX_DEPTH {
                    break;
                }
                let Some(&j) = index.get(&p) else { break };
                chain.push(Arc::clone(&fused[j]));
                chain_web.push(webs[j]);
                cur = fused[j].parent;
            }
            if chain.iter().any(|n| n.sem.entry.is_none()) {
                *drops.entry("ancestor-no-entry").or_default() += 1;
                continue;
            }
            for (case, repeat, brief) in [
                ("r1", 1u8, false),
                ("r2", 2, false),
                ("r3", 3, false),
                ("brief", 1, true),
            ] {
                let parts =
                    gorca_core::describe_where_am_i(&chain, repeat, brief, run.lang, &chain_web);
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case,
                    surface: "whereami",
                    new: parts.join(" | "),
                });
            }
        }
        if seen == 0 {
            *drops.entry("empty-corpus").or_default() += 1;
        }
    }
}

/// The records of the surface (sorted lines), the number of records and the drops by reason.
fn emit(seed: u64, lang: gorca_present::Lang) -> (String, usize, DropCounts) {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is, and no extension registry is passed.
    let _lang = gorca_i18n::scoped(lang);
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    let mut drops = DropCounts::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        records(family, &run, &mut recs, &mut drops);
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

fn golden_path(lang: &str) -> std::path::PathBuf {
    root().join("golden").join(format!("whereami.{lang}.tsv"))
}

fn check(lang_name: &str, lang: gorca_present::Lang) {
    let (got, _, _) = emit(0, lang);
    let path = golden_path(lang_name);
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
            "whereami.{lang_name}: the golden records differ ({} lines recorded, {} now); first difference at line {}:\n recorded: {:?}\n now:      {:?}\n(GORCA_BLESS=1 rewrites them)",
            w.len(),
            g.len(),
            first + 1,
            w.get(first),
            g.get(first)
        );
    }
}

#[test]
fn the_where_am_i_answer_of_every_recorded_node_is_golden() {
    check("en", gorca_present::Lang::En);
    check("ru", gorca_present::Lang::Ru);
}

/// The records the goldens hold per language: the gate compared 28296 (7074 nodes, four cases) when
/// it passed (`cargo xtask cutover whereami`, 8 of 8 families, en and ru, 0 unclassified).
const GATE_RECORDS: usize = 28296;

#[test]
fn the_goldens_hold_every_record_the_gate_compared() {
    for (lang_name, lang) in [
        ("en", gorca_present::Lang::En),
        ("ru", gorca_present::Lang::Ru),
    ] {
        let (_, n, drops) = emit(0, lang);
        assert_eq!(n, GATE_RECORDS, "{lang_name}: records the replay compares");
        assert!(
            !drops.contains_key("empty-corpus")
                && !drops.contains_key("page-unparsable")
                && !drops.contains_key("ancestor-no-entry"),
            "{lang_name}: {drops:?}"
        );
        let lines = std::fs::read_to_string(golden_path(lang_name))
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(lines, GATE_RECORDS, "{lang_name}: lines in the golden file");
    }
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        let a = emit(0, lang).0;
        assert!(!a.is_empty(), "empty replay");
        assert_eq!(a, emit(0, lang).0, "two runs differ");
        assert_eq!(
            a,
            emit(0x5eed, lang).0,
            "shuffled arrival changed the output"
        );
    }
}

/// A native and a web ancestry are in the goldens, and the answers are not empty.
#[test]
fn the_goldens_hold_native_and_web_ancestries() {
    let (text, _, _) = emit(0, gorca_present::Lang::En);
    for family in FAMILIES {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("{}/", family.dir))),
            "family {} compared no node",
            family.dir
        );
    }
    assert!(
        text.lines()
            .any(|l| l.contains("Role") || l.contains("window")),
        "an ancestry names a window"
    );
    assert!(
        !text
            .lines()
            .any(|l| l.split('\t').nth(4).is_some_and(str::is_empty)),
        "an answer is empty"
    );
}
