#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests of browse and quick navigation (M22b2 cut-over): the replay half of the cut-over
//! gate (`cargo xtask cutover browse|quicknav`, ADR-0022 section 8.9) kept after the legacy
//! presenter was deleted.
//!
//! Every web family of `tests/fixtures/sem` goes through the real normalisation, the document
//! source's text-run layer and the fusion (the document actor's translator), giving the document
//! the browse cursor walks; the description of every line, of blocks of up to four lines (say-all),
//! of the range of every node (element and table-cell commands) and of every navigable node
//! (quick navigation), from nowhere and from the line before, must equal the checked-in records
//! `tests/fixtures/sem/golden/{browse,quicknav}.{en,ru}.tsv`, in English and in Russian.
//! `the_goldens_hold_every_record_the_gate_compared` pins the number of records the gate compared
//! when it passed, so a golden file that holds fewer has lost coverage. Every difference to the
//! legacy presenter was zero or a dated, reasoned deviation row at that point.
//!
//! `GORCA_BLESS=1 cargo test -p gorca-core --lib browse::golden` rewrites the files (review the
//! diff: every changed line is a change of what a screen reader user hears). The replay is
//! repeatable and independent of the arrival order of the nodes.
//!
//! Record format, one line per compared range or node, tab separated and sorted:
//! `corpus  node  case  tokens` with `\`, tab and newline escaped.

use std::fmt::Write as _;
use std::path::PathBuf;

use gorca_doc::web::{Cat, NodeFlags, WebDoc};
use gorca_doc_source_atspi::WebQuirks;
use gorca_model::{Segment, WebEngine};
use gorca_speech::VerbalizeOpts;

use super::present;
use crate::doc_actor::agree_tests::{pages, recorded_doc};

struct Family {
    dir: &'static str,
    engine: WebEngine,
    app: &'static str,
    quirks: WebQuirks,
}

/// The families with a browse cursor.
const FAMILIES: &[Family] = &[
    Family {
        dir: "firefox",
        engine: WebEngine::Gecko,
        app: "Firefox",
        quirks: WebQuirks::GECKO,
    },
    Family {
        dir: "chromium",
        engine: WebEngine::Blink,
        app: "Chromium",
        quirks: WebQuirks::BLINK,
    },
    Family {
        dir: "electron",
        engine: WebEngine::Blink,
        app: "Signal",
        quirks: WebQuirks::BLINK,
    },
    Family {
        dir: "telegram",
        engine: WebEngine::Blink,
        app: "Telegram",
        quirks: WebQuirks::BLINK,
    },
];

/// The records the goldens hold per language. The cut-over gate compared 15910 (browse) and 3026
/// (quicknav) when it passed (`cargo xtask cutover browse` / `quicknav`, 4 of 4 families); the
/// rest are the pages recorded after it: the M8b widgets and forms of both engines and the
/// error-message page.
const GATE_BROWSE_RECORDS: usize = 17847;
const GATE_QUICKNAV_RECORDS: usize = 3500;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sem")
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn text_of(segs: &[Segment]) -> String {
    segs.iter()
        .map(|s| format!("{:?}:{}", s.kind, s.text))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The nodes quick navigation can land on: everything with a category except the plain text runs
/// and the machinery categories (the document's own indexes).
fn navigable(c: Cat) -> bool {
    let machinery = Cat::TEXT_RUN.0
        | Cat::TEXT_BLOCK.0
        | Cat::LINE_HOST.0
        | Cat::FOCUSABLE.0
        | Cat::CLICKABLE.0
        | Cat::ANNOTATION.0;
    c.0 & !machinery != 0
}

fn ui_lang(ru: bool) -> gorca_i18n::Lang {
    if ru {
        gorca_i18n::Lang::Ru
    } else {
        gorca_i18n::Lang::En
    }
}

struct Out {
    lines: Vec<(String, u64, &'static str, String)>,
}

impl Out {
    fn add(&mut self, corpus: &str, node: u64, case: &'static str, segs: &[Segment]) {
        self.lines
            .push((corpus.to_owned(), node, case, text_of(segs)));
    }
}

fn lines(doc: &WebDoc, o: VerbalizeOpts, corpus: &str, out: &mut Out) {
    let mut prev: Option<u32> = None;
    for k in 0..doc.line_count() {
        let Some(a) = doc.line_start(k) else {
            continue;
        };
        let (s, e) = doc.line_range(a);
        out.add(
            corpus,
            u64::from(s),
            "line",
            &present::segments(&present::describe(doc, s, e), o),
        );
        if let Some(p) = prev {
            out.add(
                corpus,
                u64::from(s),
                "line-from",
                &present::segments(&present::describe_from(doc, s, e, Some(p)), o),
            );
        }
        // Say-all speaks a paragraph at a time: a range of up to four lines.
        let last = (k + 3).min(doc.line_count().saturating_sub(1));
        if last > k
            && let Some(le) = doc.line_start(last)
        {
            let (_, end) = doc.line_range(le);
            out.add(
                corpus,
                u64::from(s),
                "block",
                &present::segments(&present::describe(doc, s, end), o),
            );
        }
        prev = Some(s);
    }
}

/// The range of every node that has text, as the element and table-cell commands speak it.
fn ranges(doc: &WebDoc, o: VerbalizeOpts, corpus: &str, out: &mut Out) {
    for (i, id) in doc.preorder().enumerate() {
        let Some(v) = doc.node(id) else { continue };
        if v.flags.contains(NodeFlags::HIDDEN) {
            continue;
        }
        let Some((a, b)) = doc.subtree_text_range(id) else {
            continue;
        };
        if a >= b || b - a > 400 {
            continue;
        }
        let node = i as u64;
        out.add(
            corpus,
            node,
            "range",
            &present::segments(&present::describe_from(doc, a, b, None), o),
        );
        if a > 0
            && let Some(from) = doc.line_start(doc.line_of(a.saturating_sub(1)))
        {
            out.add(
                corpus,
                node,
                "range-from",
                &present::segments(&present::describe_from(doc, a, b, Some(from)), o),
            );
        }
    }
}

fn nodes(doc: &WebDoc, o: VerbalizeOpts, corpus: &str, out: &mut Out) {
    for (i, id) in doc.preorder().enumerate() {
        let Some(v) = doc.node(id) else { continue };
        if !navigable(v.cats) || v.flags.contains(NodeFlags::HIDDEN) {
            continue;
        }
        let node = i as u64;
        out.add(
            corpus,
            node,
            "node",
            &present::segments(&present::describe_node(doc, id), o),
        );
        // Arriving from the line before the node.
        if let Some(a) = super::cursor::content_start(doc, id)
            && a > 0
            && let Some(from) = doc.line_start(doc.line_of(a.saturating_sub(1)))
        {
            out.add(
                corpus,
                node,
                "node-from",
                &present::segments(&present::describe_node_from(doc, id, Some(from)), o),
            );
        }
    }
}

/// Replay `surface` over every family: the golden text and the number of records.
fn emit(surface: &str, ru: bool, seed: u64) -> (String, usize) {
    let _lang = gorca_i18n::scoped(ui_lang(ru));
    let o = VerbalizeOpts {
        lang: ui_lang(ru),
        ..VerbalizeOpts::default()
    };
    let mut out = Out { lines: Vec::new() };
    for f in FAMILIES {
        for page in pages(f.dir) {
            let corpus = format!("{}/{page}", f.dir);
            let doc = recorded_doc(f.engine, f.app, f.quirks, &page, f.dir, seed)
                .unwrap_or_else(|| panic!("{corpus}: the recorded page builds no document"));
            assert!(doc.text_len() > 0, "{corpus}: the page has no text");
            if surface == "browse" {
                lines(&doc, o, &corpus, &mut out);
                ranges(&doc, o, &corpus, &mut out);
            } else {
                nodes(&doc, o, &corpus, &mut out);
            }
        }
    }
    out.lines
        .sort_by(|a, b| (&a.0, a.1, a.2, &a.3).cmp(&(&b.0, b.1, b.2, &b.3)));
    let mut text = String::new();
    for (corpus, node, case, said) in &out.lines {
        let _ = writeln!(text, "{corpus}\t{node}\t{case}\t{}", esc(said));
    }
    (text, out.lines.len())
}

fn golden_path(surface: &str, lang: &str) -> PathBuf {
    root().join("golden").join(format!("{surface}.{lang}.tsv"))
}

fn check(surface: &str, lang: &str, ru: bool) {
    let (got, _) = emit(surface, ru, 0);
    let path = golden_path(surface, lang);
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
            "{surface}.{lang}: the golden records differ ({} lines recorded, {} now); first difference at line {}:\n recorded: {:?}\n now:      {:?}\n(GORCA_BLESS=1 rewrites them)",
            w.len(),
            g.len(),
            first + 1,
            w.get(first),
            g.get(first)
        );
    }
}

#[test]
fn the_lines_ranges_and_blocks_of_every_recorded_page_are_golden() {
    check("browse", "en", false);
    check("browse", "ru", true);
}

#[test]
fn the_quick_navigation_description_of_every_recorded_node_is_golden() {
    check("quicknav", "en", false);
    check("quicknav", "ru", true);
}

#[test]
fn the_goldens_hold_every_record_the_gate_compared() {
    for (lang, ru) in [("en", false), ("ru", true)] {
        for (surface, want) in [
            ("browse", GATE_BROWSE_RECORDS),
            ("quicknav", GATE_QUICKNAV_RECORDS),
        ] {
            let (_, n) = emit(surface, ru, 0);
            assert_eq!(n, want, "{surface}.{lang}: records the replay compares");
            let lines = std::fs::read_to_string(golden_path(surface, lang))
                .unwrap_or_default()
                .lines()
                .count();
            assert_eq!(lines, want, "{surface}.{lang}: lines in the golden file");
        }
    }
}

#[test]
fn the_replay_is_repeatable_and_independent_of_the_arrival_order() {
    for ru in [false, true] {
        for surface in ["browse", "quicknav"] {
            let a = emit(surface, ru, 0).0;
            assert!(!a.is_empty(), "{surface}: empty replay");
            assert_eq!(a, emit(surface, ru, 0).0, "{surface}: two runs differ");
            assert_eq!(
                a,
                emit(surface, ru, 0x5eed).0,
                "{surface}: shuffled arrival changed the output"
            );
        }
    }
}

#[test]
fn every_family_has_recorded_pages_and_the_goldens_cover_the_kinds_of_words() {
    let text = std::fs::read_to_string(golden_path("browse", "en")).unwrap_or_default();
    for f in FAMILIES {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("{}/", f.dir))),
            "family {} is missing from the browse golden",
            f.dir
        );
    }
    // The words the presenter owns are all in the corpus: a role phrase, a state, a container.
    for needle in [
        "Role:link",
        "Role:heading level 1",
        "State:checked",
        "Role:list with",
        "Role:table with",
        "State:unavailable",
    ] {
        assert!(text.contains(needle), "no golden record says `{needle}`");
    }
}

#[test]
fn an_invalid_field_says_its_error_message_on_both_engines() {
    // The message is the text of the `error-message` relation's target, read at fetch time and
    // fused into the entry; the recorded error page carries it.
    let text = std::fs::read_to_string(golden_path("quicknav", "en")).unwrap_or_default();
    for engine in ["firefox", "chromium"] {
        let said = text.lines().any(|l| {
            l.starts_with(&format!("{engine}/errors\t"))
                && l.contains("State:invalid entry")
                && l.contains("Said:Enter an address like name at example dot org")
        });
        assert!(
            said,
            "{engine}: the email field does not say its error message"
        );
    }
}
