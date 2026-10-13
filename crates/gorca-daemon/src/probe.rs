//! The test-only debug accessor of the B-1 conformance suite (SPEC 5.7): read-only access to the
//! mirrors of a daemon that runs in the same process as the test, addressed the way the fake
//! applications are (unique bus name and object path), so a case can prove it exercised what it
//! claims ("the stale value was in the mirror before the mutation").
//!
//! Nothing in the product calls it; it holds no lock and sends no message, like any mirror read.

use std::sync::Arc;

use gorca_connector_atspi::{AtspiLabels, AtspiState, FetchCountersSnapshot};
use gorca_core::MirrorDirectory;
use gorca_model::{Lookup, PropMask};

/// What a mirror read of one object found.
#[derive(Clone, Debug, PartialEq)]
pub enum ProbeRead {
    /// Never cached, evicted, or the application is unknown.
    Miss,
    /// Tombstoned (or the application's mirror was released).
    Gone,
    Hit {
        name: String,
        /// Every decoded state by its AT-SPI name.
        states: Vec<&'static str>,
        value: Option<String>,
        /// Any field a focus presentation needs is stale (by event or by the app-wide epoch).
        stale: bool,
        /// Not every field a focus presentation needs is carried.
        partial: bool,
    },
}

#[derive(Clone, Debug)]
pub struct DaemonProbe {
    mirrors: Arc<MirrorDirectory>,
    state: AtspiState,
    labels: AtspiLabels,
}

impl DaemonProbe {
    pub(crate) fn new(mirrors: Arc<MirrorDirectory>, state: AtspiState) -> Self {
        let labels = state.labels();
        Self {
            mirrors,
            state,
            labels,
        }
    }

    /// The event drain is up: applications started now are heard from their first event.
    pub fn ready(&self) -> bool {
        self.state.drain_ready()
    }

    /// Read `path` of the application with unique name `bus`.
    pub fn read(&self, bus: &str, path: &str) -> ProbeRead {
        let Some(key) = self.labels.key_of(bus, path) else {
            return ProbeRead::Miss;
        };
        match self.mirrors.view(key) {
            Lookup::Miss => ProbeRead::Miss,
            Lookup::Gone => ProbeRead::Gone,
            Lookup::Hit(v) => {
                let need = gorca_core::focus_need(&v.snap);
                ProbeRead::Hit {
                    name: v.snap.name.as_deref().unwrap_or("").to_owned(),
                    states: gorca_core::snapshot_state_names(&v.snap).to_vec(),
                    value: gorca_core::value_text(v.snap.value.as_ref()),
                    stale: v.stale.intersects(PropMask::PRESENT),
                    partial: !v.snap.have.contains(need),
                }
            }
        }
    }

    /// Live nodes in the mirror of the application `bus`; `None` once it is released.
    pub fn nodes(&self, bus: &str) -> Option<usize> {
        let key = self.labels.key_of(bus, "/")?;
        let st = self.mirrors.stats(key.app)?;
        usize::try_from(st.live).ok()
    }

    /// What the connector refused to believe so far (foreign refs, rejected dumps).
    pub fn dropped(&self) -> FetchCountersSnapshot {
        self.state.fetch_counters().snapshot()
    }
}
