//! Research scenario for ADR-0003 TBD-WEB-1/2/3, run against Firefox and Chromium through the same
//! source:
//!
//! * TBD-WEB-1: which states flip on scroll/reorder, and whether an event announces the flip.
//!   A full-state snapshot (`GetState` of every node) before and after each step is diffed for
//!   nodes that exist in both; the object events seen during the step are counted by kind and
//!   state name, and every `showing`/`visible` flip is checked for a matching `StateChanged`.
//! * TBD-WEB-2: the raw attribute names and values of list items, rows and cells (is
//!   `posinset` / `setsize` / `rowindex` / `colindex` ... exposed?).
//! * TBD-WEB-3: identity of the rows that stay visible across a scroll step, for DOM
//!   disciplines that create/remove rows and that recycle them. Rows carry `aria-label="Row i"`
//!   as ground truth (the page rewrites `id` and the label together when it recycles).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gorca_doc_source_atspi::source::UpdateRecord;
use gorca_doc_source_atspi::wire::{AttrInterner, PathKey};
use gorca_doc_source_atspi::{
    EventKind, FetchOpts, FetchedDoc, PathTable, StatePolicy, fetch_document,
};
use gorca_model::{Role, StateSet};

use crate::arm::{AtspiTunedArm, B6Arm};
use crate::corpus::Entry;
use crate::oracle::first_diff;
use crate::report::{J, obj, s, u};
use crate::scenarios::{Ctx, fresh_signature};

#[derive(Clone, Debug)]
pub struct RStep {
    pub label: String,
    pub op: &'static str,
    pub n: i32,
}

fn rstep(label: &str, op: &'static str, n: i32) -> RStep {
    RStep {
        label: label.to_owned(),
        op,
        n,
    }
}

pub fn steps(entry: &Entry) -> Vec<RStep> {
    if entry.name.starts_with("virtual-") || entry.name.starts_with("virt-") {
        vec![
            rstep("scrollBy +64 px (2 rows)", "scrollBy", 64),
            rstep("scrollBy +640 px (20 rows)", "scrollBy", 640),
            rstep("scrollTo row 500", "scrollTo", 500),
            rstep("scrollBy -3200 px (back 100 rows)", "scrollBy", -3200),
            rstep("scrollTo row 5000 (jump)", "scrollTo", 5000),
            rstep("scrollTo row 0 (jump back)", "scrollTo", 0),
        ]
    } else if entry.name.starts_with("spa-list") {
        vec![
            rstep("reverse (keyed DOM moves)", "reverse", 0),
            rstep("shuffle", "shuffle", 0),
            rstep("window scroll 400 px", "scrollY", 400),
            rstep("window scroll back", "scrollY", 0),
        ]
    } else {
        vec![
            rstep("window scroll 20000 px", "scrollY", 20_000),
            rstep("window scroll 200000 px", "scrollY", 200_000),
            rstep("window scroll back", "scrollY", 0),
        ]
    }
}

struct Snap {
    nodes: HashMap<PathKey, (Role, StateSet, Option<Arc<str>>)>,
    /// accessible name -> path, for the labelled rows of the virtual pages
    rows: HashMap<Arc<str>, PathKey>,
}

async fn snapshot(ctx: &Ctx, interner: &Arc<AttrInterner>) -> Option<(Snap, FetchedDoc)> {
    let mut paths = PathTable::default();
    let o = FetchOpts {
        states: StatePolicy::All,
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    };
    let d = fetch_document(
        &ctx.target.app,
        interner,
        &mut paths,
        &ctx.target.doc_path,
        &o,
    )
    .await
    .ok()?;
    let mut nodes = HashMap::new();
    let mut rows = HashMap::new();
    for (i, nd) in d.block.nodes.iter().enumerate() {
        nodes.insert(d.keys[i], (nd.role, nd.states, nd.name.clone()));
        if let Some(nm) = &nd.name
            && nm.starts_with("Row ")
        {
            rows.insert(Arc::clone(nm), d.keys[i]);
        }
    }
    Some((Snap { nodes, rows }, d))
}

fn state_names(st: StateSet) -> Vec<String> {
    st.iter_names().map(|(n, _)| n.to_lowercase()).collect()
}

fn names_json(v: Vec<String>) -> J {
    J::A(v.into_iter().map(J::S).collect())
}

fn sorted_counts(m: HashMap<String, usize>) -> J {
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort();
    J::O(v.into_iter().map(|(k, c)| (k, u(c))).collect())
}

/// Attribute names and sample values of a few nodes per interesting role.
fn attr_samples(d: &FetchedDoc, interner: &AttrInterner, k: usize) -> J {
    use gorca_doc_source_atspi::roles::raw;
    let mut out = Vec::new();
    for (label, want) in [
        ("list", raw::LIST),
        ("listitem", raw::LIST_ITEM),
        ("table", raw::TABLE),
        ("row", raw::TABLE_ROW),
        ("cell", raw::TABLE_CELL),
    ] {
        let mut shown = 0;
        for (i, nd) in d.block.nodes.iter().enumerate() {
            if d.raw_roles[i] != want {
                continue;
            }
            let attrs: Vec<(String, J)> = nd
                .attrs
                .iter()
                .map(|(key, v)| {
                    let name = gorca_doc_source_api::web_attrs::name_of(*key)
                        .map(str::to_owned)
                        .or_else(|| interner.name_of(*key))
                        .unwrap_or_else(|| format!("key{}", key.0));
                    (name, s(v.to_string()))
                })
                .collect();
            out.push(obj([
                ("role", s(label)),
                ("name", nd.name.as_deref().map_or(J::Null, s)),
                ("attrs", J::O(attrs)),
            ]));
            shown += 1;
            if shown >= k {
                break;
            }
        }
    }
    J::A(out)
}

#[allow(clippy::too_many_lines)]
pub async fn run(ctx: &Ctx, browser: &str) -> Result<J> {
    let interner = Arc::new(AttrInterner::default());
    let arm = AtspiTunedArm::default();
    let session = arm
        .attach(&ctx.target)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    session
        .settle(Duration::from_millis(500), Duration::from_secs(60))
        .await;
    let (snap0, doc0) = snapshot(ctx, &interner)
        .await
        .ok_or_else(|| anyhow::anyhow!("initial snapshot failed"))?;
    let attrs = attr_samples(&doc0, &interner, 3);
    let mut rows = Vec::new();
    let mut prev = snap0;
    for st in steps(&ctx.entry) {
        let log_before = session.event_log.lock().map_or(0, |l| l.len());
        let upd_before = session.metrics.updates().len();
        let names_before = session
            .oracle
            .lock()
            .map(|o| o.names_with_prefix("Row "))
            .unwrap_or_default();
        let args = if st.n != 0 {
            format!(",\"n\":{}", st.n)
        } else {
            String::new()
        };
        let ack = match ctx
            .server
            .command(st.op, &args, Duration::from_secs(30))
            .await
        {
            Ok(a) => a,
            Err(e) => {
                rows.push(obj([("step", s(&st.label)), ("error", s(e.to_string()))]));
                continue;
            }
        };
        let t_wait = Instant::now();
        loop {
            let ups = session.metrics.updates();
            let quiet = ups
                .last()
                .is_some_and(|l| l.pushed_at.elapsed() > Duration::from_millis(800));
            let evs = session.event_log.lock().map_or(0, |l| l.len());
            if (ups.len() > upd_before && quiet)
                || (evs == log_before && t_wait.elapsed() > Duration::from_secs(3))
            {
                break;
            }
            if t_wait.elapsed() > Duration::from_secs(30) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let events: Vec<_> = session
            .event_log
            .lock()
            .map(|l| l[log_before..].to_vec())
            .unwrap_or_default();
        let names_after = session
            .oracle
            .lock()
            .map(|o| o.names_with_prefix("Row "))
            .unwrap_or_default();
        let Some((snap, _)) = snapshot(ctx, &interner).await else {
            rows.push(obj([
                ("step", s(&st.label)),
                ("error", s("snapshot failed")),
            ]));
            continue;
        };

        // TBD-WEB-1.
        let mut flipped = 0usize;
        let mut persisted = 0usize;
        let mut bit_counts: HashMap<String, usize> = HashMap::new();
        let mut flipped_paths: HashMap<PathKey, StateSet> = HashMap::new();
        for (p, (_, st_after, _)) in &snap.nodes {
            if let Some((_, st_before, _)) = prev.nodes.get(p) {
                persisted += 1;
                let diff = *st_before ^ *st_after;
                if !diff.is_empty() {
                    flipped += 1;
                    flipped_paths.insert(*p, diff);
                    for (nm, _) in diff.iter_names() {
                        *bit_counts.entry(nm.to_lowercase()).or_default() += 1;
                    }
                }
            }
        }
        let mut ev_counts: HashMap<String, usize> = HashMap::new();
        let mut state_ev_paths: HashMap<PathKey, Vec<String>> = HashMap::new();
        for e in &events {
            let key = match &e.kind {
                EventKind::ChildAdded { .. } => "children-added".to_owned(),
                EventKind::ChildRemoved { .. } => "children-removed".to_owned(),
                EventKind::NameChanged => "name-changed".to_owned(),
                EventKind::DescriptionChanged => "description-changed".to_owned(),
                EventKind::ValueChanged => "value-changed".to_owned(),
                EventKind::TextChanged { insert } => {
                    format!("text-{}", if *insert { "insert" } else { "delete" })
                }
                EventKind::Defunct => "defunct".to_owned(),
                EventKind::StateChanged { state, on } => {
                    state_ev_paths
                        .entry(e.path)
                        .or_default()
                        .push(format!("{state}:{}", u8::from(*on)));
                    format!("state:{state}:{}", if *on { "on" } else { "off" })
                }
            };
            *ev_counts.entry(key).or_default() += 1;
        }
        let sv = StateSet::SHOWING | StateSet::VISIBLE;
        let sv_flipped: Vec<&PathKey> = flipped_paths
            .iter()
            .filter(|(_, d)| d.intersects(sv))
            .map(|(p, _)| p)
            .collect();
        let covered = sv_flipped
            .iter()
            .filter(|p| {
                state_ev_paths.get(**p).is_some_and(|v| {
                    v.iter()
                        .any(|x| x.starts_with("showing") || x.starts_with("visible"))
                })
            })
            .count();

        // TBD-WEB-3.
        let mut retained = 0usize;
        let mut path_same = 0usize;
        let mut id_stable = 0usize;
        let mut id_lost = 0usize;
        for (nm, pb) in &prev.rows {
            if let Some(pa) = snap.rows.get(nm) {
                retained += 1;
                if pb == pa {
                    path_same += 1;
                }
                match (names_before.get(nm), names_after.get(nm)) {
                    (Some(a), Some(b)) if a == b => id_stable += 1,
                    _ => id_lost += 1,
                }
            }
        }
        let inv_after: HashMap<_, _> = names_after
            .iter()
            .map(|(n2, i)| (*i, Arc::clone(n2)))
            .collect();
        // A logical id whose content changed: "wrong node" when the old content still exists
        // elsewhere (the id should have followed it), "positional" when the old content is gone and
        // the id was handed to what took its slot (rung IL-3b, a heuristic join).
        let mut wrong = 0usize;
        let mut positional = 0usize;
        for (nm, id) in &names_before {
            if inv_after.get(id).is_some_and(|n2| n2 != nm) {
                if names_after.contains_key(nm) {
                    wrong += 1;
                } else {
                    positional += 1;
                }
            }
        }
        let ups: Vec<UpdateRecord> = session
            .metrics
            .updates()
            .into_iter()
            .skip(upd_before)
            .collect();
        let mirror_sig = session
            .oracle
            .lock()
            .map(|o| o.signature())
            .unwrap_or_default();
        let fresh_sig = fresh_signature(ctx).await.unwrap_or_default();

        rows.push(obj([
            ("step", s(&st.label)),
            ("page_info", s(&ack.info)),
            ("rows_before", u(prev.rows.len())),
            ("rows_after", u(snap.rows.len())),
            ("rows_retained", u(retained)),
            ("retained_same_path", u(path_same)),
            ("retained_path_changed", u(retained - path_same)),
            ("retained_id_stable", u(id_stable)),
            ("retained_id_lost", u(id_lost)),
            ("wrong_node", u(wrong)),
            ("positional_reassigned", u(positional)),
            ("nodes_persisting", u(persisted)),
            ("nodes_state_flipped", u(flipped)),
            ("showing_or_visible_flipped", u(sv_flipped.len())),
            ("showing_or_visible_flips_with_event", u(covered)),
            ("flipped_bits", sorted_counts(bit_counts)),
            ("events", sorted_counts(ev_counts)),
            ("source_updates", u(ups.len())),
            ("rewrite_detected", J::B(ups.iter().any(|x| x.rewrite))),
            ("resync", J::B(ups.iter().any(|x| x.resync))),
            (
                "consistent",
                J::B(first_diff(&mirror_sig, &fresh_sig).is_none()),
            ),
            ("mirror_nodes", u(mirror_sig.len())),
            ("fresh_nodes", u(fresh_sig.len())),
            (
                "example_state_flip",
                flipped_paths.iter().next().map_or(J::Null, |(p, d)| {
                    let before = prev
                        .nodes
                        .get(p)
                        .map(|x| state_names(x.1))
                        .unwrap_or_default();
                    let after = snap
                        .nodes
                        .get(p)
                        .map(|x| state_names(x.1))
                        .unwrap_or_default();
                    obj([
                        ("diff", names_json(state_names(*d))),
                        ("before", names_json(before)),
                        ("after", names_json(after)),
                    ])
                }),
            ),
        ]));
        prev = snap;
    }
    session.stop().await;
    Ok(obj([
        ("page", s(&ctx.entry.name)),
        ("browser", s(browser)),
        ("scenario", s("research")),
        (
            "metric",
            s(
                "TBD-WEB-1 state flips, TBD-WEB-2 position attributes, TBD-WEB-3 identity under scroll",
            ),
        ),
        ("attribute_samples", attrs),
        ("steps", J::A(rows)),
    ]))
}
