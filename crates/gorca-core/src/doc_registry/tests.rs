#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;
use std::time::Duration;

use gorca_doc_source_api::{WarmOutcome, WarmReport};
use gorca_model::{
    AppId, AppInfo, ConnectorId, ConnectorKind, DocDelta, LocalId, NodeKey, Role, SrcNode,
    SrcNodeId, StateSet,
};
use gorca_rt::{CancellationToken, mpsc};
use gorca_testkit::{FakeDocFactory, ScriptHandle, Seen, WarmScript};

use super::*;

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 4,
    }
}

fn nk(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("Gecko"),
        app_name: Arc::from("Firefox"),
        pid: None,
        connector_kind: ConnectorKind::Fake,
        verified_shell: false,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Utf16Bom,
        text_insert_slack: 0,
        edit_quirks: gorca_model::EditQuirks::default(),
        page_events_settle_ms: 0,
        web_engine: gorca_model::WebEngine::None,
        root: None,
    }
}

fn page() -> Vec<DocDelta> {
    let n = |id: u64, role: Role, local: Option<u64>| SrcNode {
        id: SrcNodeId(id),
        role,
        states: StateSet::VISIBLE | StateSet::ENABLED,
        name: None,
        text: None,
        attrs: smallvec::SmallVec::default(),
        local: local.map(LocalId),
    };
    vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(vec![
            n(1, Role::DocumentWeb, Some(11)),
            n(2, Role::Link, Some(12)),
        ]),
        DocDelta::Children {
            parent: SrcNodeId(1),
            children: [SrcNodeId(2)].into_iter().collect(),
        },
        DocDelta::Settled { batch: 1 },
    ]
}

struct Rig {
    tx: HubTx,
    factory: FakeDocFactory,
    join: Arc<WebJoin>,
    docs: Arc<DocDirectory>,
    notices: mpsc::Receiver<DocNotice>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

fn rig(factory: FakeDocFactory) -> Rig {
    let (tx, rx) = hub_channel();
    let join = Arc::new(WebJoin::new());
    let docs = Arc::new(DocDirectory::new());
    let (ntx, notices) = mpsc::channel(64);
    let shutdown = CancellationToken::new();
    let cfg = HubCfg {
        factory: Arc::new(factory.clone()),
        build_hold: Duration::from_millis(150),
        retry_unit: Duration::from_millis(20),
        join: Arc::clone(&join),
        docs: Arc::clone(&docs),
        notices: ntx,
        counters: Arc::new(DocCounters::default()),
    };
    let task = tokio::spawn(run_hub(cfg, rx, shutdown.clone()));
    Rig {
        tx,
        factory,
        join,
        docs,
        notices,
        shutdown,
        task,
    }
}

async fn notice(r: &mut Rig) -> DocNotice {
    tokio::time::timeout(Duration::from_secs(3), r.notices.recv())
        .await
        .expect("notice in time")
        .expect("open")
}

async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while !cond() {
        assert!(std::time::Instant::now() < end, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn start_app(r: &mut Rig) {
    r.tx.send(HubMsg::Appeared {
        app: app(),
        info: Box::new(info()),
    })
    .await
    .unwrap();
    r.tx.send(HubMsg::Ready { app: app() }).await.unwrap();
}

async fn created(r: &Rig) -> (NodeKey, ScriptHandle) {
    let f = r.factory.clone();
    let end = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(c) = f.take_created() {
            return c;
        }
        assert!(std::time::Instant::now() < end, "no source was created");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_web_app_is_announced_and_warmed_once_when_ready() {
    let f = FakeDocFactory::new(true, true);
    let mut r = rig(f.clone());
    r.tx.send(HubMsg::Appeared {
        app: app(),
        info: Box::new(info()),
    })
    .await
    .unwrap();
    assert_eq!(notice(&mut r).await, DocNotice::WebApp { app: app() });
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(f.warm_calls(), 0, "not before the application is ready");
    r.tx.send(HubMsg::Ready { app: app() }).await.unwrap();
    until("warm-up", || f.warm_calls() == 1).await;
    assert_eq!(
        f.warm_hints(),
        vec![None],
        "no document known yet: it finds one"
    );
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(f.warm_calls(), 1, "once warm it is left alone");
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn an_app_without_a_document_yet_is_probed_again_with_backoff() {
    let f = FakeDocFactory::new(true, true);
    f.script_warm(vec![
        WarmScript {
            delay: Duration::ZERO,
            outcome: Ok(WarmOutcome::NoDocument),
        },
        WarmScript::default(),
    ]);
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    until("two warm-up attempts", || f.warm_calls() == 2).await;
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn focus_in_a_document_builds_it_and_the_reader_is_told_when_it_is_useful() {
    let f = FakeDocFactory::new(true, false);
    f.map_document(nk(100), nk(11));
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    assert_eq!(notice(&mut r).await, DocNotice::WebApp { app: app() });
    r.tx.send(HubMsg::Ensure { focus: nk(100) }).await.unwrap();
    let (root, script) = created(&r).await;
    assert_eq!(root, nk(11));
    script.batches.send(page()).await.unwrap();
    let first = notice(&mut r).await;
    assert!(
        matches!(first, DocNotice::FirstUseful { app: a, .. } if a == app()),
        "{first:?}"
    );
    assert_eq!(r.docs.len(), 1);
    assert!(r.docs.by_root(nk(11)).is_some());
    assert!(r.join.get(nk(12)).is_some(), "the link is joined");
    // The same document is not built twice, but the Reader learns the node is inside it.
    r.tx.send(HubMsg::Ensure { focus: nk(101) }).await.unwrap();
    r.factory.map_document(nk(101), nk(11));
    // (map after send: the first locate may miss; ask again)
    r.tx.send(HubMsg::Ensure { focus: nk(101) }).await.unwrap();
    let mut saw_in_document = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(10), r.notices.recv()).await {
            Ok(Some(DocNotice::InDocument { focus, .. })) if focus == nk(101) => {
                saw_in_document = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert!(saw_in_document);
    assert!(f.take_created().is_none(), "one document, one source");
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_node_in_no_document_is_reported_and_not_looked_up_twice() {
    let f = FakeDocFactory::new(true, false);
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    notice(&mut r).await;
    r.tx.send(HubMsg::Ensure { focus: nk(900) }).await.unwrap();
    assert_eq!(
        notice(&mut r).await,
        DocNotice::NoDocument { focus: nk(900) }
    );
    assert_eq!(f.locate_calls(), 1);
    r.tx.send(HubMsg::Ensure { focus: nk(900) }).await.unwrap();
    assert_eq!(
        notice(&mut r).await,
        DocNotice::NoDocument { focus: nk(900) }
    );
    assert_eq!(
        f.locate_calls(),
        1,
        "remembered: browser chrome stays cheap"
    );
    // A node of an application that never announced itself.
    let stranger = NodeKey {
        app: AppId {
            connector: ConnectorId(1),
            serial: 99,
        },
        local: LocalId(1),
    };
    r.tx.send(HubMsg::Ensure { focus: stranger }).await.unwrap();
    assert_eq!(
        notice(&mut r).await,
        DocNotice::NoDocument { focus: stranger }
    );
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn builds_wait_for_the_warm_up_but_not_forever_and_a_late_warm_rebuilds() {
    let f = FakeDocFactory::new(true, true);
    f.map_document(nk(100), nk(11));
    f.script_warm(vec![WarmScript {
        delay: Duration::from_millis(500),
        outcome: Ok(WarmOutcome::Done(WarmReport {
            ready: true,
            ..WarmReport::default()
        })),
    }]);
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    notice(&mut r).await;
    until("warm-up started", || f.warm_calls() == 1).await;
    r.tx.send(HubMsg::Ensure { focus: nk(100) }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        f.take_created().is_none(),
        "the build is held while the cache warms"
    );
    // After the hold (150 ms) it goes ahead without the warm-up; ...
    let (_, mut script) = created(&r).await;
    script.batches.send(page()).await.unwrap();
    notice(&mut r).await;
    // ... and when the warm-up finally succeeds the document is rebuilt once.
    let seen = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match script.seen.recv().await {
                Some(Seen::Rebuild) => return true,
                Some(_) => {}
                None => return false,
            }
        }
    })
    .await;
    assert_eq!(seen, Ok(true));
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn an_app_that_goes_away_takes_its_documents_with_it() {
    let f = FakeDocFactory::new(true, false);
    f.map_document(nk(100), nk(11));
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    notice(&mut r).await;
    r.tx.send(HubMsg::Ensure { focus: nk(100) }).await.unwrap();
    let (_, script) = created(&r).await;
    script.batches.send(page()).await.unwrap();
    notice(&mut r).await;
    r.tx.send(HubMsg::Gone { app: app() }).await.unwrap();
    let mut gone = false;
    for _ in 0..3 {
        if matches!(notice(&mut r).await, DocNotice::Gone { .. }) {
            gone = true;
            break;
        }
    }
    assert!(gone);
    until("directory emptied", || r.docs.is_empty()).await;
    assert!(r.join.is_empty());
    r.shutdown.cancel();
    let _ = r.task.await;
}

#[tokio::test]
async fn a_page_load_in_an_app_that_just_lost_its_document_is_adopted() {
    let f = FakeDocFactory::new(true, false);
    f.map_document(nk(100), nk(11));
    f.map_document(nk(200), nk(21));
    let mut r = rig(f.clone());
    start_app(&mut r).await;
    notice(&mut r).await;
    // No document yet: a load-complete in a background tab builds nothing.
    r.tx.send(HubMsg::Loaded { node: nk(200) }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(f.take_created().is_none());
    r.tx.send(HubMsg::Ensure { focus: nk(100) }).await.unwrap();
    let (_, script) = created(&r).await;
    script.batches.send(page()).await.unwrap();
    notice(&mut r).await;
    // The tab navigates: the source ends, then the new page reports load-complete.
    script.end.send(()).await.unwrap();
    assert!(matches!(notice(&mut r).await, DocNotice::Gone { .. }));
    until("directory emptied", || r.docs.is_empty()).await;
    r.tx.send(HubMsg::Loaded { node: nk(200) }).await.unwrap();
    let (root, _) = created(&r).await;
    assert_eq!(root, nk(21));
    r.shutdown.cancel();
    let _ = r.task.await;
}
