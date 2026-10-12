//! Hand-written mirror of `wit/gorca-ext.wit`.

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

/// Role names as in the WIT `role` enum. The numeric mapping is part of the generated bindings.
pub type Role = u8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub key: NodeKey,
    pub role: Role,
    pub states: u64,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    FocusChanged(NodeKey),
    PropertiesChanged(NodeKey),
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

/// Host functions an extension may call (WIT `host` interface).
pub trait Host {
    /// Mirror only, never blocks.
    fn get_node(&self, key: NodeKey) -> Option<Node>;
    /// Host-enforced deadline.
    fn fetch(&self, key: NodeKey, mask: u32, b: Budget) -> Result<Node, QueryError>;
    fn text(&self, key: NodeKey, start: u32, end: u32, b: Budget) -> Result<String, QueryError>;
    fn speak(&self, segments: Vec<Segment>, prio: Prio);
    fn set_setting(&self, k: &str, v: &str);
    fn log(&self, level: u8, msg: &str);
}

/// Hooks an extension implements (WIT `hooks` interface).
pub trait Hooks {
    fn on_event(&mut self, ev: &Event) -> EventVerdict;
    fn present(&mut self, n: &Node, ctx: PresentCtx) -> Option<Vec<Segment>>;
    fn nav_units(&self) -> Vec<NavUnit>;
    fn nav_match(&self, unit: u32, n: &Node) -> bool;
    fn command(&mut self, id: u32) -> CommandResult;
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
