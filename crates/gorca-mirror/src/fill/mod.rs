//! The fill policy (B-1 SPEC 4.4): when to fetch what so that the next focus, state change or
//! "where am I" is answered from memory.
//!
//! A policy is a pure, synchronous state machine. It never performs I/O, never blocks and never
//! awaits. The shared [`crate::driver::FillDriver`] feeds it inputs and executes its effects
//! within the bounds of `limits.rs`. Everything that decides correctness (invalidation, late
//! reply rules, tombstones, bounds, the store) is shared code; a policy only chooses what to ask.
//!
//! This module must not name a connector, a transport or a toolkit: a Newton connector uses the
//! same driver with a push-only policy.

use std::ops::Range;
use std::time::Instant;

use gorca_ingress::DirtySet;
use gorca_model::{AppId, AppInfo, NodeEvent, NodeKey, PropMask};
use gorca_rt::BudgetClass;
use smallvec::SmallVec;

use crate::limits::FillLimits;
use crate::store::{ApplyOutcome, MirrorRead};

mod null;

pub use null::NullPolicy;

/// Coarse toolkit family, for budget tuning only.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum GenericToolkit {
    Gtk,
    Qt,
    Other,
}

/// What kind of application this is. Tunes budgets, never per-app behaviour.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AppClass {
    Shell,
    Toolkit(GenericToolkit),
    Browser,
    Terminal,
    Unknown,
}

impl AppClass {
    /// The class of an announced app. `Shell` needs the connector's verified flag; the names the
    /// application reports about itself only pick among the unprivileged classes.
    pub fn from_info(info: &AppInfo) -> Self {
        if info.verified_shell {
            return AppClass::Shell;
        }
        let toolkit = info.toolkit.to_ascii_lowercase();
        if toolkit.contains("gecko") || toolkit.contains("chrom") || toolkit.contains("blink") {
            AppClass::Browser
        } else if toolkit.contains("vte") {
            AppClass::Terminal
        } else if toolkit.contains("gtk") {
            AppClass::Toolkit(GenericToolkit::Gtk)
        } else if toolkit.contains("qt") {
            AppClass::Toolkit(GenericToolkit::Qt)
        } else if toolkit.is_empty() || toolkit == "unknown" {
            AppClass::Unknown
        } else {
            AppClass::Toolkit(GenericToolkit::Other)
        }
    }
}

/// `AppHealth` as the policy sees it (I-4: stop Prefetch/Background in `Slow` and `Hung`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum HealthView {
    Healthy,
    Slow,
    Hung,
    Flooded,
    Gone,
}

/// Free in-flight slots per class and the fill-rate tokens left.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SlotView {
    pub interactive: u8,
    pub prefetch: u8,
    pub background: u8,
    pub tokens: u32,
}

#[derive(Debug)]
pub struct FillCtx<'a> {
    pub now: Instant,
    pub app: AppId,
    pub app_class: AppClass,
    pub health: HealthView,
    /// Shared eligibility (B-1 2.2): the connector verified that `CacheDump` returns the whole
    /// tree of this app (today only a verified gnome-shell). The driver refuses `CacheDump`
    /// otherwise.
    pub dump_complete: bool,
    pub mirror: &'a dyn MirrorRead,
    /// The Reader's settled focus if it is in this app.
    pub focus: Option<NodeKey>,
    pub slots: SlotView,
    pub limits: &'a FillLimits,
}

/// Identifies one fill request; assigned by the driver.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FillReqId(pub u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DemandWhy {
    Focus,
    StateChange,
    WhereAmI,
    NavStep,
    Revalidate,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DemandOutcome {
    Hit,
    Partial { missing: PropMask },
    Stale { mask: PropMask },
    Miss,
    Gone,
}

#[derive(Debug)]
pub enum FillInput<'a> {
    Appeared {
        root: NodeKey,
    },
    /// `AppLifecycle::Ready`.
    Ready,
    /// A drained ingress batch, after the shared invalidation of 5.1 was applied.
    Events(&'a [NodeEvent]),
    /// Flood resync (already invalidated).
    Resync {
        dirty: &'a DirtySet,
    },
    /// The Reader read the mirror for a reason; `outcome` says what it found.
    Demand {
        node: NodeKey,
        why: DemandWhy,
        outcome: DemandOutcome,
    },
    /// The focus state machine reached `Presented`.
    FocusSettled {
        node: NodeKey,
    },
    WindowActivated {
        window: NodeKey,
    },
    /// A fill request finished; the driver has already applied it to the mirror.
    Reply {
        id: FillReqId,
        op: &'a FillOp,
        result: FillResult<'a>,
    },
    /// At most every 50 ms while the app has work, else on `WakeAt`.
    Tick,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum FillOp {
    Fetch {
        node: NodeKey,
        want: PropMask,
    },
    ChildSnapshots {
        node: NodeKey,
        range: Range<u32>,
        want: PropMask,
    },
    Ancestry {
        node: NodeKey,
        max_depth: u8,
        want: PropMask,
    },
    Subtree {
        root: NodeKey,
        max_nodes: u32,
    },
    CacheDump {
        max_nodes: u32,
    },
}

impl FillOp {
    /// The node whose disappearance a `Gone` reply proves.
    pub fn target(&self) -> Option<NodeKey> {
        match self {
            FillOp::Fetch { node, .. }
            | FillOp::ChildSnapshots { node, .. }
            | FillOp::Ancestry { node, .. } => Some(*node),
            FillOp::Subtree { root, .. } => Some(*root),
            FillOp::CacheDump { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FillEffect {
    /// The driver assigns the id and the deadline and applies the reply.
    Request {
        op: FillOp,
        class: BudgetClass,
    },
    Cancel(FillReqId),
    Evict(NodeKey),
    WakeAt(Instant),
}

pub type FillEffects = SmallVec<[FillEffect; 8]>;

/// Why the driver did not execute a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RefuseReason {
    /// `Hung`/`Gone` app, or Prefetch/Background in `Slow`.
    Health,
    /// No in-flight slot for this class.
    NoSlot,
    /// The fill-rate token bucket is empty.
    RateLimit,
    /// The pending queue is full (newest refused).
    QueueFull,
    /// The mirror is at its node or byte cap.
    NodeCap,
    /// `CacheDump` more than once per interval.
    DumpTooSoon,
    /// `CacheDump` for an app whose dump is not verified complete.
    DumpIneligible,
    /// Waited in the queue past its usefulness.
    Expired,
}

impl RefuseReason {
    pub const COUNT: usize = 8;
    pub fn index(self) -> usize {
        match self {
            RefuseReason::Health => 0,
            RefuseReason::NoSlot => 1,
            RefuseReason::RateLimit => 2,
            RefuseReason::QueueFull => 3,
            RefuseReason::NodeCap => 4,
            RefuseReason::DumpTooSoon => 5,
            RefuseReason::Expired => 6,
            RefuseReason::DumpIneligible => 7,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            RefuseReason::Health => "health",
            RefuseReason::NoSlot => "no_slot",
            RefuseReason::RateLimit => "rate_limit",
            RefuseReason::QueueFull => "queue_full",
            RefuseReason::NodeCap => "node_cap",
            RefuseReason::DumpTooSoon => "dump_too_soon",
            RefuseReason::Expired => "expired",
            RefuseReason::DumpIneligible => "dump_ineligible",
        }
    }
}

#[derive(Debug)]
pub enum FillResult<'a> {
    /// The reply was applied to the mirror. `keys` are the nodes of the reply when there are at
    /// most `limits.max_subtree_nodes` of them (empty for a dump).
    Applied {
        keys: &'a [NodeKey],
        nodes: usize,
        outcome: ApplyOutcome,
    },
    /// The object is gone (`UnknownObject`); it was tombstoned.
    Gone,
    /// The connector has no path for the key (evicted from its path table): a miss, not `Gone`.
    Unresolved,
    Timeout,
    NotSupported,
    Failed,
    Refused(RefuseReason),
    /// Cancelled by the policy before it was sent.
    Cancelled,
}

pub trait FillPolicy: Send + 'static {
    fn name(&self) -> &'static str;
    /// One step. Must be O(input + effects) and bounded by MA-6 (at most 250 us at the caps).
    fn step(&mut self, cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects);
    /// Exact bytes of policy-owned memory (MA-1). Bounded by `FILL_STATE_MAX_BYTES`.
    fn memory_bytes(&self) -> usize;
}
