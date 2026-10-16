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

/// Calls `$cb!(A, B, ...)` with every role variant, in WIT order. Keeps the Rust enum, the WIT
/// enum and the conversions in step (the generated enums on both sides use the same names).
#[macro_export]
macro_rules! for_each_role {
    ($cb:ident) => {
        $cb!(
            Unknown,
            Application,
            Window,
            Dialog,
            Panel,
            Document,
            DocumentWeb,
            Heading,
            Paragraph,
            Link,
            Image,
            List,
            ListItem,
            Table,
            Row,
            Cell,
            ColumnHeader,
            RowHeader,
            Button,
            ToggleButton,
            CheckBox,
            RadioButton,
            ComboBox,
            TextEntry,
            PasswordEntry,
            SpinButton,
            Slider,
            ProgressBar,
            Menu,
            MenuBar,
            MenuItem,
            Popover,
            Tab,
            TabList,
            Tree,
            TreeItem,
            Toolbar,
            StatusBar,
            Separator,
            Label,
            StaticText,
            Terminal,
            Landmark,
            Alert
        );
    };
}

macro_rules! define_role {
    ($($v:ident),* $(,)?) => {
        /// Role names as in the WIT `role` enum.
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
        #[repr(u8)]
        pub enum Role {
            $($v),*
        }

        impl Role {
            /// Every role, in WIT order.
            pub const ALL: &'static [Role] = &[$(Role::$v),*];

            /// The role with WIT index `i`.
            pub fn from_index(i: u8) -> Option<Role> {
                Self::ALL.get(usize::from(i)).copied()
            }
        }
    };
}
for_each_role!(define_role);

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
    pub role: Role,
    pub states: u64,
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

/// What a policy sees of a node (WIT `policy-facts`).
#[derive(Clone, Debug, PartialEq)]
pub struct PolicyFacts {
    pub moment: PolicyMoment,
    pub role: Role,
    pub states: u64,
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
    fn policy(&mut self, _f: &PolicyFacts) -> PolicyVerdict {
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

    #[test]
    fn roles_index_round_trips() {
        for (i, r) in Role::ALL.iter().enumerate() {
            assert_eq!(*r as usize, i);
            assert_eq!(Role::from_index(u8::try_from(i).unwrap()), Some(*r));
        }
        assert_eq!(Role::from_index(200), None);
        assert_eq!(Role::ALL.len(), 44);
    }
}
