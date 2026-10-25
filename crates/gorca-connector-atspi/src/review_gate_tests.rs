#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for review and object navigation (`cargo xtask cutover
//! review`, ADR-0022 section 8.9, M22b3).
//!
//! Every recorded corpus family is fused the way the app actor fuses it, and every node goes
//! through the legacy review paths (words read off the legacy snapshot fields: `flat_zone_text`,
//! `Role::spoken`, `class_in`) and through the fused-entry paths (`compose(Reason::Review)`,
//! `class_entry`). One record per node and case:
//!
//! * `flat`: what flat review and object review say for the whole object;
//! * `zone`: the same, and for a silent object that can be acted on its role word;
//! * `noname`: the role word of a nameless object (`InfoNoName`);
//! * `class`: the navigation class (object review) and, for web content, `class-web` (the surface
//!   walk's class, which treats `VISIBLE` alone as showing);
//! * `walk`: where each of the six object moves of simple review goes from the node.
//!
//! Environment (set by `xtask`): `GORCA_CUTOVER_SURFACE` (`review`), `GORCA_CUTOVER_OUT`,
//! `GORCA_CUTOVER_LANG` (`en` or `ru`), `GORCA_CUTOVER_SHUFFLE` (seeded arrival order of the
//! fusion writer; the records must not change).
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the same as the focus and browse gates).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_core::review_gate as rg;
use gorca_model::sem::{Group, SemEntry, SemRole, SrcRecord};
use gorca_model::{ChildList, NodeKey, NodeSnapshot, PropMask, Role, SegmentKind, WebEngine};
use gorca_present::{Deviations, DiffCtx, DiffOutcome, LegacyToken, Token, TokenText, diff_report};

use crate::golden_tests::{
    FAMILIES, Family, describe, esc, fuse, kind_mismatch_reason, no_entry_reason, web_flags,
};
use crate::legacy_agreement_tests::{page, page_meta, pages_of, root};

struct Record {
    corpus: String,
    node: u64,
    case: &'static str,
    surface: &'static str,
    outcome: &'static str,
    deviation: &'static str,
    legacy: String,
    new: String,
    witness: String,
    /// Development only: the legacy and fused role of the node.
    note: String,
}

fn join<'a>(it: impl Iterator<Item = (SegmentKind, &'a str)>) -> String {
    it.map(|(k, t)| format!("{k:?}:{t}"))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn dropped(
    corpus: &str,
    case: &'static str,
    surface: &'static str,
    reason: &'static str,
    n: u64,
    example: String,
) -> Record {
    Record {
        corpus: corpus.to_owned(),
        node: 0,
        case,
        surface,
        outcome: "dropped",
        deviation: reason,
        legacy: n.to_string(),
        new: example,
        witness: String::new(),
        note: String::new(),
    }
}

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

    fn flush(self, corpus: &str, case: &'static str, surface: &'static str, out: &mut Vec<Record>) {
        for (reason, (n, example)) in self.0 {
            out.push(dropped(corpus, case, surface, reason, n, example));
        }
    }
}

struct Run {
    lang: gorca_present::Lang,
    seed: u64,
}

fn legacy_tokens(pieces: &[(SegmentKind, String)]) -> Vec<LegacyToken> {
    pieces
        .iter()
        .map(|(k, t)| LegacyToken {
            kind: *k,
            text: Arc::from(t.as_str()),
        })
        .collect()
}

fn text_token(s: &str) -> Token {
    Token {
        kind: SegmentKind::Text,
        text: TokenText::Owned(s.into()),
    }
}

fn outcome_of(
    d: &DiffOutcome,
    legacy_empty: bool,
    new_empty: bool,
) -> (&'static str, &'static str) {
    match d {
        DiffOutcome::Equal if legacy_empty && new_empty => ("silent", ""),
        DiffOutcome::Equal => ("equal", ""),
        DiffOutcome::OrderOnly => ("order-only", ""),
        DiffOutcome::Allowed(id) => ("allowed", id),
        DiffOutcome::Different { .. } => ("different", ""),
    }
}

/// The legacy flat text of the node says nothing: its zone, if any, is the role-word stand-in.
fn legacy_flat_empty(s: &NodeSnapshot) -> bool {
    rg::legacy_flat(s).is_empty()
}

/// The AT-SPI role number of a toggle button.
const RAW_TOGGLE_BUTTON: u32 = 62;

/// The rows whose explanation is the fused role of the node.
const ROLE_ROWS: &[&str] = &[
    "role-from-model",
    "wrapper-word-dropped",
    "unworded-role-is-object",
    "static-text-word",
    "role-word-improved",
    "class-role-from-model",
    "layout-table-class",
];

/// A deviation row must not justify a regression with the model fact that causes it. Some rows
/// apply because of a fused fact (a role, a derived name, "availability does not apply", "this is
/// a toggle button"). When the source record (the connector's own normalisation of the same node,
/// which the legacy presenter was built on) contradicts that fact, the difference is a model
/// defect and not an intended improvement. Returns the contradictions, empty when the row rests
/// on evidence both sides share.
fn model_fact_witness(
    fired: &[&'static str],
    ctx: &DiffCtx<'_>,
    rec: Option<&SrcRecord>,
    entry: Option<&SemEntry>,
) -> String {
    let mut why: Vec<String> = Vec::new();
    for &id in fired {
        let w = match id {
            // Availability: the independent table of gorca-sem-atspi lists the roles a disabled
            // state is meaningful on. A role the source table disables and the fused entry calls
            // "not applicable" is the defect, not a deviation.
            "unavailable-static" => rec
                .filter(|r| gorca_sem_atspi::states::is_disableable(r.sem.role))
                .map(|r| {
                    format!(
                        "source role {:?} can be disabled (gorca-sem-atspi is_disableable), the row calls availability not applicable",
                        r.sem.role
                    )
                }),
            // The model says toggle button; the engine's role is not one.
            "toggle-state-word" => rec
                .filter(|r| ctx.toggle_button && r.raw_role != RAW_TOGGLE_BUTTON)
                .map(|r| {
                    format!(
                        "the fused entry says toggle button, the engine role {} is not one",
                        r.raw_role
                    )
                }),
            // The model says the tab is not a toggle; the engine says it is a toggle button.
            "pressed-without-toggle" => rec
                .filter(|r| ctx.untoggled && r.raw_role == RAW_TOGGLE_BUTTON)
                .map(|_| {
                    "the engine role is a toggle button, the fused entry has no pressed state"
                        .to_owned()
                }),
            // A derived name fills a gap; it never replaces a name the engine gave.
            "content-derived-name" => rec
                .and_then(|r| r.name.as_deref())
                .filter(|n| !gorca_present::name_is_blank(Some(n)))
                .map(|n| format!("the engine gave the name {n:?}, the fused name is derived")),
            // The role rows rest on the fused role: it must be the role the connector saw.
            id if ROLE_ROWS.contains(&id) => match (rec, entry) {
                (Some(r), Some(e)) if r.known.has(Group::Role) && r.sem.role != e.sem.role => {
                    Some(format!(
                        "the source record says {:?}, the fused entry {:?}",
                        r.sem.role, e.sem.role
                    ))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(w) = w {
            why.push(format!("{id}: {w}"));
        }
    }
    why.join("; ")
}

#[allow(clippy::too_many_arguments)]
fn compare(
    out: &mut Vec<Record>,
    run: &Run,
    dev: &Deviations,
    corpus: &str,
    s: &NodeSnapshot,
    case: &'static str,
    surface: &'static str,
    legacy: &[LegacyToken],
    new: &[Token],
) -> &'static str {
    let entry = s.sem.entry.as_ref();
    let mut dctx = match entry {
        Some(e) => DiffCtx::for_entry(surface, e, run.lang),
        None => DiffCtx {
            surface,
            lang: run.lang,
            ..DiffCtx::FOCUS
        },
    };
    dctx.legacy_coarse = matches!(s.role, Role::Unknown | Role::Panel | Role::Paragraph);
    dctx.legacy_fallback = case == "zone" && !legacy.is_empty() && legacy_flat_empty(s);
    let (d, fired) = diff_report(legacy, new, dev, dctx);
    let (outcome, deviation) = outcome_of(&d, legacy.is_empty(), new.is_empty());
    let witness = if outcome == "allowed" {
        model_fact_witness(&fired, &dctx, s.sem.rec.as_deref(), entry.map(|e| &**e))
    } else {
        String::new()
    };
    out.push(Record {
        corpus: corpus.to_owned(),
        node: s.key.local.0,
        case,
        surface,
        outcome,
        deviation,
        legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
        new: join(new.iter().map(|t| (t.kind, t.text.as_str()))),
        witness,
        note: format!(
            "legacy {:?} raw {} fused {:?} name-prov {:?}",
            s.role,
            s.sem.rec.as_ref().map_or(0, |r| r.raw_role),
            entry.map(|e| e.sem.role),
            entry.and_then(|e| e.prov.get(gorca_model::sem::Group::Name)),
        ),
    });
    outcome
}

/// The six object moves of simple review from every node, as one token per move.
fn walk_tokens(
    src: &rg::PageSource,
    k: NodeKey,
    class_fn: &dyn Fn(&NodeSnapshot) -> rg::PresClass,
) -> Vec<Token> {
    const MOVES: [(&str, rg::NavMove); 6] = [
        ("parent", rg::NavMove::Parent),
        ("child", rg::NavMove::FirstChild),
        ("next", rg::NavMove::Next),
        ("prev", rg::NavMove::Prev),
        ("flow-next", rg::NavMove::NextInFlow),
        ("flow-prev", rg::NavMove::PrevInFlow),
    ];
    MOVES
        .iter()
        .map(|(name, mv)| {
            let r = rg::step_with(src, k, *mv, true, rg::ReviewLimits::default(), class_fn);
            let to = match r {
                rg::StepResult::Found(n) => n.local.0.to_string(),
                rg::StepResult::Empty => "-".to_owned(),
                rg::StepResult::Stop(_) => "stop".to_owned(),
            };
            text_token(&format!("{name}={to}"))
        })
        .collect()
}

fn new_nav_class(s: &NodeSnapshot) -> rg::PresClass {
    rg::class_of_snapshot(s, false)
}

fn class_token(c: rg::PresClass) -> String {
    match c {
        rg::PresClass::Unavailable => "Unavailable",
        rg::PresClass::Layout => "Layout",
        rg::PresClass::Content => "Content",
    }
    .to_owned()
}

/// The page as a navigation source: every node carries its child list (page order).
fn page_source(fused: &[Arc<NodeSnapshot>]) -> rg::PageSource {
    let mut kids: HashMap<NodeKey, Vec<NodeKey>> = HashMap::new();
    for s in fused {
        if let Some(p) = s.parent {
            kids.entry(p).or_default().push(s.key);
        }
    }
    let mut map = HashMap::new();
    for s in fused {
        let mut c = (**s).clone();
        c.children = ChildList::Known(
            kids.remove(&s.key)
                .unwrap_or_default()
                .into_iter()
                .collect(),
        );
        c.have |= PropMask::CHILDREN | PropMask::PARENT;
        map.insert(s.key, Arc::new(c));
    }
    rg::PageSource(map)
}

fn review_records(family: &Family, run: &Run, dev: &Deviations, out: &mut Vec<Record>) {
    let web_run = family.web != WebEngine::None;
    let surface = if web_run { "review-web" } else { "review" };
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let (Some(snaps), Some((metas, unreadable))) =
            (page(family.dir, &name), page_meta(family.dir, &name))
        else {
            out.push(dropped(
                &label,
                "flat",
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
                "flat",
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
        let webs = web_flags(&fused);
        let src = page_source(&fused);
        let mut compared = 0u64;
        // Nodes whose class differs from the legacy one by an intended row: the walks that differ
        // must follow from those and nothing else.
        let mut class_rows: HashSet<NodeKey> = HashSet::new();
        let mut walked: Vec<&Arc<NodeSnapshot>> = Vec::new();
        for (s, web) in fused.iter().zip(&webs) {
            if *web != web_run {
                drops.add(kind_mismatch_reason(web_run), || describe(s));
                continue;
            }
            if s.sem.entry.is_none() {
                drops.add(no_entry_reason(s), || describe(s));
                continue;
            }
            compared += 1;
            walked.push(s);
            // flat: the text of the whole object.
            let legacy = legacy_tokens(&rg::legacy_flat(s));
            let new = rg::object_tokens(s, None);
            compare(out, run, dev, &label, s, "flat", surface, &legacy, &new);
            // zone: with the role word of a silent object that can be acted on.
            let legacy = legacy_tokens(&rg::legacy_zone(s));
            let new = rg::zone_tokens(s, web_run, None, rg::actionable(s));
            compare(out, run, dev, &label, s, "zone", surface, &legacy, &new);
            // noname: what an object without a name is called.
            if s.name.as_deref().is_none_or(|n| n.trim().is_empty()) {
                let w = rg::legacy_role_word(s);
                let legacy: Vec<LegacyToken> = if w.is_empty() {
                    Vec::new()
                } else {
                    legacy_tokens(&[(SegmentKind::Role, w.to_owned())])
                };
                let new = [Token {
                    kind: SegmentKind::Role,
                    text: TokenText::Static(rg::role_word(s, web_run)),
                }];
                compare(out, run, dev, &label, s, "noname", surface, &legacy, &new);
            }
            // class: object review's classification, and the surface walk's for web content.
            let legacy = [LegacyToken {
                kind: SegmentKind::Text,
                text: Arc::from(class_token(rg::class(s))),
            }];
            let new = [text_token(&class_token(rg::class_of_snapshot(s, false)))];
            if compare(out, run, dev, &label, s, "class", surface, &legacy, &new) == "allowed" {
                class_rows.insert(s.key);
            }
            if web_run {
                let legacy = [LegacyToken {
                    kind: SegmentKind::Text,
                    text: Arc::from(class_token(rg::class_in(s, true))),
                }];
                let new = [text_token(&class_token(rg::class_of_snapshot(s, true)))];
                compare(
                    out,
                    run,
                    dev,
                    &label,
                    s,
                    "class-web",
                    surface,
                    &legacy,
                    &new,
                );
            }
        }
        // walk: where the six object moves go from each node. A walk that differs is explained
        // only when replaying it with the new class on just the nodes whose class row applies
        // (the legacy class everywhere else) gives the new walk.
        let patched = |s: &NodeSnapshot| {
            if class_rows.contains(&s.key) {
                rg::class_of_snapshot(s, false)
            } else {
                rg::class(s)
            }
        };
        for s in walked {
            let legacy_walk = walk_tokens(&src, s.key, &rg::class);
            let new_walk = walk_tokens(&src, s.key, &new_nav_class);
            let legacy: Vec<LegacyToken> = legacy_walk
                .iter()
                .map(|t| LegacyToken {
                    kind: t.kind,
                    text: Arc::from(t.text.as_str()),
                })
                .collect();
            let same = |a: &[Token], b: &[Token]| {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(x, y)| x.text.as_str() == y.text.as_str())
            };
            if same(&new_walk, &walk_tokens(&src, s.key, &patched))
                && !same(&new_walk, &legacy_walk)
                && dev.allows("walk-follows-class", surface)
            {
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case: "walk",
                    surface,
                    outcome: "allowed",
                    deviation: "walk-follows-class",
                    legacy: join(legacy.iter().map(|t| (t.kind, &*t.text))),
                    new: join(new_walk.iter().map(|t| (t.kind, t.text.as_str()))),
                    witness: String::new(),
                    note: String::new(),
                });
            } else {
                compare(
                    out, run, dev, &label, s, "walk", surface, &legacy, &new_walk,
                );
            }
        }
        drops.flush(&label, "flat", surface, out);
        if compared == 0 {
            out.push(dropped(
                &label,
                "flat",
                surface,
                "empty-corpus",
                1,
                "no node of the page was compared".to_owned(),
            ));
        }
    }
}

/// Replay `review` over every family present in the fixture tree; records sorted for byte
/// comparison.
fn emit(seed: u64, lang: gorca_present::Lang) -> String {
    emit_with(seed, lang, false)
}

fn emit_with(seed: u64, lang: gorca_present::Lang, notes: bool) -> String {
    // The catalog language is `lang` on this thread whatever the process language is; render,
    // compose and the diff carry the same language.
    let _lang = gorca_i18n::scoped(lang);
    let dev = Deviations::builtin();
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        review_records(family, &run, &dev, &mut recs);
    }
    recs.sort_by(|a, b| {
        (
            &a.corpus,
            a.node,
            a.case,
            a.surface,
            a.outcome,
            a.deviation,
            &a.legacy,
            &a.new,
        )
            .cmp(&(
                &b.corpus,
                b.node,
                b.case,
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
        if notes {
            text.pop();
            let _ = writeln!(text, "\t{}", r.note);
        }
    }
    text
}

fn lang_of(s: &str) -> Option<gorca_present::Lang> {
    match s {
        "en" => Some(gorca_present::Lang::En),
        "ru" => Some(gorca_present::Lang::Ru),
        _ => None,
    }
}

/// The gate's replay entry (run by `cargo xtask cutover review`; a no-op otherwise).
#[test]
fn review_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    assert_eq!(surface, "review", "not a review surface");
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let lang_name = std::env::var("GORCA_CUTOVER_LANG").unwrap_or_else(|_| "en".to_owned());
    let lang = lang_of(&lang_name).expect("GORCA_CUTOVER_LANG is en or ru");
    std::fs::write(path, emit(seed, lang)).expect("write records");
}

/// Development view: the grouped differences (run with `--ignored --nocapture`).
#[test]
#[ignore = "development summary of the replay"]
fn review_summary() {
    let text = emit_with(0, gorca_present::Lang::En, true);
    if let Ok(dir) = std::env::var("GORCA_B3_DUMP") {
        std::fs::write(format!("{dir}/review.tsv"), &text).expect("dump");
    }
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        *by.entry(format!("{} {}", f[2], f[4])).or_default() += 1;
    }
    eprintln!("{by:#?}");
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        let a = emit(0, lang);
        assert!(!a.is_empty(), "empty replay");
        assert_eq!(a, emit(0, lang), "two runs differ");
        assert_eq!(a, emit(0x5eed, lang), "shuffled arrival changed the output");
    }
}

#[test]
fn every_family_has_recorded_pages_and_every_page_compares_something() {
    let text = emit(0, gorca_present::Lang::En);
    for family in FAMILIES {
        assert!(
            text.lines()
                .any(|l| l.starts_with(&format!("{}/", family.dir))
                    && l.split('\t').nth(4).is_none_or(|o| o != "dropped")),
            "family {} compared no node",
            family.dir
        );
    }
    assert!(
        !text
            .lines()
            .any(|l| l.split('\t').nth(5) == Some("empty-corpus")
                || l.split('\t').nth(5) == Some("page-unparsable")),
        "a recorded page is empty or unparsable"
    );
}

#[cfg(test)]
mod witness_tests {
    use super::*;
    use gorca_model::sem::{GroupMask, Semantics};

    fn rec(role: SemRole, raw_role: u32, name: Option<&str>) -> SrcRecord {
        SrcRecord {
            sem: Semantics {
                role,
                ..Semantics::default()
            },
            known: GroupMask::EMPTY.with(Group::Role),
            name: name.map(Arc::from),
            raw_role,
            ..SrcRecord::default()
        }
    }

    fn entry(role: SemRole) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e
    }

    fn ctx() -> DiffCtx<'static> {
        DiffCtx {
            surface: "review",
            ..DiffCtx::FOCUS
        }
    }

    #[test]
    fn a_disableable_source_role_the_model_calls_not_applicable_is_flagged() {
        let tree = rec(SemRole::Tree, 65, None);
        let w = model_fact_witness(&["unavailable-static"], &ctx(), Some(&tree), None);
        assert!(
            w.contains("unavailable-static"),
            "a tree can be disabled: {w}"
        );
        let doc = rec(SemRole::Document, 95, None);
        assert!(model_fact_witness(&["unavailable-static"], &ctx(), Some(&doc), None).is_empty());
    }

    #[test]
    fn toggle_facts_are_checked_against_the_engine_role() {
        let c = DiffCtx {
            toggle_button: true,
            ..ctx()
        };
        let plain = rec(SemRole::Button, 43, None);
        assert!(
            !model_fact_witness(&["toggle-state-word"], &c, Some(&plain), None).is_empty(),
            "the model says toggle button, the engine a push button"
        );
        let toggle = rec(SemRole::Button, RAW_TOGGLE_BUTTON, None);
        assert!(model_fact_witness(&["toggle-state-word"], &c, Some(&toggle), None).is_empty());

        let tab = DiffCtx {
            untoggled: true,
            ..ctx()
        };
        assert!(
            !model_fact_witness(&["pressed-without-toggle"], &tab, Some(&toggle), None).is_empty(),
            "the engine says toggle button, the model no pressed state"
        );
        assert!(
            model_fact_witness(&["pressed-without-toggle"], &tab, Some(&plain), None).is_empty()
        );
    }

    #[test]
    fn a_derived_name_over_an_engine_name_is_flagged() {
        let named = rec(SemRole::ListItem, 0, Some("Inbox"));
        assert!(
            !model_fact_witness(&["content-derived-name"], &ctx(), Some(&named), None).is_empty()
        );
        let blank = rec(SemRole::ListItem, 0, Some("  "));
        assert!(
            model_fact_witness(&["content-derived-name"], &ctx(), Some(&blank), None).is_empty()
        );
        let none = rec(SemRole::ListItem, 0, None);
        assert!(
            model_fact_witness(&["content-derived-name"], &ctx(), Some(&none), None).is_empty()
        );
    }

    #[test]
    fn a_role_row_needs_the_fused_role_to_be_the_one_the_connector_saw() {
        let r = rec(SemRole::Article, 0, None);
        for id in ROLE_ROWS {
            assert!(
                model_fact_witness(&[id], &ctx(), Some(&r), Some(&entry(SemRole::Article)))
                    .is_empty(),
                "{id}: the same role"
            );
            assert!(
                !model_fact_witness(&[id], &ctx(), Some(&r), Some(&entry(SemRole::Region)))
                    .is_empty(),
                "{id}: fusion changed the role"
            );
        }
        // A rest that is no role row is not asked.
        assert!(
            model_fact_witness(
                &["name-whitespace"],
                &ctx(),
                Some(&r),
                Some(&entry(SemRole::Region))
            )
            .is_empty()
        );
    }
}
