// SPDX-License-Identifier: GPL-3.0-or-later
//! Arm B' of ADR-0026 §2.4.6: one actor per plugin on its own OS thread, bounded futex wait.
//!
//! * Each plugin is an [`Actor`] with an interactive and a background job slot (a newer job
//!   displaces an older one in its slot, which is answered `Cancelled`) and one running job.
//! * The writer sends all jobs of a turn (the waited ones first), then waits on a gate word whose
//!   low half is the mask of the waited jobs that replied (a short spin, a `futex_wait` with an
//!   absolute `CLOCK_MONOTONIC` deadline just short of the next hold, a final spin). Each waited
//!   job has its own hold (SPS, ADR-0026 section 2.4.7.4): the wait ends when every waited job
//!   has replied or passed its hold. It never touches tokio or a channel receive. Replies always
//!   go through the sink; the word only ends the wait.
//! * Timer slack is 1 ns on the writer thread (once) and on every worker.
//! * A supervisor orphans a wedged actor (no heartbeat), answers its jobs `ActorLost` and
//!   attaches a fresh executor on a new thread; the old thread is detached.
//!
//! Reference: `docs/bakeoff/m18a-0-plugin-call-model/harness/arms/arm-bprime`, rewritten for
//! production (no benchmark knobs, no allocation-free spare-slot swapping, no CPU pinning).

#![allow(clippy::verbose_bit_mask)] // the countdown word is `tag << 16 | remaining`

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst},
};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gorca_rt::process::monotonic_ns;
use gorca_rt::wait::{
    futex_wait_abs, futex_wait_bitset_abs, futex_wake, futex_wake_bitset, set_timer_slack,
};

use crate::call::{
    DeriveJob, Exec, HandoffOutcome, PlannedJob, PluginId, PluginReply, ReplySink, ReplyTiming,
    Turn, WLaneRunner,
};
use crate::slot::{FaultKind, JobId};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Tuning of the runner.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Writer spin before it parks on the futex.
    pub spin: Duration,
    /// Final writer spin before `D` (a late futex wake would cost the timer slack).
    pub tail: Duration,
    /// Worker spin before it parks.
    pub idle_spin: Duration,
    /// A job running longer than this with no heartbeat makes the actor wedged.
    pub lost_after: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            spin: Duration::from_micros(10),
            tail: Duration::from_micros(30),
            idle_spin: Duration::from_micros(40),
            lost_after: Duration::from_millis(50),
        }
    }
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

// ---- the wait gate -------------------------------------------------------------------------

/// A waited job's place in the gate of one turn.
#[derive(Debug, Clone, Copy)]
struct Ticket {
    tag: u16,
    bit: u8,
}

/// `word = tag << 16 | got`, `want = tag << 16 | wanted`: bit `b` of `got` is set once the
/// waited job with ticket bit `b` of turn `tag` has been answered (done, cancelled or lost). A
/// reply of another turn carries another tag and does not count.
#[derive(Debug, Default)]
struct Gate {
    word: AtomicU32,
    want: AtomicU32,
    parked: AtomicU32,
}

impl Gate {
    fn arm(&self, tag: u16, want: u32) {
        self.want
            .store((u32::from(tag) << 16) | (want & 0xffff), SeqCst);
        self.word.store(u32::from(tag) << 16, SeqCst);
    }

    /// The mask of the waited jobs of turn `tag` answered so far.
    fn got(&self, tag: u16) -> u32 {
        let v = self.word.load(Acquire);
        if v >> 16 == u32::from(tag) {
            v & 0xffff
        } else {
            0
        }
    }

    /// One waited job of turn `t.tag` is answered and its reply is in the mailbox; wake the
    /// writer only when it is parked and nothing else is awaited.
    fn done(&self, t: Ticket) {
        let bit = 1u32 << (t.bit & 15);
        loop {
            let v = self.word.load(SeqCst);
            if v >> 16 != u32::from(t.tag) || v & bit != 0 {
                return;
            }
            let nv = v | bit;
            if self.word.compare_exchange(v, nv, SeqCst, SeqCst).is_ok() {
                let w = self.want.load(SeqCst);
                if w >> 16 == u32::from(t.tag)
                    && nv & w & 0xffff == w & 0xffff
                    && self.parked.load(SeqCst) != 0
                {
                    futex_wake(&self.word);
                }
                return;
            }
        }
    }
}

// ---- parking -------------------------------------------------------------------------------

/// The word every parked worker of one runner waits on, each with its own bit. The writer wakes
/// all the workers that have a job with **one** system call (`FUTEX_WAKE_BITSET`), instead of one
/// call per parked worker: ten sequential wakes cost the writer 55-130 us on the dev host, which
/// is a third of the whole window (ADR-0026 section 2.4.7.4 step 2, in-flight fix 3).
#[derive(Debug, Default)]
struct Parking {
    seq: AtomicU32,
}

impl Parking {
    /// Wake the parked workers whose bits are in `mask`. A worker that read the sequence before
    /// this call returns from its wait at once, so no wake is lost.
    fn wake(&self, mask: u32) {
        if mask != 0 {
            self.seq.fetch_add(1, SeqCst);
            futex_wake_bitset(&self.seq, i32::MAX, mask);
        }
    }
}

/// The wake bit of a plugin's worker (plugins beyond 32 share bits: a spurious wake only).
fn wake_bit(plugin: PluginId) -> u32 {
    1 << (plugin % 32)
}

// ---- actor ---------------------------------------------------------------------------------

struct Queued {
    job: DeriveJob,
    sink: Arc<dyn ReplySink>,
    tag: Option<Ticket>,
    /// `CLOCK_MONOTONIC` ns when the writer handed it to the actor (start latency base).
    sent_ns: u64,
}

#[derive(Default)]
struct Slots {
    interactive: Option<Queued>,
    background: Option<Queued>,
}

struct Running {
    /// Incarnation of the worker that runs it: a detached (orphaned) thread must never take or
    /// clear the entry of its replacement.
    inc: u64,
    plugin: PluginId,
    job: JobId,
    slot_epoch: u64,
    sink: Arc<dyn ReplySink>,
    tag: Option<Ticket>,
}

struct Actor {
    plugin: PluginId,
    slots: Mutex<Slots>,
    parking: Arc<Parking>,
    parked: AtomicU32,
    /// Until this `CLOCK_MONOTONIC` instant an idle worker spins instead of parking (key-down
    /// pre-wake): it is already running when the focus change brings its job.
    spin_until: AtomicU64,
    inc: AtomicU64,
    running: Mutex<Option<Running>>,
    running_since: AtomicU64,
    stop: AtomicBool,
    replaced: AtomicU32,
    /// No worker could be (re)started: every job is answered `ActorLost` at once.
    dead: AtomicBool,
}

fn lost(plugin: PluginId, job: JobId, slot_epoch: u64) -> PluginReply {
    PluginReply::Fault {
        plugin,
        job,
        slot_epoch,
        kind: FaultKind::ActorLost,
        exec: Duration::ZERO,
    }
}

fn cancelled(j: &DeriveJob) -> PluginReply {
    PluginReply::Cancelled {
        plugin: j.plugin,
        job: j.job,
        slot_epoch: j.slot_epoch,
    }
}

impl Actor {
    fn new(plugin: PluginId, parking: Arc<Parking>) -> Arc<Self> {
        Arc::new(Self {
            plugin,
            slots: Mutex::new(Slots::default()),
            parking,
            parked: AtomicU32::new(0),
            spin_until: AtomicU64::new(0),
            inc: AtomicU64::new(0),
            running: Mutex::new(None),
            running_since: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            replaced: AtomicU32::new(0),
            dead: AtomicBool::new(false),
        })
    }

    /// The wake bit of this actor if its worker is parked (else 0: it is running or about to
    /// look at its slot, and needs no wake).
    fn wake_mask(&self) -> u32 {
        if self.parked.load(SeqCst) != 0 {
            wake_bit(self.plugin)
        } else {
            0
        }
    }

    /// Writer side: queue a job; a displaced job is answered `Cancelled`. Returns the wake mask
    /// the caller must pass to [`Parking::wake`] (batched over the jobs of a handoff).
    fn dispatch(&self, mut q: Queued, gate: &Gate) -> u32 {
        q.sent_ns = monotonic_ns();
        if self.dead.load(Acquire) {
            q.sink.send(lost(q.job.plugin, q.job.job, q.job.slot_epoch));
            if let Some(t) = q.tag {
                gate.done(t);
            }
            return 0;
        }
        let displaced = {
            let mut s = lock(&self.slots);
            let slot = if q.job.interactive {
                &mut s.interactive
            } else {
                &mut s.background
            };
            slot.replace(q)
        };
        if let Some(old) = displaced {
            old.sink.send(cancelled(&old.job));
            if let Some(t) = old.tag {
                gate.done(t);
            }
        }
        self.wake_mask()
    }

    fn cancel(&self, job: JobId, gate: &Gate) {
        let removed = {
            let mut s = lock(&self.slots);
            if s.background.as_ref().is_some_and(|q| q.job.job == job) {
                s.background.take()
            } else if s.interactive.as_ref().is_some_and(|q| q.job.job == job) {
                s.interactive.take()
            } else {
                None
            }
        };
        if let Some(q) = removed {
            q.sink.send(cancelled(&q.job));
            if let Some(t) = q.tag {
                gate.done(t);
            }
        }
    }

    /// Answer every queued job `ActorLost` (the actor has no worker).
    fn drain_lost(&self, gate: &Gate) {
        let queued: Vec<Queued> = {
            let mut s = lock(&self.slots);
            [s.interactive.take(), s.background.take()]
                .into_iter()
                .flatten()
                .collect()
        };
        for q in queued {
            q.sink.send(lost(q.job.plugin, q.job.job, q.job.slot_epoch));
            if let Some(t) = q.tag {
                gate.done(t);
            }
        }
    }

    fn pending(&self) -> bool {
        let s = lock(&self.slots);
        s.interactive.is_some() || s.background.is_some()
    }

    /// Worker side: interactive first.
    fn take(&self, inc: u64) -> Option<Queued> {
        let mut s = lock(&self.slots);
        if self.inc.load(Acquire) != inc {
            return None;
        }
        s.interactive.take().or_else(|| s.background.take())
    }

    /// Supervisor: answer the running and queued jobs with `ActorLost`, bump the generation so
    /// the old thread discards a late result.
    fn orphan(&self, gate: &Gate) -> u64 {
        let mut replies: Vec<(PluginReply, Arc<dyn ReplySink>, Option<Ticket>)> = Vec::new();
        if let Some(r) = lock(&self.running).take() {
            replies.push((lost(r.plugin, r.job, r.slot_epoch), r.sink, r.tag));
        }
        let inc;
        {
            let mut s = lock(&self.slots);
            for q in [s.interactive.take(), s.background.take()]
                .into_iter()
                .flatten()
            {
                replies.push((
                    lost(q.job.plugin, q.job.job, q.job.slot_epoch),
                    q.sink,
                    q.tag,
                ));
            }
            inc = self.inc.fetch_add(1, AcqRel) + 1;
        }
        self.running_since.store(0, Release);
        self.replaced.fetch_add(1, Relaxed);
        for (r, sink, tag) in replies {
            sink.send(r);
            if let Some(t) = tag {
                gate.done(t);
            }
        }
        inc
    }
}

fn execute(
    actor: &Actor,
    inc: u64,
    exec: &mut dyn Exec,
    q: Queued,
    gate: &Gate,
    cold: bool,
) -> bool {
    let Queued {
        job,
        sink,
        tag,
        sent_ns,
    } = q;
    let picked = monotonic_ns();
    *lock(&actor.running) = Some(Running {
        inc,
        plugin: job.plugin,
        job: job.job,
        slot_epoch: job.slot_epoch,
        sink: Arc::clone(&sink),
        tag,
    });
    actor.running_since.store(monotonic_ns().max(1), Release);
    let r = catch_unwind(AssertUnwindSafe(|| exec.run(&job)));
    if actor.inc.load(Acquire) == inc {
        actor.running_since.store(0, Release);
    }
    let reply = if let Ok(r) = r {
        r
    } else {
        exec.reset();
        lost(job.plugin, job.job, job.slot_epoch)
    };
    // The supervisor may have answered already: then this result is discarded.
    {
        let mut running = lock(&actor.running);
        if running.as_ref().is_none_or(|r| r.inc != inc) {
            return false;
        }
        *running = None;
    }
    // Reply first, then count down: a writer that sees the mask complete finds every reply in
    // its mailbox.
    sink.send_timed(
        reply,
        ReplyTiming {
            start: Duration::from_nanos(picked.saturating_sub(sent_ns)),
            sent_ns,
            picked_ns: picked,
            cold,
            at_ns: monotonic_ns(),
        },
    );
    if let Some(t) = tag {
        gate.done(t);
    }
    true
}

fn worker_main(
    actor: &Arc<Actor>,
    inc: u64,
    mut exec: Box<dyn Exec>,
    gate: &Arc<Gate>,
    cfg: Config,
    ready: &AtomicU32,
) {
    let _ = set_timer_slack(1);
    exec.warm();
    ready.fetch_add(1, Release);
    let mut idle_since = monotonic_ns();
    // Set when the worker went to park (or was about to) since its last job: the job it takes
    // next started cold. A spinning worker (idle tail or pre-armed) takes it hot.
    let mut went_to_park = false;
    loop {
        if actor.stop.load(Acquire) || actor.inc.load(Acquire) != inc {
            return;
        }
        if let Some(q) = actor.take(inc) {
            if !execute(actor, inc, exec.as_mut(), q, gate, went_to_park) {
                return;
            }
            went_to_park = false;
            // Off the writer turn: a faulted instance is rebuilt before the next job arrives.
            exec.warm();
            idle_since = monotonic_ns();
            continue;
        }
        if monotonic_ns().saturating_sub(idle_since) < ns(cfg.idle_spin) {
            // Spinning: a job that arrives now starts hot, even after an earlier park.
            went_to_park = false;
            std::hint::spin_loop();
            continue;
        }
        if monotonic_ns() < actor.spin_until.load(Acquire) {
            went_to_park = false;
            std::hint::spin_loop();
            continue;
        }
        // Park (Dekker with the writer: publish `parked`, re-check, then futex on the sequence).
        went_to_park = true;
        let seq = actor.parking.seq.load(SeqCst);
        actor.parked.store(1, SeqCst);
        if !actor.pending() && !actor.stop.load(SeqCst) && actor.inc.load(SeqCst) == inc {
            futex_wait_bitset_abs(
                &actor.parking.seq,
                seq,
                monotonic_ns() + 20_000_000,
                wake_bit(actor.plugin),
            );
        }
        actor.parked.store(0, SeqCst);
        idle_since = monotonic_ns();
    }
}

type Maker = Box<dyn Fn(PluginId) -> Option<Box<dyn Exec>> + Send + Sync>;

struct Core {
    cfg: Config,
    parking: Arc<Parking>,
    actors: Vec<Arc<Actor>>,
    gate: Arc<Gate>,
    maker: Maker,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    stop: AtomicBool,
    stop_word: AtomicU32,
    ready: Arc<AtomicU32>,
    next_tag: AtomicU32,
}

impl Core {
    fn spawn(&self, actor: &Arc<Actor>, inc: u64, exec: Box<dyn Exec>) -> std::io::Result<()> {
        let a = Arc::clone(actor);
        let g = Arc::clone(&self.gate);
        let cfg = self.cfg;
        let ready = Arc::clone(&self.ready);
        let h = std::thread::Builder::new()
            .name(format!("gorca-plugin-{}", actor.plugin))
            .spawn(move || worker_main(&a, inc, exec, &g, cfg, &ready))?;
        lock(&self.threads).push(h);
        Ok(())
    }

    /// Keep every idle worker spinning for `spin` (wakes the parked ones).
    fn prewake(&self, spin: Duration) {
        let until = monotonic_ns() + ns(spin);
        let mut mask = 0;
        for a in &self.actors {
            a.spin_until.store(until, Release);
            mask |= a.wake_mask();
        }
        self.parking.wake(mask);
    }

    /// Keep the workers of the plugins in `who` (bit per plugin id) spinning until the
    /// `CLOCK_MONOTONIC` instant `until_ns` (wakes the parked ones).
    fn prewake_set(&self, who: u64, until_ns: u64) {
        let mut mask = 0;
        for a in &self.actors {
            if a.plugin < 64 && who & (1 << a.plugin) != 0 {
                a.spin_until.store(until_ns, Release);
                mask |= a.wake_mask();
            }
        }
        self.parking.wake(mask);
    }

    /// End every pre-armed spin: the focus change came, a worker runs its job or parks.
    fn disarm(&self) {
        for a in &self.actors {
            a.spin_until.store(0, Release);
        }
    }

    /// Replace the executor of every actor stuck in one job for too long.
    fn supervise(&self) -> u32 {
        let now = monotonic_ns();
        let mut n = 0;
        for a in &self.actors {
            let since = a.running_since.load(Acquire);
            if since == 0 || now.saturating_sub(since) < ns(self.cfg.lost_after) {
                continue;
            }
            let inc = a.orphan(&self.gate);
            if let Some(exec) = (self.maker)(a.plugin)
                && self.spawn(a, inc, exec).is_ok()
            {
                n += 1;
            } else {
                // No replacement: fail the actor closed so no job waits forever.
                a.dead.store(true, Release);
                a.drain_lost(&self.gate);
            }
        }
        // Finished (orphaned and returned) threads are dropped here, not kept until shutdown.
        lock(&self.threads).retain(|h| !h.is_finished());
        n
    }
}

thread_local! {
    static SLACK_SET: Cell<bool> = const { Cell::new(false) };
}

fn writer_slack() {
    SLACK_SET.with(|s| {
        if !s.get() {
            let _ = set_timer_slack(1);
            s.set(true);
        }
    });
}

/// A handle that pre-wakes the workers of one runner (the key-down hook of the daemon). Weak:
/// it never keeps a closed scope's threads alive.
#[derive(Clone)]
pub struct PrewakeHandle(std::sync::Weak<Core>);

impl std::fmt::Debug for PrewakeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrewakeHandle")
    }
}

impl PrewakeHandle {
    /// Keep the runner's idle workers spinning for `spin`. False when the runner is gone.
    pub fn arm(&self, spin: Duration) -> bool {
        match self.0.upgrade() {
            Some(c) => {
                c.prewake(spin);
                true
            }
            None => false,
        }
    }

    /// Keep the workers of the plugins in `who` (bit per plugin id) spinning until the
    /// `CLOCK_MONOTONIC` instant `until_ns`. False when the runner is gone.
    pub fn arm_set(&self, who: u64, until_ns: u64) -> bool {
        match self.0.upgrade() {
            Some(c) => {
                c.prewake_set(who, until_ns);
                true
            }
            None => false,
        }
    }
}

/// The production [`WLaneRunner`].
pub struct ActorRunner {
    core: Arc<Core>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    down: bool,
}

impl std::fmt::Debug for ActorRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorRunner")
            .field("actors", &self.core.actors.len())
            .finish_non_exhaustive()
    }
}

impl ActorRunner {
    /// Build from `(plugin id, executor)` pairs. `maker` creates a fresh executor for an
    /// orphaned plugin.
    pub fn new(
        execs: Vec<(PluginId, Box<dyn Exec>)>,
        maker: Maker,
        cfg: Config,
    ) -> std::io::Result<Self> {
        let parking = Arc::new(Parking::default());
        let mut actors = Vec::with_capacity(execs.len());
        let mut pending = Vec::with_capacity(execs.len());
        for (id, e) in execs {
            let a = Actor::new(id, Arc::clone(&parking));
            actors.push(Arc::clone(&a));
            pending.push((a, e));
        }
        let core = Arc::new(Core {
            cfg,
            parking,
            actors,
            gate: Arc::new(Gate::default()),
            maker,
            threads: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            stop_word: AtomicU32::new(0),
            ready: Arc::new(AtomicU32::new(0)),
            next_tag: AtomicU32::new(0),
        });
        let n = u32::try_from(pending.len()).unwrap_or(u32::MAX);
        for (a, e) in pending {
            core.spawn(&a, 0, e)?;
        }
        // Wait (bounded) until every actor has built its instance, so the first interactive
        // handoff finds warm actors. This runs at load time, never on a writer turn.
        let end = monotonic_ns() + 2_000_000_000;
        while core.ready.load(Acquire) < n && monotonic_ns() < end {
            std::thread::yield_now();
        }
        let sc = Arc::clone(&core);
        let supervisor = std::thread::Builder::new()
            .name("gorca-plugin-sup".into())
            .spawn(move || {
                while !sc.stop.load(Acquire) {
                    // Sleep on the stop word with an absolute deadline: no `sleep`, and shutdown
                    // wakes it at once.
                    futex_wait_abs(&sc.stop_word, 0, monotonic_ns() + 2_000_000);
                    sc.supervise();
                }
            })?;
        Ok(Self {
            core,
            supervisor: Some(supervisor),
            down: false,
        })
    }

    /// The pre-wake handle of this runner.
    #[must_use]
    pub fn prewake_handle(&self) -> PrewakeHandle {
        PrewakeHandle(Arc::downgrade(&self.core))
    }

    /// Replace wedged actors now (the supervisor does this every 2 ms).
    pub fn supervise_now(&self) -> u32 {
        self.core.supervise()
    }

    /// How many times the plugin's actor was replaced.
    #[must_use]
    pub fn replaced(&self, plugin: PluginId) -> u32 {
        self.actor(plugin).map_or(0, |a| a.replaced.load(Relaxed))
    }

    fn actor(&self, plugin: PluginId) -> Option<&Arc<Actor>> {
        self.core.actors.iter().find(|a| a.plugin == plugin)
    }

    /// Hand one job to its actor. A job for an unknown plugin is answered `ActorLost` at once
    /// (and its ticket counted, so the wait does not hold for it).
    /// Returns the wake mask of the job's actor (0 when it is running).
    fn send_one(&self, job: DeriveJob, sink: &Arc<dyn ReplySink>, tag: Option<Ticket>) -> u32 {
        if let Some(a) = self.actor(job.plugin) {
            a.dispatch(
                Queued {
                    job,
                    sink: Arc::clone(sink),
                    tag,
                    sent_ns: 0,
                },
                &self.core.gate,
            )
        } else {
            sink.send(lost(job.plugin, job.job, job.slot_epoch));
            if let Some(t) = tag {
                self.core.gate.done(t);
            }
            0
        }
    }

    /// Send the jobs and wake the parked workers among their actors with one system call.
    fn send(&self, jobs: Vec<DeriveJob>, sink: &Arc<dyn ReplySink>) {
        let mut mask = 0;
        for job in jobs {
            mask |= self.send_one(job, sink, None);
        }
        self.core.parking.wake(mask);
    }

    /// One writer handoff. Waited jobs (`hold` set, at most 16) are sent first, then the rest;
    /// the writer then waits until every waited job has replied or passed its own hold, and never
    /// past `turn.deadline_ns` (the absolute clamp of ADR-0026 section 2.4.7.4 step 7).
    fn handoff(
        &mut self,
        turn: &Turn,
        jobs: Vec<PlannedJob>,
        sink: &Arc<dyn ReplySink>,
        disarm: bool,
    ) -> HandoffOutcome {
        writer_slack();
        let start = monotonic_ns();
        let gate = Arc::clone(&self.core.gate);
        // The runner owns turn tags: a caller-supplied tag could be reused while a late reply of
        // the earlier turn is still in flight and would count down the wrong wait.
        let tag = u16::try_from(self.core.next_tag.fetch_add(1, Relaxed) % 0xffff).unwrap_or(0) + 1;
        let mut waited: Vec<PlannedJob> = Vec::with_capacity(jobs.len());
        let mut rest: Vec<PlannedJob> = Vec::new();
        for j in jobs {
            if j.hold.is_some() && waited.len() < 16 {
                waited.push(j);
            } else {
                rest.push(j);
            }
        }
        let n_w = waited.len();
        let want: u32 = if n_w >= 16 { 0xffff } else { (1u32 << n_w) - 1 };
        gate.arm(tag, want);
        let mut hold_ns = [0u64; 16];
        let mut mask = 0;
        for (i, pj) in waited.into_iter().enumerate() {
            hold_ns[i] = pj.hold.map_or(0, ns);
            let bit = u8::try_from(i).unwrap_or(0);
            mask |= self.send_one(pj.job, sink, Some(Ticket { tag, bit }));
        }
        // One wake for every parked worker that has a waited job; the holds count from the
        // moment the waited jobs are out, the others follow at once.
        self.core.parking.wake(mask);
        let t_sent = monotonic_ns();
        let mut mask = 0;
        for pj in rest {
            mask |= self.send_one(pj.job, sink, None);
        }
        self.core.parking.wake(mask);
        if disarm {
            self.core.disarm();
        }
        let mut abs = [0u64; 16];
        for i in 0..n_w {
            // Best case for the admission that follows: a single reply.
            abs[i] = t_sent
                .saturating_add(hold_ns[i])
                .min(turn.deadline_ns.saturating_sub(turn.per_reply_ns));
        }
        let mut released = 0u32;
        let mut woke_late = 0u64;
        if turn.wait && n_w > 0 {
            let spin_end = start + ns(self.core.cfg.spin);
            let tail = ns(self.core.cfg.tail);
            loop {
                let got = gate.got(tag);
                let pending = want & !got & !released;
                if pending == 0 {
                    break;
                }
                let now = monotonic_ns();
                // Every reply in costs `per_reply_ns` to admit after the wait, inside `D`.
                let cut = turn
                    .deadline_ns
                    .saturating_sub(turn.per_reply_ns * (u64::from((got & want).count_ones()) + 1));
                if turn.per_reply_ns > 0 && now >= cut {
                    woke_late = woke_late.max(now - cut);
                    break;
                }
                let mut next = u64::MAX;
                for (i, a) in abs.iter().enumerate().take(n_w) {
                    if pending & (1 << i) == 0 {
                        continue;
                    }
                    if now >= *a {
                        released |= 1 << i;
                        woke_late = woke_late.max(now - *a);
                    } else {
                        next = next.min(*a);
                    }
                }
                if next == u64::MAX {
                    continue;
                }
                // A reply that comes in while parked does not wake the writer (only the last
                // one does) but moves the cut earlier: park no longer than the earliest cut.
                let lowest_cut = turn
                    .deadline_ns
                    .saturating_sub(turn.per_reply_ns * (n_w as u64 + 1));
                let next = if turn.per_reply_ns > 0 {
                    next.min(lowest_cut.max(now))
                } else {
                    next
                };
                if now < spin_end || now + tail + 5_000 >= next {
                    std::hint::spin_loop();
                    continue;
                }
                gate.parked.store(1, SeqCst);
                let v = gate.word.load(SeqCst);
                if want & !(v & 0xffff) & !released != 0 {
                    futex_wait_abs(&gate.word, v, next - tail);
                }
                gate.parked.store(0, SeqCst);
            }
        }
        HandoffOutcome {
            asked: u32::try_from(n_w).unwrap_or(u32::MAX),
            arrived: (gate.got(tag) & want).count_ones(),
            waited: Duration::from_nanos(monotonic_ns().saturating_sub(start)),
            woke_late: Duration::from_nanos(woke_late),
        }
    }
}

impl WLaneRunner for ActorRunner {
    fn interactive(
        &mut self,
        turn: &Turn,
        jobs: Vec<DeriveJob>,
        sink: &Arc<dyn ReplySink>,
    ) -> HandoffOutcome {
        // Every job waited, all to the same deadline: the shared countdown of the interim policy
        // (ADR-0026 section 2.4.6), as one hold per job.
        let hold = Duration::from_nanos(turn.deadline_ns.saturating_sub(monotonic_ns()));
        let planned = jobs
            .into_iter()
            .map(|job| PlannedJob {
                job,
                hold: Some(hold),
            })
            .collect();
        self.handoff(turn, planned, sink, false)
    }

    fn interactive_planned(
        &mut self,
        turn: &Turn,
        jobs: Vec<PlannedJob>,
        sink: &Arc<dyn ReplySink>,
    ) -> HandoffOutcome {
        self.handoff(turn, jobs, sink, true)
    }

    fn background(&mut self, jobs: Vec<DeriveJob>, sink: &Arc<dyn ReplySink>) {
        writer_slack();
        self.send(jobs, sink);
    }

    fn cancel(&mut self, plugin: PluginId, job: JobId) {
        if let Some(a) = self.actor(plugin) {
            a.cancel(job, &self.core.gate);
        }
    }

    fn shutdown(&mut self) {
        if self.down {
            return;
        }
        self.down = true;
        self.core.stop.store(true, Release);
        self.core.stop_word.store(1, SeqCst);
        futex_wake(&self.core.stop_word);
        for a in &self.core.actors {
            a.stop.store(true, Release);
        }
        self.core.parking.wake(u32::MAX);
        let mut hs: Vec<_> = std::mem::take(&mut *lock(&self.core.threads));
        // A healthy worker finishes its current call (bounded by fuel) and exits. Wait a bounded
        // time for them; a wedged one is detached (it holds no writer state).
        let end = monotonic_ns() + 100_000_000;
        while !hs.is_empty() && monotonic_ns() < end {
            let (done, rest): (Vec<_>, Vec<_>) = hs
                .into_iter()
                .partition(std::thread::JoinHandle::is_finished);
            for h in done {
                let _ = h.join();
            }
            hs = rest;
            if !hs.is_empty() {
                futex_wait_abs(&self.core.stop_word, 1, monotonic_ns() + 1_000_000);
            }
        }
        if let Some(s) = self.supervisor.take() {
            let _ = s.join();
        }
    }
}

impl Drop for ActorRunner {
    fn drop(&mut self) {
        self.shutdown();
    }
}
