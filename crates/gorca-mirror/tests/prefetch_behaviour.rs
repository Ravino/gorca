//! The adopted fill policy (`FocusPrefetch`) through the real shared driver and store, against a
//! small modelled tree: what it asks for, when, and what it leaves alone. No bus, no connector.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_mirror::driver::{DriverCx, FillDriver, FillError, FillReply, IssuedFill};
use gorca_mirror::fill::{AppClass, FillInput, FillOp, FocusPrefetch, HealthView, PrefetchConfig};
use gorca_mirror::{ApplyOrigin, FillLimits, MirrorHandle, MirrorWriter, mirror_for};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeEvent, NodeKey,
    NodeSnapshot, PropMask, Relations, Role, SnapshotBatch, StateSet,
};
use gorca_rt::BudgetClass;

const LIGHT: PropMask = PropMask::NAME
    .union(PropMask::VALUE)
    .union(PropMask::ROLE)
    .union(PropMask::STATES);

fn app() -> AppId {
    AppId {
        connector: ConnectorId(0),
        serial: 1,
    }
}

fn key(i: usize) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(i as u64),
    }
}

struct MNode {
    parent: Option<usize>,
    children: Vec<usize>,
    role: Role,
}

/// app root (0) -> window (1) -> panel (2) -> `rows` buttons, plus a side panel (3) with 5 buttons.
struct Model {
    nodes: Vec<MNode>,
}

impl Model {
    fn new(rows: usize) -> Self {
        let mut m = Model { nodes: Vec::new() };
        m.add(None, Role::Application);
        m.add(Some(0), Role::Window);
        m.add(Some(1), Role::Panel);
        m.add(Some(1), Role::Panel);
        for _ in 0..rows {
            m.add(Some(2), Role::Button);
        }
        for _ in 0..5 {
            m.add(Some(3), Role::Button);
        }
        m
    }

    fn add(&mut self, parent: Option<usize>, role: Role) -> usize {
        self.nodes.push(MNode {
            parent,
            children: Vec::new(),
            role,
        });
        let i = self.nodes.len() - 1;
        if let Some(p) = parent {
            self.nodes[p].children.push(i);
        }
        i
    }

    fn snap(&self, i: usize, have: PropMask, parent: bool, count: bool) -> Arc<NodeSnapshot> {
        let n = &self.nodes[i];
        let mut have = have;
        if parent {
            have |= PropMask::PARENT;
        }
        if count {
            have |= PropMask::CHILDREN;
        }
        Arc::new(NodeSnapshot {
            key: key(i),
            role: n.role,
            states: StateSet::empty(),
            name: Some(Arc::from(format!("n{i}"))),
            description: None,
            value: None,
            parent: if parent { n.parent.map(key) } else { None },
            children: if count {
                ChildList::Count(u32::try_from(n.children.len()).unwrap())
            } else {
                ChildList::Unknown
            },
            // A dump row knows its interfaces (no Value, no Text): its VALUE is not needed.
            interfaces: if have.contains(PropMask::DESCRIPTION) {
                Ifaces::ACTION
            } else {
                Ifaces::empty()
            },
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: None,
            have,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    fn serve(&self, op: &FillOp) -> FillReply {
        let idx = |k: NodeKey| usize::try_from(k.local.0).unwrap();
        let batch = match op {
            FillOp::Fetch { node, want } => {
                vec![self.snap(idx(*node), *want, want.contains(PropMask::PARENT), false)]
            }
            FillOp::ChildSnapshots { node, range, want } => {
                let kids = &self.nodes[idx(*node)].children;
                let (s, e) = (range.start as usize, (range.end as usize).min(kids.len()));
                kids.get(s..e)
                    .unwrap_or(&[])
                    .iter()
                    .map(|c| self.snap(*c, *want, true, want.contains(PropMask::CHILDREN)))
                    .collect()
            }
            FillOp::Siblings {
                parent,
                node,
                before,
                after,
                want,
            } => {
                let kids = &self.nodes[idx(*parent)].children;
                match kids.iter().position(|c| *c == idx(*node)) {
                    None => Vec::new(),
                    Some(at) => {
                        let s = at.saturating_sub(*before as usize);
                        let e = (at + *after as usize + 1).min(kids.len());
                        kids[s..e]
                            .iter()
                            .map(|c| self.snap(*c, *want, true, want.contains(PropMask::CHILDREN)))
                            .collect()
                    }
                }
            }
            FillOp::Ancestry {
                node,
                max_depth,
                want,
            } => {
                let mut v = Vec::new();
                let mut cur = Some(idx(*node));
                while let Some(c) = cur {
                    if v.len() > usize::from(*max_depth) {
                        break;
                    }
                    v.push(self.snap(c, *want, true, want.contains(PropMask::CHILDREN)));
                    cur = self.nodes[c].parent;
                }
                v
            }
            FillOp::Subtree { root, max_nodes } => {
                let mut v = Vec::new();
                let mut q = std::collections::VecDeque::from([idx(*root)]);
                while let Some(c) = q.pop_front() {
                    if v.len() >= *max_nodes as usize {
                        break;
                    }
                    v.push(self.snap(c, LIGHT, true, true));
                    q.extend(self.nodes[c].children.iter().copied());
                }
                v
            }
            FillOp::CacheDump { .. } => (0..self.nodes.len())
                .map(|i| self.snap(i, PropMask::CACHE_ITEM, true, true))
                .collect(),
        };
        FillReply::Ok(SnapshotBatch::new(batch))
    }
}

struct Rig {
    model: Model,
    driver: FillDriver,
    writer: MirrorWriter,
    reader: MirrorHandle,
    now: Instant,
    health: HealthView,
    focus: Option<NodeKey>,
    /// Every op issued so far, with its class.
    log: Vec<(FillOp, BudgetClass)>,
    /// Ops whose reply is withheld (a slow app).
    hold: bool,
    held: Vec<IssuedFill>,
}

impl Rig {
    fn new(rows: usize, dump_complete: bool) -> Rig {
        Rig::with(rows, dump_complete, PrefetchConfig::default())
    }

    fn with(rows: usize, dump_complete: bool, cfg: PrefetchConfig) -> Rig {
        let (writer, reader) = mirror_for(app(), None);
        let mut driver = FillDriver::new(
            app(),
            AppClass::Unknown,
            Box::new(FocusPrefetch::new(cfg)),
            FillLimits::default(),
            None,
        );
        driver.set_dump_complete(dump_complete);
        Rig {
            model: Model::new(rows),
            driver,
            writer,
            reader,
            now: Instant::now(),
            health: HealthView::Healthy,
            focus: None,
            log: Vec::new(),
            hold: false,
            held: Vec::new(),
        }
    }

    /// Serve everything issued, feed the replies back, drain deferred applies; until quiet.
    fn settle(&mut self) {
        for _ in 0..64 {
            let issued = self.driver.take_issued();
            if issued.is_empty() && !self.driver.has_work() {
                return;
            }
            for f in issued {
                self.log.push((f.op.clone(), f.class));
                if self.hold {
                    self.held.push(f);
                    continue;
                }
                let reply = self.model.serve(&f.op);
                let cx = DriverCx {
                    now: self.now,
                    health: self.health,
                    mirror: &self.reader,
                    focus: self.focus,
                };
                self.driver
                    .on_reply(&cx, &mut self.writer, f.id, f.issued_at, reply);
            }
            let cx = DriverCx {
                now: self.now,
                health: self.health,
                mirror: &self.reader,
                focus: self.focus,
            };
            self.driver.poll_timers(&cx, &mut self.writer);
        }
    }

    fn input(&mut self, input: FillInput<'_>) {
        let cx = DriverCx {
            now: self.now,
            health: self.health,
            mirror: &self.reader,
            focus: self.focus,
        };
        self.driver.on_input(&cx, &mut self.writer, input);
        self.settle();
    }

    fn advance(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        self.input(FillInput::Tick);
    }

    /// The Reader's cold path: a focus fetch without parent link, then the settle.
    fn focus_on(&mut self, i: usize) {
        self.focus = Some(key(i));
        self.writer.set_focus(Some(key(i)));
        if self.reader.view(key(i)).hit().is_none() {
            let snap = self.model.snap(i, LIGHT, false, false);
            let seq = self.writer.next_seq();
            self.writer.apply(
                SnapshotBatch::new(vec![snap]),
                ApplyOrigin::Reply { issued_at: seq },
            );
        }
        self.input(FillInput::FocusSettled { node: key(i) });
    }

    fn count(&self, f: impl Fn(&FillOp) -> bool) -> usize {
        self.log.iter().filter(|(o, _)| f(o)).count()
    }

    /// Every `Siblings` request for `parent`, in order: (node, before, after).
    fn all_siblings(&self, parent: usize) -> Vec<(NodeKey, u32, u32)> {
        self.log
            .iter()
            .filter_map(|(o, _)| match o {
                FillOp::Siblings {
                    parent: p,
                    node,
                    before,
                    after,
                    ..
                } if *p == key(parent) => Some((*node, *before, *after)),
                _ => None,
            })
            .collect()
    }

    /// The windows and run extensions (not the look-ahead re-reads of the next few siblings).
    fn windows(&self, parent: usize) -> Vec<(NodeKey, u32, u32)> {
        self.all_siblings(parent)
            .into_iter()
            .filter(|(_, b, a)| !is_lookahead(*b, *a))
            .collect()
    }

    /// The look-ahead re-reads: nothing before the anchor, at most a few siblings after it.
    fn lookaheads(&self, parent: usize) -> Vec<(NodeKey, u32, u32)> {
        self.all_siblings(parent)
            .into_iter()
            .filter(|(_, b, a)| is_lookahead(*b, *a))
            .collect()
    }

    fn calls(&self) -> usize {
        self.log.len()
    }
}

fn is_lookahead(before: u32, after: u32) -> bool {
    before == 0 && after < 6
}

fn hit(r: &Rig, i: usize) -> bool {
    r.reader
        .view(key(i))
        .hit()
        .is_some_and(|v| v.satisfies(LIGHT))
}

use gorca_mirror::MirrorRead;

const PANEL: usize = 2;
const fn row(i: usize) -> usize {
    4 + i
}

#[test]
fn a_first_focus_asks_for_its_ancestry_and_one_window_then_a_walk_along_it_is_free() {
    let mut r = Rig::new(100, false);
    r.focus_on(row(70));
    // The focus fetch had no parent link: the ancestry brings it, with the ancestors.
    assert_eq!(r.count(|o| matches!(o, FillOp::Ancestry { .. })), 1);
    // One request, no probing: the connector finds the index in its own child-list reply.
    assert_eq!(r.windows(PANEL), vec![(key(row(70)), 1, 6)]);
    for i in 69..=76 {
        assert!(hit(&r, row(i)), "row {i} is in the window");
    }
    assert!(!hit(&r, row(68)) && !hit(&r, row(77)));
    for i in 71..=73 {
        r.advance(100);
        r.focus_on(row(i));
    }
    assert_eq!(
        r.windows(PANEL).len(),
        1,
        "moving along a fresh run asks for no new window"
    );
    assert!(
        r.log.iter().all(|(_, c)| *c == BudgetClass::Prefetch),
        "only Prefetch"
    );
    assert_eq!(
        r.count(|o| matches!(
            o,
            FillOp::Subtree { .. } | FillOp::ChildSnapshots { .. } | FillOp::CacheDump { .. }
        )),
        0,
        "no crawl, no page probing, no dump"
    );
}

#[test]
fn near_the_open_edge_the_run_grows_once_in_that_direction() {
    let mut r = Rig::new(100, false);
    r.focus_on(row(40)); // window rows 39..=46
    r.advance(100);
    r.focus_on(row(44)); // idx 5 of 8: three from the end
    assert_eq!(
        r.windows(PANEL).last().copied(),
        Some((key(row(46)), 0, 6)),
        "grown forward past the last key"
    );
    assert!(hit(&r, row(52)));
    let n = r.windows(PANEL).len();
    r.advance(100);
    r.focus_on(row(45));
    assert_eq!(r.windows(PANEL).len(), n, "asked once, then mirrored");
    // Backwards: the first key is row 39; the focus on it is at the edge.
    r.advance(100);
    r.focus_on(row(39));
    assert_eq!(
        r.windows(PANEL).last().copied(),
        Some((key(row(39)), 6, 0)),
        "grown backwards before the first key"
    );
    assert!(hit(&r, row(33)));
}

#[test]
fn a_run_that_reaches_a_list_end_never_grows_past_it() {
    let mut r = Rig::new(40, false);
    r.focus_on(row(1)); // window rows 0..=9, the start of the list
    r.advance(100);
    r.focus_on(row(0));
    r.advance(100);
    r.focus_on(row(2));
    assert!(
        r.windows(PANEL)
            .iter()
            .all(|(_, b, a)| !(*b == 8 && *a == 0)),
        "no backward growth at the start: {:?}",
        r.windows(PANEL)
    );
    let mut r = Rig::new(40, false);
    r.focus_on(row(38)); // rows 36..=39: the end
    r.advance(100);
    r.focus_on(row(39));
    assert_eq!(r.windows(PANEL).len(), 1, "no forward growth at the end");
}

#[test]
fn a_run_is_not_refetched_by_age_only_the_next_few_siblings_are_re_read() {
    let mut r = Rig::new(40, false);
    r.focus_on(row(10));
    let base = r.windows(PANEL).len();
    r.advance(20_000);
    r.focus_on(row(11));
    r.advance(20_000);
    r.focus_on(row(12));
    assert_eq!(r.windows(PANEL).len(), base, "no age-based window refetch");
    // What was re-read is the stretch ahead of the focus, nothing else.
    let la = r.lookaheads(PANEL);
    assert!(!la.is_empty(), "the siblings ahead were re-read");
    assert!(la.iter().all(|(_, b, a)| *b == 0 && *a <= 2), "{la:?}");
    // A standing focus costs nothing more.
    let n = r.calls();
    r.advance(20_000);
    r.advance(20_000);
    assert_eq!(r.calls(), n);
}

#[test]
fn a_dirty_run_is_refetched_after_the_debounce_once() {
    let mut r = Rig::new(40, false);
    r.focus_on(row(10));
    let base = r.windows(PANEL).len();
    r.advance(1_000);
    r.input(FillInput::Events(&[NodeEvent::ChildrenChanged {
        node: key(PANEL),
    }]));
    assert_eq!(r.windows(PANEL).len(), base, "debounced: not at once");
    r.advance(400);
    assert_eq!(r.windows(PANEL).len(), base + 1, "refetched proactively");
    r.focus_on(row(11));
    assert_eq!(r.windows(PANEL).len(), base + 1, "and not again");
    // A structure event on a parent without a run costs nothing.
    let n = r.calls();
    r.input(FillInput::Events(&[NodeEvent::ChildrenChanged {
        node: key(3),
    }]));
    r.advance(1_000);
    assert_eq!(r.calls(), n);
}

#[test]
fn a_stale_name_ahead_of_the_focus_is_re_read_but_one_far_away_is_left() {
    let mut r = Rig::new(20, false);
    r.focus_on(row(1));
    let base = r.windows(PANEL).len();
    r.writer.invalidate(key(row(3)), PropMask::NAME, r.now);
    r.advance(100);
    r.focus_on(row(2));
    assert_eq!(r.windows(PANEL).len(), base, "a re-read, not a window");
    assert!(
        r.lookaheads(PANEL)
            .iter()
            .any(|(n, _, _)| *n == key(row(3))),
        "{:?}",
        r.lookaheads(PANEL)
    );
    assert!(hit(&r, row(3)), "the re-read confirmed it");
    // Far from the focus a stale sibling is not worth a request.
    r.writer.invalidate(key(row(9)), PropMask::NAME, r.now);
    let n = r.all_siblings(PANEL).len();
    r.advance(100);
    r.focus_on(row(3));
    assert!(
        r.all_siblings(PANEL)[n..]
            .iter()
            .all(|(node, _, _)| *node != key(row(9)))
    );
}

#[test]
fn a_node_that_left_its_parent_gets_no_run_and_no_retry_loop() {
    let mut r = Rig::new(20, false);
    r.model.nodes[PANEL].children.retain(|c| *c != row(5));
    r.focus_on(row(5));
    assert_eq!(r.windows(PANEL).len(), 1);
    for _ in 0..10 {
        r.advance(200);
    }
    assert_eq!(r.windows(PANEL).len(), 1, "the empty reply is not retried");
}

#[test]
fn the_shell_dump_is_off_by_default_and_a_verified_app_is_prefetched_lazily() {
    let mut r = Rig::new(30, true);
    r.input(FillInput::Appeared { root: key(0) });
    r.input(FillInput::Ready);
    assert_eq!(r.calls(), 0, "nothing at Appeared or Ready");
    r.focus_on(row(2));
    r.advance(2_000);
    assert_eq!(r.count(|o| matches!(o, FillOp::CacheDump { .. })), 0);
    assert_eq!(r.windows(PANEL).len(), 1, "lazy prefetch as everywhere");
}

#[test]
fn with_the_flag_a_verified_app_is_dumped_once_after_a_quiet_focus() {
    let mut r = Rig::with(30, true, PrefetchConfig { shell_dump: true });
    r.input(FillInput::Appeared { root: key(0) });
    r.input(FillInput::Ready);
    assert_eq!(r.calls(), 0, "nothing at Appeared or Ready");
    r.focus_on(row(2));
    assert_eq!(r.windows(PANEL).len(), 0, "lazy until the dump is in");
    r.advance(300);
    assert_eq!(r.count(|o| matches!(o, FillOp::CacheDump { .. })), 0);
    r.advance(300);
    assert_eq!(r.count(|o| matches!(o, FillOp::CacheDump { .. })), 1);
    assert!(
        r.log
            .iter()
            .any(|(o, c)| matches!(o, FillOp::CacheDump { .. }) && *c == BudgetClass::Background)
    );
    let before = r.calls();
    r.advance(100);
    r.focus_on(row(20));
    r.advance(2_000);
    assert_eq!(
        r.count(|o| matches!(o, FillOp::CacheDump { .. })),
        1,
        "never again: {:?}",
        r.log
    );
    assert_eq!(r.calls(), before, "the dump covers the node");
}

#[test]
fn an_ineligible_app_is_never_dumped_and_a_refuted_flag_stops_it() {
    let cfg = PrefetchConfig { shell_dump: true };
    let mut r = Rig::with(30, false, cfg);
    r.focus_on(row(2));
    r.advance(2_000);
    assert_eq!(r.count(|o| matches!(o, FillOp::CacheDump { .. })), 0);
    let mut r = Rig::with(30, true, cfg);
    r.focus_on(row(2));
    r.driver.set_dump_complete(false);
    r.advance(1_000);
    assert_eq!(r.count(|o| matches!(o, FillOp::CacheDump { .. })), 0);
}

#[test]
fn nothing_is_asked_of_a_slow_or_hung_app_beyond_what_health_allows() {
    for health in [HealthView::Hung, HealthView::Slow] {
        let mut r = Rig::new(40, true);
        r.health = health;
        r.focus_on(row(3));
        r.advance(1_000);
        r.input(FillInput::WindowActivated { window: key(1) });
        r.input(FillInput::Events(&[NodeEvent::ChildrenChanged {
            node: key(PANEL),
        }]));
        r.advance(1_000);
        assert!(
            r.log
                .iter()
                .all(|(_, c)| *c == BudgetClass::Interactive && health == HealthView::Slow),
            "{health:?}: {:?}",
            r.log
        );
    }
}

#[test]
fn a_failure_backs_non_interactive_work_off() {
    let mut r = Rig::new(40, false);
    r.hold = true;
    r.focus_on(row(3));
    let held = std::mem::take(&mut r.held);
    assert!(!held.is_empty());
    r.hold = false;
    for f in held {
        let cx = DriverCx {
            now: r.now,
            health: r.health,
            mirror: &r.reader,
            focus: r.focus,
        };
        let (id, at) = (f.id, f.issued_at);
        r.driver.on_reply(
            &cx,
            &mut r.writer,
            id,
            at,
            FillReply::Err(FillError::Timeout),
        );
    }
    let before = r.calls();
    r.advance(100);
    r.focus_on(row(30));
    assert_eq!(r.calls(), before, "within the backoff nothing is sent");
    r.advance(1_500);
    r.focus_on(row(31));
    assert!(r.calls() > before, "after it, prefetching resumes");
}

#[test]
fn a_renamed_focus_is_refetched_at_a_bounded_rate_a_state_change_is_the_readers() {
    let mut r = Rig::new(10, false);
    r.focus_on(row(1));
    // The focus rename re-read asks for exactly the light fields (the parent-link read adds PARENT).
    let n = |r: &Rig| r.count(|o| matches!(o, FillOp::Fetch { want, .. } if *want == LIGHT));
    assert_eq!(n(&r), 0);
    r.input(FillInput::Events(&[NodeEvent::PropertiesChanged {
        node: key(row(1)),
        mask: PropMask::STATES,
    }]));
    assert_eq!(n(&r), 0, "S1 is the Reader's");
    let rename = [NodeEvent::PropertiesChanged {
        node: key(row(1)),
        mask: PropMask::NAME,
    }];
    r.input(FillInput::Events(&rename));
    assert_eq!(n(&r), 1);
    assert!(
        r.log
            .iter()
            .any(|(o, c)| matches!(o, FillOp::Fetch { .. }) && *c == BudgetClass::Interactive)
    );
    // A flood of renames within the gap is one more fetch, not one per batch.
    for _ in 0..20 {
        r.input(FillInput::Events(&rename));
    }
    assert_eq!(n(&r), 1, "held back by the gap");
    r.advance(150);
    assert_eq!(n(&r), 2, "one deferred refetch once the gap passed");
}

#[test]
fn a_window_activation_refreshes_the_focus_run_once_when_it_is_old() {
    let mut r = Rig::new(40, false);
    r.focus_on(row(10));
    let base = r.windows(PANEL).len();
    r.input(FillInput::WindowActivated { window: key(1) });
    assert_eq!(
        r.windows(PANEL).len(),
        base,
        "just fetched: not again {:?}",
        r.log
    );
    r.advance(3_000);
    r.input(FillInput::WindowActivated { window: key(1) });
    assert_eq!(r.windows(PANEL).len(), base + 1);
}

#[test]
fn an_idle_app_with_nothing_pending_sets_no_timer() {
    let mut r = Rig::new(10, false);
    r.focus_on(row(1));
    r.advance(1_000);
    r.advance(1_000);
    let n = r.calls();
    for _ in 0..20 {
        r.advance(1_000);
    }
    assert_eq!(r.calls(), n);
    assert!(r.driver.next_wake().is_none() || !r.driver.has_work());
}

#[test]
fn where_am_i_on_an_unmirrored_chain_asks_for_the_ancestry_interactively() {
    let mut r = Rig::new(10, false);
    r.focus_on(row(1));
    r.input(FillInput::Demand {
        node: key(row(2)),
        why: gorca_mirror::fill::DemandWhy::WhereAmI,
        outcome: gorca_mirror::fill::DemandOutcome::Miss,
    });
    assert!(r.log.iter().any(
        |(o, c)| matches!(o, FillOp::Ancestry { node, .. } if *node == key(row(2)))
            && *c == BudgetClass::Interactive
    ));
}

#[test]
fn the_policy_state_is_bounded_however_many_parents_the_focus_visits() {
    let mut r = Rig::new(10, false);
    // 300 parents, each with a few children.
    let mut firsts = Vec::new();
    for _ in 0..300 {
        let p = r.model.add(Some(1), Role::Panel);
        let mut f = 0;
        for k in 0..4 {
            let c = r.model.add(Some(p), Role::Button);
            if k == 0 {
                f = c;
            }
        }
        firsts.push(f);
    }
    for f in firsts {
        r.advance(10);
        r.focus_on(f);
    }
    assert!(r.driver.policy_memory_bytes() < 256 * 1024);
}

#[test]
fn a_long_forward_walk_is_served_from_the_mirror_in_few_requests_and_bounded_state() {
    let mut r = Rig::new(300, false);
    r.focus_on(row(0));
    let mut misses = 0;
    for i in 1..300 {
        r.advance(50);
        // Leaving a node invalidates its `focused` state (an event): not a reason to refetch.
        r.writer
            .invalidate(key(row(i - 1)), PropMask::STATES, r.now);
        // What the Reader would find in the mirror when the focus lands.
        if !hit(&r, row(i)) {
            misses += 1;
        }
        r.focus_on(row(i));
    }
    assert!(misses <= 2, "{misses} misses");
    assert!(
        r.count(|o| matches!(o, FillOp::Ancestry { .. })) <= 2,
        "the chain is asked for once, not per focus"
    );
    let windows = r.windows(PANEL).len();
    assert!(
        (40..=70).contains(&windows),
        "300 rows in steps of 6: {windows} requests"
    );
    // The run is capped: the memory stays small however far the walk went.
    assert!(r.driver.policy_memory_bytes() < 64 * 1024);
    // Walking back finds the dropped part by asking again.
    let before = r.windows(PANEL).len();
    for i in (150..=160).rev() {
        r.advance(50);
        r.focus_on(row(i));
    }
    assert!(r.windows(PANEL).len() > before);
}

#[test]
fn a_dirty_parent_whose_focus_is_gone_is_re_read_once_and_its_run_dropped() {
    let mut r = Rig::new(20, false);
    r.focus_on(row(5));
    let fetches =
        |r: &Rig| r.count(|o| matches!(o, FillOp::Fetch { node, .. } if *node == key(PANEL)));
    assert_eq!(fetches(&r), 0);
    // The focused row is removed (a subtree replace): the focus run has nothing to anchor on.
    r.writer
        .tombstone(key(row(5)), r.now, gorca_mirror::RemovalCause::Event);
    r.input(FillInput::Events(&[NodeEvent::ChildrenChanged {
        node: key(PANEL),
    }]));
    r.advance(400);
    assert_eq!(fetches(&r), 1, "the parent is re-read: {:?}", r.log);
    r.advance(1_000);
    assert_eq!(fetches(&r), 1, "once");
}
