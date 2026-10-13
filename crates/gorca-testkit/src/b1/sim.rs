//! `SimSut`: SPEC section 5 as an executable reference reader, in virtual time.
//!
//! The reader here is deliberately small and literal: the invalidation table of 5.1, the read
//! rule of 5.2, RV-1 of 5.3, the ordering rules LR-1..LR-4 of 5.4, the focus rule of 5.5 and S1
//! of 5.6, over a plain `HashMap` mirror. Fake apps are the real [`World`]s with a serial
//! service-time queue, so the timing races of the conformance cases (event before reply, reply
//! delayed past a removal, a hung app) are reproduced exactly and deterministically.
//!
//! Its jobs: (1) show that every case of [`super::ic`] passes for a reader that implements the
//! contract; (2) show that each case fails for a reader with the matching defect ([`Fault`]), so a
//! case cannot pass vacuously (harness self-test H-10(b)); (3) give the walk driver and the
//! message accounting something to run against before the real daemon is wired.
//!
//! It is not a model of gorca's performance: presentations cost a fixed overhead.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap, HashMap};

use super::proto::{AppStats, Cmd, CmdReply, HangMode, ModelSpec, P2pMode};
use super::service::ServiceModel;
use super::sut::{Feature, Presentation, Probe, Source, Sut, SutError, SutResult};
use super::world::{Effect, PropKind, World, state};

/// A defect to inject into the reference reader; each is what one case must catch.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Fault {
    #[default]
    None,
    /// `StaleProbe` of SPEC H-10(b): events never invalidate and RV-1 never runs.
    NeverInvalidate,
    /// A reader that never caches: every focus is a miss.
    NeverCache,
    /// Presents from the mirror without the RV-1 revalidation.
    NoRv1,
    /// Issues `Cache.GetItems` every 100 ms per app (a polling reader).
    DumpEvery100ms,
    /// Violates LR-1: a late reply resurrects a tombstoned key.
    ReviveGhosts,
    /// Violates LR-2: a node re-dirtied during its fetch is stored `Fresh`.
    IgnoreRedirty,
    /// Treats `Gone` as final (5.5): a removed-then-re-added key is never presented again.
    GoneIsFinal,
    /// Keeps the mirror when an app goes away, and shares it with the next instance.
    NoRetire,
    /// Does not stop sending when an app is `Hung`.
    IgnoreHealth,
    /// Does not apply `Flooded` batches as dirty-all.
    IgnoreFlood,
}

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub fault: Fault,
    /// Bus signal travel time.
    pub event_us: u64,
    /// Reply travel time (P2P or bus).
    pub reply_us: u64,
    /// gorca-side processing between an input and the utterance.
    pub overhead_us: u64,
    /// Interactive deadline.
    pub interactive_ms: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            fault: Fault::None,
            event_us: 100,
            reply_us: 100,
            overhead_us: 200,
            interactive_ms: 120,
        }
    }
}

const M_NAME: u32 = 1;
const M_DESC: u32 = 2;
const M_VALUE: u32 = 4;
const M_ROLE: u32 = 8;
const M_STATES: u32 = 16;
const M_CHILDREN: u32 = 256;
const M_ALL: u32 = u32::MAX;
const M_PRESENT: u32 = M_NAME | M_DESC | M_VALUE | M_ROLE | M_STATES;
const BASE_NS: u64 = 1_000_000_000_000;

#[derive(Clone, Debug, PartialEq)]
struct Snap {
    name: String,
    desc: String,
    role: u32,
    states: u64,
    value: Option<String>,
    parent: Option<String>,
}

impl Snap {
    fn said(&self) -> Said {
        Said {
            name: self.name.clone(),
            role: self.role,
            states: self.states & state::TRACKED,
            value: self.value.clone(),
        }
    }
}

/// The fields an utterance carried, for the RV-1 comparison.
#[derive(Clone, Debug, PartialEq)]
struct Said {
    name: String,
    role: u32,
    states: u64,
    value: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    snap: Snap,
    stale: u32,
    confirmed: u64,
}

enum View<'a> {
    Miss,
    Gone,
    Hit(&'a Entry, u32),
}

#[derive(Default, Debug)]
struct Mirror {
    entries: HashMap<String, Entry>,
    tomb: HashMap<String, u64>,
    inval: HashMap<String, (u64, u32)>,
    seq: u64,
    epoch: u64,
}

impl Mirror {
    fn view(&self, path: &str) -> View<'_> {
        match self.entries.get(path) {
            Some(e) => {
                let mut stale = e.stale;
                if e.confirmed < self.epoch {
                    stale = M_ALL;
                }
                View::Hit(e, stale)
            }
            None if self.tomb.contains_key(path) => View::Gone,
            None => View::Miss,
        }
    }

    fn invalidate(&mut self, path: &str, mask: u32, fault: Fault) {
        self.seq += 1;
        if fault == Fault::NeverInvalidate {
            return;
        }
        let e = self.inval.entry(path.to_owned()).or_insert((0, 0));
        *e = (self.seq, e.1 | mask);
        if let Some(en) = self.entries.get_mut(path) {
            en.stale |= mask;
        }
    }

    fn invalidate_all(&mut self) {
        self.seq += 1;
        self.epoch = self.seq;
    }

    fn tombstone(&mut self, path: &str) {
        self.seq += 1;
        self.tomb.insert(path.to_owned(), self.seq);
        self.entries.remove(path);
    }

    fn apply(&mut self, path: &str, snap: Snap, issued_at: u64, fault: Fault) {
        if fault == Fault::NeverCache {
            return;
        }
        if let Some(t) = self.tomb.get(path).copied() {
            if t > issued_at && fault != Fault::ReviveGhosts {
                return; // LR-1
            }
            self.tomb.remove(path); // LR-3: proof of life
        }
        self.seq += 1;
        let redirty = match self.inval.get(path) {
            Some((s, m)) if *s > issued_at && fault != Fault::IgnoreRedirty => *m, // LR-2
            _ => 0,
        };
        self.entries.insert(
            path.to_owned(),
            Entry {
                snap,
                stale: redirty,
                confirmed: issued_at,
            },
        );
    }
}

#[derive(Debug)]
enum Msg {
    Event(usize, Effect),
    Serve(u64),
    Reply(u64, Result<Snap, ()>),
    Timeout(u64),
    AppGone(usize),
    ProbeTick(usize),
    DumpTick(usize),
    StormTick(usize, u32, u32),
}

struct Timed {
    at: u64,
    ord: u64,
    msg: Msg,
}

impl PartialEq for Timed {
    fn eq(&self, o: &Self) -> bool {
        (self.at, self.ord) == (o.at, o.ord)
    }
}
impl Eq for Timed {}
impl PartialOrd for Timed {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Timed {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.at, self.ord).cmp(&(o.at, o.ord))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Purpose {
    Present(Source),
    Proof,
    Rv1(u64),
    S1,
    Probe,
}

#[derive(Debug)]
struct Req {
    idx: usize,
    path: String,
    issued_seq: u64,
    purpose: Purpose,
    stim_us: u64,
    /// What was said (RV-1).
    said: Option<Said>,
}

#[derive(Debug, Default)]
struct Flood {
    window_start: u64,
    count: u64,
    flooded: bool,
}

struct SimApp {
    label: String,
    bus: String,
    spec: ModelSpec,
    world: World,
    svc: ServiceModel,
    busy_until: u64,
    hang: (HangMode, u64),
    delays: Vec<(String, u64, u32)>,
    calls: AppStats,
    retired: bool,
    mirror: Option<Mirror>,
    timeouts: u32,
    hung: bool,
    flood: Flood,
    /// P9-1: an AT listener is registered (always true for models without the gate).
    listeners: bool,
}

pub struct SimSut {
    cfg: SimConfig,
    now: u64,
    ord: u64,
    queue: BinaryHeap<Reverse<Timed>>,
    apps: Vec<SimApp>,
    live: HashMap<String, usize>,
    reqs: HashMap<u64, Req>,
    next_req: u64,
    next_utt: u64,
    pres: Vec<Presentation>,
    truth: Vec<TruthLine>,
    focus: Option<(usize, String)>,
    cur_utt: Option<(u64, u64)>,
    saved_mirrors: HashMap<String, Mirror>,
    bus_n: u32,
    models_dir: std::path::PathBuf,
}

use super::proto::TruthLine;

impl std::fmt::Debug for SimSut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimSut")
            .field("fault", &self.cfg.fault)
            .field("now_us", &self.now)
            .finish_non_exhaustive()
    }
}

/// Roles that take their accessible name from their content (P9-3).
fn content_named(role: u32) -> bool {
    use super::world::role as r;
    matches!(
        role,
        r::PUSH_BUTTON | r::CHECK_BOX | r::TOGGLE_BUTTON | r::LIST_ITEM | r::MENU_ITEM | r::LABEL
    )
}

fn fmt_value(v: Option<f64>) -> Option<String> {
    v.map(|x| format!("{x}"))
}

impl SimSut {
    pub fn new(cfg: SimConfig) -> Self {
        Self {
            cfg,
            now: 0,
            ord: 0,
            queue: BinaryHeap::new(),
            apps: Vec::new(),
            live: HashMap::new(),
            reqs: HashMap::new(),
            next_req: 1,
            next_utt: 1,
            pres: Vec::new(),
            truth: Vec::new(),
            focus: None,
            cur_utt: None,
            saved_mirrors: HashMap::new(),
            bus_n: 10,
            models_dir: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/harness/b1/models"),
        }
    }

    pub fn with_fault(fault: Fault) -> Self {
        Self::new(SimConfig {
            fault,
            ..SimConfig::default()
        })
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    fn push(&mut self, at: u64, msg: Msg) {
        self.ord += 1;
        self.queue.push(Reverse(Timed {
            at,
            ord: self.ord,
            msg,
        }));
    }

    fn t_ns(&self) -> u64 {
        BASE_NS + self.now * 1000
    }

    fn idx(&self, app: &str) -> SutResult<usize> {
        self.live
            .get(app)
            .copied()
            .ok_or_else(|| SutError::Other(format!("no live app {app}")))
    }

    // ---- the reader ----------------------------------------------------------------------

    fn is_focus(&self, idx: usize, path: &str) -> bool {
        self.focus
            .as_ref()
            .is_some_and(|(i, p)| *i == idx && p == path)
    }

    fn snap_of(app: &SimApp, path: &str) -> Option<Snap> {
        let v = app.world.view(path)?;
        Some(Snap {
            name: v.name,
            desc: v.desc,
            role: v.role,
            states: v.states,
            value: fmt_value(v.value),
            parent: v.parent,
        })
    }

    fn issue(
        &mut self,
        idx: usize,
        path: &str,
        purpose: Purpose,
        stim_us: u64,
        said: Option<Said>,
    ) {
        let fault = self.cfg.fault;
        let app = &mut self.apps[idx];
        if app.retired {
            return;
        }
        if app.hung && purpose != Purpose::Probe && fault != Fault::IgnoreHealth {
            return; // I-4: only probes reach a hung app
        }
        app.listeners = true; // the shared base registers before the first fill request (P9-1)
        let issued_seq = app.mirror.as_ref().map_or(0, |m| m.seq);
        let id = self.next_req;
        self.next_req += 1;
        let conn = if app.spec.p2p == P2pMode::Peer {
            "peer"
        } else {
            "bus"
        };
        *app.calls
            .calls
            .entry(format!("{conn}/Accessible.Fetch"))
            .or_insert(0) += 1;
        let arrive = self.now + self.cfg.event_us;
        let start = arrive.max(app.busy_until);
        let svc_us = (app.svc.sample_ns() / 1000).max(1);
        let serve_at = start + svc_us;
        let never = app.hang.0 == HangMode::Never;
        if !never {
            app.busy_until = serve_at;
        }
        self.reqs.insert(
            id,
            Req {
                idx,
                path: path.to_owned(),
                issued_seq,
                purpose,
                stim_us,
                said,
            },
        );
        if !never {
            self.push(serve_at, Msg::Serve(id));
        }
        self.push(self.now + self.cfg.interactive_ms * 1000, Msg::Timeout(id));
    }

    fn present(
        &mut self,
        idx: usize,
        path: &str,
        snap: &Snap,
        source: Source,
        may_refine: bool,
        stim_us: u64,
        refines: Option<u64>,
    ) -> u64 {
        let utt = self.next_utt;
        self.next_utt += 1;
        let t = self.now + self.cfg.overhead_us;
        self.cur_utt = Some((utt, self.now));
        self.pres.push(Presentation {
            t_ns: BASE_NS + t * 1000,
            stimulus_ns: BASE_NS + stim_us * 1000,
            utt,
            app: self.apps[idx].label.clone(),
            app_bus: self.apps[idx].bus.clone(),
            path: path.to_owned(),
            source,
            role: snap.role,
            states: snap.states & !(1u64 << state::FOCUSED),
            name: snap.name.clone(),
            value: snap.value.clone(),
            may_refine,
            refines,
        });
        utt
    }

    fn begin_present(&mut self, idx: usize, path: &str, stim_us: u64, ev_seq: u64) {
        let fault = self.cfg.fault;
        let decision = {
            let Some(m) = self.apps[idx].mirror.as_ref() else {
                return;
            };
            match m.view(path) {
                View::Gone => {
                    if fault == Fault::GoneIsFinal {
                        None
                    } else {
                        Some((None, true))
                    }
                }
                View::Miss => Some((None, false)),
                View::Hit(e, stale) => {
                    let need = if e.snap.value.is_some() {
                        M_PRESENT
                    } else {
                        M_PRESENT & !M_VALUE
                    };
                    if stale & need == 0 {
                        Some((
                            Some((e.snap.clone(), Source::Hit, e.confirmed >= ev_seq)),
                            false,
                        ))
                    } else if stale & (M_NAME | M_ROLE) == 0 {
                        Some((Some((e.snap.clone(), Source::Stale, false)), false))
                    } else {
                        Some((None, false))
                    }
                }
            }
        };
        match decision {
            None => {}
            Some((None, proof)) => {
                let p = if proof {
                    Purpose::Proof
                } else {
                    Purpose::Present(Source::MissReply)
                };
                self.issue(idx, path, p, stim_us, None);
            }
            Some((Some((snap, src, confirmed_after)), _)) => {
                let utt = self.present(idx, path, &snap, src, true, stim_us, None);
                if fault != Fault::NoRv1 && fault != Fault::NeverInvalidate && !confirmed_after {
                    self.issue(idx, path, Purpose::Rv1(utt), stim_us, Some(snap.said()));
                }
            }
        }
    }

    fn on_event(&mut self, idx: usize, eff: Effect, stim_us: u64) {
        let fault = self.cfg.fault;
        if self.apps[idx].retired {
            return;
        }
        let ev_seq = {
            let Some(m) = self.apps[idx].mirror.as_mut() else {
                return;
            };
            m.seq += 1;
            m.seq
        };
        match eff {
            Effect::Prop { path, kind } => {
                let mask = match kind {
                    PropKind::Name => M_NAME,
                    PropKind::Description => M_DESC,
                    PropKind::Value => M_VALUE,
                };
                if let Some(m) = self.apps[idx].mirror.as_mut() {
                    m.invalidate(&path, mask, fault);
                    if mask == M_NAME {
                        // P9-3: names derived from content change silently; invalidate NAME on the
                        // known ancestors up to the first role that does not take its name from
                        // content (at most 8 levels).
                        let mut cur = m.entries.get(&path).and_then(|e| e.snap.parent.clone());
                        for _ in 0..8 {
                            let Some(a) = cur else { break };
                            let role = m.entries.get(&a).map(|e| e.snap.role);
                            let next = m.entries.get(&a).and_then(|e| e.snap.parent.clone());
                            m.invalidate(&a, M_NAME, fault);
                            if role.is_none_or(|r| !content_named(r)) {
                                break;
                            }
                            cur = next;
                        }
                    }
                }
                if kind == PropKind::Value && self.is_focus(idx, &path) {
                    self.issue(idx, &path, Purpose::S1, stim_us, None);
                }
            }
            Effect::State {
                path,
                state: st,
                on,
            } => {
                if st == state::FOCUSED || st == state::ACTIVE {
                    return;
                }
                let m = self.apps[idx].mirror.as_mut();
                if let Some(m) = m {
                    if st == state::DEFUNCT && on {
                        m.tombstone(&path);
                        return;
                    }
                    m.invalidate(&path, M_STATES, fault);
                }
                if st == state::SELECTED && on && !self.is_focus(idx, &path) {
                    // S-2: the Shell switcher announces only `selected`; it acts as focus.
                    self.begin_present(idx, &path, stim_us, ev_seq);
                } else if self.is_focus(idx, &path) {
                    self.issue(idx, &path, Purpose::S1, stim_us, None);
                }
            }
            Effect::Children {
                parent, add, child, ..
            } => {
                if let Some(m) = self.apps[idx].mirror.as_mut() {
                    m.invalidate(&parent, M_CHILDREN, fault);
                    if !add {
                        m.tombstone(&child);
                    }
                }
            }
            Effect::Window { path, kind } => {
                if let Some(m) = self.apps[idx].mirror.as_mut() {
                    if kind == "destroy" {
                        let doomed: Vec<String> = m
                            .entries
                            .iter()
                            .filter(|(p, e)| {
                                **p == path || {
                                    let mut cur = e.snap.parent.as_deref();
                                    let mut hops = 0;
                                    let mut under = false;
                                    while let Some(c) = cur {
                                        if c == path {
                                            under = true;
                                            break;
                                        }
                                        hops += 1;
                                        if hops > 64 {
                                            break;
                                        }
                                        cur =
                                            m.entries.get(c).and_then(|x| x.snap.parent.as_deref());
                                    }
                                    under
                                }
                            })
                            .map(|(p, _)| p.clone())
                            .collect();
                        m.tombstone(&path);
                        for d in doomed {
                            m.tombstone(&d);
                        }
                    } else {
                        m.invalidate(&path, M_STATES | M_NAME, fault);
                    }
                }
            }
            Effect::Focus { path } => {
                self.focus = Some((idx, path.clone()));
                self.begin_present(idx, &path, stim_us, ev_seq);
            }
            Effect::CacheAdd(_) | Effect::CacheRemove { .. } => {}
        }
    }

    fn on_reply(&mut self, id: u64, res: Result<Snap, ()>) {
        let fault = self.cfg.fault;
        let Some(req) = self.reqs.remove(&id) else {
            return;
        };
        let idx = req.idx;
        if self.apps[idx].retired {
            return;
        }
        self.apps[idx].timeouts = 0;
        self.apps[idx].hung = false;
        let path = req.path.clone();
        match res {
            Err(()) => {
                if let Some(m) = self.apps[idx].mirror.as_mut() {
                    m.tombstone(&path); // UnknownObject is authoritative (I-1)
                }
            }
            Ok(snap) => {
                if let Some(m) = self.apps[idx].mirror.as_mut() {
                    m.apply(&path, snap.clone(), req.issued_seq, fault);
                }
                match req.purpose {
                    Purpose::Present(src) => {
                        self.present(idx, &path, &snap, src, false, req.stim_us, None);
                    }
                    Purpose::Proof => {
                        self.present(
                            idx,
                            &path,
                            &snap,
                            Source::MissReply,
                            false,
                            req.stim_us,
                            None,
                        );
                    }
                    Purpose::S1 => {
                        self.present(idx, &path, &snap, Source::S1, false, req.stim_us, None);
                    }
                    Purpose::Rv1(utt) => {
                        if req.said.as_ref().is_some_and(|s| *s != snap.said()) {
                            let fresh_audio = self
                                .cur_utt
                                .is_some_and(|(u, start)| u == utt && self.now - start < 150_000);
                            let src = if fresh_audio {
                                Source::Refine
                            } else {
                                Source::Followup
                            };
                            self.present(idx, &path, &snap, src, false, req.stim_us, Some(utt));
                        }
                    }
                    Purpose::Probe => {}
                }
            }
        }
    }

    fn on_serve(&mut self, id: u64) {
        let Some(req) = self.reqs.get(&id) else {
            return;
        };
        let idx = req.idx;
        let app = &mut self.apps[idx];
        if app.retired {
            return;
        }
        let path = req.path.clone();
        let res = Self::snap_of(app, &path).ok_or(());
        let mut extra = if app.hang.0 == HangMode::Delay {
            app.hang.1
        } else {
            0
        };
        if let Some(r) = app.delays.iter_mut().find(|r| r.2 > 0) {
            r.2 -= 1;
            extra += r.1;
        }
        app.delays.retain(|r| r.2 > 0);
        let at = self.now + self.cfg.reply_us + extra * 1000;
        self.push(at, Msg::Reply(id, res));
    }

    fn on_timeout(&mut self, id: u64) {
        let Some(req) = self.reqs.remove(&id) else {
            return;
        };
        let idx = req.idx;
        let app = &mut self.apps[idx];
        if app.retired {
            return;
        }
        app.timeouts += 1;
        if app.timeouts >= 3 && !app.hung {
            app.hung = true;
            let at = self.now + 1_000_000;
            self.push(at, Msg::ProbeTick(idx));
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Event(idx, eff) => {
                let stim = self.now.saturating_sub(self.cfg.event_us);
                self.on_event(idx, eff, stim);
            }
            Msg::Serve(id) => self.on_serve(id),
            Msg::Reply(id, r) => self.on_reply(id, r),
            Msg::Timeout(id) => self.on_timeout(id),
            Msg::AppGone(idx) => {
                if self.cfg.fault != Fault::NoRetire {
                    self.apps[idx].mirror = None;
                }
                self.reqs.retain(|_, r| r.idx != idx);
            }
            Msg::ProbeTick(idx) => {
                let app = &self.apps[idx];
                if !app.retired && app.hung {
                    let root = app
                        .world
                        .node(0)
                        .map(|n| n.path.clone())
                        .unwrap_or_default();
                    self.issue(idx, &root, Purpose::Probe, self.now, None);
                    let at = self.now + 1_000_000;
                    self.push(at, Msg::ProbeTick(idx));
                }
            }
            Msg::DumpTick(idx) => {
                let app = &mut self.apps[idx];
                if !app.retired {
                    *app.calls
                        .calls
                        .entry("bus/Cache.GetItems".to_owned())
                        .or_insert(0) += 1;
                    let at = self.now + 100_000;
                    self.push(at, Msg::DumpTick(idx));
                }
            }
            Msg::StormTick(idx, left_ms, per_ms) => self.storm_tick(idx, left_ms, per_ms),
        }
    }

    fn storm_tick(&mut self, idx: usize, left_ms: u32, per_ms: u32) {
        let fault = self.cfg.fault;
        let now = self.now;
        let app = &mut self.apps[idx];
        if app.retired {
            return;
        }
        let term = app.world.storm_target();
        let f = &mut app.flood;
        if now - f.window_start >= 100_000 {
            f.window_start = now;
            f.count = 0;
            f.flooded = false;
        }
        f.count += u64::from(per_ms);
        let flooded_now = f.count > 2_500 && fault != Fault::IgnoreFlood;
        if let Some(m) = app.mirror.as_mut() {
            if flooded_now {
                if !f.flooded {
                    f.flooded = true;
                    m.invalidate_all(); // dirty-all, O(1)
                }
            } else if let Some(t) = term {
                m.invalidate(&t, M_NAME | M_VALUE, fault);
            }
        }
        if left_ms > 1 {
            self.push(self.now + 1000, Msg::StormTick(idx, left_ms - 1, per_ms));
        }
    }

    fn spawn_app(&mut self, spec: ModelSpec, label: &str) -> usize {
        self.bus_n += 1;
        let idx = self.apps.len();
        let spec_gate = spec.quirks.listener_gate;
        let world = World::new(spec.clone(), label);
        let mirror = if self.cfg.fault == Fault::NoRetire {
            self.saved_mirrors
                .remove(label)
                .or_else(|| Some(Mirror::default()))
        } else {
            Some(Mirror::default())
        };
        self.apps.push(SimApp {
            label: label.to_owned(),
            bus: format!(":1.{}", self.bus_n),
            svc: ServiceModel::new(spec.service, spec.seed ^ 0xA5A5 ^ idx as u64),
            spec,
            world,
            busy_until: 0,
            hang: (HangMode::Off, 0),
            delays: Vec::new(),
            calls: AppStats::default(),
            retired: false,
            mirror,
            timeouts: 0,
            hung: false,
            flood: Flood::default(),
            listeners: !spec_gate,
        });
        self.live.insert(label.to_owned(), idx);
        if self.cfg.fault == Fault::DumpEvery100ms {
            self.push(self.now + 100_000, Msg::DumpTick(idx));
        }
        idx
    }

    fn retire(&mut self, idx: usize) {
        self.apps[idx].retired = true;
        let label = self.apps[idx].label.clone();
        if self.live.get(&label) == Some(&idx) {
            self.live.remove(&label);
        }
        if self.cfg.fault == Fault::NoRetire {
            // The defect: the mirror outlives the app and is handed to the next instance.
            if let Some(m) = self.apps[idx].mirror.take() {
                self.saved_mirrors.insert(label, m);
            }
        }
        let at = self.now + self.cfg.event_us + 1000;
        self.push(at, Msg::AppGone(idx));
    }

    fn load_model(&self, model: &str) -> SutResult<ModelSpec> {
        let p = self.models_dir.join(format!("{model}.json"));
        let text = std::fs::read_to_string(&p)
            .map_err(|e| SutError::Other(format!("{}: {e}", p.display())))?;
        serde_json::from_str(&text).map_err(|e| SutError::Other(format!("{}: {e}", p.display())))
    }
}

impl Sut for SimSut {
    fn arm(&self) -> String {
        format!("sim:{:?}", self.cfg.fault).to_lowercase()
    }

    fn supports(&self, f: Feature) -> bool {
        match f {
            Feature::LatencyKnobs | Feature::Probe | Feature::VirtualTime | Feature::WhereAmI => {
                true
            }
            Feature::CacheFill | Feature::Malformed => false,
        }
    }

    fn start_app(&mut self, model: &str, label: &str) -> SutResult<()> {
        self.start_app_with(model, label, &|_| {})
    }

    fn start_app_with(
        &mut self,
        model: &str,
        label: &str,
        edit: &dyn Fn(&mut ModelSpec),
    ) -> SutResult<()> {
        let mut spec = self.load_model(model)?;
        edit(&mut spec);
        self.spawn_app(spec, label);
        Ok(())
    }

    fn restart_app(&mut self, label: &str) -> SutResult<()> {
        let spec = match self.live.get(label) {
            Some(i) => {
                let spec = self.apps[*i].spec.clone();
                self.retire(*i);
                spec
            }
            None => self
                .apps
                .iter()
                .rev()
                .find(|a| a.label == label)
                .map(|a| a.spec.clone())
                .ok_or_else(|| SutError::Other(format!("never started {label}")))?,
        };
        self.spawn_app(spec, label);
        Ok(())
    }

    fn cmd(&mut self, app: &str, c: Cmd) -> SutResult<CmdReply> {
        let idx = self.idx(app)?;
        let t_ns = self.t_ns();
        match c {
            Cmd::Hang { mode, ms } => {
                self.apps[idx].hang = (mode, ms);
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            Cmd::DelayReplies {
                member, ms, count, ..
            } => {
                self.apps[idx].delays.push((member, ms, count));
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            Cmd::Storm {
                rate_per_s,
                duration_ms,
            } => {
                let per_ms = u32::try_from(rate_per_s / 1000).unwrap_or(u32::MAX).max(1);
                let ms = u32::try_from(duration_ms).unwrap_or(u32::MAX).max(1);
                self.push(self.now, Msg::StormTick(idx, ms, per_ms));
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            Cmd::Stats => {
                let mut s = self.apps[idx].calls.clone();
                s.nodes_live = u64::from(self.apps[idx].world.live_count());
                s.bus_callers = 1;
                Ok(CmdReply::ok(serde_json::to_value(s).unwrap_or_default()))
            }
            Cmd::ClosePeers | Cmd::PeerAcceptDelay { .. } => {
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            Cmd::RegisterListener => {
                self.apps[idx].listeners = true;
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            Cmd::Kill | Cmd::Quit => {
                self.retire(idx);
                Ok(CmdReply::ok(serde_json::Value::Null))
            }
            other => {
                let (reply, fx) = self.apps[idx].world.apply(&other, t_ns);
                let lines = self.apps[idx].world.take_truth();
                self.truth.extend(lines);
                let gated = !self.apps[idx].listeners;
                for e in fx {
                    if gated
                        && matches!(
                            e,
                            Effect::Prop { .. } | Effect::State { .. } | Effect::Children { .. }
                        )
                    {
                        continue; // P9-1: GTK4 sends no Object event without a listener
                    }
                    self.push(self.now + self.cfg.event_us, Msg::Event(idx, e));
                }
                Ok(reply)
            }
        }
    }

    fn advance(&mut self, ms: u64) {
        let target = self.now + ms * 1000;
        while let Some(Reverse(top)) = self.queue.peek() {
            if top.at > target {
                break;
            }
            let Some(Reverse(t)) = self.queue.pop() else {
                break;
            };
            self.now = self.now.max(t.at);
            self.handle(t.msg);
        }
        self.now = target;
    }

    fn now_ns(&self) -> u64 {
        self.t_ns()
    }

    fn presentations(&self) -> Vec<Presentation> {
        self.pres.clone()
    }

    fn truth(&self) -> Vec<TruthLine> {
        let mut t = self.truth.clone();
        t.sort_by_key(|l| l.t_ns);
        t
    }

    fn probe(&self, app: &str, path: &str) -> Probe {
        let Some(idx) = self.live.get(app) else {
            return Probe::Miss;
        };
        let Some(m) = self.apps[*idx].mirror.as_ref() else {
            return Probe::Miss;
        };
        match m.view(path) {
            View::Miss => Probe::Miss,
            View::Gone => Probe::Gone,
            View::Hit(e, stale) => Probe::Hit {
                name: e.snap.name.clone(),
                states: e.snap.states,
                value: e.snap.value.clone(),
                stale: stale & M_PRESENT != 0,
                partial: false,
            },
        }
    }

    fn mirror_nodes(&self, bus_name: &str) -> Option<usize> {
        self.apps
            .iter()
            .find(|a| a.bus == bus_name)
            .and_then(|a| a.mirror.as_ref())
            .map(|m| m.entries.len() + m.tomb.len() + m.inval.len())
    }

    fn bus_name(&self, app: &str) -> String {
        self.live
            .get(app)
            .map(|i| self.apps[*i].bus.clone())
            .unwrap_or_default()
    }

    fn calls(&mut self, app: &str) -> SutResult<AppStats> {
        let idx = self.idx(app)?;
        let mut s = self.apps[idx].calls.clone();
        s.nodes_live = u64::from(self.apps[idx].world.live_count());
        s.bus_callers = 1;
        Ok(s)
    }

    fn where_am_i(&mut self) -> SutResult<Vec<String>> {
        let Some((idx, path)) = self.focus.clone() else {
            return Err(SutError::Other("no focus".into()));
        };
        let fault = self.cfg.fault;
        let mut chain = vec![path];
        for _ in 0..3 {
            let next = chain.last().and_then(|p| {
                self.apps[idx]
                    .mirror
                    .as_ref()
                    .and_then(|m| m.entries.get(p))
                    .and_then(|e| e.snap.parent.clone())
            });
            match next {
                Some(n) => chain.push(n),
                None => break,
            }
        }
        let stim = self.now;
        let spoken = chain.clone();
        for p in chain {
            let snap = self.apps[idx]
                .mirror
                .as_ref()
                .and_then(|m| m.entries.get(&p))
                .map(|e| e.snap.clone());
            match snap {
                Some(snap) => {
                    let utt = self.present(idx, &p, &snap, Source::Hit, true, stim, None);
                    if fault != Fault::NoRv1 && fault != Fault::NeverInvalidate {
                        self.issue(idx, &p, Purpose::Rv1(utt), stim, Some(snap.said()));
                    }
                }
                None => self.issue(idx, &p, Purpose::Present(Source::MissReply), stim, None),
            }
        }
        Ok(spoken)
    }

    fn set_latency(&mut self, event_us: u64, reply_us: u64) {
        self.cfg.event_us = event_us;
        self.cfg.reply_us = reply_us;
    }
}

/// Stable view of per-source counts, for the self-tests and the walk report.
pub fn source_histogram(p: &[Presentation]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for x in p {
        *m.entry(format!("{:?}", x.source)).or_insert(0) += 1;
    }
    m
}
