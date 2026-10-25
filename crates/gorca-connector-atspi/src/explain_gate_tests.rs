#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for `explain` (`cargo xtask cutover explain`, ADR-0022
//! section 8.9, M22b4).
//!
//! Every recorded corpus family is fused the way the app actor fuses it; every node is presented
//! as a focus (`compose` + `render_kinds`) and explained: the legacy view of an explanation is
//! its list of segments (the words as the speech lane tagged them), the new one is its list of
//! IR fields (rule row, group and via of each field). They must agree one to one: the kinds of
//! the segments equal the segment kinds of the fields that rendered words, and the report lists
//! every one of those fields under its own kind. A field the words do not come from, or words
//! no field explains, is a difference.
//!
//! Environment (set by `xtask`): `GORCA_CUTOVER_SURFACE` (`explain`), `GORCA_CUTOVER_OUT`,
//! `GORCA_CUTOVER_LANG` (`en` or `ru`), `GORCA_CUTOVER_SHUFFLE` (non-zero seed: the arrival order
//! of the nodes at the fusion writer is shuffled; the records must not change).
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the other gates' format).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gorca_core::{SourceDirectory, explain_utterance};
use gorca_model::{Epoch, NodeSnapshot, Prio, Segment, UttId, Utterance, WebEngine};
use gorca_present::{ComposeCtx, Reason, RenderOpts, ReportFlags, compose, render_kinds};

use crate::golden_tests::{
    FAMILIES, Family, describe, esc, fuse, kind_mismatch_reason, no_entry_reason, web_flags,
};
use crate::recorded_corpus_tests::{page, pages_of, root};

struct Record {
    corpus: String,
    node: u64,
    outcome: &'static str,
    deviation: &'static str,
    legacy: String,
    new: String,
}

fn dropped(corpus: &str, node: u64, reason: &'static str, example: String) -> Record {
    Record {
        corpus: corpus.to_owned(),
        node,
        outcome: "dropped",
        deviation: reason,
        legacy: "1".to_owned(),
        new: example,
    }
}

/// The two views of the explanation of one composed focus utterance.
fn views(
    entry: &gorca_model::sem::SemEntry,
    ir: &gorca_present::PresIr,
    opts: RenderOpts<'_>,
) -> (String, String) {
    let rendered = render_kinds(ir, opts);
    let utt = Utterance {
        id: UttId(1),
        segments: rendered
            .iter()
            .map(|(_, t)| Segment::new(t.text.as_str().into(), t.kind))
            .collect(),
        lang: None,
        epoch: Epoch::default(),
        prio: Prio::Interrupt,
        may_refine: false,
        survives_focus_change: false,
        ttl_secs: 0,
        refines: None,
    };
    let secret = gorca_present::is_secret_entry(entry);
    let report = explain_utterance(
        &utt,
        Some(entry),
        None,
        &SourceDirectory::with_atspi_primary(),
        false,
    )
    .with_ir(&ir.redacted(), false, secret);
    let legacy = report
        .segments
        .iter()
        .map(|s| format!("{:?}", s.kind))
        .collect::<Vec<_>>()
        .join(" ");
    // The fields that rendered words: walk the IR fields against the rendered kinds, in order.
    let mut new = Vec::new();
    let mut next = 0usize;
    let listed = report.fields.len() == ir.fields.len();
    for (i, f) in ir.fields.iter().enumerate() {
        if rendered.get(next).is_some_and(|(k, _)| *k == f.kind) {
            next += 1;
            let name = f
                .kind
                .name()
                .map_or_else(|| format!("ext-{}", f.kind.0), str::to_owned);
            if listed && report.fields.get(i).is_some_and(|l| l.kind == name) {
                new.push(format!("{:?}", f.kind.segment_kind(opts.description_pause)));
            } else {
                new.push(format!("unlisted:{name}"));
            }
        }
    }
    if next != rendered.len() {
        new.push("words-without-field".to_owned());
    }
    (legacy, new.join(" "))
}

fn records(family: &Family, lang: gorca_present::Lang, seed: u64, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let Some(snaps) = page(family.dir, &name) else {
            out.push(dropped(&label, 0, "page-unparsable", String::new()));
            continue;
        };
        let fused = fuse(tables, &label, &snaps, seed);
        let webs = web_flags(&fused);
        let mut compared = 0u64;
        for (s, web) in fused.iter().zip(&webs) {
            let s: &NodeSnapshot = s;
            if *web != web_run {
                out.push(dropped(
                    &label,
                    s.key().local.0,
                    kind_mismatch_reason(web_run),
                    describe(s),
                ));
                continue;
            }
            let Some(entry) = s.sem().entry.as_ref() else {
                out.push(dropped(
                    &label,
                    s.key().local.0,
                    no_entry_reason(s),
                    describe(s),
                ));
                continue;
            };
            let ctx = ComposeCtx {
                flags: ReportFlags::default(),
                may_refine: false,
                web: web_run,
                skip_selected: false,
                hedge_inferred: false,
                lang,
            };
            let Ok(ir) = compose(entry, s.key(), Reason::Focus, ctx) else {
                continue;
            };
            let opts = RenderOpts {
                description_pause: web_run,
                lang,
                ..RenderOpts::default()
            };
            compared += 1;
            let (legacy, new) = views(entry, &ir, opts);
            out.push(Record {
                corpus: label.clone(),
                node: s.key().local.0,
                outcome: if legacy == new { "equal" } else { "different" },
                deviation: "",
                legacy,
                new,
            });
        }
        if compared == 0 {
            out.push(dropped(
                &label,
                0,
                "empty-corpus",
                "no node of the page was compared".to_owned(),
            ));
        }
    }
}

fn emit(lang: gorca_present::Lang, seed: u64) -> String {
    let _lang = gorca_i18n::scoped(lang);
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        records(family, lang, seed, &mut recs);
    }
    // One dropped record per (corpus, reason): the count and the first example, like the other
    // gates.
    let mut drops: BTreeMap<(String, &'static str), (u64, String)> = BTreeMap::new();
    let mut kept = Vec::new();
    for r in recs {
        if r.outcome == "dropped" {
            let e = drops
                .entry((r.corpus.clone(), r.deviation))
                .or_insert((0, r.new.clone()));
            e.0 += 1;
        } else {
            kept.push(r);
        }
    }
    kept.sort_by(|a, b| {
        (&a.corpus, a.node, &a.legacy, &a.new).cmp(&(&b.corpus, b.node, &b.legacy, &b.new))
    });
    let mut text = String::new();
    for r in &kept {
        let _ = writeln!(
            text,
            "{}\t{}\tfocus\texplain\t{}\t{}\t{}\t{}\t",
            r.corpus,
            r.node,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new)
        );
    }
    for ((corpus, reason), (n, example)) in &drops {
        let _ = writeln!(
            text,
            "{corpus}\t0\tfocus\texplain\tdropped\t{reason}\t{n}\t{}\t",
            esc(example)
        );
    }
    text
}

fn lang_of(name: &str) -> gorca_present::Lang {
    if name == "ru" {
        gorca_present::Lang::Ru
    } else {
        gorca_present::Lang::En
    }
}

/// The gate's replay entry (run by `cargo xtask cutover`; a no-op otherwise).
#[test]
fn explain_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    assert_eq!(surface, "explain");
    let lang = lang_of(&std::env::var("GORCA_CUTOVER_LANG").unwrap_or_default());
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    std::fs::write(path, emit(lang, seed)).expect("write records");
}

#[test]
fn the_replay_is_repeatable_and_order_free() {
    let a = emit(gorca_present::Lang::En, 0);
    assert!(!a.is_empty());
    assert_eq!(a, emit(gorca_present::Lang::En, 0), "two runs differ");
    assert_eq!(
        a,
        emit(gorca_present::Lang::En, 0x5eed),
        "shuffled arrival changed the records"
    );
    assert!(
        !a.lines().any(|l| l.split('\t').nth(4) == Some("different")),
        "an explanation disagrees with its words"
    );
}
