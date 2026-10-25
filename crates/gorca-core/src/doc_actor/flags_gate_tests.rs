#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for the document carriers (`cargo xtask cutover docflags`,
//! ADR-0022 section 8.9, M22b4): the node flags and the live-region properties of every node of
//! every recorded web page, derived the legacy way (from the source node's attribute map) and from
//! the fused semantics (`flags_sem`, `Props::from_sem`).
//!
//! Environment (set by `xtask`): `GORCA_CUTOVER_SURFACE` (`docflags`), `GORCA_CUTOVER_OUT`,
//! `GORCA_CUTOVER_LANG` (accepted; the surface says no words), `GORCA_CUTOVER_SHUFFLE` (non-zero
//! seed: the fusion stage receives the nodes in a shuffled order; the records must not change).
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the other gates' format). `case` is `flags`
//! (node flags and heading level) or `live` (the region properties of the node).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gorca_doc::web::NodeFlags;
use gorca_doc_source_atspi::WebQuirks;
use gorca_model::WebEngine;

use super::agree_tests::{build, pages, recorded_doc};
use super::flags_sem::flags_of;
use super::fuse::SemLookup;
use super::live::{legacy_props, props_from_sem};
use super::translate::derive_flags;

struct Family {
    dir: &'static str,
    engine: WebEngine,
    app: &'static str,
    quirks: WebQuirks,
}

/// Keep in step with `WEB_FAMILIES` in `xtask/src/cutover.rs`.
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

/// A drop: a page nothing of which could be compared.
fn dropped(corpus: &str, reason: &'static str, example: String) -> Record {
    Record {
        corpus: corpus.to_owned(),
        node: 0,
        case: "all",
        outcome: "dropped",
        deviation: reason,
        legacy: "1".to_owned(),
        new: example,
    }
}

/// The documented differences of the live-region properties (M22a-C2 agreement, kept as dated
/// rows of `deviations.toml`): an `alertdialog` was an assertive atomic region for the legacy
/// parse and is no live region in the model, and `status` is atomic by default in the model.
#[allow(clippy::ref_option)]
fn live_deviation(
    legacy: &Option<String>,
    new: &Option<String>,
    role: gorca_model::sem::SemRole,
) -> Option<&'static str> {
    use gorca_model::sem::SemRole;
    match (legacy, new) {
        (Some(_), None) if role == SemRole::AlertDialog => Some("alertdialog-not-live"),
        (Some(l), Some(n))
            if role == SemRole::Status && l.replace("atomic: false", "atomic: true") == *n =>
        {
            Some("status-atomic")
        }
        _ => None,
    }
}

fn records(family: &Family, seed: u64, out: &mut Vec<Record>) {
    for page in pages(family.dir) {
        let corpus = format!("{}/{page}", family.dir);
        let Ok(built) = build(
            family.engine,
            family.app,
            family.quirks,
            &page,
            family.dir,
            false,
            seed,
        ) else {
            out.push(dropped(&corpus, "page-unparsable", String::new()));
            continue;
        };
        let mut compared = 0u64;
        for n in &built.nodes {
            let Some(e) = built.stage.sem(n.id) else {
                continue;
            };
            compared += 1;
            let (lf, ll) = derive_flags(n, false);
            let (nf, nl) = flags_of(n, Some(&e), false);
            let (legacy, new) = (format!("{lf:?} level {ll}"), format!("{nf:?} level {nl}"));
            // A node the engine reports as an alert is a live region by its category already:
            // it carries no separate LIVE flag, the legacy parse set one when the author wrote an
            // explicit `aria-live` on it.
            let dev = (n.role == gorca_model::Role::Alert
                && lf != nf
                && nf | NodeFlags::LIVE == lf
                && ll == nl)
                .then_some("alert-live-flag");
            out.push(Record {
                corpus: corpus.clone(),
                node: n.id.0,
                case: "flags",
                outcome: match (legacy == new, dev) {
                    (true, _) => "equal",
                    (false, Some(_)) => "allowed",
                    (false, None) => "different",
                },
                deviation: dev.unwrap_or(""),
                legacy,
                new,
            });
        }
        if compared == 0 {
            out.push(dropped(
                &corpus,
                "empty-corpus",
                "no node of the page has a fused entry".to_owned(),
            ));
        }
        // The region properties, over the document the actor builds from the same page.
        let Some(doc) = recorded_doc(
            family.engine,
            family.app,
            family.quirks,
            &page,
            family.dir,
            seed,
        ) else {
            out.push(dropped(&corpus, "no-document", String::new()));
            continue;
        };
        for id in doc.preorder() {
            let Some(v) = doc.node(id) else { continue };
            let Some(sem) = v.sem_ref() else { continue };
            let legacy = legacy_props(&v).map(|p| format!("{p:?}"));
            let new = props_from_sem(&v).map(|p| format!("{p:?}"));
            let dev = if legacy == new {
                None
            } else {
                live_deviation(&legacy, &new, sem.sem.role)
            };
            let outcome = match (legacy == new, dev) {
                (true, _) if legacy.is_none() => "silent",
                (true, _) => "equal",
                (false, Some(_)) => "allowed",
                (false, None) => "different",
            };
            out.push(Record {
                corpus: corpus.clone(),
                node: v.meta.and_then(|m| m.src).map_or(0, |s| s.0),
                case: "live",
                outcome,
                deviation: dev.unwrap_or(""),
                legacy: legacy.unwrap_or_else(|| "-".to_owned()),
                new: new.unwrap_or_else(|| "-".to_owned()),
            });
        }
    }
}

fn emit(seed: u64) -> String {
    let mut recs: Vec<Record> = Vec::new();
    for f in FAMILIES {
        records(f, seed, &mut recs);
    }
    recs.sort_by(|a, b| {
        (&a.corpus, a.node, a.case, &a.legacy, &a.new)
            .cmp(&(&b.corpus, b.node, b.case, &b.legacy, &b.new))
    });
    let mut text = String::new();
    for r in &recs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\tdocflags\t{}\t{}\t{}\t{}\t",
            r.corpus,
            r.node,
            r.case,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new)
        );
    }
    text
}

/// The gate's replay entry (run by `cargo xtask cutover`; a no-op otherwise).
#[test]
fn flags_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    assert_eq!(surface, "docflags");
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    std::fs::write(path, emit(seed)).expect("write records");
}

#[test]
fn the_replay_is_repeatable_and_order_free() {
    let a = emit(0);
    assert!(!a.is_empty());
    assert_eq!(a, emit(0), "two runs differ");
    assert_eq!(a, emit(0x5eed), "shuffled arrival changed the records");
    let by: BTreeMap<&str, usize> = a.lines().fold(BTreeMap::new(), |mut m, l| {
        *m.entry(l.split('\t').nth(4).unwrap_or("")).or_default() += 1;
        m
    });
    eprintln!("docflags records by outcome: {by:?}");
}
