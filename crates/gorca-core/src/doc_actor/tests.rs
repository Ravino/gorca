#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gorca_doc_source_api::{Activity, web_attrs};
use gorca_model::{
    AppId, ConnectorId, DocAction, DocDelta, DocId, LocalId, NodeKey, Role, SrcNode, SrcNodeId,
    StateSet,
};
use gorca_rt::{CancellationToken, mpsc};
use gorca_testkit::{ScriptHandle, ScriptedDocSource, Seen};

use super::*;

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

fn nk(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn src(id: u64, role: Role, local: Option<u64>) -> SrcNode {
    SrcNode {
        id: SrcNodeId(id),
        role,
        states: StateSet::VISIBLE | StateSet::SHOWING | StateSet::ENABLED,
        name: None,
        text: None,
        attrs: smallvec::SmallVec::default(),
        local: local.map(LocalId),
        rec: None,
    }
}

fn block(mut n: SrcNode) -> SrcNode {
    n.attrs.push((web_attrs::DISPLAY, Arc::from("block")));
    n
}

fn run_node(id: u64, t: &str) -> SrcNode {
    let mut n = src(id, Role::StaticText, None);
    n.text = Some(Arc::from(t));
    n
}

fn kids(parent: u64, ks: &[u64]) -> DocDelta {
    DocDelta::Children {
        parent: SrcNodeId(parent),
        children: ks.iter().map(|k| SrcNodeId(*k)).collect(),
    }
}

/// body(1) -> p(2) -> "hello"(3), a(4) -> "docs"(5)
fn first_batch() -> Vec<DocDelta> {
    vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(vec![
            block(src(1, Role::DocumentWeb, Some(11))),
            block(src(2, Role::Paragraph, Some(12))),
            run_node(3, "hello "),
            src(4, Role::Link, Some(14)),
            run_node(5, "docs"),
        ]),
        kids(1, &[2]),
        kids(2, &[3, 4]),
        kids(4, &[5]),
        DocDelta::Settled { batch: 1 },
    ]
}

struct Rig {
    handle: DocHandle,
    script: ScriptHandle,
    join: Arc<WebJoin>,
    docs: Arc<DocDirectory>,
    notices: mpsc::Receiver<DocNotice>,
    counters: Arc<DocCounters>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

fn rig() -> Rig {
    rig_with(super::repair::RepairPolicy::ALL)
}

fn rig_with(policy: super::repair::RepairPolicy) -> Rig {
    rig_full(policy, None)
}

fn rig_full(
    policy: super::repair::RepairPolicy,
    fuse: Option<gorca_doc_source_api::FuseSetup>,
) -> Rig {
    let (source, script) = ScriptedDocSource::new();
    let source = match fuse {
        Some(f) => source.with_fuse(f),
        None => source,
    };
    let join = Arc::new(WebJoin::new());
    let docs = Arc::new(DocDirectory::new());
    let (ntx, notices) = mpsc::channel(32);
    let shutdown = CancellationToken::new();
    let counters = Arc::new(DocCounters::default());
    let (handle, fut) = prepare(DocActorSpec {
        doc: DocId(7),
        app: app(),
        root: nk(11),
        source: Box::new(source),
        join: Arc::clone(&join),
        directory: Arc::clone(&docs),
        notices: ntx,
        shutdown: shutdown.clone(),
        counters: Arc::clone(&counters),
        repair: policy,
    });
    docs.insert(handle.clone());
    Rig {
        handle,
        script,
        join,
        docs,
        notices,
        counters,
        shutdown,
        task: tokio::spawn(fut),
    }
}

async fn next_notice(r: &mut Rig) -> DocNotice {
    tokio::time::timeout(Duration::from_secs(3), r.notices.recv())
        .await
        .expect("notice in time")
        .expect("channel open")
}

async fn next_seen(r: &mut Rig) -> Seen {
    tokio::time::timeout(Duration::from_secs(3), r.script.seen.recv())
        .await
        .expect("request in time")
        .expect("channel open")
}

#[tokio::test]
async fn a_settled_batch_is_published_and_joined_before_the_reader_is_told() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    assert_eq!(
        next_notice(&mut r).await,
        DocNotice::FirstUseful {
            doc: DocId(7),
            app: app()
        }
    );
    // JN-1: the join answers by the time the notice is out, and the node is in the snapshot.
    let (doc, id) = r.join.get(nk(14)).expect("link joined");
    assert_eq!(doc, DocId(7));
    r.handle.read.with(|d| {
        assert_eq!(d.node(id).map(|v| v.role), Some(Role::Link));
        assert_eq!(d.text_in(0, d.text_len()), "\nhello docs");
        assert!(d.validate().is_ok());
    });
    assert_eq!(r.counters.publishes.load(Ordering::Relaxed), 1);
    // A later batch is a `Changed`.
    let mut change = vec![
        DocDelta::Upsert(vec![run_node(3, "bye ")]),
        DocDelta::Settled { batch: 2 },
    ];
    r.script
        .batches
        .send(std::mem::take(&mut change))
        .await
        .unwrap();
    assert!(matches!(
        next_notice(&mut r).await,
        DocNotice::Changed { doc: DocId(7), .. }
    ));
    r.handle
        .read
        .with(|d| assert_eq!(d.text_in(0, d.text_len()), "\nbye docs"));
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn activity_and_actions_reach_the_source_by_its_own_ids() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    let (_, link) = r.join.get(nk(14)).unwrap();
    r.handle
        .ctl
        .send(DocCtl::Act {
            node: link,
            act: DocAction::Click,
        })
        .await
        .unwrap();
    assert_eq!(
        next_seen(&mut r).await,
        Seen::Action(SrcNodeId(4), DocAction::Click)
    );
    r.handle
        .ctl
        .send(DocCtl::Activity(Activity::Dormant))
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Activity(Activity::Dormant));
    r.handle.ctl.send(DocCtl::Rebuild).await.unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Rebuild);
    // An action on a node that is not in the document goes nowhere.
    r.handle
        .ctl
        .send(DocCtl::Act {
            node: DocNodeId::from_parts(999, 0),
            act: DocAction::Focus,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), r.script.seen.recv())
            .await
            .is_err()
    );
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_selection_request_is_always_answered_even_when_its_node_is_gone() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    r.handle
        .ctl
        .send(DocCtl::Select {
            req: 7,
            node: DocNodeId::from_parts(999, 0),
            act: DocAction::ClearSelection,
        })
        .await
        .unwrap();
    assert!(matches!(
        next_notice(&mut r).await,
        DocNotice::SelectionDone {
            req: 7,
            outcome: crate::doc_actor::SelOutcome::Failed,
            ..
        }
    ));
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_batch_the_mirror_rejects_asks_the_source_for_a_rebuild_and_keeps_the_last_good_state() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    // The source names a child twice: a protocol violation (SL-3).
    r.script
        .batches
        .send(vec![kids(2, &[3, 3]), DocDelta::Settled { batch: 2 }])
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Rebuild);
    assert_eq!(r.counters.rejected.load(Ordering::Relaxed), 1);
    r.handle.read.with(|d| {
        assert!(d.validate().is_ok());
        assert_eq!(d.text_in(0, d.text_len()), "\nhello docs");
    });
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_source_that_ends_retires_the_document_and_leaves_no_key_behind() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    assert!(r.join.get(nk(12)).is_some());
    r.script.end.send(()).await.unwrap();
    assert_eq!(next_notice(&mut r).await, DocNotice::Gone { doc: DocId(7) });
    assert!(r.join.get(nk(12)).is_none(), "keys leave with the document");
    assert!(r.join.is_empty());
    assert!(r.docs.get(DocId(7)).is_none());
    let _ = r.task.await;
}

#[tokio::test]
async fn shutdown_ends_the_actor_and_its_source() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    r.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(3), r.task)
        .await
        .expect("actor stops")
        .unwrap();
    assert!(r.join.is_empty());
}

#[test]
fn the_join_replaces_and_removes_only_its_own_keys() {
    let j = WebJoin::new();
    let id = DocNodeId::from_parts(1, 0);
    j.set(nk(1), (DocId(1), id));
    j.set(nk(1), (DocId(2), id));
    assert_eq!(j.get(nk(1)).map(|v| v.0), Some(DocId(2)));
    j.remove(nk(1), DocId(1));
    assert!(j.get(nk(1)).is_some(), "another document owns it now");
    j.remove(nk(1), DocId(2));
    assert!(j.get(nk(1)).is_none());
}

/// A source that detaches a task holding its shutdown token (the frame wake does), then ends.
struct DetachingSource {
    stopped: Arc<std::sync::atomic::AtomicBool>,
    end: tokio::sync::oneshot::Receiver<()>,
}

impl gorca_doc_source_api::DocSource for DetachingSource {
    fn kind(&self) -> gorca_doc_source_api::SourceKind {
        gorca_doc_source_api::SourceKind::Atspi
    }

    fn caps(&self) -> gorca_doc_source_api::SourceCaps {
        ScriptedDocSource::new().0.caps()
    }

    fn attach(
        self: Box<Self>,
        ctx: gorca_doc_source_api::DocSourceCtx,
    ) -> gorca_rt::BoxFuture<'static, Result<(), gorca_doc_source_api::SourceError>> {
        Box::pin(async move {
            let ctx = ctx;
            let token = ctx.shutdown.clone();
            let stopped = Arc::clone(&self.stopped);
            tokio::spawn(async move {
                token.cancelled().await;
                stopped.store(true, Ordering::SeqCst);
            });
            let _ = self.end.await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn a_task_the_source_detached_stops_when_the_document_ends() {
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (end_tx, end) = tokio::sync::oneshot::channel();
    let join = Arc::new(WebJoin::new());
    let docs = Arc::new(DocDirectory::new());
    let (ntx, mut notices) = mpsc::channel(32);
    let shutdown = CancellationToken::new();
    let (handle, fut) = prepare(DocActorSpec {
        doc: DocId(7),
        app: app(),
        root: nk(11),
        source: Box::new(DetachingSource {
            stopped: Arc::clone(&stopped),
            end,
        }),
        join,
        directory: Arc::clone(&docs),
        notices: ntx,
        shutdown: shutdown.clone(),
        counters: Arc::new(DocCounters::default()),
        repair: super::repair::RepairPolicy::NONE,
    });
    docs.insert(handle.clone());
    let task = tokio::spawn(fut);
    end_tx.send(()).unwrap();
    // The document ends while the app, and so the actor's own shutdown, lives on.
    while let Some(n) = notices.recv().await {
        if n == (DocNotice::Gone { doc: DocId(7) }) {
            break;
        }
    }
    let _ = task.await;
    drop(handle);
    for _ in 0..100 {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        stopped.load(Ordering::SeqCst),
        "the detached task outlived its document"
    );
    assert!(!shutdown.is_cancelled());
}

// ---- CHR-0 mirror freshness ------------------------------------------------------------------

async fn no_seen_within(r: &mut Rig, ms: u64) {
    let got = tokio::time::timeout(Duration::from_millis(ms), r.script.seen.recv()).await;
    assert!(got.is_err(), "no request expected, got {got:?}");
}

#[tokio::test]
async fn a_load_complete_repairs_a_mirrored_document_at_once() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    r.handle
        .ctl
        .send(DocCtl::Repair(repair::RepairReason::LoadComplete))
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_second_repair_waits_for_the_gap_the_first_one_earned() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    r.handle
        .ctl
        .send(DocCtl::Repair(repair::RepairReason::FocusMiss))
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    // The first repair found nothing to change: a guess right after it is held back (400 ms
    // doubled), then goes through on its own.
    r.handle
        .ctl
        .send(DocCtl::Repair(repair::RepairReason::FocusMiss))
        .await
        .unwrap();
    no_seen_within(&mut r, 400).await;
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_gecko_document_never_repairs() {
    let mut r = rig_with(repair::RepairPolicy::for_engine(
        gorca_model::WebEngine::Gecko,
    ));
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    for why in [
        repair::RepairReason::LoadComplete,
        repair::RepairReason::FocusMiss,
    ] {
        r.handle.ctl.send(DocCtl::Repair(why)).await.unwrap();
    }
    no_seen_within(&mut r, 600).await;
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_dormant_document_does_not_repair_and_requests_coalesce_into_one_control_message() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    r.handle
        .ctl
        .send(DocCtl::Activity(Activity::Dormant))
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Activity(Activity::Dormant));
    r.handle.request_repair(repair::RepairReason::LoadComplete);
    no_seen_within(&mut r, 500).await;
    // Awake again: a storm of requests reaches the source as one repair.
    r.handle
        .ctl
        .send(DocCtl::Activity(Activity::Awake))
        .await
        .unwrap();
    assert_eq!(next_seen(&mut r).await, Seen::Activity(Activity::Awake));
    for _ in 0..50 {
        r.handle.request_repair(repair::RepairReason::LoadComplete);
        r.handle.request_repair(repair::RepairReason::Orphans);
    }
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    no_seen_within(&mut r, 300).await;
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn events_on_unknown_nodes_repair_after_the_settle_period_only_if_still_unknown() {
    let mut r = rig_with(repair::RepairPolicy {
        young: false,
        ..repair::RepairPolicy::ALL
    });
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    // A node the join already knows is no orphan.
    r.handle.orphans.note(nk(14));
    no_seen_within(&mut r, 1500).await;
    // An unknown one is, but only after the settle period.
    r.handle.orphans.note(nk(99));
    no_seen_within(&mut r, 400).await;
    assert_eq!(next_seen(&mut r).await, Seen::RepairCheap);
    // The cheap look answered (the fake reports nothing fetched) while the orphan is still
    // unknown: the next orphan repair skips the look and refetches (D-2).
    r.handle.orphans.note(nk(99));
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_tiny_young_mirror_is_checked_at_two_seconds_unless_the_load_was_reported() {
    let mut r = rig();
    r.script.batches.send(first_batch()).await.unwrap();
    next_notice(&mut r).await;
    no_seen_within(&mut r, 1500).await;
    assert_eq!(next_seen(&mut r).await, Seen::Repair);
    r.shutdown.cancel();
    let _ = r.task.await;
}

// ---- fusion in the writer (M22a-C2) --------------------------------------------------------------

mod fused {
    use gorca_fuse::{PrimarySlot, Profile, QuirkSet, Rules};
    use gorca_model::sem::{Group, GroupMask, SemRole, SrcRecord};

    use super::*;

    fn setup() -> gorca_doc_source_api::FuseSetup {
        let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
        profile.default_role = SemRole::Generic;
        gorca_doc_source_api::FuseSetup {
            name: Arc::from("test"),
            profile,
            quirks: QuirkSet::default(),
        }
    }

    fn rec(role: SemRole, name: Option<&str>) -> Arc<SrcRecord> {
        let mut r = SrcRecord::default();
        r.sem.role = role;
        r.name = name.map(Arc::from);
        r.known = GroupMask::EMPTY;
        r.known.set(Group::Role, true);
        r.known.set(Group::Name, true);
        Arc::new(r)
    }

    fn with_rec(mut n: SrcNode, role: SemRole, name: Option<&str>) -> SrcNode {
        n.rec = Some(rec(role, name));
        n
    }

    /// body(1) -> p(2) -> "hello"(3), a(4) -> "docs"(5), with normalised records.
    fn batch() -> Vec<DocDelta> {
        vec![
            DocDelta::Reset {
                root: SrcNodeId(1),
                nav_id: 1,
            },
            DocDelta::Upsert(vec![
                with_rec(
                    block(src(1, Role::DocumentWeb, Some(11))),
                    SemRole::DocumentWeb,
                    None,
                ),
                with_rec(
                    block(src(2, Role::Paragraph, Some(12))),
                    SemRole::Paragraph,
                    None,
                ),
                run_node(3, "hello "),
                with_rec(src(4, Role::Link, Some(14)), SemRole::Link, None),
                run_node(5, "docs"),
            ]),
            kids(1, &[2]),
            kids(2, &[3, 4]),
            kids(4, &[5]),
            DocDelta::Settled { batch: 1 },
        ]
    }

    fn sem_name(r: &Rig, local: u64) -> Option<String> {
        let (_, id) = r.join.get(nk(local))?;
        r.handle.read.with(|d| {
            d.node(id)
                .and_then(|v| v.meta)
                .and_then(|m| m.sem.as_ref())
                .and_then(|e| e.name.as_deref().map(str::to_owned))
        })
    }

    fn sem_role(r: &Rig, local: u64) -> Option<SemRole> {
        let (_, id) = r.join.get(nk(local))?;
        r.handle.read.with(|d| {
            d.node(id)
                .and_then(|v| v.meta)
                .and_then(|m| m.sem.as_ref())
                .map(|e| e.sem.role)
        })
    }

    #[tokio::test]
    async fn the_published_mirror_carries_the_fused_entries() {
        let mut r = rig_full(super::super::repair::RepairPolicy::ALL, Some(setup()));
        r.script.batches.send(batch()).await.unwrap();
        next_notice(&mut r).await;
        // Fused before the publish: the very first snapshot has the entries.
        assert_eq!(sem_role(&r, 14), Some(SemRole::Link));
        assert_eq!(sem_role(&r, 11), Some(SemRole::DocumentWeb));
        // The link has no name of its own: S3 named it from its content (the text run).
        assert_eq!(sem_name(&r, 14).as_deref(), Some("docs"));
        r.shutdown.cancel();
        let _ = r.task.await;
    }

    #[tokio::test]
    async fn a_change_in_a_childs_content_patches_the_parents_entry_without_an_upsert() {
        let mut r = rig_full(super::super::repair::RepairPolicy::ALL, Some(setup()));
        r.script.batches.send(batch()).await.unwrap();
        next_notice(&mut r).await;
        r.script
            .batches
            .send(vec![
                DocDelta::Upsert(vec![run_node(5, "pages")]),
                DocDelta::Settled { batch: 2 },
            ])
            .await
            .unwrap();
        assert!(matches!(
            next_notice(&mut r).await,
            DocNotice::Changed { .. }
        ));
        assert_eq!(sem_name(&r, 14).as_deref(), Some("pages"));
        r.handle.read.with(|d| assert!(d.validate().is_ok()));
        r.shutdown.cancel();
        let _ = r.task.await;
    }

    #[tokio::test]
    async fn a_forwarded_fact_fuses_in_the_writer_and_an_older_one_loses() {
        let mut r = rig_full(super::super::repair::RepairPolicy::ALL, Some(setup()));
        let mut b = batch();
        // Give the link a name of its own so the fact has something to beat.
        b[1] = DocDelta::Upsert(vec![
            with_rec(
                block(src(1, Role::DocumentWeb, Some(11))),
                SemRole::DocumentWeb,
                None,
            ),
            with_rec(
                block(src(2, Role::Paragraph, Some(12))),
                SemRole::Paragraph,
                None,
            ),
            run_node(3, "hello "),
            with_rec(src(4, Role::Link, Some(14)), SemRole::Link, Some("Docs")),
            run_node(5, "docs"),
        ]);
        r.script.batches.send(b).await.unwrap();
        next_notice(&mut r).await;
        assert_eq!(sem_name(&r, 14).as_deref(), Some("Docs"));
        let t = r.handle.timeline.load(Ordering::Acquire);
        assert_eq!(t, 1, "one batch was applied");
        // Issued after the batch: it wins. A fused entry is no content change: the Reader is not
        // told, the mirror just carries the new entry.
        r.handle
            .ctl
            .send(DocCtl::Fact {
                local: LocalId(14),
                rec: rec(SemRole::Link, Some("Fresh")),
                issued_at: t,
            })
            .await
            .unwrap();
        for _ in 0..100 {
            if sem_name(&r, 14).as_deref() == Some("Fresh") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(sem_name(&r, 14).as_deref(), Some("Fresh"));
        assert!(r.notices.try_recv().is_err(), "no content changed");
        // A later batch rewrites the node; a fact issued before that batch is older than it.
        r.script
            .batches
            .send(vec![
                DocDelta::Upsert(vec![with_rec(
                    src(4, Role::Link, Some(14)),
                    SemRole::Link,
                    Some("Batch"),
                )]),
                DocDelta::Settled { batch: 2 },
            ])
            .await
            .unwrap();
        next_notice(&mut r).await;
        assert_eq!(sem_name(&r, 14).as_deref(), Some("Batch"));
        r.handle
            .ctl
            .send(DocCtl::Fact {
                local: LocalId(14),
                rec: rec(SemRole::Link, Some("Stale")),
                issued_at: t,
            })
            .await
            .unwrap();
        // Facts about keys no join knows are dropped quietly.
        r.handle
            .ctl
            .send(DocCtl::Fact {
                local: LocalId(777),
                rec: rec(SemRole::Link, Some("Nobody")),
                issued_at: 99,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(sem_name(&r, 14).as_deref(), Some("Batch"));
        assert!(
            r.notices.try_recv().is_err(),
            "nothing changed, nothing to tell"
        );
        r.shutdown.cancel();
        let _ = r.task.await;
    }

    #[tokio::test]
    async fn a_source_without_fusion_tables_publishes_nodes_without_entries() {
        let mut r = rig();
        r.script.batches.send(batch()).await.unwrap();
        next_notice(&mut r).await;
        assert_eq!(sem_role(&r, 14), None);
        // A fact for such a document is ignored.
        r.handle
            .ctl
            .send(DocCtl::Fact {
                local: LocalId(14),
                rec: rec(SemRole::Link, Some("X")),
                issued_at: 1,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(sem_role(&r, 14), None);
        r.shutdown.cancel();
        let _ = r.task.await;
    }
}
