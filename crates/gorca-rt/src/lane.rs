// SPDX-License-Identifier: GPL-3.0-or-later
//! Hot lanes: dedicated OS threads (ADR-0001 section 3.2, PC-4).

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, PoisonError, RwLock};
use std::thread::JoinHandle;

/// The dedicated threads of section 3.2. `workers` and `cpu` are pools, not lanes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Lane {
    Input,
    /// One per event connection (1 to 4, B-2).
    Drain(u8),
    Reader,
    SpeechIo,
    Watchdog,
}

type StartHook = Arc<dyn Fn(Lane) + Send + Sync>;

static START_HOOK: RwLock<Option<StartHook>> = RwLock::new(None);

/// Install (or clear) the process-wide hook that runs on every hot-lane thread as its first act,
/// before the lane body: the runtime topology (bake-off B-5) places the thread there (affinity,
/// scheduling class). Threads that are not spawned through [`spawn_lane`] call [`lane_started`]
/// themselves.
pub fn set_start_hook(hook: Option<StartHook>) {
    *START_HOOK.write().unwrap_or_else(PoisonError::into_inner) = hook;
}

/// Run the start hook for the calling thread, which is `lane`'s thread.
pub fn lane_started(lane: Lane) {
    let hook = START_HOOK
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(h) = hook {
        h(lane);
    }
}

/// Scheduling priority of a lane.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LanePriority {
    Normal,
    /// Low `SCHED_FIFO` through rtkit (`org.freedesktop.RealtimeKit1`).
    /// Applied by the daemon's runtime topology (bake-off B-5 arm c) through the start hook;
    /// without rtkit it is a no-op.
    FifoLow,
}

impl Lane {
    /// Thread name: `gorca-<lane>` (ADR-0001 ID-2).
    pub fn thread_name(self) -> String {
        match self {
            Lane::Input => "gorca-input".to_owned(),
            Lane::Drain(n) => format!("gorca-drain-{n}"),
            Lane::Reader => "gorca-reader".to_owned(),
            Lane::SpeechIo => "gorca-speech-io".to_owned(),
            Lane::Watchdog => "gorca-watchdog".to_owned(),
        }
    }

    pub fn priority(self) -> LanePriority {
        match self {
            Lane::Input | Lane::Drain(_) | Lane::Reader | Lane::SpeechIo => LanePriority::FifoLow,
            Lane::Watchdog => LanePriority::Normal,
        }
    }
}

/// How a lane thread ended. A panic is caught at the lane boundary (section 6.5).
#[derive(Debug, Eq, PartialEq)]
pub enum LaneExit {
    Clean,
    Panicked(String),
}

#[derive(Debug)]
pub struct LaneHandle {
    lane: Lane,
    join: JoinHandle<LaneExit>,
}

impl LaneHandle {
    pub fn lane(&self) -> Lane {
        self.lane
    }

    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }

    /// Wait for the lane to end. A failure to join (should not happen: panics are caught) is
    /// reported as a panic.
    pub fn join(self) -> LaneExit {
        self.join
            .join()
            .unwrap_or_else(|p| LaneExit::Panicked(panic_message(p.as_ref())))
    }
}

/// Spawn a named lane thread. The closure must observe a shutdown signal of its own.
pub fn spawn_lane<F>(lane: Lane, body: F) -> io::Result<LaneHandle>
where
    F: FnOnce() + Send + 'static,
{
    let join = std::thread::Builder::new()
        .name(lane.thread_name())
        .spawn(move || {
            match catch_unwind(AssertUnwindSafe(|| {
                lane_started(lane);
                body();
            })) {
                Ok(()) => LaneExit::Clean,
                Err(p) => LaneExit::Panicked(panic_message(p.as_ref())),
            }
        })?;
    Ok(LaneHandle { lane, join })
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_names() {
        assert_eq!(Lane::Input.thread_name(), "gorca-input");
        assert_eq!(Lane::Drain(2).thread_name(), "gorca-drain-2");
        assert_eq!(Lane::Reader.priority(), LanePriority::FifoLow);
        assert_eq!(Lane::Watchdog.priority(), LanePriority::Normal);
    }

    #[test]
    fn lane_runs_under_its_name() {
        let (tx, rx) = std::sync::mpsc::channel();
        let h = spawn_lane(Lane::Reader, move || {
            let name = std::thread::current().name().map(str::to_owned);
            let _ = tx.send(name);
        })
        .unwrap();
        assert_eq!(h.join(), LaneExit::Clean);
        assert_eq!(rx.recv().unwrap().as_deref(), Some("gorca-reader"));
    }

    #[test]
    fn panic_is_contained_at_the_lane_boundary() {
        let h = spawn_lane(Lane::Drain(0), || panic!("boom")).unwrap();
        assert_eq!(h.join(), LaneExit::Panicked("boom".to_owned()));
    }
}
