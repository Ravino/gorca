#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for browse and quick navigation (`cargo xtask cutover
//! browse|quicknav`, ADR-0022 section 8.9, M22b2).
//!
//! Every web family of `tests/fixtures/sem` goes through the real normalisation and fusion (the
//! document actor's translator), giving the document the browse cursor walks, and then through
//! two presenters: the legacy `present::describe_from` / `describe_node_from` (aria words read off
//! the legacy node metadata) and `ir::describe_from` / `describe_node_from` (the fused entry
//! through `gorca-present`). One record per line of the flat text (`browse`) and per navigable node
//! (`quicknav`) says whether the two agree.
//!
//! The fixtures hold names but not the contents of the Text interface, so the flat text is the
//! synthetic one of `agree_tests::synthetic_texts`. Native families (GTK, Shell, VTE) have no
//! browse cursor and are not replayed.
//!
//! Environment (set by `xtask`): `GORCA_CUTOVER_SURFACE` (`browse` or `quicknav`),
//! `GORCA_CUTOVER_OUT`, `GORCA_CUTOVER_LANG` (`en` or `ru`), `GORCA_CUTOVER_SHUFFLE`.
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the same as the focus and change gates).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_doc::web::{Cat, NodeFlags, WebDoc};
use gorca_doc_source_atspi::WebQuirks;
use gorca_model::{Segment, WebEngine};
use gorca_present::{
    Deviations, DiffCtx, DiffOutcome, Lang, LegacyToken, Token, TokenText, diff_report,
};
use gorca_speech::VerbalizeOpts;

use super::{ir, present};
use crate::doc_actor::agree_tests::{pages, recorded_doc};

struct Family {
    dir: &'static str,
    engine: WebEngine,
    app: &'static str,
    quirks: WebQuirks,
}

/// The families with a browse cursor. Keep in step with `FAMILIES` in `xtask/src/cutover.rs`.
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

struct Record {
    corpus: String,
    node: u64,
    case: &'static str,
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

fn join(it: impl Iterator<Item = (gorca_model::SegmentKind, String)>) -> String {
    it.map(|(k, t)| format!("{k:?}:{t}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn legacy_tokens(segs: &[Segment]) -> Vec<LegacyToken> {
    segs.iter()
        .map(|s| LegacyToken {
            kind: s.kind,
            text: Arc::clone(&s.text),
        })
        .collect()
}

fn new_tokens(segs: &[Segment]) -> Vec<Token> {
    segs.iter()
        .map(|s| Token {
            kind: s.kind,
            text: TokenText::Shared(Arc::clone(&s.text)),
        })
        .collect()
}

fn text_of(segs: &[Segment]) -> String {
    join(segs.iter().map(|s| (s.kind, s.text.to_string())))
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

struct Run {
    surface: &'static str,
    lang: Lang,
    seed: u64,
}

/// What the fused entries of the nodes a range covers say that the legacy mapping could not: the
/// facts the intended-difference rows of browse are gated by.
#[derive(Copy, Clone, Default)]
struct RangeFacts {
    unavailable_item: bool,
    canvas_graphic: bool,
    alert_dialog: bool,
}

/// The legacy roles whose "unavailable" the legacy words did say.
fn legacy_control(role: gorca_model::Role) -> bool {
    use gorca_model::Role as R;
    matches!(
        role,
        R::Button
            | R::ToggleButton
            | R::CheckBox
            | R::RadioButton
            | R::ComboBox
            | R::TextEntry
            | R::PasswordEntry
            | R::SpinButton
    )
}

fn range_facts(doc: &WebDoc, a: u32, b: u32) -> RangeFacts {
    use gorca_model::StateSet;
    use gorca_model::sem::{Avail, SemRole};
    let mut f = RangeFacts::default();
    let Some((n0, _)) = doc.at_offset(a.min(doc.text_len().saturating_sub(1))) else {
        return f;
    };
    let mut ids: Vec<gorca_model::DocNodeId> = Vec::new();
    let mut cur = doc.parent(n0);
    while let Some(p) = cur {
        ids.push(p);
        cur = doc.parent(p);
    }
    let mut off = a;
    for id in doc.preorder_from(n0) {
        if off >= b {
            break;
        }
        ids.push(id);
        off += doc.own_len(id).unwrap_or(0).max(1);
    }
    for id in ids {
        let Some(v) = doc.node(id) else { continue };
        let Some(e) = v.meta.and_then(|m| m.sem.as_deref()) else {
            continue;
        };
        let legacy_enabled = v.meta.is_none_or(|m| m.states.contains(StateSet::ENABLED));
        if e.sem.avail == Avail::Disabled && !legacy_enabled && !legacy_control(v.role) {
            f.unavailable_item = true;
        }
        if e.sem.role == SemRole::Img && v.role != gorca_model::Role::Image {
            f.canvas_graphic = true;
        }
        if e.sem.role == SemRole::AlertDialog {
            f.alert_dialog = true;
        }
    }
    f
}

#[allow(clippy::too_many_arguments)]
fn compare(
    out: &mut Vec<Record>,
    run: &Run,
    corpus: &str,
    node: u64,
    case: &'static str,
    dev: &Deviations,
    facts: RangeFacts,
    legacy: &[Segment],
    new: &[Segment],
) {
    let lt = legacy_tokens(legacy);
    let nt = new_tokens(new);
    let ctx = DiffCtx {
        surface: run.surface,
        lang: run.lang,
        unavailable_item: facts.unavailable_item,
        canvas_graphic: facts.canvas_graphic,
        alert_dialog: facts.alert_dialog,
        ..DiffCtx::FOCUS
    };
    let (d, _fired) = diff_report(&lt, &nt, dev, ctx);
    let (outcome, deviation) = match d {
        DiffOutcome::Equal if lt.is_empty() && nt.is_empty() => ("silent", ""),
        DiffOutcome::Equal => ("equal", ""),
        DiffOutcome::OrderOnly => ("order-only", ""),
        DiffOutcome::Allowed(id) => ("allowed", id),
        DiffOutcome::Different { .. } => ("different", ""),
    };
    out.push(Record {
        corpus: corpus.to_owned(),
        node,
        case,
        surface: run.surface,
        outcome,
        deviation,
        legacy: text_of(legacy),
        new: text_of(new),
    });
}

fn dropped(
    out: &mut Vec<Record>,
    run: &Run,
    corpus: &str,
    case: &'static str,
    reason: &'static str,
    n: u64,
    example: String,
) {
    out.push(Record {
        corpus: corpus.to_owned(),
        node: 0,
        case,
        surface: run.surface,
        outcome: "dropped",
        deviation: reason,
        legacy: n.to_string(),
        new: example,
    });
}

fn ui_lang(lang: Lang) -> gorca_i18n::Lang {
    if lang == Lang::Ru {
        gorca_i18n::Lang::Ru
    } else {
        gorca_i18n::Lang::En
    }
}

fn opts(lang: Lang) -> VerbalizeOpts {
    VerbalizeOpts {
        lang: ui_lang(lang),
        ..VerbalizeOpts::default()
    }
}

fn lines(doc: &WebDoc, run: &Run, corpus: &str, dev: &Deviations, out: &mut Vec<Record>) {
    let o = opts(run.lang);
    let mut prev: Option<u32> = None;
    for k in 0..doc.line_count() {
        let Some(a) = doc.line_start(k) else {
            continue;
        };
        let (s, e) = doc.line_range(a);
        let legacy = present::segments(&present::describe(doc, s, e), o);
        let new = present::segments(&ir::describe(doc, s, e), o);
        let facts = range_facts(doc, s, e);
        compare(
            out,
            run,
            corpus,
            u64::from(s),
            "line",
            dev,
            facts,
            &legacy,
            &new,
        );
        if let Some(p) = prev {
            let legacy = present::segments(&present::describe_from(doc, s, e, Some(p)), o);
            let new = present::segments(&ir::describe_from(doc, s, e, Some(p)), o);
            compare(
                out,
                run,
                corpus,
                u64::from(s),
                "line-from",
                dev,
                facts,
                &legacy,
                &new,
            );
        }
        // Say-all speaks a paragraph at a time: a range of up to four lines.
        let last = (k + 3).min(doc.line_count().saturating_sub(1));
        if last > k
            && let Some(le) = doc.line_start(last)
        {
            let (_, end) = doc.line_range(le);
            let legacy = present::segments(&present::describe(doc, s, end), o);
            let new = present::segments(&ir::describe(doc, s, end), o);
            let facts = range_facts(doc, s, end);
            compare(
                out,
                run,
                corpus,
                u64::from(s),
                "block",
                dev,
                facts,
                &legacy,
                &new,
            );
        }
        prev = Some(s);
    }
}

/// The range of every node that has text, as the element and table-cell commands speak it: from
/// nowhere and from the start of the line before.
fn ranges(doc: &WebDoc, run: &Run, corpus: &str, dev: &Deviations, out: &mut Vec<Record>) {
    let o = opts(run.lang);
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
        let facts = range_facts(doc, a, b);
        let legacy = present::segments(&present::describe_from(doc, a, b, None), o);
        let new = present::segments(&ir::describe_from(doc, a, b, None), o);
        compare(out, run, corpus, node, "range", dev, facts, &legacy, &new);
        if a > 0
            && let Some(from) = doc.line_start(doc.line_of(a.saturating_sub(1)))
        {
            let legacy = present::segments(&present::describe_from(doc, a, b, Some(from)), o);
            let new = present::segments(&ir::describe_from(doc, a, b, Some(from)), o);
            compare(
                out,
                run,
                corpus,
                node,
                "range-from",
                dev,
                facts,
                &legacy,
                &new,
            );
        }
    }
}

fn nodes(doc: &WebDoc, run: &Run, corpus: &str, dev: &Deviations, out: &mut Vec<Record>) {
    let o = opts(run.lang);
    let mut hidden = 0u64;
    let mut first_hidden = String::new();
    for (i, id) in doc.preorder().enumerate() {
        let Some(v) = doc.node(id) else { continue };
        if !navigable(v.cats) {
            continue;
        }
        if v.flags.contains(NodeFlags::HIDDEN) {
            hidden += 1;
            if first_hidden.is_empty() {
                first_hidden = format!("{:?} {:?}", v.role, v.name);
            }
            continue;
        }
        let node = i as u64;
        let legacy = present::segments(&present::describe_node(doc, id), o);
        let new = present::segments(&ir::describe_node(doc, id), o);
        let (ns, ne) = doc.subtree_text_range(id).unwrap_or((0, 0));
        let facts = range_facts(doc, ns, ne.max(ns + 1));
        compare(out, run, corpus, node, "node", dev, facts, &legacy, &new);
        // Arriving from the line before the node.
        if let Some(a) = super::cursor::content_start(doc, id)
            && a > 0
            && let Some(from) = doc.line_start(doc.line_of(a.saturating_sub(1)))
        {
            let legacy = present::segments(&present::describe_node_from(doc, id, Some(from)), o);
            let new = present::segments(&ir::describe_node_from(doc, id, Some(from)), o);
            compare(
                out,
                run,
                corpus,
                node,
                "node-from",
                dev,
                facts,
                &legacy,
                &new,
            );
        }
    }
    if hidden > 0 {
        dropped(
            out,
            run,
            corpus,
            "node",
            "hidden-node",
            hidden,
            first_hidden,
        );
    }
}

fn emit(surface: &'static str, lang: Lang, seed: u64) -> String {
    let _lang = gorca_i18n::scoped(ui_lang(lang));
    let dev = Deviations::builtin();
    let run = Run {
        surface,
        lang,
        seed,
    };
    let mut recs: Vec<Record> = Vec::new();
    for f in FAMILIES {
        for page in pages(f.dir) {
            let corpus = format!("{}/{page}", f.dir);
            let Some(doc) = recorded_doc(f.engine, f.app, f.quirks, &page, f.dir, run.seed) else {
                dropped(
                    &mut recs,
                    &run,
                    &corpus,
                    "page",
                    "page-unparsable",
                    1,
                    String::new(),
                );
                continue;
            };
            if doc.text_len() == 0 {
                dropped(
                    &mut recs,
                    &run,
                    &corpus,
                    "page",
                    "empty-corpus",
                    1,
                    "the page has no text".to_owned(),
                );
                continue;
            }
            if surface == "browse" {
                lines(&doc, &run, &corpus, &dev, &mut recs);
                ranges(&doc, &run, &corpus, &dev, &mut recs);
            } else {
                nodes(&doc, &run, &corpus, &dev, &mut recs);
            }
        }
    }
    recs.sort_by(|a, b| {
        (&a.corpus, a.node, a.case, a.outcome, &a.legacy, &a.new)
            .cmp(&(&b.corpus, b.node, b.case, b.outcome, &b.legacy, &b.new))
    });
    let mut text = String::new();
    for r in &recs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            r.corpus,
            r.node,
            r.case,
            r.surface,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new),
        );
    }
    text
}

fn lang_of(s: &str) -> Option<Lang> {
    match s {
        "en" => Some(Lang::En),
        "ru" => Some(Lang::Ru),
        _ => None,
    }
}

/// The gate's replay entry (run by `cargo xtask cutover`; a no-op otherwise).
#[test]
fn browse_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    let surface: &'static str = match surface.as_str() {
        "browse" => "browse",
        "quicknav" => "quicknav",
        other => panic!("not a browse surface: {other}"),
    };
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let lang_name = std::env::var("GORCA_CUTOVER_LANG").unwrap_or_else(|_| "en".to_owned());
    let lang = lang_of(&lang_name).expect("GORCA_CUTOVER_LANG is en or ru");
    std::fs::write(path, emit(surface, lang, seed)).expect("write records");
}

/// Development view: the grouped differences of both surfaces, first examples (run with
/// `--ignored --nocapture`).
#[test]
#[ignore = "development summary of the replay"]
fn browse_summary() {
    for surface in ["browse", "quicknav"] {
        let text = emit(surface, Lang::En, 0);
        if let Ok(dir) = std::env::var("GORCA_B2_DUMP") {
            std::fs::write(format!("{dir}/{surface}.tsv"), &text).expect("dump");
        }
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        let mut shown = 0;
        for l in text.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            *by.entry(f[4]).or_default() += 1;
            if f[4] == "different" && shown < 60 {
                shown += 1;
                eprintln!(
                    "{surface} {} #{} {}\n   legacy {}\n   new    {}",
                    f[0], f[1], f[2], f[6], f[7]
                );
            }
        }
        eprintln!("{surface}: {by:?}");
    }
}

/// Development view of the nodes that cover one line: `GORCA_B2_NODE=family/page@offset`
/// (run with `--ignored --nocapture`).
#[test]
#[ignore = "development view of one line"]
fn browse_nodes_at() {
    let Ok(spec) = std::env::var("GORCA_B2_NODE") else {
        return;
    };
    let (corpus, off) = spec.split_once('@').expect("family/page@offset");
    let off: u32 = off.parse().expect("offset");
    let (dir, page) = corpus.split_once('/').expect("family/page");
    let f = FAMILIES.iter().find(|f| f.dir == dir).expect("family");
    let doc = recorded_doc(f.engine, f.app, f.quirks, page, dir, 0).expect("doc");
    let (s, e) = doc.line_range(off);
    eprintln!("line {s}..{e}");
    let Some((n0, _)) = doc.at_offset(s) else {
        return;
    };
    let mut up = vec![];
    let mut cur = doc.parent(n0);
    while let Some(p) = cur {
        up.push(p);
        cur = doc.parent(p);
    }
    for id in up.into_iter().rev().chain(doc.preorder_from(n0).take(30)) {
        let Some(v) = doc.node(id) else { continue };
        let sem = v.meta.and_then(|m| m.sem.as_deref());
        eprintln!(
            "{id:?} legacy={:?} sem={:?} flags={:?} name={:?} text={:?} range={:?} cats={:#x}",
            v.role,
            sem.map(|e| (e.sem.role, e.sem.flags)),
            v.flags,
            v.name,
            v.text,
            doc.subtree_text_range(id),
            v.cats.0
        );
    }
}
