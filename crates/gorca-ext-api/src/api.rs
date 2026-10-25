// SPDX-License-Identifier: MIT OR Apache-2.0
//! Hand-written mirror of `wit/gorca-ext.wit`.
//!
//! These are the types an extension is written against. The same source builds two ways
//! (ADR-0008 section 3.3): natively (linked into a host, hooks called through [`Hooks`]) and as a
//! `wasm32-wasip2` component (the [`export_extension!`](crate::export_extension) macro maps the
//! WIT exports onto [`Hooks`]).

pub type NodeKey = (u64, u64);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Budget {
    Interactive,
    Prefetch,
    Background,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum QueryError {
    Timeout,
    Gone,
    NotSupported,
    Denied,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Prio {
    Interrupt,
    Message,
    Text,
    Progress,
    Notification,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SegmentKind {
    Text,
    Role,
    State,
    Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    pub text: String,
    pub kind: SegmentKind,
}

impl Segment {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            kind: SegmentKind::Text,
        }
    }
}

/// A semantic role: a code of the append-only role registry ([`crate::codes::ROLES`], the
/// `gorca-model` `SemRole` codes). An unknown code means "unknown", never a trap.
pub type RoleCode = u16;

/// A fused group: a code of the group registry ([`crate::codes::GROUPS`]).
pub type GroupCode = u16;

/// Role codes an extension can name without looking the registry up (kept equal to
/// [`crate::codes::ROLES`] by a test).
pub mod role {
    use super::RoleCode;
    pub const UNKNOWN: RoleCode = 0;
    pub const ALERT: RoleCode = 2;
    pub const ALERT_DIALOG: RoleCode = 3;
    pub const DIALOG: RoleCode = 19;
    pub const METER: RoleCode = 45;
    pub const PROGRESS_BAR: RoleCode = 50;
    pub const SLIDER: RoleCode = 61;
    pub const WINDOW: RoleCode = 79;
}

/// The value of one fused group in the shape its registry row names (WIT `group-value`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupValue {
    /// Enumerated groups: role, check, press, expand, select, current, invalid, popup, ...
    Code(u16),
    /// Bit-set groups: flags, avail.
    Bits(u64),
    /// Integer groups: level.
    Number(u32),
    /// Text groups: name, description.
    Text(String),
}

/// A numeric value as the application reports it (WIT `range`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Range {
    pub current: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub key: NodeKey,
    pub role: RoleCode,
    /// The `flags` group bits of the fused entry.
    pub flags: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub range: Option<Range>,
}

/// Bits of the property mask of `Event::PropertiesChanged` and of `Host::fetch`.
pub mod prop {
    pub const NAME: u32 = 1 << 0;
    pub const DESCRIPTION: u32 = 1 << 1;
    pub const VALUE: u32 = 1 << 2;
    pub const ROLE: u32 = 1 << 3;
    pub const STATES: u32 = 1 << 4;
    pub const BOUNDS: u32 = 1 << 5;
    pub const ATTRS: u32 = 1 << 6;
    pub const RELATIONS: u32 = 1 << 7;
    pub const CHILDREN: u32 = 1 << 8;
    /// Hint on `PropertiesChanged` only: the node just became selected.
    pub const SELECTED_NOW: u32 = 1 << 9;
    pub const PARENT: u32 = 1 << 10;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    FocusChanged(NodeKey),
    /// `(node, mask)`, the mask in [`prop`] bits.
    PropertiesChanged(NodeKey, u32),
    ChildrenChanged(NodeKey),
    TextChanged(NodeKey),
    CaretMoved(NodeKey, u32),
    SelectionChanged(NodeKey),
    Announcement(String),
    WindowActivated(NodeKey),
    NodeRemoved(NodeKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventVerdict {
    Pass,
    Suppress,
    Rewrite(Vec<Segment>),
    ReEvaluate,
}

impl EventVerdict {
    /// Verdicts compose left to right (section 10): the first non-`Pass` verdict wins.
    #[must_use]
    pub fn then(self, next: EventVerdict) -> EventVerdict {
        match self {
            EventVerdict::Pass => next,
            EventVerdict::Suppress | EventVerdict::Rewrite(_) | EventVerdict::ReEvaluate => self,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresentReason {
    Focus,
    Navigation,
    SayAll,
    Review,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PresentCtx {
    pub epoch: u64,
    pub reason: PresentReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavUnit {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResult {
    Handled,
    NotHandled,
    Speak(Vec<Segment>),
}

/// When the core asks an application policy (WIT `policy-moment`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PolicyMoment {
    Focus,
    Value,
    Activate,
    Showing,
}

/// What a policy sees of a node (WIT `policy-view`).
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyView {
    pub moment: PolicyMoment,
    pub role: RoleCode,
    pub flags: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub range: Option<Range>,
}

/// What a policy decides (WIT `policy-verdict`).
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyVerdict {
    /// No opinion: the core presents the node as for any application.
    Defer,
    Silence,
    /// Replace the value in the core's presentation.
    Value(String),
    /// Say this instead.
    Say(Vec<Segment>),
    /// Read the node as a notice when it appears.
    Notice,
}

/// Host functions an extension may call (WIT `host` interface). In the native build the host
/// installs an implementation for the duration of each hook call ([`crate::host::with_host`]);
/// in the component build the calls are WIT imports.
pub trait Host {
    /// Mirror only, never blocks.
    fn get_node(&self, key: NodeKey) -> Option<Node>;
    /// One fused group of a node of this application. Mirror only, never blocks.
    fn read_group(&self, key: NodeKey, group: GroupCode) -> Option<GroupValue>;
    /// Host-enforced deadline.
    fn fetch(&self, key: NodeKey, mask: u32, b: Budget) -> Result<Node, QueryError>;
    fn text(&self, key: NodeKey, start: u32, end: u32, b: Budget) -> Result<String, QueryError>;
    fn speak(&self, segments: Vec<Segment>, prio: Prio);
    /// Own namespace only.
    fn get_setting(&self, k: &str) -> Option<String>;
    fn set_setting(&self, k: &str, v: &str);
    fn log(&self, level: u8, msg: &str);
}

/// Hooks an extension implements (WIT `hooks` interface). Every hook has a default that defers
/// to the core, so an extension implements only what it needs.
pub trait Hooks {
    fn on_event(&mut self, _ev: &Event) -> EventVerdict {
        EventVerdict::Pass
    }
    fn present(&mut self, _n: &Node, _ctx: PresentCtx) -> Option<Vec<Segment>> {
        None
    }
    /// The application policy: facts in, verdict out (ADR-0008 W3). Defers by default.
    fn policy(&mut self, _f: &PolicyView) -> PolicyVerdict {
        PolicyVerdict::Defer
    }
    fn nav_units(&self) -> Vec<NavUnit> {
        Vec::new()
    }
    fn nav_match(&self, _unit: u32, _n: &Node) -> bool {
        false
    }
    fn command(&mut self, _id: u32) -> CommandResult {
        CommandResult::NotHandled
    }
    /// The terminal policy, if this extension is one (WIT interface `terminal`). `None` by
    /// default: `step` then answers no actions, `snapshot` nothing.
    fn terminal(&mut self) -> Option<&mut dyn crate::terminal::TerminalHooks> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_compose_left_to_right() {
        assert_eq!(
            EventVerdict::Pass.then(EventVerdict::Suppress),
            EventVerdict::Suppress
        );
        assert_eq!(
            EventVerdict::Suppress.then(EventVerdict::ReEvaluate),
            EventVerdict::Suppress
        );
        assert_eq!(
            EventVerdict::Pass.then(EventVerdict::Pass),
            EventVerdict::Pass
        );
    }
}
