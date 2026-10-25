// SPDX-License-Identifier: GPL-3.0-or-later
//! The call-model boundary (ADR-0026 §2.4.6): messages and the [`WLaneRunner`] trait.
//!
//! The owner decided on B': a plugin is an actor, the writer talks to it only by messages and
//! waits at most the shared deadline `D`. The trait keeps the writer independent of how jobs are
//! executed; `actor::ActorRunner` is the production implementation. No type here is a
//! connector's; node identity is a host-side index.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::host::Effect;
use crate::instance::{CallDone, CallFault, DirtyNode, Loaded, PluginInstance, ReadSet};
use crate::limits::FuelGrant;
use crate::slot::{Event, FaultKind, JobId};
use crate::trust::{Perms, TrustTier};
use crate::view::ModelView;

/// Plugin id within one runner.
pub type PluginId = u32;

/// The shared interactive deadline, measured from the start of the writer turn (r4).
pub const D_INTERACTIVE: Duration = Duration::from_micros(300);

/// Writer -> plugin actor.
#[derive(Debug, Clone)]
pub struct DeriveJob {
    /// Target plugin.
    pub plugin: PluginId,
    /// Job id.
    pub job: JobId,
    /// The slot incarnation the job was sent under.
    pub slot_epoch: u64,
    /// Interactive slice or background.
    pub interactive: bool,
    /// The rule epoch passed to the guest.
    pub epoch: u64,
    /// Dirty nodes.
    pub nodes: Vec<DirtyNode>,
    /// Immutable snapshot the call reads.
    pub view: Arc<dyn ModelView>,
    /// Fuel grant.
    pub grant: FuelGrant,
    /// When the writer queued it (for the `queued` metric, never an overrun).
    pub queued_at: Instant,
}

/// Plugin actor -> writer (through the writer's own mailbox).
#[derive(Debug)]
pub enum PluginReply {
    /// Returned normally.
    Done {
        /// Plugin.
        plugin: PluginId,
        /// Job.
        job: JobId,
        /// Slot incarnation.
        slot_epoch: u64,
        /// Buffered effects; the writer admits them (taint and token are host-side).
        effects: Vec<Effect>,
        /// The read set R of the call, for the revision token (section 2.6.1).
        reads: ReadSet,
        /// Guest wall time.
        exec: Duration,
        /// Mailbox delay.
        queued: Duration,
        /// Fuel consumed.
        fuel_used: u64,
        /// Wall overrun rule hit.
        wall_overrun: bool,
    },
    /// Faulted; effects dropped.
    Fault {
        /// Plugin.
        plugin: PluginId,
        /// Job.
        job: JobId,
        /// Slot incarnation.
        slot_epoch: u64,
        /// Kind.
        kind: FaultKind,
        /// Guest wall time.
        exec: Duration,
    },
    /// Superseded or cancelled: not a fault, no strike.
    Cancelled {
        /// Plugin.
        plugin: PluginId,
        /// Job.
        job: JobId,
        /// Slot incarnation.
        slot_epoch: u64,
    },
}

impl PluginReply {
    /// The incarnation the reply belongs to.
    #[must_use]
    pub fn slot_epoch(&self) -> u64 {
        match self {
            Self::Done { slot_epoch, .. }
            | Self::Fault { slot_epoch, .. }
            | Self::Cancelled { slot_epoch, .. } => *slot_epoch,
        }
    }

    /// The slot events this reply stands for, in order.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        match self {
            Self::Done { wall_overrun, .. } => {
                if *wall_overrun {
                    vec![Event::CallOk, Event::WallOverrun]
                } else {
                    vec![Event::CallOk]
                }
            }
            Self::Fault { kind, .. } => vec![Event::Fault(*kind)],
            Self::Cancelled { .. } => vec![Event::Cancelled],
        }
    }
}

/// The writer's mailbox as seen by an actor. `send` must not block (a bounded queue that drops
/// and counts, or an unbounded push owned by the writer).
pub trait ReplySink: Send + Sync {
    /// Post a reply.
    fn send(&self, reply: PluginReply);

    /// Post a reply with what the actor measured about the job (the scheduler's observations,
    /// ADR-0026 section 2.4.7.6). Sinks that do not care keep the default.
    fn send_timed(&self, reply: PluginReply, _timing: ReplyTiming) {
        self.send(reply);
    }
}

/// What the actor measured about one job, besides the guest time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyTiming {
    /// From the writer's hand-off to the moment the worker picked the job up (thread start
    /// latency plus any wait behind an earlier job of the same actor).
    pub start: Duration,
    /// `CLOCK_MONOTONIC` ns at which the writer handed the job to the actor.
    pub sent_ns: u64,
    /// `CLOCK_MONOTONIC` ns at which the worker picked the job up.
    pub picked_ns: u64,
    /// The worker had gone to park since its last job: the start was cold.
    pub cold: bool,
    /// `CLOCK_MONOTONIC` ns at which the reply was posted.
    pub at_ns: u64,
}

/// A job with the time the writer will hold for it (`None`: dispatched, never waited for).
#[derive(Debug, Clone)]
pub struct PlannedJob {
    /// The job.
    pub job: DeriveJob,
    /// How long after the waited jobs are out the writer waits for it at most.
    pub hold: Option<Duration>,
}

/// The wait budget of one writer turn.
#[derive(Debug, Clone, Copy)]
pub struct Turn {
    /// Absolute `CLOCK_MONOTONIC` ns of the deadline `D = t0 + 0.3 ms`.
    pub deadline_ns: u64,
    /// False when the writer already waited for this keypress path or flaps: send only.
    pub wait: bool,
}

impl Turn {
    /// A turn whose writer started at `t0_ns` (`gorca_rt::process::monotonic_ns`).
    #[must_use]
    pub fn starting_at(t0_ns: u64, wait: bool) -> Self {
        Self::clamped(t0_ns, wait, 0)
    }

    /// A turn whose wait must end `reserve_ns` before `D` so that apply + fuse fit inside it
    /// (the absolute clamp of ADR-0026 section 2.4.7.4 step 7).
    #[must_use]
    pub fn clamped(t0_ns: u64, wait: bool, reserve_ns: u64) -> Self {
        Self {
            deadline_ns: (t0_ns + u64::try_from(D_INTERACTIVE.as_nanos()).unwrap_or(300_000))
                .saturating_sub(reserve_ns),
            wait,
        }
    }
}

/// What a bounded wait produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffOutcome {
    /// Jobs sent.
    pub asked: u32,
    /// Replies that arrived before the wait ended.
    pub arrived: u32,
    /// Time spent waiting.
    pub waited: Duration,
    /// Wake minus `D` when the deadline ended the wait (WQ-6 metric).
    pub woke_late: Duration,
}

/// The call model: how the writer hands derive jobs to plugin actors.
pub trait WLaneRunner: Send {
    /// Send the focus-chain jobs of one turn at once, then wait until all replied or the turn's
    /// deadline. Replies always arrive through `sink`; the wait only ends early. The calling
    /// thread's timer slack is set to 1 ns permanently (ADR-0026 r4); call it from writer
    /// threads, not from arbitrary pool threads.
    fn interactive(
        &mut self,
        turn: &Turn,
        jobs: Vec<DeriveJob>,
        sink: &Arc<dyn ReplySink>,
    ) -> HandoffOutcome;

    /// As [`WLaneRunner::interactive`] with a hold per job (SPS): the jobs with a hold are sent
    /// first and the writer waits until each has replied or passed its own hold, never past
    /// `turn.deadline_ns`; the jobs without one are dispatched in the same turn and not waited
    /// for. Pre-armed workers are released once the jobs are out. The default has no per-job
    /// holds: it waits for the jobs that have one, to the turn's deadline.
    fn interactive_planned(
        &mut self,
        turn: &Turn,
        jobs: Vec<PlannedJob>,
        sink: &Arc<dyn ReplySink>,
    ) -> HandoffOutcome {
        let (w, r): (Vec<_>, Vec<_>) = jobs.into_iter().partition(|j| j.hold.is_some());
        let r: Vec<DeriveJob> = r.into_iter().map(|j| j.job).collect();
        if !r.is_empty() {
            self.background(
                r.into_iter()
                    .map(|mut j| {
                        j.interactive = false;
                        j
                    })
                    .collect(),
                sink,
            );
        }
        self.interactive(turn, w.into_iter().map(|j| j.job).collect(), sink)
    }

    /// Send background jobs; no wait.
    fn background(&mut self, jobs: Vec<DeriveJob>, sink: &Arc<dyn ReplySink>);

    /// Cancel a queued job (a running one is only stopped by fuel).
    fn cancel(&mut self, plugin: PluginId, job: JobId);

    /// Stop all actors.
    fn shutdown(&mut self);
}

/// What an actor thread executes. Production: [`PluginExec`]; tests may substitute.
pub trait Exec: Send + 'static {
    /// Run one job and describe the outcome.
    fn run(&mut self, job: &DeriveJob) -> PluginReply;
    /// Drop guest state after a host panic.
    fn reset(&mut self);
    /// Build the instance off the writer turn (at start and after a fault), so the first
    /// interactive call does not pay the instantiation. Default: nothing.
    fn warm(&mut self) {}
}

/// The wasmtime-backed executor of one plugin: owns the instance, rebuilds it after a fault.
pub struct PluginExec {
    loaded: Loaded,
    grants: Perms,
    tier: TrustTier,
    memory_mib: u32,
    instance: Option<PluginInstance>,
    serial: u64,
}

impl std::fmt::Debug for PluginExec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginExec")
            .field("serial", &self.serial)
            .field("live", &self.instance.is_some())
            .finish_non_exhaustive()
    }
}

impl PluginExec {
    /// An executor with no live instance (`Cold`).
    #[must_use]
    pub fn new(loaded: Loaded, grants: Perms, tier: TrustTier, memory_mib: u32) -> Self {
        Self {
            loaded,
            grants,
            tier,
            memory_mib,
            instance: None,
            serial: 0,
        }
    }

    /// Serial of the current (or last) instance; a rebuild increments it.
    #[must_use]
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// True when an instance is live.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.instance.is_some()
    }

    /// Counters of the live instance.
    #[must_use]
    pub fn counters(&self) -> Option<crate::host::Counters> {
        self.instance.as_ref().map(PluginInstance::counters)
    }
}

impl Exec for PluginExec {
    fn run(&mut self, job: &DeriveJob) -> PluginReply {
        let queued = job.queued_at.elapsed();
        let fault = |kind, exec| PluginReply::Fault {
            plugin: job.plugin,
            job: job.job,
            slot_epoch: job.slot_epoch,
            kind,
            exec,
        };
        if self.instance.is_none() {
            self.serial += 1;
            match PluginInstance::new(
                &self.loaded,
                self.serial,
                self.grants,
                self.tier,
                self.memory_mib,
                Arc::clone(&job.view),
            ) {
                Ok(i) => self.instance = Some(i),
                Err(k) => return fault(k, Duration::ZERO),
            }
        }
        let Some(inst) = self.instance.as_mut() else {
            return fault(FaultKind::Instantiate, Duration::ZERO);
        };
        match inst.derive(job.interactive, job.epoch, &job.nodes, &job.view, job.grant) {
            Ok(CallDone {
                reads,
                effects,
                exec,
                fuel_used,
                wall_overrun,
            }) => PluginReply::Done {
                plugin: job.plugin,
                job: job.job,
                slot_epoch: job.slot_epoch,
                effects,
                reads,
                exec,
                queued,
                fuel_used,
                wall_overrun,
            },
            Err(CallFault { kind, exec }) => {
                // The store is not trusted after a fault: rebuild on the next call.
                self.instance = None;
                fault(kind, exec)
            }
        }
    }

    fn reset(&mut self) {
        self.instance = None;
    }

    fn warm(&mut self) {
        if self.instance.is_none() {
            self.serial += 1;
            self.instance = PluginInstance::new(
                &self.loaded,
                self.serial,
                self.grants,
                self.tier,
                self.memory_mib,
                Arc::new(crate::view::EmptyView),
            )
            .ok();
        }
    }
}
