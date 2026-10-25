// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin port: PF-2, PF-4, PF-7, strata by tier, the wait governor and the displaced-job
//! rule (a busy plugin's newest job is never skipped).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::field_reassign_with_default
)]

use std::sync::{Arc, Mutex};

use super::*;

#[derive(Default)]
struct Shared {
    jobs: Vec<(bool, Job)>,
    replies: Vec<Reply>,
    waits: Vec<bool>,
}

struct Fake {
    metas: Vec<PluginMeta>,
    sh: Arc<Mutex<Shared>>,
}

impl ScopeRunner for Fake {
    fn plugins(&self) -> &[PluginMeta] {
        &self.metas
    }
    fn watches(&self, row: &Row) -> Vec<(usize, u16)> {
        // Every plugin watches nodes with class token `watch`.
        if row
            .class
            .as_deref()
            .is_some_and(|c| c.split_whitespace().any(|x| x == "watch"))
        {
            (0..self.metas.len()).map(|p| (p, 1)).collect()
        } else {
            Vec::new()
        }
    }
    fn interactive(&mut self, _t0: u64, wait: bool, jobs: Vec<Job>) -> Handoff {
        let mut s = self.sh.lock().unwrap();
        s.waits.push(wait);
        let n = jobs.len() as u32;
        s.jobs.extend(jobs.into_iter().map(|j| (true, j)));
        Handoff {
            asked: n,
            ..Handoff::default()
        }
    }
    fn background(&mut self, jobs: Vec<Job>) {
        self.sh
            .lock()
            .unwrap()
            .jobs
            .extend(jobs.into_iter().map(|j| (false, j)));
    }
    fn poll(&mut self, out: &mut Vec<Reply>) {
        out.append(&mut self.sh.lock().unwrap().replies);
    }
    fn outstanding(&self) -> bool {
        false
    }
}

struct Flat {
    kids: HashMap<u32, Vec<u32>>,
}

impl Tree for Flat {
    fn parent(&self, n: u32) -> Option<u32> {
        self.kids
            .iter()
            .find(|(_, c)| c.contains(&n))
            .map(|(p, _)| *p)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.kids.get(&n).cloned().unwrap_or_default()
    }
}

fn row(name: &str, class: &str) -> Row {
    Row {
        role: SemRole::Button.code(),
        name: Some(Arc::from(name)),
        class: (!class.is_empty()).then(|| Arc::from(class)),
        ..Row::default()
    }
}

fn bridge(tiers: &[PluginTier]) -> (PluginBridge, Arc<Mutex<Shared>>) {
    let sh = Arc::new(Mutex::new(Shared::default()));
    let metas = tiers
        .iter()
        .enumerate()
        .map(|(i, t)| PluginMeta {
            name: Arc::from(format!("p{i}")),
            tier: *t,
        })
        .collect();
    let runner = Fake {
        metas,
        sh: Arc::clone(&sh),
    };
    (
        PluginBridge::new(
            Box::new(runner),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        ),
        sh,
    )
}

fn tree() -> Flat {
    let mut kids = HashMap::new();
    kids.insert(0, vec![1, 2]);
    kids.insert(1, vec![3]);
    Flat { kids }
}

fn name_effect(node: u32, text: &str, patch: bool) -> FactEffect {
    FactEffect {
        node,
        group: Group::Name.group_code().0,
        value: Some(FactValue::Text(Arc::from(text))),
        patch,
    }
}

fn done(plugin: usize, t: u64, target: u32, effects: Vec<FactEffect>, reads: Vec<u32>) -> Reply {
    Reply {
        plugin,
        t,
        targets: vec![target],
        body: ReplyBody::Done {
            effects,
            reads,
            overflow: false,
        },
    }
}

fn populate(b: &mut PluginBridge) {
    b.upsert(0, row("root", ""));
    b.upsert(1, row("chat", "watch"));
    b.upsert(2, row("other", ""));
    b.upsert(3, row("child text", ""));
}

#[test]
fn a_watched_row_is_derived_in_the_background_and_a_first_party_patch_is_s1() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    assert_eq!(b.pending_len(), 1, "only the watched node is dirty");
    b.background(&tree());
    let (interactive, job) = sh.lock().unwrap().jobs[0].clone();
    assert!(!interactive);
    assert_eq!(job.nodes.len(), 1);
    assert_eq!(job.nodes[0].node, 1);
    assert_eq!(job.nodes[0].cause, cause::ATTACH);
    // The snapshot holds the target, its ancestors and its subtree.
    for n in [0, 1, 3] {
        assert!(job.rows.rows.contains_key(&n), "node {n}");
    }
    sh.lock().unwrap().replies.push(done(
        0,
        job.t,
        1,
        vec![name_effect(1, "Alice, 1 unread", true)],
        vec![1, 3],
    ));
    assert!(b.drain());
    let ops = b.take_ops();
    assert!(matches!(
        ops.as_slice(),
        [FactOp::Patch {
            node: 1,
            taint: TrustTier::T0Core,
            ..
        }]
    ));
    assert_eq!(b.live_len(), 1);
    // PF-4: admitting a fact enqueued nothing.
    assert_eq!(b.pending_len(), 0);
    b.background(&tree());
    assert_eq!(
        sh.lock().unwrap().jobs.len(),
        1,
        "no job followed the admission"
    );
}

#[test]
fn strata_follow_the_tier() {
    for (tier, patch, want) in [
        (PluginTier::FirstParty, true, "patch"),
        (PluginTier::TrustedThirdParty, true, "patch-tainted"),
        (PluginTier::Untrusted, true, "inferred"),
        (PluginTier::FirstParty, false, "inferred"),
    ] {
        let (mut b, sh) = bridge(&[tier]);
        populate(&mut b);
        b.background(&tree());
        let t = sh.lock().unwrap().jobs[0].1.t;
        sh.lock()
            .unwrap()
            .replies
            .push(done(0, t, 1, vec![name_effect(1, "x", patch)], vec![1]));
        b.drain();
        let got = match b.take_ops().as_slice() {
            [
                FactOp::Patch {
                    taint: TrustTier::T0Core,
                    ..
                },
            ] => "patch",
            [
                FactOp::Patch {
                    taint: TrustTier::T3Untrusted,
                    ..
                },
            ] => "patch-tainted",
            [FactOp::Inferred { .. }] => "inferred",
            other => panic!("{other:?}"),
        };
        assert_eq!(got, want, "{tier:?} patch={patch}");
    }
}

#[test]
fn a_reply_for_a_view_that_changed_is_dropped_and_rederived() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.begin_turn();
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    // The child text changes after the snapshot turn.
    b.begin_turn();
    b.upsert(3, row("new text", ""));
    sh.lock()
        .unwrap()
        .replies
        .push(done(0, t, 1, vec![name_effect(1, "old", true)], vec![1, 3]));
    b.drain();
    assert!(
        b.take_ops().is_empty(),
        "a stale reply admits nothing (PF-7)"
    );
    assert_eq!(b.stats().stale_dropped, 1);
    assert_eq!(b.pending_len(), 1, "the target is re-derived");
}

#[test]
fn a_change_of_a_read_node_retracts_and_rederives_only_that_target() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock()
        .unwrap()
        .replies
        .push(done(0, t, 1, vec![name_effect(1, "v1", true)], vec![1, 3]));
    b.drain();
    b.take_ops();
    // An unrelated node changes: nothing happens (PF-4).
    b.begin_turn();
    b.upsert(2, row("other changed", ""));
    assert_eq!(b.pending_len(), 0);
    assert!(b.take_ops().is_empty());
    // A node the call read changes: the fact is retracted, the target is dirty.
    b.begin_turn();
    b.upsert(3, row("child text 2", ""));
    assert_eq!(b.pending_len(), 1);
    assert!(matches!(
        b.take_ops().as_slice(),
        [FactOp::Retract {
            node: 1,
            patch: true,
            ..
        }]
    ));
    assert_eq!(b.live_len(), 0);
    // An identical row is not a change.
    b.begin_turn();
    assert!(!b.upsert(3, row("child text 2", "")));
}

#[test]
fn a_plugin_sees_only_observed_rows_never_another_plugins_patch() {
    // PF-2: plugin 0's patch is applied to the engine, never to the rows the snapshot copies.
    let (mut b, sh) = bridge(&[PluginTier::FirstParty, PluginTier::FirstParty]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock().unwrap().replies.push(done(
        0,
        t,
        1,
        vec![name_effect(1, "patched by p0", true)],
        vec![1],
    ));
    b.drain();
    assert!(!b.take_ops().is_empty());
    b.begin_turn();
    b.upsert(3, row("child changed", ""));
    b.background(&tree());
    let s = sh.lock().unwrap();
    let last = &s.jobs.last().unwrap().1;
    let seen = last.rows.rows.get(&1).unwrap().row.name.as_deref();
    assert_eq!(seen, Some("chat"), "the observed name, not the patch");
}

#[test]
fn secret_nodes_are_absent_from_snapshots() {
    let (mut b, sh) = bridge(&[PluginTier::Untrusted]);
    b.upsert(0, row("root", ""));
    b.upsert(1, row("chat", "watch"));
    let mut pw = row("pw", "");
    pw.secret = true;
    b.upsert(3, pw);
    b.background(&tree());
    let s = sh.lock().unwrap();
    assert!(!s.jobs[0].1.rows.rows.contains_key(&3));
}

#[test]
fn focus_derives_only_nodes_without_a_live_fact_and_waits_once_per_path() {
    let gov = Arc::new(Governor::default());
    let sh = Arc::new(Mutex::new(Shared::default()));
    let mk = |sh: &Arc<Mutex<Shared>>| {
        PluginBridge::new(
            Box::new(Fake {
                metas: vec![PluginMeta {
                    name: Arc::from("p0"),
                    tier: PluginTier::FirstParty,
                }],
                sh: Arc::clone(sh),
            }),
            Arc::clone(&gov),
            ScopeSpec::default(),
        )
    };
    // Two writers of one keypress path (an app actor and a document actor).
    let mut app = mk(&sh);
    let mut doc = mk(&sh);
    for b in [&mut app, &mut doc] {
        populate(b);
    }
    let t0 = 10_000_000;
    app.interactive(&[1, 0], &tree(), t0);
    doc.interactive(&[1, 0], &tree(), t0 + 50_000);
    assert_eq!(
        sh.lock().unwrap().waits,
        vec![true, false],
        "one wait per path"
    );
    // A later keypress, outside the window, waits again (once the writer's own limit passed).
    app.begin_turn();
    app.upsert(3, row("changed", ""));
    app.interactive(&[1], &tree(), t0 + 100_000_000);
    assert_eq!(sh.lock().unwrap().waits.last(), Some(&true));
    // A flapping focus: the same writer asks again 1 ms later and does not wait.
    let (mut b, sh2) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.interactive(&[1], &tree(), 50_000_000);
    b.begin_turn();
    b.upsert(3, row("changed", ""));
    b.interactive(&[1], &tree(), 51_000_000);
    assert_eq!(sh2.lock().unwrap().waits, vec![true, false]);
}

#[test]
fn a_displaced_or_skipped_job_is_requeued_not_lost() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.interactive(&[1], &tree(), 1_000_000);
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock().unwrap().replies.push(Reply {
        plugin: 0,
        t,
        targets: vec![1],
        body: ReplyBody::Failed { requeue: true },
    });
    b.drain();
    assert_eq!(b.pending_len(), 1, "the displaced job's node runs next");
    assert_eq!(b.stats().requeued, 1);
    b.background(&tree());
    assert_eq!(sh.lock().unwrap().jobs.len(), 2);
}

#[test]
fn masking_removes_the_plugins_facts_and_unmasking_rederives() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock()
        .unwrap()
        .replies
        .push(done(0, t, 1, vec![name_effect(1, "x", true)], vec![1]));
    b.drain();
    b.take_ops();
    sh.lock().unwrap().replies.push(Reply {
        plugin: 0,
        t: 0,
        targets: vec![],
        body: ReplyBody::Masked,
    });
    b.drain();
    assert!(matches!(
        b.take_ops().as_slice(),
        [FactOp::Retract { .. }, FactOp::Mask { plugin: 0 }]
    ));
    assert_eq!(b.live_len(), 0);
    sh.lock().unwrap().replies.push(Reply {
        plugin: 0,
        t: 0,
        targets: vec![],
        body: ReplyBody::Unmasked,
    });
    b.drain();
    assert_eq!(b.pending_len(), 1);
}

#[test]
fn computed_ops_reach_the_engine_and_leave_with_the_mask() {
    use gorca_fuse::{Budget, Input, NoPatches, NoQuirks, PrimarySlot, Profile, Rules};
    let mut ivm = Ivm::new(
        Profile::new(&[PrimarySlot::P0], Rules::aria()),
        NoQuirks,
        NoPatches,
    );
    ivm.add_node(NodeIdx(1), None).unwrap();
    ivm.apply([
        Input::Life {
            node: NodeIdx(1),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        },
        Input::Obs {
            node: NodeIdx(1),
            src: PrimarySlot::P0,
            seq: 1,
            write: Write::Val(Val::Name(Arc::from("observed"))),
        },
    ]);
    ivm.run(Budget::UNLIMITED);
    let metas = vec![
        PluginMeta {
            name: Arc::from("fp"),
            tier: PluginTier::FirstParty,
        },
        PluginMeta {
            name: Arc::from("tp"),
            tier: PluginTier::Untrusted,
        },
    ];
    let mut src = PluginSources::register(&mut ivm, &metas);
    let name = |ivm: &Ivm<NoQuirks, NoPatches>| {
        ivm.entry_view(NodeIdx(1))
            .unwrap()
            .name
            .as_deref()
            .map(str::to_owned)
    };
    src.apply(
        &mut ivm,
        vec![FactOp::Patch {
            node: 1,
            plugin: Arc::from("fp"),
            write: Write::Val(Val::Name(Arc::from("patched"))),
            taint: TrustTier::T0Core,
        }],
    );
    ivm.run(Budget::UNLIMITED);
    assert_eq!(name(&ivm).as_deref(), Some("patched"));
    src.apply(&mut ivm, vec![FactOp::Mask { plugin: 0 }]);
    ivm.run(Budget::UNLIMITED);
    assert_eq!(name(&ivm).as_deref(), Some("observed"));
    // An S4 fact never overrides an observed name (it only fills).
    src.apply(
        &mut ivm,
        vec![FactOp::Inferred {
            node: 1,
            plugin: 1,
            write: Write::Val(Val::Name(Arc::from("inferred"))),
        }],
    );
    ivm.run(Budget::UNLIMITED);
    assert_eq!(name(&ivm).as_deref(), Some("observed"));
}

#[test]
fn identity_is_the_executable_and_only_that_is_verified() {
    let name: Arc<str> = Arc::from("web.telegram.org");
    // A self-reported name is never evidence.
    let (app, verified) = scope_identity(&name, None);
    assert_eq!((&*app, verified), ("web.telegram.org", false));
    let (app, verified) = scope_identity(&name, Some(&Arc::from("")));
    assert!(!verified && &*app == "web.telegram.org");
    // With the executable from the bus credentials the identity is the executable.
    let (app, verified) = scope_identity(&name, Some(&Arc::from("firefox-esr")));
    assert_eq!((&*app, verified), ("firefox-esr", true));
    let (app, verified) = scope_identity(&name, Some(&Arc::from("flatpak:org.mozilla.firefox")));
    // A flatpak id names the application for first-party plugins but is never verified.
    assert_eq!((&*app, verified), ("flatpak:org.mozilla.firefox", false));
    let (app, verified) = scope_identity(&name, Some(&Arc::from("snap:chromium")));
    assert_eq!((&*app, verified), ("snap:chromium", true));
}

#[test]
fn a_secret_node_is_never_watched_derived_or_given_a_fact() {
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    b.upsert(0, row("root", ""));
    // A password field with a class the plugin watches: no watch, no job, however often it changes.
    let mut pw = row("Password", "watch");
    pw.secret = true;
    b.upsert(1, pw.clone());
    b.begin_turn();
    pw.text = Some(Arc::from("abc"));
    b.upsert(1, pw);
    assert_eq!(b.pending_len(), 0, "a secret node is not dirty");
    b.interactive(&[1], &tree(), 1_000_000);
    b.background(&tree());
    assert!(
        sh.lock().unwrap().jobs.is_empty(),
        "no job for a secret focus"
    );

    // A node that becomes secret after it was derived loses its fact and its token.
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock()
        .unwrap()
        .replies
        .push(done(0, t, 1, vec![name_effect(1, "v", true)], vec![1]));
    b.drain();
    b.take_ops();
    assert_eq!(b.live_len(), 1);
    b.begin_turn();
    let mut now_secret = row("chat", "watch");
    now_secret.secret = true;
    b.upsert(1, now_secret);
    assert_eq!(b.live_len(), 0);
    assert!(matches!(
        b.take_ops().as_slice(),
        [FactOp::Retract {
            node: 1,
            patch: true,
            ..
        }]
    ));

    // A reply naming a secret node is dropped whole.
    let (mut b, sh) = bridge(&[PluginTier::FirstParty]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    let mut pw = row("pw", "");
    pw.secret = true;
    b.upsert(3, pw);
    // The call read node 3 before it turned secret; its fact about 3 must not be admitted.
    sh.lock().unwrap().replies.push(done(
        0,
        t + 1,
        1,
        vec![name_effect(3, "leak", true), name_effect(1, "ok", true)],
        vec![1, 3],
    ));
    b.drain();
    let ops = b.take_ops();
    assert!(
        ops.iter()
            .all(|o| !matches!(o, FactOp::Patch { node: 3, .. })),
        "{ops:?}"
    );
    assert_eq!(b.stats().secret_dropped, 1);
}

#[test]
fn masking_retracts_the_facts_so_an_unmask_cannot_bring_back_stale_ones() {
    let (mut b, sh) = bridge(&[PluginTier::Untrusted]);
    populate(&mut b);
    b.background(&tree());
    let t = sh.lock().unwrap().jobs[0].1.t;
    sh.lock()
        .unwrap()
        .replies
        .push(done(0, t, 1, vec![name_effect(1, "s4", false)], vec![1]));
    b.drain();
    assert_eq!(b.take_ops().len(), 1);
    sh.lock().unwrap().replies.push(Reply {
        plugin: 0,
        t: 0,
        targets: vec![],
        body: ReplyBody::Masked,
    });
    b.drain();
    let ops = b.take_ops();
    assert!(
        matches!(
            ops.as_slice(),
            [
                FactOp::Retract {
                    node: 1,
                    patch: false,
                    ..
                },
                FactOp::Mask { .. }
            ]
        ),
        "{ops:?}"
    );
}
