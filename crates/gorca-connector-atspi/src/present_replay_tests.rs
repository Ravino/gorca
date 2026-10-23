// SPDX-License-Identifier: GPL-3.0-or-later
//! M22b1 replay harness: the recorded native corpus (`tests/fixtures/sem`) is fused the way the
//! app actor does, every node is presented by the legacy presenter and by `gorca-present`, and the
//! token multisets are diffed (ADR-0022 section 2.11, M-13). Offline, deterministic, no bus.
//!
//! Web pages are skipped (the web focus rows are slice 3). The table printed with `--nocapture`
//! groups the differences by (legacy tokens, new tokens) so a slice can work through it; the test
//! asserts a **ceiling** on the differences outside `deviations.toml`, which only goes down.

use std::collections::BTreeMap;
use std::sync::Arc;

use std::time::Instant;

use gorca_model::{
    AppId, ConnectorId, Epoch, LocalId, NodeKey, NodeSnapshot, PropMask, UttId, WebEngine,
};

use crate::decode::{RawNode, snapshot};
use gorca_present::{
    ComposeCtx, Deviations, DiffCtx, DiffOutcome, LegacyToken, Reason, RenderOpts, ReportFlags,
    compose, diff, render,
};

use crate::legacy_agreement_tests::{page, pages_of, root};

/// The outcome of one replay.
#[derive(Debug, Default)]
pub(crate) struct Replay {
    pub nodes: u64,
    pub skipped_web: u64,
    pub skipped_no_entry: u64,
    pub equal: u64,
    pub order_only: u64,
    pub allowed: u64,
    pub different: u64,
    /// (legacy tokens, new tokens) of each differing node, with a count and one example.
    pub table: BTreeMap<String, (u64, String)>,
    /// Allowed differences by deviation row id.
    pub allowed_by: BTreeMap<&'static str, u64>,
}

fn show<'a>(it: impl Iterator<Item = (gorca_model::SegmentKind, &'a str)>) -> String {
    // Names and values are page/app content: group on their kind, keep words (role, state).
    it.map(|(k, t)| match k {
        gorca_model::SegmentKind::Text | gorca_model::SegmentKind::Value => format!("{k:?}:*"),
        _ => format!("{k:?}:{t}"),
    })
    .collect::<Vec<_>>()
    .join(" | ")
}

/// Replay `snaps` (already fused) through both presenters.
pub(crate) fn replay(
    label: &str,
    snaps: &[Arc<NodeSnapshot>],
    dev: &Deviations,
    web_run: bool,
    out: &mut Replay,
) {
    for s in snaps {
        out.nodes += 1;
        let is_web = s
            .attrs
            .iter()
            .any(|(k, _)| *k == gorca_doc_source_api::web_attrs::TAG);
        if is_web != web_run {
            out.skipped_web += 1;
            continue;
        }
        let Some(entry) = s.sem.entry.as_ref() else {
            out.skipped_no_entry += 1;
            continue;
        };
        let u = gorca_core::present_focus(s, UttId(1), Epoch(1));
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
            web: is_web,
            skip_selected: false,
        };
        let Ok(ir) = compose(entry, s.key, Reason::Focus, ctx) else {
            continue;
        };
        let tokens = render(
            &ir,
            RenderOpts {
                description_pause: web_run,
            },
        );
        let ctx = DiffCtx::for_entry(if web_run { "focus-web" } else { "focus" }, entry);
        match diff(&legacy, &tokens, dev, ctx) {
            DiffOutcome::Equal => out.equal += 1,
            DiffOutcome::OrderOnly => out.order_only += 1,
            DiffOutcome::Allowed(id) => {
                out.allowed += 1;
                *out.allowed_by.entry(id).or_default() += 1;
            }
            DiffOutcome::Different { .. } => {
                out.different += 1;
                let sig = format!(
                    "legacy [{}] new [{}]",
                    show(legacy.iter().map(|t| (t.kind, &*t.text))),
                    show(tokens.iter().map(|t| (t.kind, t.text.as_str())))
                );
                let exact = format!(
                    "{label}#{} sem={:?} legacy={:?} new={:?}",
                    s.key.local.0,
                    entry.sem.role,
                    legacy.iter().map(|t| &*t.text).collect::<Vec<_>>(),
                    tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>()
                );
                let e = out.table.entry(sig).or_insert_with(|| (0, exact));
                e.0 += 1;
            }
        }
    }
}

/// The native part of the corpus: GTK pages (the web engines' pages are web content).
pub(crate) fn run_native_corpus(dev: &Deviations) -> Replay {
    let mut out = Replay::default();
    let tables = crate::sem_profile::tables_for("GTK", "app", WebEngine::None);
    for name in pages_of("gtk4") {
        let Some(snaps) = page("gtk4", &name) else {
            continue;
        };
        let fused = gorca_core::fuse_snapshots(tables, &snaps);
        replay(&format!("gtk4/{name}"), &fused, dev, false, &mut out);
    }
    out
}

/// A synthetic native slider with a numeric AT-SPI value: the legacy presenter speaks the value
/// from the snapshot, the fused path from `value_text` (set by the connector), so they agree.
#[test]
fn a_native_range_value_replays_equal() {
    use crate::decode::RawValue;
    let tables = crate::sem_profile::tables_for("GTK", "app", WebEngine::None);
    let slider = RawNode {
        role: 51, // ATSPI_ROLE_SLIDER
        name: Some("Volume".into()),
        value: Some(RawValue::Number {
            current: 30.0,
            min: 0.0,
            max: 100.0,
        }),
        have: PropMask::ROLE | PropMask::NAME | PropMask::VALUE,
        ..RawNode::default()
    };
    let snaps = vec![snapshot(
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(1),
        },
        &slider,
        Instant::now(),
    )];
    let fused = gorca_core::fuse_snapshots(tables, &snaps);
    let mut out = Replay::default();
    replay(
        "synthetic/slider",
        &fused,
        &Deviations::builtin(),
        false,
        &mut out,
    );
    assert_eq!(
        (out.equal, out.different, out.allowed),
        (1, 0, 0),
        "{:?}",
        out.table
    );
}

/// The web part of the corpus: Firefox and Chromium pages, web nodes only.
pub(crate) fn run_web_corpus(dev: &Deviations) -> Replay {
    let mut out = Replay::default();
    for (engine, web) in [
        ("firefox", WebEngine::Gecko),
        ("chromium", WebEngine::Blink),
    ] {
        let tables = crate::sem_profile::tables_for("", "app", web);
        for name in pages_of(engine) {
            let Some(snaps) = page(engine, &name) else {
                continue;
            };
            let fused = gorca_core::fuse_snapshots(tables, &snaps);
            replay(&format!("{engine}/{name}"), &fused, dev, true, &mut out);
        }
    }
    out
}

fn print_table(title: &str, r: &Replay) {
    let compared = r.equal + r.order_only + r.allowed + r.different;
    eprintln!(
        "present replay ({title}): nodes {} compared {compared} equal {} order-only {} allowed {} \
         different {} skipped {} no-entry {}",
        r.nodes, r.equal, r.order_only, r.allowed, r.different, r.skipped_web, r.skipped_no_entry
    );
    eprintln!("  allowed by row: {:?}", r.allowed_by);
    let mut rows: Vec<_> = r.table.iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (sig, (n, ex)) in rows.iter().take(40) {
        eprintln!("  {n:>4}x {sig}   e.g. {ex}");
    }
}

/// Differences outside `deviations.toml` the web replay shows: none (was 17 of 2239).
const WEB_DIFFERENT_CEILING: u64 = 0;

#[test]
fn the_web_corpus_replays_through_both_presenters() {
    let r = run_web_corpus(&Deviations::builtin());
    print_table("web", &r);
    let compared = r.equal + r.order_only + r.allowed + r.different;
    assert!(
        compared > 200,
        "the web corpus was not replayed ({compared})"
    );
    assert!(
        r.different == WEB_DIFFERENT_CEILING,
        "{} differences",
        r.different
    );
    // A row nothing in the corpus needs is dead weight (the dated list only shrinks): every
    // `focus-web` row must have a witness here.
    let unwitnessed: Vec<_> = Deviations::builtin()
        .ids_for("focus-web")
        .into_iter()
        .filter(|id| !r.allowed_by.contains_key(id.as_str()))
        .collect();
    assert!(
        unwitnessed.is_empty(),
        "rows with no witness: {unwitnessed:?}"
    );
}

/// Differences outside `deviations.toml` the native replay is allowed to show today. Slice 2 drives
/// it to zero; raising it needs a reason in the commit. Slice 2 reached 0 (was 169 of 375 GTK4 nodes);
/// 11 nodes are explained by `deviations.toml` rows.
const NATIVE_DIFFERENT_CEILING: u64 = 0;

#[test]
fn the_native_corpus_replays_through_both_presenters() {
    assert!(root().exists(), "fixtures missing");
    let dev = Deviations::builtin();
    let r = run_native_corpus(&dev);
    let compared = r.equal + r.order_only + r.allowed + r.different;
    eprintln!(
        "present replay (gtk4): nodes {} compared {compared} equal {} order-only {} allowed {} \
         different {} skipped web {} no-entry {}",
        r.nodes, r.equal, r.order_only, r.allowed, r.different, r.skipped_web, r.skipped_no_entry
    );
    let mut rows: Vec<_> = r.table.iter().collect();
    rows.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (sig, (n, ex)) in rows.iter().take(25) {
        eprintln!("  {n:>4}x {sig}   e.g. {ex}");
    }
    assert!(compared > 50, "the corpus was not replayed ({compared})");
    assert!(
        r.different == NATIVE_DIFFERENT_CEILING,
        "{} differences outside deviations.toml",
        r.different
    );
}
