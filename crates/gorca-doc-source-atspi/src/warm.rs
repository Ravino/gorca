//! Cache-domain warming (ADR-0003 AS-3, WF-3).
//!
//! Firefox serves AT-SPI queries from the parent process's cache of the content tree and starts
//! with only `NameAndDescription | State` active. The first query of any other kind (attributes,
//! text, bounds, relations, table data) returns empty and *activates* the domain; the content
//! process then re-sends it for every node, which takes a moment on a big page. A block fetch
//! that starts before that finishes silently loses `tag` / `id` / `class` / ARIA attributes for
//! the nodes it read early (measured: the first mirror of a page missed `tag` on its first
//! link). Activation is process-global, so this runs once per application connection.
//!
//! The probe: issue one throwaway query per domain on the document root, then `GetTree` a few
//! top-level subtrees until the number of nodes carrying a `tag` attribute is non-zero and stable.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc_source_api::web_attrs;
use gorca_rt::Deadline;

use crate::fetch::get_tree_raw;
use crate::p2p::AppConn;
use crate::wire::{self, AttrInterner};

const ACC: &str = "org.a11y.atspi.Accessible";

#[derive(Clone, Debug, Default)]
pub struct WarmReport {
    /// Tagged-node count in the sample was non-zero and stable across two probes.
    pub ready: bool,
    pub probes: u32,
    pub waited: Duration,
    pub sampled_nodes: usize,
    /// Time inside D-Bus calls, and in our own sleeps between rounds (both inside `waited`).
    pub blocked: Duration,
    pub slept: Duration,
    pub rounds: u32,
}

/// Await `f`, adding its wall time to `acc`.
async fn timed<T>(acc: &mut Duration, f: impl std::future::Future<Output = T>) -> T {
    let t = Instant::now();
    let r = f.await;
    *acc += t.elapsed();
    r
}

fn dl(ms: u64) -> Deadline {
    Deadline(Instant::now() + Duration::from_millis(ms))
}

/// Warm the cache domains for the app behind `conn`, using the document at `doc`.
pub async fn warm_domains(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    doc: &str,
    budget: Duration,
) -> WarmReport {
    let t0 = Instant::now();
    let mut rep = WarmReport::default();
    if conn.is_warm() {
        rep.ready = true;
        return rep;
    }
    // One throwaway query per domain; results are ignored (the first answer may be empty).
    let lane = conn.next_lane();
    let _ = timed(
        &mut rep.blocked,
        conn.call(lane, dl(800), doc, ACC, "GetAttributes", &()),
    )
    .await;
    let _ = timed(
        &mut rep.blocked,
        conn.call(
            lane,
            dl(800),
            doc,
            "org.a11y.atspi.Text",
            "GetText",
            &(0i32, 1i32),
        ),
    )
    .await;
    let _ = timed(
        &mut rep.blocked,
        conn.call(
            lane,
            dl(800),
            doc,
            "org.a11y.atspi.Component",
            "GetExtents",
            &(0u32,),
        ),
    )
    .await;
    let _ = timed(
        &mut rep.blocked,
        conn.call(lane, dl(800), doc, ACC, "GetRelationSet", &()),
    )
    .await;
    let _ = timed(
        &mut rep.blocked,
        conn.call(lane, dl(800), doc, "org.a11y.atspi.Table", "GetNRows", &()),
    )
    .await;
    rep.probes += 5;

    let end = t0 + budget;
    let mut last: Option<(usize, usize)> = None;
    loop {
        rep.rounds += 1;
        let kids = match timed(
            &mut rep.blocked,
            conn.call(lane, dl(800), doc, ACC, "GetChildren", &()),
        )
        .await
        {
            Ok(m) => wire::parse_children(m.body().data().bytes()).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        rep.probes += 1;
        let mut tagged = 0usize;
        let mut sampled = 0usize;
        // First, last and a middle child: the top and the bottom of a long page.
        let picks: Vec<usize> = match kids.len() {
            0 => Vec::new(),
            1 => vec![0],
            n => vec![0, n / 2, n - 1],
        };
        for i in picks {
            let Ok(m) = timed(
                &mut rep.blocked,
                get_tree_raw(conn, lane, &kids[i].1, dl(2000)),
            )
            .await
            else {
                continue;
            };
            rep.probes += 1;
            if let Ok(t) = wire::parse_get_tree(m.body().data().bytes(), interner) {
                sampled += t.nodes.len();
                tagged += t
                    .nodes
                    .iter()
                    .filter(|n| n.attrs.iter().any(|(k, _)| *k == web_attrs::TAG))
                    .count();
            }
        }
        rep.sampled_nodes = sampled;
        // Ready when elements carry tags and the count stopped growing between two probes
        // (Chromium text nodes never have a tag, so "all tagged" would never hold there).
        if (tagged > 0 || sampled == 0) && last == Some((tagged, sampled)) {
            rep.ready = true;
            break;
        }
        last = Some((tagged, sampled));
        if Instant::now() >= end {
            break;
        }
        timed(
            &mut rep.slept,
            tokio::time::sleep(Duration::from_millis(25)),
        )
        .await;
    }
    rep.waited = t0.elapsed();
    if rep.ready {
        conn.mark_warm();
    }
    rep
}
