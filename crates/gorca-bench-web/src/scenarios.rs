// SPDX-License-Identifier: GPL-3.0-or-later
//! B-6 scenarios. Each returns a JSON value for `bench/results/web-prep/`.
//!
//! * `build`: M-9 / WM-1, full-tree build time, stage by stage, over a matrix of strategies.
//! * `churn`: WM-4 / M-13, mutation -> mirror latency and identity stability under SPA churn,
//!   with a consistency check of the mirror against a fresh fetch after every step.
//! * `scale`: M-15, decode scaling of `GetTree` replies over 1..N threads, and fetch scaling
//!   over the number of P2P connections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use gorca_doc_source_atspi::fetch::{get_tree_raw, tree_args};
use gorca_doc_source_atspi::source::{SourceConfig, UpdateRecord};
use gorca_doc_source_atspi::wire::{self, AttrInterner};
use gorca_doc_source_atspi::{
    AppConn, FetchOpts, PathTable, Split, StatePolicy, TextPolicy, fetch_document,
};
use gorca_rt::Deadline;

use crate::arm::{AtspiTunedArm, B6Arm, Target};
use crate::corpus::Entry;
use crate::discover;
use crate::oracle::{Signature, first_diff};
use crate::report::{J, Summary, ms, n, obj, s, summarize, u};
use crate::server::PageServer;

pub struct Ctx {
    pub target: Target,
    pub server: Arc<PageServer>,
    pub entry: Entry,
    pub runs: usize,
    pub out_dir: PathBuf,
    pub anchor_instant: Instant,
    pub anchor_epoch_ms: f64,
}

impl Ctx {
    pub fn new(
        target: Target,
        server: Arc<PageServer>,
        entry: Entry,
        runs: usize,
        out_dir: PathBuf,
    ) -> Self {
        let anchor_instant = Instant::now();
        let anchor_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        Self {
            target,
            server,
            entry,
            runs,
            out_dir,
            anchor_instant,
            anchor_epoch_ms,
        }
    }

    fn epoch_ms(&self, t: Instant) -> f64 {
        if t >= self.anchor_instant {
            self.anchor_epoch_ms + ms(t - self.anchor_instant)
        } else {
            self.anchor_epoch_ms - ms(self.anchor_instant - t)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// build (M-9)

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: &'static str,
    pub conns: usize,
    pub opts: FetchOpts,
}

fn opts(split: Split, window: usize, states: StatePolicy, text: TextPolicy) -> FetchOpts {
    FetchOpts {
        window,
        split,
        states,
        text,
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    }
}

pub fn frontier() -> Split {
    Split::Frontier {
        min_subtrees: 16,
        max_depth: 4,
    }
}

/// The strategy matrix. The first entry is the shipped default.
pub fn variants(full: bool) -> Vec<Variant> {
    let v = |name, conns, o| Variant {
        name,
        conns,
        opts: o,
    };
    let mut out = vec![
        v(
            "default frontier c4 w32 stateful",
            4,
            opts(frontier(), 32, StatePolicy::Stateful, TextPolicy::None),
        ),
        v(
            "whole c4 w32 stateful",
            4,
            opts(Split::Whole, 32, StatePolicy::Stateful, TextPolicy::None),
        ),
        v(
            "frontier c4 w32 states-all",
            4,
            opts(frontier(), 32, StatePolicy::All, TextPolicy::None),
        ),
        v(
            "frontier c4 w32 no-states",
            4,
            opts(frontier(), 32, StatePolicy::None, TextPolicy::None),
        ),
        v(
            "frontier c4 w32 stateful +text",
            4,
            opts(frontier(), 32, StatePolicy::Stateful, TextPolicy::All),
        ),
    ];
    if full {
        for c in [1, 2, 8] {
            out.push(v(
                match c {
                    1 => "frontier c1 w32 stateful",
                    2 => "frontier c2 w32 stateful",
                    _ => "frontier c8 w32 stateful",
                },
                c,
                opts(frontier(), 32, StatePolicy::Stateful, TextPolicy::None),
            ));
        }
        for w in [8, 16, 64, 128] {
            out.push(v(
                match w {
                    8 => "frontier c4 w8 stateful",
                    16 => "frontier c4 w16 stateful",
                    64 => "frontier c4 w64 stateful",
                    _ => "frontier c4 w128 stateful",
                },
                4,
                opts(frontier(), w, StatePolicy::Stateful, TextPolicy::None),
            ));
        }
    }
    out
}

/// First block on a fresh connection to a browser that has not been asked for attributes yet: the
/// worst case, with the cache-domain warm-up (WF-3) inside the measurement. One sample per launch.
async fn cold_first_block(ctx: &Ctx) -> J {
    let Ok(app) = discover::open_app(&ctx.target.bus, &ctx.target.app_bus_name, 4).await else {
        return J::Null;
    };
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let o = FetchOpts {
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    };
    let t = Instant::now();
    match gorca_doc_source_atspi::fetch::fetch_first_block(
        &app,
        &interner,
        &mut paths,
        &ctx.target.doc_path,
        &o,
        400,
    )
    .await
    {
        Ok((d, plan, consumed)) => obj([
            ("ms", n(ms(t.elapsed()))),
            ("warm_ms", n(ms(d.timings.warm))),
            ("frontier_ms", n(ms(d.timings.frontier))),
            ("get_tree_ms", n(ms(d.timings.get_tree))),
            ("block_nodes", u(d.block.node_count())),
            (
                "subtrees_left",
                u(plan.leaves.len().saturating_sub(consumed)),
            ),
        ]),
        Err(e) => s(e.to_string()),
    }
}

pub async fn build(ctx: &Ctx, vs: &[Variant]) -> Result<J> {
    let cold = cold_first_block(ctx).await;
    let mut rows = Vec::new();
    for var in vs {
        let app = discover::open_app(&ctx.target.bus, &ctx.target.app_bus_name, var.conns).await?;
        let p2p = app.is_p2p();
        // Warm outside the timed runs (activation is process-global; a fresh connection only
        // re-confirms it).
        let warm_interner = Arc::new(AttrInterner::default());
        let warm = gorca_doc_source_atspi::warm::warm_domains(
            &app,
            &warm_interner,
            &ctx.target.doc_path,
            Duration::from_secs(5),
        )
        .await;
        let t = Target {
            app,
            ..ctx.target.clone()
        };
        let arm = AtspiTunedArm {
            fetch: var.opts.clone(),
            ..AtspiTunedArm::default()
        };
        let mut total = Vec::new();
        let mut cols: HashMap<&str, Vec<f64>> = HashMap::new();
        let mut nodes = 0usize;
        let mut heaviest_nodes = 0u64;
        let mut incomplete = 0usize;
        let mut calls = 0u64;
        let mut failed = 0u64;
        let mut bytes = 0u64;
        let mut errors = Vec::new();
        for _ in 0..ctx.runs {
            match arm.build(&t).await {
                Ok(r) => {
                    let tm = &r.timings;
                    total.push(ms(tm.total));
                    for (k, d) in [
                        ("frontier", tm.frontier),
                        ("get_tree", tm.get_tree),
                        ("roles", tm.roles),
                        ("states", tm.states),
                        ("text", tm.text),
                        ("assemble", tm.assemble),
                    ] {
                        cols.entry(k).or_default().push(ms(d));
                    }
                    // Time until structure + roles are available (index-building can start).
                    cols.entry("t_roles_ready")
                        .or_default()
                        .push(ms(tm.frontier + tm.get_tree + tm.assemble + tm.roles));
                    // Time until the first structure arrived (heading/landmark names from attrs).
                    cols.entry("t_structure_ready")
                        .or_default()
                        .push(ms(tm.frontier + tm.get_tree));
                    cols.entry("heaviest_get_tree_call")
                        .or_default()
                        .push(ms(tm.heaviest_get_tree));
                    heaviest_nodes = heaviest_nodes.max(tm.heaviest_get_tree_nodes);
                    nodes = r.nodes;
                    calls = tm.calls;
                    failed += tm.failed_calls;
                    bytes = tm.tree_bytes;
                    incomplete += usize::from(r.incomplete);
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        let mut stages: Vec<(String, J)> = Vec::new();
        let mut keys: Vec<&&str> = cols.keys().collect();
        keys.sort();
        for k in keys {
            stages.push(((*k).to_owned(), Summary::to_json(&summarize(&cols[*k]))));
        }
        rows.push(obj([
            ("variant", s(var.name)),
            ("p2p", J::B(p2p)),
            ("warm_ready", J::B(warm.ready)),
            ("warm_ms", n(ms(warm.waited))),
            ("connections", u(var.conns)),
            ("window", u(var.opts.window)),
            ("nodes", u(nodes)),
            ("runs_ok", u(total.len())),
            ("incomplete_runs", u(incomplete)),
            ("rpc_calls", n(calls as f64)),
            ("failed_calls", n(failed as f64)),
            ("get_tree_bytes", n(bytes as f64)),
            ("heaviest_get_tree_reply_nodes", n(heaviest_nodes as f64)),
            ("total_ms", summarize(&total).to_json()),
            ("stages_ms", J::O(stages)),
            ("errors", J::A(errors.into_iter().map(J::S).collect())),
        ]));
    }
    // Viewport-first: time until the top of the document (interior + first subtrees, with roles
    // and states) is complete, i.e. when the first line is speakable.
    let first = first_block_stats(ctx).await;
    Ok(obj([
        ("page", s(&ctx.entry.name)),
        ("scenario", s("build")),
        ("metric", s("M-9 / WM-1 full-tree build time")),
        ("first_block", first),
        ("first_block_cold", cold),
        ("variants", J::A(rows)),
    ]))
}

async fn first_block_stats(ctx: &Ctx) -> J {
    let mut ms_v = Vec::new();
    let mut nodes = 0usize;
    let mut leaves = 0usize;
    let mut errors = Vec::new();
    let o = FetchOpts {
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    };
    for _ in 0..ctx.runs {
        let interner = Arc::new(AttrInterner::default());
        let mut paths = PathTable::default();
        let t = Instant::now();
        match gorca_doc_source_atspi::fetch::fetch_first_block(
            &ctx.target.app,
            &interner,
            &mut paths,
            &ctx.target.doc_path,
            &o,
            400,
        )
        .await
        {
            Ok((d, plan, consumed)) => {
                ms_v.push(ms(t.elapsed()));
                nodes = d.block.node_count();
                leaves = plan.leaves.len().saturating_sub(consumed);
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    obj([
        ("first_nodes_target", u(400)),
        ("block_nodes", u(nodes)),
        ("subtrees_left", u(leaves)),
        ("ms", summarize(&ms_v).to_json()),
        ("errors", J::A(errors.into_iter().map(J::S).collect())),
    ])
}

// ---------------------------------------------------------------------------------------------
// churn (WM-4, M-13)

#[derive(Clone, Debug)]
pub struct Step {
    pub label: String,
    pub op: &'static str,
    pub n: u32,
}

fn step(label: &str, op: &'static str, n: u32) -> Step {
    Step {
        label: label.to_owned(),
        op,
        n,
    }
}

pub fn steps_for(entry: &Entry) -> Vec<Step> {
    if entry.name.starts_with("spa-list") {
        let mut v = vec![
            step("prepend 1", "prepend", 1),
            step("prepend 20", "prepend", 20),
            step("append 20", "append", 20),
            step("edit item 5", "edit", 5),
            step("rename 10 buttons", "rename", 10),
            step("reverse (keyed DOM moves)", "reverse", 0),
            step("shuffle (keyed DOM moves)", "shuffle", 0),
            step("remove 10 in the middle", "removeMid", 10),
            step("recreate all (key reset)", "recreate", 0),
            step("rewrite in place (unkeyed)", "rewrite", 0),
        ];
        v.push(step("route swap (200 cards)", "route", 200));
        v
    } else {
        vec![
            step("mut.rename 1", "rename", 1),
            step("mut.addCards 1", "addCards", 1),
            step("mut.addCards 100", "addCards", 100),
            step("mut.rename 100", "rename", 100),
            step("mut.removeCards 100", "removeCards", 100),
            step("mut.addCards 1000", "addCards", 1000),
            step("mut.rename 1000", "rename", 1000),
            step("mut.route", "route", 0),
        ]
    }
}

pub async fn fresh_signature(ctx: &Ctx) -> Option<Signature> {
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let o = FetchOpts {
        states: StatePolicy::None,
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    };
    let d = fetch_document(
        &ctx.target.app,
        &interner,
        &mut paths,
        &ctx.target.doc_path,
        &o,
    )
    .await
    .ok()?;
    let by_id: HashMap<_, _> = d.block.nodes.iter().map(|x| (x.id, x)).collect();
    let kids: HashMap<_, _> = d
        .block
        .children
        .iter()
        .map(|(p, c)| (*p, c.clone()))
        .collect();
    let mut out = Vec::with_capacity(d.block.nodes.len());
    let mut stack = vec![d.block.root];
    while let Some(id) = stack.pop() {
        let Some(nd) = by_id.get(&id) else { continue };
        let tag = nd
            .attrs
            .iter()
            .find(|(k, _)| *k == gorca_doc_source_api::web_attrs::TAG)
            .map(|(_, v)| Arc::clone(v));
        out.push((nd.role, nd.name.clone(), tag));
        if let Some(c) = kids.get(&id) {
            for x in c.iter().rev() {
                stack.push(*x);
            }
        }
    }
    Some(out)
}

pub async fn churn(ctx: &Ctx, use_dom_id: bool) -> Result<J> {
    let arm = AtspiTunedArm {
        source: SourceConfig {
            use_dom_id,
            ..AtspiTunedArm::default().source
        },
        ..AtspiTunedArm::default()
    };
    let session = arm
        .attach(&ctx.target)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let t_attach = Instant::now();
    let built = session
        .settle(Duration::from_millis(400), Duration::from_secs(120))
        .await;
    let build_ms = ms(t_attach.elapsed());
    let initial = {
        let sig = session
            .oracle
            .lock()
            .map(|o| o.signature())
            .unwrap_or_default();
        let fresh = fresh_signature(ctx).await.unwrap_or_default();
        (sig.len(), fresh.len(), first_diff(&sig, &fresh))
    };
    let mut rows = Vec::new();
    for st in steps_for(&ctx.entry) {
        let before_updates = session.metrics.updates().len();
        let ids_before = session
            .oracle
            .lock()
            .map(|o| o.dom_ids())
            .unwrap_or_default();
        let args = if st.n > 0 {
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
        // Wait for the burst: at least one update, then 700 ms without a new one.
        let t_wait = Instant::now();
        let mut seen_update = false;
        loop {
            let ups = session.metrics.updates();
            if ups.len() > before_updates {
                seen_update = true;
                if let Some(last) = ups.last()
                    && last.pushed_at.elapsed() > Duration::from_millis(700)
                {
                    break;
                }
            }
            if t_wait.elapsed() > Duration::from_secs(if seen_update { 30 } else { 6 }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let ups: Vec<UpdateRecord> = session
            .metrics
            .updates()
            .into_iter()
            .skip(before_updates)
            .collect();
        let (sig_len, fresh_len, diff, diff_detail) = {
            let sig = session
                .oracle
                .lock()
                .map(|o| o.signature())
                .unwrap_or_default();
            let fresh = fresh_signature(ctx).await.unwrap_or_default();
            let d = first_diff(&sig, &fresh);
            let detail = d.map_or(J::Null, |i| {
                s(format!(
                    "mirror {:?} vs fresh {:?}",
                    sig.get(i),
                    fresh.get(i)
                ))
            });
            (sig.len(), fresh.len(), d, detail)
        };
        // A mirror that disagrees with the page (Chromium reorders a keyed list silently) is
        // repaired by revalidate-on-read (IL-R1): ask the source to re-list the list's children.
        let mut revalidated = false;
        let mut consistent_after_revalidate = J::Null;
        if diff.is_some() {
            let parent = session
                .oracle
                .lock()
                .ok()
                .and_then(|o| o.dom_ids().get("items").copied());
            if let Some(parent) = parent {
                let (tx, rx) = gorca_rt::oneshot::channel();
                let sent = session
                    .source_requests
                    .send(gorca_doc_source_api::DocRequest::Revalidate {
                        parent,
                        deadline: Deadline(Instant::now() + Duration::from_secs(3)),
                        reply: tx,
                    })
                    .await
                    .is_ok();
                if sent {
                    let _ = tokio::time::timeout(Duration::from_secs(4), rx).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    revalidated = true;
                    let sig = session
                        .oracle
                        .lock()
                        .map(|o| o.signature())
                        .unwrap_or_default();
                    let fresh = fresh_signature(ctx).await.unwrap_or_default();
                    consistent_after_revalidate = J::B(first_diff(&sig, &fresh).is_none());
                }
            }
        }
        let ids_after = session
            .oracle
            .lock()
            .map(|o| o.dom_ids())
            .unwrap_or_default();

        // Identity: logical id of each surviving DOM id before and after.
        let mut stable = 0usize;
        let mut moved = 0usize;
        let mut gone = 0usize;
        for (dom, id_before) in &ids_before {
            match ids_after.get(dom) {
                Some(id_after) if id_after == id_before => stable += 1,
                Some(_) => moved += 1,
                None => gone += 1,
            }
        }
        // Wrong node: a logical id that existed before now carries a different DOM id.
        let inv_after: HashMap<_, _> = ids_after.iter().map(|(d, i)| (*i, d.clone())).collect();
        let wrong = ids_before
            .iter()
            .filter(|(dom, id)| inv_after.get(id).is_some_and(|d2| d2 != *dom))
            .count();

        let t0 = ack.t0;
        let first_push = ups.iter().map(|u| u.pushed_at).min();
        let last_push = ups.iter().map(|u| u.pushed_at).max();
        let first_event = ups.iter().filter_map(|u| u.first_event).min();
        let mut st_sum = gorca_doc_source_atspi::MatchStats::default();
        for u2 in &ups {
            st_sum.add(&u2.stats);
        }
        let ev_total: u32 = ups.iter().map(|u2| u2.events).sum();
        rows.push(obj([
            ("step", s(&st.label)),
            ("page_info", s(&ack.info)),
            ("page_op_ms", n(ack.t1 - ack.t0)),
            ("updates", u(ups.len())),
            ("events", n(f64::from(ev_total))),
            (
                "dom_to_first_event_ms",
                first_event.map_or(J::Null, |t| n(ctx.epoch_ms(t) - t0)),
            ),
            (
                "dom_to_first_push_ms",
                first_push.map_or(J::Null, |t| n(ctx.epoch_ms(t) - t0)),
            ),
            (
                "dom_to_last_push_ms",
                last_push.map_or(J::Null, |t| n(ctx.epoch_ms(t) - t0)),
            ),
            (
                "parents_refreshed",
                u(ups.iter().map(|x| x.parents_refreshed as usize).sum()),
            ),
            (
                "props_refreshed",
                u(ups.iter().map(|x| x.props_refreshed as usize).sum()),
            ),
            (
                "nodes_fetched",
                u(ups.iter().map(|x| x.nodes_fetched).sum()),
            ),
            ("resync", J::B(ups.iter().any(|x| x.resync))),
            ("rewrite_detected", J::B(ups.iter().any(|x| x.rewrite))),
            (
                "rewrite_note",
                ups.iter()
                    .find_map(|x| x.rewrite_note.clone())
                    .map_or(J::Null, s),
            ),
            ("errors", u(ups.iter().map(|x| x.errors as usize).sum())),
            ("mirror_nodes", u(sig_len)),
            ("fresh_nodes", u(fresh_len)),
            ("consistent", J::B(diff.is_none())),
            ("revalidated", J::B(revalidated)),
            ("consistent_after_revalidate", consistent_after_revalidate),
            ("first_diff_index", diff.map_or(J::Null, u)),
            ("first_diff_detail", diff_detail),
            ("ids_tracked", u(ids_before.len())),
            ("id_stable", u(stable)),
            ("id_moved", u(moved)),
            ("id_gone", u(gone)),
            ("wrong_node", u(wrong)),
            (
                "match_rungs",
                obj([
                    ("by_path", n(f64::from(st_sum.by_path))),
                    ("by_dom_id", n(f64::from(st_sum.by_dom_id))),
                    ("by_full_key", n(f64::from(st_sum.by_full_key))),
                    ("by_role_tag", n(f64::from(st_sum.by_role_tag))),
                    ("new_ids", n(f64::from(st_sum.new_ids))),
                    ("dropped", n(f64::from(st_sum.dropped))),
                    ("heuristic_share", n(st_sum.heuristic_share())),
                ]),
            ),
        ]));
    }
    let dropped_note = session
        .oracle
        .lock()
        .map(|o| (o.deltas, o.resets, o.unknown_parent))
        .unwrap_or_default();
    let first_block = session.metrics.first_block();
    let build_detail = session.metrics.build().map_or(J::Null, |(t, nodes)| {
        obj([
            ("nodes", u(nodes)),
            ("warm_ms", n(ms(t.warm))),
            ("frontier_ms", n(ms(t.frontier))),
            ("get_tree_ms", n(ms(t.get_tree))),
            ("roles_ms", n(ms(t.roles))),
            ("states_ms", n(ms(t.states))),
            ("total_ms", n(ms(t.total))),
        ])
    });
    session.stop().await;
    Ok(obj([
        ("page", s(&ctx.entry.name)),
        ("scenario", s("churn")),
        ("metric", s("WM-4 update latency, M-13 identity stability")),
        ("use_dom_id", J::B(use_dom_id)),
        ("initial_build_settled", J::B(built)),
        ("initial_build_ms", n(build_ms)),
        ("initial_build_detail", build_detail),
        (
            "initial_first_block_ms",
            first_block.map_or(J::Null, |(d, _)| n(ms(d))),
        ),
        (
            "initial_first_block_nodes",
            first_block.map_or(J::Null, |(_, k)| u(k)),
        ),
        ("initial_mirror_nodes", u(initial.0)),
        ("initial_fresh_nodes", u(initial.1)),
        ("initial_consistent", J::B(initial.2.is_none())),
        ("total_deltas", n(dropped_note.0 as f64)),
        ("resets", n(dropped_note.1 as f64)),
        ("unknown_parent_children_deltas", n(dropped_note.2 as f64)),
        ("steps", J::A(rows)),
    ]))
}

// ---------------------------------------------------------------------------------------------
// scale (M-15)

/// Collect `GetTree` replies of at least `min_subtrees` subtrees below the document root.
async fn collect_subtree_replies(
    app: &AppConn,
    doc: &str,
    min_subtrees: usize,
) -> Result<Vec<Vec<u8>>> {
    let dl = || Deadline(Instant::now() + Duration::from_secs(10));
    let mut frontier = vec![doc.to_owned()];
    for _ in 0..5 {
        if frontier.len() >= min_subtrees {
            break;
        }
        let mut next = Vec::new();
        for p in &frontier {
            let m = app
                .call(
                    app.next_lane(),
                    dl(),
                    p,
                    "org.a11y.atspi.Accessible",
                    "GetChildren",
                    &(),
                )
                .await?;
            let kids = wire::parse_children(m.body().data().bytes())?;
            if kids.is_empty() {
                next.push(p.clone());
            } else {
                next.extend(kids.into_iter().map(|(_, c)| c));
            }
        }
        frontier = next;
    }
    let mut out = Vec::new();
    for p in &frontier {
        let m = get_tree_raw(app, app.next_lane(), p, dl()).await?;
        out.push(m.body().data().bytes().to_vec());
    }
    let _ = tree_args;
    Ok(out)
}

pub async fn scale(ctx: &Ctx) -> Result<J> {
    let replies = collect_subtree_replies(&ctx.target.app, &ctx.target.doc_path, 64).await?;
    let total_bytes: usize = replies.iter().map(Vec::len).sum();
    let interner = AttrInterner::default();
    // Node count once.
    let mut nodes = 0usize;
    for r in &replies {
        nodes += wire::parse_get_tree(r, &interner).map_or(0, |t| t.nodes.len());
    }
    let replies = Arc::new(replies);
    let threads_list = [1usize, 2, 4, 8, 16];
    let mut rows = Vec::new();
    let mut base_ms = 0.0;
    for &th in &threads_list {
        let mut samples = Vec::new();
        for _ in 0..ctx.runs.max(5) {
            let t = Instant::now();
            let next = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|sc| {
                for _ in 0..th {
                    sc.spawn(|| {
                        let it = AttrInterner::default();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(r) = replies.get(i) else { break };
                            let _ = wire::parse_get_tree(r, &it);
                        }
                    });
                }
            });
            samples.push(ms(t.elapsed()));
        }
        let sm = summarize(&samples);
        if th == 1 {
            base_ms = sm.p50;
        }
        rows.push(obj([
            ("threads", u(th)),
            ("decode_ms", sm.to_json()),
            (
                "speedup_vs_1",
                n(if sm.p50 > 0.0 { base_ms / sm.p50 } else { 0.0 }),
            ),
        ]));
    }
    // Fetch scaling over connections (structure + roles + stateful states).
    let mut conn_rows = Vec::new();
    for c in [1usize, 2, 4, 8, 16] {
        let v = Variant {
            name: "conn-scaling",
            conns: c,
            opts: opts(frontier(), 32, StatePolicy::Stateful, TextPolicy::None),
        };
        let r = build(ctx, std::slice::from_ref(&v)).await?;
        conn_rows.push(obj([("connections", u(c)), ("result", r)]));
    }
    Ok(obj([
        ("page", s(&ctx.entry.name)),
        ("scenario", s("scale")),
        (
            "metric",
            s("M-15 parallel decode scaling and connection scaling"),
        ),
        ("subtree_replies", u(replies.len())),
        ("nodes", u(nodes)),
        ("reply_bytes", u(total_bytes)),
        (
            "cores",
            u(std::thread::available_parallelism().map_or(1, usize::from)),
        ),
        ("decode_scaling", J::A(rows)),
        ("connection_scaling", J::A(conn_rows)),
    ]))
}

// ---------------------------------------------------------------------------------------------
// prewarm: is cache-domain activation really per application, not per document?

/// The browser starts on a tiny page; the cache domains are activated there (as gorca does when it
/// attaches to the application, before any page of interest exists); then the page under test is
/// loaded in the same tab and its first block and full build are timed on the already warm
/// connection. Compare with the cold numbers of the `build` scenario.
pub async fn prewarm(ctx: &Ctx, target_path: &str) -> Result<J> {
    let interner = Arc::new(AttrInterner::default());
    let t = Instant::now();
    let w = gorca_doc_source_atspi::warm::warm_domains(
        &ctx.target.app,
        &interner,
        &ctx.target.doc_path,
        Duration::from_secs(10),
    )
    .await;
    let warm_ms = ms(t.elapsed());
    ctx.server.clear_loaded();
    // Fire and forget: the page navigates away right after it acknowledges (or before).
    let args = format!(",\"arg\":\"{target_path}\"");
    let _ = ctx
        .server
        .command("goto", &args, Duration::from_secs(3))
        .await;
    let loaded = ctx
        .server
        .wait_loaded(Duration::from_secs(90))
        .await
        .is_some();
    let doc = discover::wait_for_document(
        &ctx.target.app,
        &ctx.target.app_root,
        &ctx.entry.title,
        Duration::from_secs(90),
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut paths = PathTable::default();
    let o = FetchOpts {
        budget: Duration::from_secs(60),
        ..FetchOpts::default()
    };
    let t = Instant::now();
    let first = gorca_doc_source_atspi::fetch::fetch_first_block(
        &ctx.target.app,
        &interner,
        &mut paths,
        &doc.path,
        &o,
        400,
    )
    .await;
    let first_ms = ms(t.elapsed());
    let t = Instant::now();
    let full = fetch_document(&ctx.target.app, &interner, &mut paths, &doc.path, &o).await;
    let full_ms = ms(t.elapsed());
    Ok(obj([
        ("page", s(&ctx.entry.name)),
        ("scenario", s("prewarm")),
        (
            "metric",
            s(
                "cache-domain activation is per application: first block and full build on a connection warmed on another document",
            ),
        ),
        ("warm_on_first_document_ms", n(warm_ms)),
        ("warm_ready", J::B(w.ready)),
        ("navigated_and_loaded_ack", J::B(loaded)),
        (
            "first_block_ms",
            first.as_ref().map_or(J::Null, |_| n(first_ms)),
        ),
        (
            "first_block_warm_ms",
            first
                .as_ref()
                .map_or(J::Null, |(d, _, _)| n(ms(d.timings.warm))),
        ),
        (
            "first_block_nodes",
            first
                .as_ref()
                .map_or(J::Null, |(d, _, _)| u(d.block.node_count())),
        ),
        (
            "full_build_ms",
            full.as_ref().map_or(J::Null, |_| n(full_ms)),
        ),
        (
            "full_build_nodes",
            full.as_ref().map_or(J::Null, |d| u(d.block.node_count())),
        ),
    ]))
}
