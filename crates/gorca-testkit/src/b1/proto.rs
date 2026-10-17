// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire-level vocabulary shared by the B-1 fake application (`tests/harness/b1/fakeapp`), the
//! in-process model of it ([`super::world`]) and the drivers: the model schema (SPEC 8.1), the
//! control commands, the truth log (SPEC 6.1) and the call counters.
//!
//! Everything here is plain data (serde). No transport crate is involved, so the conformance
//! suite can run without a bus.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How `Application.GetApplicationBusAddress` answers (SPEC G-4, A-3, D-7).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum P2pMode {
    /// A real peer socket (atk-bridge, A-1).
    #[default]
    Peer,
    /// The address of the a11y bus itself (GTK4, G-4): "P2P" is a second bus connection.
    SameBus,
    /// Empty string (libatspi initialised in the same process, A-3).
    None,
}

/// How object paths are minted.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStyle {
    /// `/org/a11y/atspi/accessible/<n>`, monotonic counter (atk-bridge, A-1).
    #[default]
    Counter,
    /// `/org/a11y/atspi/accessible/<32 hex digits>` (GTK4, G-5).
    Uuid,
}

/// Toolkit quirk flags (SPEC section 2.1). The pessimistic profile has every flag on.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quirks {
    /// G-1 / S-1: names derived from a child label change without an event on the labelled node.
    pub silent_derived_names: bool,
    /// G-2: hidden contexts get neither `AddAccessible` nor `RemoveAccessible`, and are not in
    /// `GetItems`.
    pub hidden_skips_cache: bool,
    /// G-3: a node added "during a dump" is neither in the reply nor signalled.
    pub add_during_dump_invisible: bool,
    /// G-6: list rows are recycled while scrolling (same path, other item).
    pub recycle_rows: bool,
    /// H4: whether a recycled row announces its new name.
    pub recycle_evented: bool,
    /// A-2: `GetItems` omits `TRANSIENT` objects and children of `MANAGES_DESCENDANTS` nodes.
    pub dump_omits_transient: bool,
    /// S-2: the switcher is rebuilt (new paths) on every `rebuild_switcher`.
    pub switcher_rebuild: bool,
    /// P-9: `GetItems` returns only realised contexts (root, window, and nodes a client has
    /// read); a fresh app answers 2 items.
    pub dump_realised_only: bool,
    /// P-9 H1: widget removal never sends `RemoveAccessible`.
    pub never_remove_accessible: bool,
    /// P9-2: a child property/state update also sends `ChildrenChanged:add` on the parent.
    pub spurious_parent_add: bool,
    /// P-9: a hidden context stays in the tree with `SHOWING` set (a ghost): hiding is silent.
    pub hidden_ghost: bool,
    /// P9-1: no `Object` event is emitted before an AT listener registered.
    pub listener_gate: bool,
}

impl Quirks {
    /// The pessimistic GTK4 profile until P-9 answers H1-H4 (SPEC 2.2).
    pub fn pessimistic_gtk4() -> Self {
        Self {
            silent_derived_names: true,
            hidden_skips_cache: true,
            add_during_dump_invisible: true,
            recycle_rows: true,
            recycle_evented: false,
            dump_omits_transient: false,
            switcher_rebuild: false,
            dump_realised_only: false,
            never_remove_accessible: true,
            spurious_parent_add: true,
            hidden_ghost: false,
            listener_gate: false,
        }
    }
}

/// Service time of one request, log-normal fitted to a median and a 99th percentile.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub p50_ms: f64,
    pub p99_ms: f64,
}

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            p50_ms: 0.15,
            p99_ms: 1.0,
        }
    }
}

/// Which shape the tree generator builds (landmarks differ, see [`super::world`]).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    #[default]
    Generic,
    Shell,
    Settings,
    Files,
    /// GTK3 (gedit): a `MANAGES_DESCENDANTS` list that `GetItems` does not descend into.
    Gtk3,
    /// Firefox chrome with one `DocumentWeb` child (IC-20).
    FfChrome,
    /// One terminal node and nothing else: the `storm` model.
    Storm,
}

/// One app model (`tests/harness/b1/models/*.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSpec {
    pub name: String,
    pub app_name: String,
    pub toolkit: String,
    pub shape: Shape,
    pub p2p: P2pMode,
    pub path_style: PathStyle,
    pub seed: u64,
    /// Node count at rest and at the busiest moment (overview open, biggest panel).
    pub nodes_idle: u32,
    pub nodes_peak: u32,
    /// Depth range of the deepest chain.
    pub depth_min: u32,
    pub depth_max: u32,
    pub service: ServiceSpec,
    /// Extra service time per item of `Cache.GetItems`, microseconds.
    pub getitems_us_per_item: f64,
    pub quirks: Quirks,
    /// Sidebar rows (settings, files).
    pub sidebar_rows: u32,
    /// Realised rows of the list (files) and the logical item count behind them.
    pub list_rows: u32,
    pub list_items: u32,
    /// Nodes per list row (row, icon, label, size, date, check).
    pub row_nodes: u32,
    /// Own this well-known name on the harness session bus (the verified gnome-shell of the
    /// eligibility check, SPEC 2.2). The driver passes `--own-name`.
    pub owns_name: Option<String>,
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self {
            name: "generic".to_owned(),
            app_name: "generic".to_owned(),
            toolkit: "gtk4".to_owned(),
            shape: Shape::Generic,
            p2p: P2pMode::SameBus,
            path_style: PathStyle::Uuid,
            seed: 1,
            nodes_idle: 200,
            nodes_peak: 400,
            depth_min: 8,
            depth_max: 12,
            service: ServiceSpec::default(),
            getitems_us_per_item: 15.0,
            quirks: Quirks::pessimistic_gtk4(),
            sidebar_rows: 0,
            list_rows: 0,
            list_items: 0,
            row_nodes: 6,
            owns_name: None,
        }
    }
}

/// How hard an app is to reach.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HangMode {
    /// Normal operation.
    Off,
    /// Requests are accepted and never answered.
    Never,
    /// Every reply is delayed by `ms`.
    Delay,
}

/// Which connection a call arrived on.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnKind {
    Bus,
    SameBus,
    Peer,
}

/// How a reorder is announced (IC-8, H3).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReorderMode {
    /// `children-changed:remove` + `:add` of the same paths, plus `RemoveAccessible` + `AddAccessible`.
    RemoveAdd,
    /// Only `children-changed` on the parent.
    ChildrenOnly,
    /// No event at all.
    Silent,
}

/// What a destroy announces.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestroyEvents {
    /// `state-changed:defunct`, `children-changed:remove`, `RemoveAccessible` (subject to G-2).
    Full,
    /// Only `state-changed:defunct` on the node.
    DefunctOnly,
    /// Nothing: the path simply starts answering `UnknownObject`.
    Silent,
}

/// A control command (one JSON object per line on the fake app's stdin).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// Move focus: `state-changed:focused` off/on plus `Focus:Focus`.
    Focus {
        node: String,
    },
    Rename {
        node: String,
        name: String,
        evented: bool,
    },
    /// `state` is an AT-SPI state name (`checked`, `expanded`, `selected`, `sensitive`, ...).
    SetState {
        node: String,
        state: String,
        on: bool,
        evented: bool,
    },
    SetValue {
        node: String,
        value: f64,
        evented: bool,
    },
    /// Replace everything below `root` with `nodes` fresh nodes (settings panel switch).
    ReplaceSubtree {
        root: String,
        nodes: u32,
        remove_events: bool,
        add_events: bool,
    },
    /// Scroll the realised rows of the files list by `steps` items.
    Scroll {
        steps: i32,
    },
    Hide {
        node: String,
    },
    Destroy {
        node: String,
        events: DestroyEvents,
    },
    Reorder {
        parent: String,
        mode: ReorderMode,
    },
    /// Add a leaf below `parent`. `during_dump` is G-3.
    Add {
        parent: String,
        name: String,
        during_dump: bool,
        silent: bool,
    },
    /// A-2: the path starts answering `UnknownObject` (lease expiry) without any event.
    ExpireLease {
        node: String,
    },
    /// S-2: rebuild the switcher with `items` new entries (new paths).
    RebuildSwitcher {
        items: u32,
    },
    /// `state-changed:selected` 0 on the old item, 1 on the new one, no focus event.
    Select {
        node: String,
    },
    /// `Window:Destroy` on a window and everything below it dies.
    WindowDestroy {
        node: String,
    },
    WindowActivate {
        node: String,
    },
    Hang {
        mode: HangMode,
        ms: u64,
    },
    /// Delay the reply to the next `count` calls of `member` (`Accessible.GetState`, ...; empty
    /// matches any) by `ms`. The reply content is computed at service time (pre-mutation).
    DelayReplies {
        member: String,
        ms: u64,
        count: u32,
        conn: Option<ConnKind>,
    },
    /// Flood `TextChanged` events at `rate_per_s` for `duration_ms`: on the terminal node, or
    /// when `distinct_nodes` is set, spread over that many distinct objects (a reader that
    /// folds events per node needs more distinct nodes than it holds to see a flood).
    Storm {
        rate_per_s: u64,
        duration_ms: u64,
        #[serde(default)]
        distinct_nodes: u32,
        /// Real objects (paths) that are flooded too, after each round of the distinct ones: a
        /// node that is flooded once the reader's table is full is the one that must come out
        /// dirty.
        #[serde(default)]
        targets: Vec<String>,
    },
    /// Drop every peer connection (EOF mid-request, IC-19).
    ClosePeers,
    /// An AT listener registers (lifts the P9-1 gate when the model has it).
    RegisterListener,
    /// Delay accepting new peer connections by `ms` (connect hang, IC-19).
    PeerAcceptDelay {
        ms: u64,
    },
    /// Make refs in `GetChildren` / `GetItems` hostile: a foreign bus name (`foreign_bus`), a
    /// self-parent and a parent cycle (IC-16, MA-3).
    Malform {
        on: bool,
        foreign_bus: String,
    },
    /// Current truth of one node.
    Get {
        node: String,
    },
    /// Names of the landmarks and the paths behind them.
    Landmarks,
    /// Call counters.
    Stats,
    /// Exit at once (no goodbye, as a crash).
    Kill,
    /// Orderly exit.
    Quit,
}

/// One answer to a [`Cmd`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CmdReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Command specific: `path`, `old`, `new`, landmark map, stats, ...
    #[serde(default)]
    pub data: serde_json::Value,
}

impl CmdReply {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            error: None,
            data,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            data: serde_json::Value::Null,
        }
    }

    /// String field of `data`.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(serde_json::Value::as_str)
    }
}

/// One line of the truth log (SPEC 6.1): a mutation, or a focus change when `focus` is set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TruthLine {
    /// CLOCK_MONOTONIC nanoseconds.
    pub t_ns: u64,
    pub app: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub field: String,
    /// Previous and new value. `name` strings, `value` numbers, `states` arrays of state names.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub old: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub new: serde_json::Value,
    #[serde(default)]
    pub evented: bool,
    /// The node is a secret field (a password entry): a harness that compares this log with a
    /// presentation log redacts `old` and `new` with the same key and function
    /// (`gorca_model::RedactKey::tag`), because the daemon never logs such text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sensitive: bool,
    /// `true` on a focus line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focus: bool,
}

/// Call counters of a fake app (SPEC 6.1 "Messages").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppStats {
    /// `"<conn>/<Interface.Member>"` to count.
    pub calls: BTreeMap<String, u64>,
    pub signals_sent: u64,
    /// Calls whose target path lies strictly below a `Document*` node (IC-20).
    pub calls_below_document: u64,
    /// Calls to an unknown or dead path.
    pub calls_unknown_object: u64,
    pub nodes_live: u64,
    /// Distinct callers seen on the bus connection (IC-19: no duplicate connection in `SameBus`).
    pub bus_callers: u64,
    /// `CLOCK_MONOTONIC` ns when the counters were read (lets a driver align clocks).
    pub now_ns: u64,
}

impl AppStats {
    /// Total method calls over all connections.
    pub fn total_calls(&self) -> u64 {
        self.calls.values().sum()
    }

    /// Calls of one member over all connections (`GetItems`, `GetState`, ...).
    pub fn member_calls(&self, member: &str) -> u64 {
        self.calls
            .iter()
            .filter(|(k, _)| k.rsplit('.').next() == Some(member))
            .map(|(_, v)| v)
            .sum()
    }

    /// Calls made over one connection kind.
    pub fn conn_calls(&self, conn: ConnKind) -> u64 {
        let prefix = match conn {
            ConnKind::Bus => "bus/",
            ConnKind::SameBus => "samebus/",
            ConnKind::Peer => "peer/",
        };
        self.calls
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, v)| v)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_json_roundtrip() {
        let c = Cmd::Rename {
            node: "controls[3]".to_owned(),
            name: "x".to_owned(),
            evented: false,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"cmd\":\"rename\""), "{s}");
        assert_eq!(serde_json::from_str::<Cmd>(&s).unwrap(), c);
    }

    #[test]
    fn model_defaults_fill_missing_fields() {
        let m: ModelSpec = serde_json::from_str(r#"{"name":"x","nodes_idle":5}"#).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.nodes_idle, 5);
        assert!(m.quirks.hidden_skips_cache);
    }

    #[test]
    fn stats_helpers() {
        let mut s = AppStats::default();
        s.calls.insert("bus/Cache.GetItems".to_owned(), 2);
        s.calls.insert("peer/Accessible.GetState".to_owned(), 5);
        assert_eq!(s.member_calls("GetItems"), 2);
        assert_eq!(s.conn_calls(ConnKind::Peer), 5);
        assert_eq!(s.total_calls(), 7);
    }
}
