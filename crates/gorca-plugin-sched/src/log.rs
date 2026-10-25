// SPDX-License-Identifier: GPL-3.0-or-later
//! The input log of the scheduler (`GORCA_SCHED_LOG`, SPS-6): every external input and the hash
//! of every decision, as numbers only. No text, no names, no node content: a plugin id is an
//! index. A ring of fixed size, written without allocation once full.

use crate::{
    Assignment, KeyClass, Label, Observation, Params, Place, PlaceMode, Scheduler, SpinBucket,
    Start, Tier, Triggered, TurnTimes,
};

/// One input of the scheduler, with the hash or mask of what it decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rec {
    /// A key went down.
    KeyDown {
        /// Context index.
        ctx: u8,
        /// A typing key.
        typing: bool,
        /// Caller clock.
        now_us: u64,
        /// Decision: workers armed.
        threads: u8,
        /// Decision: which.
        who: u32,
    },
    /// A key's focus change triggered these plugins (external `observe_context`).
    Context {
        /// Context index.
        ctx: u8,
        /// Triggered plugins.
        triggered: u32,
    },
    /// A dispatch epoch.
    Plan {
        /// Caller clock.
        now_us: u64,
        /// Measured time since the turn start, `u32::MAX` = none.
        spent_us: u32,
        /// The reserve passed to `plan_with`, `u32::MAX` = none.
        fuse_us: u32,
        /// Hot queues; 255 = own threads.
        queues: u8,
        /// Armed workers at the time (informational; derived on replay).
        armed: u32,
        /// Triggered plugins.
        mask: u32,
        /// Busy among them.
        busy: u32,
        /// Decision hash.
        hash: u64,
    },
    /// A reply.
    Observe {
        /// Plugin.
        pid: u8,
        /// Started hot.
        hot: bool,
        /// Start latency.
        start_us: u32,
        /// Guest time.
        exec_us: u32,
        /// Send to reply.
        resp_us: u32,
        /// Assignment: deadline.
        deadline_us: u32,
        /// Assignment: hold.
        hold_us: u32,
        /// Assignment: exec allowance.
        exec_allow_us: u32,
        /// Assignment: clipped.
        clipped: bool,
        /// The writer waited for it.
        waited: bool,
        /// Emitted facts.
        useful: bool,
    },
}

/// A bounded ring of [`Rec`].
#[derive(Clone, Debug)]
pub struct Ring {
    buf: Vec<Rec>,
    cap: usize,
    next: usize,
    total: u64,
}

impl Ring {
    /// A ring of `cap` entries (allocated once).
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
            cap: cap.max(1),
            next: 0,
            total: 0,
        }
    }

    /// Append, overwriting the oldest entry when full.
    pub fn push(&mut self, r: Rec) {
        if self.buf.len() < self.cap {
            self.buf.push(r);
        } else {
            self.buf[self.next] = r;
        }
        self.next = (self.next + 1) % self.cap;
        self.total += 1;
    }

    /// Entries recorded in all (including overwritten ones).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Rec> {
        let (a, b) = if self.buf.len() < self.cap {
            (&self.buf[..], &self.buf[..0])
        } else {
            (&self.buf[self.next..], &self.buf[..self.next])
        };
        a.iter().chain(b.iter())
    }

    /// Whether older entries were overwritten (a replay needs a complete log).
    #[must_use]
    pub fn wrapped(&self) -> bool {
        self.total > self.buf.len() as u64
    }

    /// One line per entry, numbers only.
    #[must_use]
    pub fn to_text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for r in self.iter() {
            let _ = match r {
                Rec::KeyDown {
                    ctx,
                    typing,
                    now_us,
                    threads,
                    who,
                } => writeln!(s, "K {ctx} {} {now_us} {threads} {who}", u8::from(*typing)),
                Rec::Context { ctx, triggered } => writeln!(s, "C {ctx} {triggered}"),
                Rec::Plan {
                    now_us,
                    spent_us,
                    fuse_us,
                    queues,
                    armed,
                    mask,
                    busy,
                    hash,
                } => writeln!(
                    s,
                    "P {now_us} {spent_us} {fuse_us} {queues} {armed} {mask} {busy} {hash:016x}"
                ),
                Rec::Observe {
                    pid,
                    hot,
                    start_us,
                    exec_us,
                    resp_us,
                    deadline_us,
                    hold_us,
                    exec_allow_us,
                    clipped,
                    waited,
                    useful,
                } => writeln!(
                    s,
                    "O {pid} {} {start_us} {exec_us} {resp_us} {deadline_us} {hold_us} {exec_allow_us} {} {} {}",
                    u8::from(*hot),
                    u8::from(*clipped),
                    u8::from(*waited),
                    u8::from(*useful)
                ),
            };
        }
        s
    }
}

/// The outcome of a replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replayed {
    /// Entries replayed.
    pub entries: u64,
    /// Decisions that differ from the recorded ones.
    pub mismatches: u64,
}

/// Replay a complete log into a fresh scheduler and compare every recorded decision. Returns
/// `None` when the ring wrapped (the log is not complete).
#[must_use]
pub fn replay(ring: &Ring, plugins: &[(Label, Tier)], p: Params) -> Option<Replayed> {
    if ring.wrapped() {
        return None;
    }
    let mut s = Scheduler::new(plugins, p);
    let mut bucket = SpinBucket::new(p.spin_bucket_us, p.spin_rate_us_per_s);
    let (mut entries, mut mismatches) = (0, 0);
    for r in ring.iter() {
        entries += 1;
        match *r {
            Rec::KeyDown {
                ctx,
                typing,
                now_us,
                threads,
                who,
            } => {
                let out = s.keydown_ctx(usize::from(ctx), typing, now_us, &mut bucket);
                mismatches += u64::from(out.threads != threads || out.who != who);
            }
            Rec::Context { ctx, triggered } => s.observe_context(usize::from(ctx), triggered),
            Rec::Plan {
                now_us,
                spent_us,
                fuse_us,
                queues,
                mask,
                busy,
                hash,
                ..
            } => {
                let trig: Vec<Triggered> = (0..32usize)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|pid| Triggered {
                        pid,
                        busy: busy & (1 << pid) != 0,
                    })
                    .collect();
                let mode = if queues == 255 {
                    PlaceMode::Own
                } else {
                    PlaceMode::Queues(usize::from(queues))
                };
                let plan = s.plan_with(
                    &trig,
                    TurnTimes {
                        now_us,
                        spent_us: (spent_us != u32::MAX).then_some(spent_us),
                    },
                    mode,
                    (fuse_us != u32::MAX).then_some(fuse_us),
                );
                mismatches += u64::from(crate::plan_hash(&plan) != hash);
            }
            Rec::Observe {
                pid,
                hot,
                start_us,
                exec_us,
                resp_us,
                deadline_us,
                hold_us,
                exec_allow_us,
                clipped,
                waited,
                useful,
            } => {
                let a = Assignment {
                    pid: usize::from(pid),
                    place: if hot { Place::Hot(0) } else { Place::Parked },
                    offset_us: 0,
                    deadline_us,
                    grant_fuel: 0,
                    exec_allow_us,
                    clipped,
                    hold_us,
                };
                s.observe(&Observation {
                    pid: usize::from(pid),
                    state: if hot { Start::Hot } else { Start::Cold },
                    start_us,
                    exec_us,
                    resp_us,
                    assignment: &a,
                    waited,
                    useful,
                });
            }
        }
    }
    Some(Replayed {
        entries,
        mismatches,
    })
}

/// The key class of a context index (for logs written by [`Scheduler::on_key_down`]).
#[must_use]
pub fn class_of(ctx: u8) -> Option<KeyClass> {
    Some(match ctx {
        0 => KeyClass::Nav,
        1 => KeyClass::Tab,
        2 => KeyClass::Enter,
        3 => KeyClass::EditArrow,
        4 => KeyClass::Type,
        5 => KeyClass::Other,
        _ => return None,
    })
}
