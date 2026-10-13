//! The system under test, as the conformance suite sees it (SPEC 5.7).
//!
//! A [`Sut`] is "a reader (R0 or an arm) attached to fake applications". The cases drive the
//! fake apps only through [`Cmd`]s (the same JSON vocabulary the `b1-fakeapp` binary speaks),
//! let time pass, and observe presentations, the truth log and, for the vacuity checks, the
//! mirror through a test-only probe.
//!
//! Two implementations exist:
//!
//! * [`super::sim::SimSut`]: an in-process, virtual-time reference reader that implements
//!   SPEC section 5 literally. It proves that each case passes for a correct reader and fails for
//!   each broken one (harness self-test H-10(b)); it is also the executable form of the contract.
//! * the daemon-backed one (`gorca-bench-b1`, wired when PW-4 and PW-10 land): `b1-fakeapp`
//!   processes in an isolated session, `gorca-daemon` as the reader, the presentation log
//!   (PW-10) for [`Presentation`]s and a test-only debug accessor for [`Probe`] reads.

use super::proto::{AppStats, Cmd, CmdReply, TruthLine};

/// Where a presentation came from (SPEC 6.1 `source`, plus S1).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Source {
    /// Mirror hit, complete and not stale.
    Hit,
    /// Mirror node with missing fields (not NAME/ROLE).
    Partial,
    /// Mirror node with stale fields (not NAME/ROLE).
    Stale,
    /// Spoken from the reply of an Interactive fetch (cold path, proof of life).
    MissReply,
    /// RV-1 replaced an earlier utterance (audio < 150 ms).
    Refine,
    /// RV-1 spoke a short follow-up with the changed field.
    Followup,
    /// State/value change of the focused node (5.6).
    S1,
}

/// One utterance the reader produced, as logged by PW-10.
#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    /// `CLOCK_MONOTONIC` ns when the utterance was handed to speech.
    pub t_ns: u64,
    /// `CLOCK_MONOTONIC` ns of the stimulus (the focus or state event at the app).
    pub stimulus_ns: u64,
    pub utt: u64,
    pub app: String,
    /// Unique bus name of the app instance the node belongs to.
    pub app_bus: String,
    pub path: String,
    pub source: Source,
    pub role: u32,
    pub states: u64,
    pub name: String,
    pub value: Option<String>,
    pub may_refine: bool,
    /// For `Refine` / `Followup`: the utterance corrected.
    pub refines: Option<u64>,
}

/// A test-only read of the reader's mirror (the `MirrorRead` probe of SPEC 5.7).
#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    /// Never cached (or evicted, or not resolvable).
    Miss,
    /// Tombstoned.
    Gone,
    Hit {
        name: String,
        states: u64,
        value: Option<String>,
        /// Any field of PRESENT stale (event-invalidated or by the app-wide epoch).
        stale: bool,
        /// Not all of PRESENT is carried (GetItems node).
        partial: bool,
    },
}

/// Optional abilities of a SUT; cases that need one skip (not pass) when it is missing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Feature {
    /// Per-direction latency knobs ([`Sut::set_latency`]): needed for the ordering races.
    LatencyKnobs,
    /// The SUT exposes whether a node is in its mirror ([`Sut::probe`]).
    Probe,
    /// Virtual time: `advance` is instantaneous and exact.
    VirtualTime,
    /// The reader subscribes to cache signals / dumps (arm B style): IC-12 and IC-13 bite.
    CacheFill,
    /// [`Sut::where_am_i`] works.
    WhereAmI,
    /// The fake app can plant hostile refs ([`Cmd::Malform`]) and the SUT counts what it drops.
    Malformed,
    /// A flood marks the app's nodes dirty. A SUT whose ingress folds a storm on one node into
    /// one slot (the real daemon) has no flood to observe, and the case then checks the rest.
    FloodDirtySet,
}

#[derive(Debug, thiserror::Error)]
pub enum SutError {
    #[error("{0}")]
    Other(String),
}

pub type SutResult<T> = Result<T, SutError>;

pub trait Sut {
    /// Label of the reader under test (`r0`, `sim:correct`, arm name).
    fn arm(&self) -> String;
    fn supports(&self, f: Feature) -> bool;
    /// Start a fake app from a shipped model (`settings`, `files`, ...) under `label`.
    fn start_app(&mut self, model: &str, label: &str) -> SutResult<()>;
    /// Start with an edited model (quirk profile, p2p mode, sizes).
    fn start_app_with(
        &mut self,
        model: &str,
        label: &str,
        edit: &dyn Fn(&mut super::proto::ModelSpec),
    ) -> SutResult<()>;
    /// Kill the app abruptly and start a new instance (same paths, new bus name).
    fn restart_app(&mut self, label: &str) -> SutResult<()>;
    /// Run a control command on the app.
    fn cmd(&mut self, app: &str, c: Cmd) -> SutResult<CmdReply>;
    /// Let `ms` of (virtual or real) time pass, delivering everything due.
    fn advance(&mut self, ms: u64);
    /// Current time, `CLOCK_MONOTONIC` ns (virtual for the simulation).
    fn now_ns(&self) -> u64;
    /// Everything presented so far, in order.
    fn presentations(&self) -> Vec<Presentation>;
    /// Truth lines of all apps, all instances, in time order.
    fn truth(&self) -> Vec<TruthLine>;
    /// Read the reader's mirror of `path` in the **current** instance of `app`.
    fn probe(&self, app: &str, path: &str) -> Probe;
    /// Mirror node count of the instance with this unique bus name; `None` once released.
    fn mirror_nodes(&self, bus_name: &str) -> Option<usize>;
    /// Unique bus name of the current instance of `app`.
    fn bus_name(&self, app: &str) -> String;
    /// Call counters of the current instance of `app`.
    fn calls(&mut self, app: &str) -> SutResult<AppStats>;
    /// The where-am-I command: speak the focused node and its ancestors (5.3: every spoken
    /// node served from the mirror is revalidated). `WhereAmI` only.
    ///
    /// Returns the paths it will speak (the focused node and its known ancestors), so the walk
    /// can log them as stimuli: the run-directory classifier derives latency from the last
    /// stimulus on a path.
    fn where_am_i(&mut self) -> SutResult<Vec<String>> {
        Err(SutError::Other(
            "where-am-i is not supported by this SUT".into(),
        ))
    }
    /// Set event (bus signal) and reply (call) travel times, microseconds. `LatencyKnobs` only.
    fn set_latency(&mut self, event_us: u64, reply_us: u64);
}

/// Convenience over [`Sut`] used by every case.
pub trait SutExt: Sut {
    /// Run a command that must succeed; return its reply.
    fn must(&mut self, app: &str, c: Cmd) -> SutResult<CmdReply> {
        let r = self.cmd(app, c)?;
        if r.ok {
            Ok(r)
        } else {
            Err(SutError::Other(format!(
                "command failed on {app}: {}",
                r.error.unwrap_or_default()
            )))
        }
    }

    /// Path of a node reference, via `Get`.
    fn path_of(&mut self, app: &str, node: &str) -> SutResult<String> {
        let r = self.must(
            app,
            Cmd::Get {
                node: node.to_owned(),
            },
        )?;
        r.str("path")
            .map(str::to_owned)
            .ok_or_else(|| SutError::Other(format!("no path for {node}")))
    }

    /// Focus a node and let the reader settle (200 ms is far above any service time).
    fn focus(&mut self, app: &str, node: &str) -> SutResult<String> {
        let path = self.path_of(app, node)?;
        self.must(app, Cmd::Focus { node: path.clone() })?;
        self.advance(200);
        Ok(path)
    }

    /// Presentations of one node, in order.
    fn presented(&self, path: &str) -> Vec<Presentation> {
        self.presentations()
            .into_iter()
            .filter(|p| p.path == path)
            .collect()
    }
}

impl<T: Sut + ?Sized> SutExt for T {}
