// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stub node writer (SPEC 2.3): clock `T`, S0 registers, a `Slot` per plugin, the admission
//! path (slot epoch, MQ-2 token, effect caps already enforced by the host), a deterministic `FUSE`
//! (CR-2 rank so arrival order cannot change the winner, PF-6), the stub presenter and the sink.
//! The arms never touch any of it: they implement [`WLaneRunner`] only.

use crate::budget::{progressive_fill, BudgetCfg, Grant, PluginStats};
use crate::host::{
    set_writer_turn, DoneReply, FactEffect, FaultKind, FaultReply, PluginBatch, PluginReply, Value,
};
use crate::model::*;
use crate::presenter::{compose, render, Fused, FusedEntry, SpeechSink};
use crate::runner::{mailbox, HandoffOutcome, MailboxRx, TurnCtx, WLaneRunner, WriterMailbox};
use crate::slot::{Action, Slot, SlotEvent, SlotState};
use crate::timing::now_ns;
use gorca_present_ir::Reason;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

pub const MAXP: usize = 24;

/// Per-plugin outcome of one focus change (the reason breakdown of M3).
pub mod oc {
    pub const NONE: u8 = 0;
    pub const OK: u8 = 1;
    pub const LATE: u8 = 2;
    pub const BUSY: u8 = 3;
    pub const OUT_OF_FUEL: u8 = 4;
    pub const FAULT: u8 = 5;
    pub const DEMOTED: u8 = 6;
    /// Slot in `Backoff`/`Disabled`/`Unloaded`: excluded from the M3 denominator.
    pub const EXCLUDED: u8 = 7;
    pub const STALE_TOKEN: u8 = 8;
    /// The deadline had passed before the jobs were even sent (snapshot exhausted the budget).
    pub const SNAPSHOT_EXHAUSTED: u8 = 9;
    pub const NAMES: [&str; 10] = [
        "none",
        "ok",
        "late_reply",
        "pa_busy",
        "out_of_fuel",
        "fault",
        "demoted",
        "excluded",
        "stale_token",
        "snapshot_exhausted",
    ];
}

#[derive(Clone, Debug)]
pub struct PluginSpec {
    pub name: String,
    /// Tier for CR-2 rank (lower wins): 1 first-party, 3 third-party.
    pub tier: u8,
    /// Bit set over role codes the plugin's `[[watch]]` selector matches.
    pub watch_roles: u64,
    pub adversary: bool,
}

pub struct PluginEntry {
    pub spec: PluginSpec,
    pub slot: Slot,
    pub stats: PluginStats,
    pub pending: Vec<NodeId>,
    pub live_reads: HashSet<NodeId>,
    pub c: PluginCounters,
    /// Job (and its grant) of the last interactive dispatch, to classify its reply.
    last_interactive: Option<(JobId, Grant)>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PluginCounters {
    pub derive_calls: u64,
    pub interactive_calls: u64,
    pub pairs: u64,
    pub pairs_ok: u64,
    pub faults: BTreeMap<String, u64>,
    pub strikes: u64,
    pub overruns: u64,
    pub budget_misses: u64,
    pub stale_epoch_dropped: u64,
    pub stale_token_dropped: u64,
    pub late_admitted: u64,
    pub refresh_requests: u64,
    pub refresh_dropped: u64,
    pub coalesced: u64,
    pub preempted: u64,
    pub demoted_turns: u64,
    pub reasons: BTreeMap<String, u64>,
    pub exec_ns_max: u64,
    pub queued_ns_max: u64,
    pub fuel_max: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Counters {
    pub turns: u64,
    pub background_turns: u64,
    pub waits: u64,
    pub wait_denied_flap: u64,
    pub wait_denied_path: u64,
    pub busy_skips: u64,
    pub impossible_cells: u64,
    pub grants_sum_over_budget: u64,
    pub max_grant_sum_ns: u64,
    pub s0_events: u64,
    pub admitted_facts: u64,
}

/// One row per focus change; fixed size, pre-allocated ring, no allocation in the turn.
#[derive(Copy, Clone, Debug)]
pub struct TurnRecord {
    pub turn: u64,
    pub t_sched_ns: u64,
    pub t0_ns: u64,
    pub stall_ns: u32,
    pub lag_ns: u32,
    pub speech_ns: u32,
    pub asked: u16,
    pub arrived: u16,
    pub busy_skips: u16,
    pub woke_late_ns: u32,
    pub waited_ns: u32,
    pub pairs: u16,
    pub pairs_ok: u16,
    pub waited: bool,
    pub grant_sum_ns: u32,
    pub outcomes: [u8; MAXP],
    pub exec_ns: [u32; MAXP],
    pub queued_ns: [u32; MAXP],
    pub fuel: [u32; MAXP],
}

impl TurnRecord {
    fn blank() -> TurnRecord {
        TurnRecord {
            turn: 0,
            t_sched_ns: 0,
            t0_ns: 0,
            stall_ns: 0,
            lag_ns: 0,
            speech_ns: 0,
            asked: 0,
            arrived: 0,
            busy_skips: 0,
            woke_late_ns: 0,
            waited_ns: 0,
            pairs: 0,
            pairs_ok: 0,
            waited: false,
            grant_sum_ns: 0,
            outcomes: [0; MAXP],
            exec_ns: [0; MAXP],
            queued_ns: [0; MAXP],
            fuel: [0; MAXP],
        }
    }
}

#[derive(Clone, Debug)]
pub struct WriterCfg {
    pub budget: BudgetCfg,
    /// Focus-chain size (nodes per focus change).
    pub chain: usize,
    /// A flapping writer takes an interactive wait at most once per `flap_ms`.
    pub flap_ms: u64,
    /// Bounded wait measured from `t0` (0.3 ms).
    pub wait_ns: u64,
    /// Fuel grant of background slices, in ns of `kappa_int`.
    pub bg_grant_ns: u64,
    /// Pre-allocated capacity of the turn record ring.
    pub record_cap: usize,
}

impl WriterCfg {
    pub fn spec(kappa_int: f64) -> WriterCfg {
        WriterCfg {
            budget: BudgetCfg::spec(kappa_int),
            chain: 16,
            flap_ms: 20,
            wait_ns: 300_000,
            bg_grant_ns: 2_000_000,
            record_cap: 0,
        }
    }
}

/// The keypress path shared by K1 and K2: exactly one bounded wait per path.
#[derive(Clone, Default)]
pub struct PathToken {
    pub claimed: Arc<AtomicBool>,
    pub waits: Arc<AtomicU32>,
}

pub struct FocusEvent {
    pub focus: NodeId,
    /// Scheduled instant of the keypress / focus change (open loop), CLOCK_MONOTONIC ns.
    pub t_sched_ns: u64,
}

#[derive(Copy, Clone, Debug)]
pub struct TurnTimes {
    pub t0_ns: u64,
    pub t_publish_ns: u64,
    pub t_speech_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Admitted {
    plugin: PluginId,
    tier: u8,
    group: u16,
    value: Value,
    conf: u16,
}

pub struct StubWriter {
    pub table: NodeTable,
    pub cfg: WriterCfg,
    pub plugins: Vec<PluginEntry>,
    pub counters: Counters,
    pub sink: SpeechSink,
    pub records: Vec<TurnRecord>,
    mb: WriterMailbox,
    rx: MailboxRx,
    facts: HashMap<NodeId, Vec<Admitted>>,
    last_fact_turn: HashMap<(PluginId, NodeId), u64>,
    refresh_seen: HashSet<(PluginId, NodeId, u64)>,
    turn: u64,
    focus_counter: u64,
    last_wait_ns: u64,
    focused: Option<NodeId>,
    cur_outcomes: [u8; MAXP],
    cur_exec: [u32; MAXP],
    cur_queued: [u32; MAXP],
    cur_fuel: [u32; MAXP],
    chain: Vec<NodeId>,
    text: String,
    pub sem_rev: u32,
    /// Reply log for the PF-6 arrival-order test (off by default).
    pub debug_admit_order: Option<Vec<(PluginId, NodeId, u16)>>,
}

pub fn make_job(turn: u64, plugin: PluginId, background: bool) -> JobId {
    (turn << 7) | ((background as u64) << 6) | (plugin as u64 & 63)
}
pub fn job_turn(job: JobId) -> u64 {
    job >> 7
}
pub fn job_is_background(job: JobId) -> bool {
    job & 64 != 0
}

impl StubWriter {
    pub fn new(table: NodeTable, specs: Vec<PluginSpec>, cfg: WriterCfg) -> StubWriter {
        let (mb, rx) = mailbox();
        let plugins = specs
            .into_iter()
            .map(|spec| PluginEntry {
                spec,
                slot: Slot::new(SlotState::Cold),
                stats: PluginStats::default(),
                pending: Vec::new(),
                live_reads: HashSet::new(),
                c: PluginCounters::default(),
                last_interactive: None,
            })
            .collect();
        let cap = cfg.record_cap;
        StubWriter {
            table,
            cfg,
            plugins,
            counters: Counters::default(),
            sink: SpeechSink::default(),
            records: Vec::with_capacity(cap),
            mb,
            rx,
            facts: HashMap::new(),
            last_fact_turn: HashMap::new(),
            refresh_seen: HashSet::new(),
            turn: 0,
            focus_counter: 0,
            last_wait_ns: 0,
            focused: None,
            cur_outcomes: [0; MAXP],
            cur_exec: [0; MAXP],
            cur_queued: [0; MAXP],
            cur_fuel: [0; MAXP],
            chain: Vec::with_capacity(64),
            text: String::with_capacity(512),
            sem_rev: 0,
            debug_admit_order: None,
        }
    }

    pub fn mailbox(&self) -> WriterMailbox {
        self.mb.clone()
    }

    pub fn mailbox_depth(&self) -> usize {
        self.rx.depth()
    }

    pub fn turn(&self) -> u64 {
        self.turn
    }

    fn watched(&self, p: usize, node: NodeId) -> bool {
        let role = self.table.nodes[node as usize].role;
        self.plugins[p].spec.watch_roles & (1u64 << role) != 0
    }

    // ---- S0 events and the trigger rule -----------------------------------------------------

    /// An S0 class change (an application event). Marks plugins dirty by the trigger rule: a
    /// `[[watch]]` match, or a stamp change in a node of the plugin's live read set. Never called
    /// for admissions or for a plugin's own facts.
    pub fn apply_s0_event(&mut self, node: NodeId, class: Class) {
        if node as usize >= self.table.len() {
            return;
        }
        self.table.touch(node, class);
        self.counters.s0_events += 1;
        for p in 0..self.plugins.len() {
            let hit = self.watched(p, node) || self.plugins[p].live_reads.contains(&node);
            if hit {
                let e = &mut self.plugins[p];
                if !e.pending.contains(&node) && e.pending.len() < 64 {
                    e.pending.push(node);
                }
            }
        }
    }

    /// Sends the coalesced dirty sets of every plugin whose slot can take a call and is not busy
    /// on the background lane. Returns the number of jobs sent. Never waits.
    pub fn pump_background(&mut self, runner: &mut dyn WLaneRunner) -> usize {
        let now = now_ns();
        let mut batches: Vec<PluginBatch> = Vec::new();
        let g_fuel = (self.cfg.budget.kappa_int * self.cfg.bg_grant_ns as f64) as u64;
        for p in 0..self.plugins.len() {
            if self.plugins[p].pending.is_empty() {
                continue;
            }
            self.plugins[p].slot.tick(now);
            let act = self.plugins[p]
                .slot
                .step(SlotEvent::CallNeeded { interactive: false }, now);
            match act {
                Action::Call | Action::Instantiate | Action::DemotedToBackground => {
                    self.turn += 1;
                    let job = make_job(self.turn, p as PluginId, true);
                    let e = &mut self.plugins[p];
                    let nodes: Vec<NodeId> = e.pending.drain(..).take(64).collect();
                    e.slot.busy = Some(job);
                    batches.push(PluginBatch {
                        plugin: p as PluginId,
                        job,
                        slot_epoch: e.slot.epoch,
                        nodes,
                        interactive: false,
                        grant: Grant {
                            fuel: g_fuel,
                            ns: self.cfg.bg_grant_ns,
                            capped: false,
                        },
                        created_ns: now_ns(),
                    });
                }
                Action::Coalesce => self.plugins[p].c.coalesced += 1,
                Action::Skip => self.plugins[p].pending.clear(),
                _ => {}
            }
        }
        let n = batches.len();
        if n > 0 {
            self.counters.background_turns += 1;
            let f = FrozenRowsFactory { table: &self.table };
            runner.background(&batches, &f, &self.mb);
            self.drain();
        }
        n
    }

    // ---- the keypress path -------------------------------------------------------------------

    /// One interactive focus change (SPEC 1, Arm B' steps 0 to 6, as the shared writer turn).
    pub fn focus_change(
        &mut self,
        runner: &mut dyn WLaneRunner,
        ev: &FocusEvent,
        path: Option<&PathToken>,
    ) -> TurnTimes {
        set_writer_turn(true);
        // Step 0: the clock. t0 is the first instruction of the turn.
        let t0 = now_ns();
        let deadline = t0 + self.cfg.wait_ns;
        self.turn += 1;
        self.focus_counter += 1;
        let turn = self.turn;
        self.cur_outcomes = [0; MAXP];
        self.cur_exec = [0; MAXP];
        self.cur_queued = [0; MAXP];
        self.cur_fuel = [0; MAXP];

        // Step 1: S0 recompute (focus state moves, stamps, T).
        if let Some(prev) = self.focused {
            self.table.nodes[prev as usize].states &= !STATE_FOCUSED;
            self.table.touch(prev, Class::Core);
        }
        let focus = ev.focus.min(self.table.len() as u32 - 1);
        self.table.nodes[focus as usize].states |= STATE_FOCUSED;
        self.table.touch(focus, Class::Core);
        self.focused = Some(focus);
        let mut chain = std::mem::take(&mut self.chain);
        self.table.focus_chain(focus, self.cfg.chain, &mut chain);

        // Decide per plugin.
        let mut batches: Vec<PluginBatch> = Vec::with_capacity(self.plugins.len());
        let mut demoted: Vec<(usize, Vec<NodeId>)> = Vec::new();
        let mut preds: Vec<(PluginId, u64)> = Vec::with_capacity(self.plugins.len());
        let mut pair_nodes: [u16; MAXP] = [0; MAXP];
        let mut busy_skips = 0u16;
        let mut pairs = 0u32;
        for p in 0..self.plugins.len().min(MAXP) {
            let nodes: Vec<NodeId> = chain
                .iter()
                .copied()
                .filter(|n| self.watched(p, *n))
                .collect();
            if nodes.is_empty() {
                continue;
            }
            self.plugins[p].slot.tick(t0);
            let act = self.plugins[p]
                .slot
                .step(SlotEvent::CallNeeded { interactive: true }, t0);
            match act {
                Action::Skip => {
                    self.cur_outcomes[p] = oc::EXCLUDED;
                    continue;
                }
                Action::DemotedToBackground => {
                    pairs += nodes.len() as u32;
                    pair_nodes[p] = nodes.len() as u16;
                    self.cur_outcomes[p] = oc::DEMOTED;
                    self.plugins[p].c.demoted_turns += 1;
                    demoted.push((p, nodes));
                    continue;
                }
                Action::Coalesce => {
                    pairs += nodes.len() as u32;
                    pair_nodes[p] = nodes.len() as u16;
                    let busy_job = self.plugins[p].slot.busy.unwrap_or(0);
                    if job_is_background(busy_job) {
                        // An interactive job pre-empts a running background job of the same actor.
                        runner.cancel(p as PluginId, busy_job);
                        self.plugins[p].slot.busy = None;
                        self.plugins[p].c.preempted += 1;
                    } else {
                        self.cur_outcomes[p] = oc::BUSY;
                        busy_skips += 1;
                        self.plugins[p].c.coalesced += 1;
                        for n in &nodes {
                            if !self.plugins[p].pending.contains(n)
                                && self.plugins[p].pending.len() < 64
                            {
                                self.plugins[p].pending.push(*n);
                            }
                        }
                        continue;
                    }
                }
                Action::Call | Action::Instantiate => {
                    pairs += nodes.len() as u32;
                    pair_nodes[p] = nodes.len() as u16;
                }
                Action::Impossible => {
                    self.counters.impossible_cells += 1;
                    continue;
                }
                _ => continue,
            }
            preds.push((p as PluginId, self.plugins[p].stats.pred_fuel()));
            let job = make_job(turn, p as PluginId, false);
            batches.push(PluginBatch {
                plugin: p as PluginId,
                job,
                slot_epoch: self.plugins[p].slot.epoch,
                nodes,
                interactive: true,
                grant: Grant {
                    fuel: 0,
                    ns: 0,
                    capped: false,
                },
                created_ns: 0,
            });
        }
        // MQ-3 grants.
        let grants = progressive_fill(&self.cfg.budget, &preds, self.focus_counter);
        let pool = self.cfg.budget.pool_fuel(batches.len().max(1));
        let share = pool / batches.len().max(1) as u64;
        let mut gsum = 0u64;
        let created = now_ns();
        for (b, g) in batches.iter_mut().zip(grants) {
            let e = &mut self.plugins[b.plugin as usize];
            let kp = e.stats.kappa_p(self.cfg.budget.kappa_int).max(1e-6);
            b.grant = Grant {
                fuel: g.fuel,
                ns: (g.fuel as f64 / kp) as u64,
                capped: g.capped,
            };
            gsum += (g.fuel as f64 / self.cfg.budget.kappa_int) as u64;
            b.created_ns = created;
            e.slot.busy = Some(b.job);
            e.last_interactive = Some((b.job, b.grant));
            e.c.interactive_calls += 1;
            let over = e.stats.pred_fuel() > share;
            e.stats.note_focus(over);
        }
        if !batches.is_empty() {
            let limit = self.cfg.budget.b_ns
                + batches.len() as u64 * self.cfg.budget.c_max_ns
                + batches.len() as u64;
            // The sum bound holds at rho about 1 (every plugin predicted to use its share); with
            // cheap predictions the unused fuel is deliberately handed on (MQ-3 returns).
            let full = preds.iter().all(|p| p.1 >= share);
            if full && gsum > limit {
                self.counters.grants_sum_over_budget += 1;
            }
            self.counters.max_grant_sum_ns = self.counters.max_grant_sum_ns.max(gsum);
        }

        // Step 3 and 4: hand off, wait (if this writer owns the wait).
        let mut wait = true;
        if let Some(pt) = path {
            if pt.claimed.swap(true, Ordering::AcqRel) {
                wait = false;
                self.counters.wait_denied_path += 1;
            }
        }
        if wait
            && self.last_wait_ns != 0
            && t0.saturating_sub(self.last_wait_ns) < self.cfg.flap_ms * 1_000_000
        {
            wait = false;
            self.counters.wait_denied_flap += 1;
        }
        let ctx = TurnCtx {
            turn,
            t0_ns: t0,
            deadline_ns: deadline,
            t: self.table.t,
            wait,
        };
        let out: HandoffOutcome = if batches.is_empty() {
            HandoffOutcome::default()
        } else {
            let f = FrozenRowsFactory { table: &self.table };
            runner.interactive(&ctx, &batches, &f, &self.mb)
        };
        if wait && !batches.is_empty() {
            self.last_wait_ns = t0;
            self.counters.waits += 1;
            if let Some(pt) = path {
                pt.waits.fetch_add(1, Ordering::AcqRel);
            }
        }

        // Step 5: admit whatever arrived, fuse, publish.
        self.drain();
        let mut pairs_ok = 0u32;
        for b in &batches {
            let p = b.plugin as usize;
            if self.cur_outcomes[p] == oc::NONE {
                // Nothing came back by now: late (or deadline already gone before the send).
                self.cur_outcomes[p] = if now_ns() > deadline && out.arrived == 0 && !wait {
                    oc::LATE
                } else {
                    oc::LATE
                };
            }
            if self.cur_outcomes[p] == oc::OK {
                pairs_ok += pair_nodes[p] as u32;
            }
        }
        for p in 0..self.plugins.len().min(MAXP) {
            let o = self.cur_outcomes[p];
            if o != oc::NONE && o != oc::EXCLUDED {
                let e = &mut self.plugins[p];
                *e.c.reasons
                    .entry(oc::NAMES[o as usize].to_string())
                    .or_insert(0) += pair_nodes[p] as u64;
                e.c.pairs += pair_nodes[p] as u64;
                if o == oc::OK {
                    e.c.pairs_ok += pair_nodes[p] as u64;
                }
            }
        }
        let entry = self.fuse(focus);
        let t_publish = now_ns();
        set_writer_turn(false);

        // Present and speak (the stub's non-plugin work; included in keypress latency, not stall).
        let ir = compose(&entry, Reason::Focus);
        let mut text = std::mem::take(&mut self.text);
        render(&ir, &mut text);
        let t_speech = self.sink.speak(&text);
        self.text = text;

        // Demoted plugins run on the background lane, after the utterance.
        if !demoted.is_empty() {
            for (p, nodes) in demoted {
                let e = &mut self.plugins[p];
                for n in nodes {
                    if !e.pending.contains(&n) && e.pending.len() < 64 {
                        e.pending.push(n);
                    }
                }
            }
            self.pump_background(runner);
        }

        self.counters.turns += 1;
        self.counters.busy_skips += busy_skips as u64;
        let rec_ok = self.records.len() < self.records.capacity();
        if rec_ok {
            let mut r = TurnRecord::blank();
            r.turn = turn;
            r.t_sched_ns = ev.t_sched_ns;
            r.t0_ns = t0;
            r.stall_ns = crate::timing::sat32(t_publish - t0);
            r.lag_ns = crate::timing::sat32(t0.saturating_sub(ev.t_sched_ns));
            r.speech_ns = crate::timing::sat32(t_speech.saturating_sub(ev.t_sched_ns));
            r.asked = out.asked as u16;
            r.arrived = out.arrived as u16;
            r.busy_skips = busy_skips + out.busy_skips as u16;
            r.woke_late_ns = out.woke_late.as_nanos().min(u32::MAX as u128) as u32;
            r.waited_ns = out.waited.as_nanos().min(u32::MAX as u128) as u32;
            r.pairs = pairs as u16;
            r.pairs_ok = pairs_ok as u16;
            r.waited = wait && !batches.is_empty();
            r.grant_sum_ns = gsum.min(u32::MAX as u64) as u32;
            r.outcomes = self.cur_outcomes;
            r.exec_ns = self.cur_exec;
            r.queued_ns = self.cur_queued;
            r.fuel = self.cur_fuel;
            self.records.push(r);
        }
        self.chain = chain;
        TurnTimes {
            t0_ns: t0,
            t_publish_ns: t_publish,
            t_speech_ns: t_speech,
        }
    }

    // ---- admission ---------------------------------------------------------------------------

    /// Admit every reply currently in the mailbox. Returns how many were processed.
    pub fn drain(&mut self) -> usize {
        let mut n = 0;
        while let Some(r) = self.rx.try_recv() {
            self.admit(r);
            n += 1;
        }
        n
    }

    pub fn admit(&mut self, r: PluginReply) {
        match r {
            PluginReply::Done(d) => self.admit_done(d),
            PluginReply::Fault(f) => self.admit_fault(f),
            PluginReply::Cancelled {
                plugin,
                job,
                slot_epoch,
            } => {
                if let Some(e) = self.plugins.get_mut(plugin as usize) {
                    if !e.slot.reply_is_current(slot_epoch) {
                        e.c.stale_epoch_dropped += 1;
                    } else if e.slot.busy == Some(job) {
                        e.slot.busy = None;
                    }
                }
            }
        }
    }

    fn note_exec(&mut self, p: usize, job: JobId, exec: u64, queued: u64, fuel: u64) {
        let e = &mut self.plugins[p];
        e.c.exec_ns_max = e.c.exec_ns_max.max(exec);
        e.c.queued_ns_max = e.c.queued_ns_max.max(queued);
        e.c.fuel_max = e.c.fuel_max.max(fuel);
        if job_turn(job) == self.turn && !job_is_background(job) && p < MAXP {
            self.cur_exec[p] = crate::timing::sat32(exec);
            self.cur_queued[p] = crate::timing::sat32(queued);
            self.cur_fuel[p] = fuel.min(u32::MAX as u64) as u32;
        }
    }

    fn admit_done(&mut self, d: DoneReply) {
        let p = d.plugin as usize;
        if p >= self.plugins.len() {
            return;
        }
        let now = now_ns();
        if !self.plugins[p].slot.reply_is_current(d.slot_epoch) {
            self.plugins[p].c.stale_epoch_dropped += 1;
            return;
        }
        if self.plugins[p].slot.busy == Some(d.job) {
            self.plugins[p].slot.busy = None;
        }
        self.note_exec(p, d.job, d.exec_ns, d.queued_ns, d.fuel_used);
        self.plugins[p].c.derive_calls += 1;
        if d.interactive {
            self.plugins[p].stats.record(d.fuel_used, d.exec_ns);
        }
        self.plugins[p].slot.step(SlotEvent::CallOk, now);
        if d.wall_overrun {
            self.plugins[p].c.overruns += 1;
            let act = self.plugins[p].slot.step(SlotEvent::WallOverrun, now);
            if matches!(
                act,
                Action::FaultToCold | Action::FaultToBackoff | Action::FaultToDisabled
            ) {
                self.plugins[p].c.strikes += 1;
                *self.plugins[p]
                    .c
                    .faults
                    .entry(format!("{:?}", FaultKind::WallOverrunFault))
                    .or_insert(0) += 1;
            }
        }
        let jt = job_turn(d.job);
        let cur = jt == self.turn && !job_is_background(d.job);
        // MQ-2: the host-minted token decides, `g` from the token, never taken at admission.
        if d.token.is_stale(&self.table) {
            self.plugins[p].c.stale_token_dropped += 1;
            if cur && p < MAXP {
                self.cur_outcomes[p] = oc::STALE_TOKEN;
            }
            return;
        }
        if jt < self.turn {
            self.plugins[p].c.late_admitted += 1;
        }
        let mut reads: HashSet<NodeId> = d.token.reads.iter().map(|x| x.0).collect();
        if reads.len() > 256 {
            reads.clear();
        }
        self.plugins[p].live_reads = reads;
        let tier = self.plugins[p].spec.tier;
        for FactEffect {
            node,
            group,
            value,
            conf,
            ..
        } in d.facts
        {
            if let Some(log) = self.debug_admit_order.as_mut() {
                log.push((d.plugin, node, group));
            }
            let v = self.facts.entry(node).or_default();
            let a = Admitted {
                plugin: d.plugin,
                tier,
                group,
                value,
                conf,
            };
            if let Some(slot) = v
                .iter_mut()
                .find(|x| x.plugin == d.plugin && x.group == group)
            {
                *slot = a;
            } else {
                v.push(a);
            }
            self.last_fact_turn.insert((d.plugin, node), jt);
            self.counters.admitted_facts += 1;
        }
        for n in d.refreshes {
            // At most one outstanding request per (node, observed revision): repeats are dropped.
            let rev = self
                .table
                .nodes
                .get(n as usize)
                .map(|x| x.stamp.iter().sum::<u64>())
                .unwrap_or(0);
            if self.refresh_seen.insert((d.plugin, n, rev)) {
                self.plugins[p].c.refresh_requests += 1;
            } else {
                self.plugins[p].c.refresh_dropped += 1;
            }
        }
        if cur && p < MAXP {
            self.cur_outcomes[p] = oc::OK;
        }
        self.sem_rev = self.sem_rev.wrapping_add(1);
    }

    fn admit_fault(&mut self, f: FaultReply) {
        let p = f.plugin as usize;
        if p >= self.plugins.len() {
            return;
        }
        let now = now_ns();
        if !self.plugins[p].slot.reply_is_current(f.slot_epoch) {
            self.plugins[p].c.stale_epoch_dropped += 1;
            return;
        }
        if self.plugins[p].slot.busy == Some(f.job) {
            self.plugins[p].slot.busy = None;
        }
        self.note_exec(p, f.job, f.exec_ns, f.queued_ns, f.fuel_used);
        let cur = job_turn(f.job) == self.turn && !job_is_background(f.job);
        if f.kind == FaultKind::OutOfFuel && !self.plugins[p].stats.oof_is_strike(&f.grant) {
            // A budget miss, not a fault: no facts, no strike (ADR-0026 2.4.2).
            self.plugins[p].c.budget_misses += 1;
            if cur && p < MAXP {
                self.cur_outcomes[p] = oc::OUT_OF_FUEL;
            }
            return;
        }
        let act = self.plugins[p].slot.step(SlotEvent::Fault, now);
        if act == Action::Impossible {
            self.counters.impossible_cells += 1;
        }
        let e = &mut self.plugins[p];
        e.c.strikes += 1;
        *e.c.faults.entry(format!("{:?}", f.kind)).or_insert(0) += 1;
        if cur && p < MAXP {
            self.cur_outcomes[p] = if f.kind == FaultKind::OutOfFuel {
                oc::OUT_OF_FUEL
            } else {
                oc::FAULT
            };
        }
    }

    // ---- FUSE ----------------------------------------------------------------------------------

    /// Fused entry of `node`: CR-2 rank (tier, specificity, plugin id, anchor key), so arrival
    /// order cannot change the winner (PF-6). Specificity here is the group code (all equal tier
    /// ties break by plugin id, then node as the anchor key).
    pub fn fuse(&self, node: NodeId) -> FusedEntry {
        let n = &self.table.nodes[node as usize];
        let mut winners: BTreeMap<u16, &Admitted> = BTreeMap::new();
        if let Some(v) = self.facts.get(&node) {
            for a in v {
                match winners.get(&a.group) {
                    Some(w) if (w.tier, w.plugin) <= (a.tier, a.plugin) => {}
                    _ => {
                        winners.insert(a.group, a);
                    }
                }
            }
        }
        FusedEntry {
            node,
            role: n.role,
            name: n.name.clone(),
            sem_rev: self.sem_rev,
            winners: winners
                .into_values()
                .map(|a| Fused {
                    group: a.group,
                    plugin: a.plugin,
                    value: a.value.clone(),
                })
                .collect(),
        }
    }

    /// A digest of the fused entries of `nodes` (PF-1 differential, PF-6 permutation).
    pub fn fused_digest(&self, nodes: &[NodeId]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for n in nodes {
            let e = self.fuse(*n);
            e.node.hash(&mut h);
            for w in &e.winners {
                (w.group, w.plugin).hash(&mut h);
                match &w.value {
                    Value::Text(t) => t.hash(&mut h),
                    Value::Number(x) => x.hash(&mut h),
                    Value::Code(x) => x.hash(&mut h),
                    Value::Bits(x) => x.hash(&mut h),
                    _ => 0u8.hash(&mut h),
                }
            }
        }
        h.finish()
    }

    pub fn reset_records(&mut self) {
        self.records.clear();
        self.counters = Counters::default();
        for e in &mut self.plugins {
            e.c = PluginCounters::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_encoding_roundtrip() {
        let j = make_job(12345, 7, true);
        assert_eq!(job_turn(j), 12345);
        assert!(job_is_background(j));
        assert!(!job_is_background(make_job(1, 7, false)));
    }
}
