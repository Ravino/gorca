// SPDX-License-Identifier: GPL-3.0-or-later
//! Node snapshots and freshness (ADR-0001 sections 4.2 and 4.4).

use std::sync::Arc;
use std::time::Instant;

use bitflags::bitflags;
use smallvec::SmallVec;

use crate::geometry::Rect;
use crate::ids::NodeKey;
use crate::role::Role;
use crate::sem::{SemEntry, SrcRecord};

bitflags! {
    /// Connector-normalised state bits.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct StateSet: u64 {
        const FOCUSED    = 1 << 0;
        const FOCUSABLE  = 1 << 1;
        const SELECTED   = 1 << 2;
        const CHECKED    = 1 << 3;
        const PRESSED    = 1 << 4;
        const EXPANDED   = 1 << 5;
        const COLLAPSED  = 1 << 6;
        const DISABLED   = 1 << 7;
        const EDITABLE   = 1 << 8;
        const READ_ONLY  = 1 << 9;
        const SHOWING    = 1 << 10;
        const VISIBLE    = 1 << 11;
        const BUSY       = 1 << 12;
        const MODAL      = 1 << 13;
        const MULTILINE  = 1 << 14;
        const REQUIRED   = 1 << 15;
        const INVALID    = 1 << 16;
        const HAS_POPUP  = 1 << 17;
        const ACTIVE     = 1 << 18;
        const DEFUNCT    = 1 << 19;
        /// AT-SPI `Enabled`. Not spoken; the presentation log and revalidation compare it.
        const ENABLED    = 1 << 20;
        const SELECTABLE = 1 << 21;
        const TRANSIENT  = 1 << 22;
        const MANAGES_DESCENDANTS = 1 << 23;
        /// A link whose target was visited (AT-SPI `Visited`).
        const VISITED    = 1 << 24;
        /// A tri-state check box in its middle state (AT-SPI `Indeterminate`): "half checked".
        const MIXED      = 1 << 25;
    }
}

bitflags! {
    /// Advisory interface bits. Browsers advertise these too broadly (WEB 1.2, 2.2), so
    /// generators branch on role and states, never on interface presence.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct Ifaces: u32 {
        const TEXT          = 1 << 0;
        const EDITABLE_TEXT = 1 << 1;
        const HYPERTEXT     = 1 << 2;
        const TABLE         = 1 << 3;
        const ACTION        = 1 << 4;
        const SELECTION     = 1 << 5;
        const DOCUMENT      = 1 << 6;
        const VALUE         = 1 << 7;
        const COLLECTION    = 1 << 8;
    }
}

bitflags! {
    /// Which properties changed or are wanted (`NodeEvent::PropertiesChanged`, `BackendOp::Fetch`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct PropMask: u32 {
        const NAME        = 1 << 0;
        const DESCRIPTION = 1 << 1;
        const VALUE       = 1 << 2;
        const ROLE        = 1 << 3;
        const STATES      = 1 << 4;
        const BOUNDS      = 1 << 5;
        const ATTRS       = 1 << 6;
        const RELATIONS   = 1 << 7;
        const CHILDREN    = 1 << 8;
        /// Hint on `PropertiesChanged` only: the node just *became* selected
        /// (`state-changed:selected(1)`). Never part of a fetch mask.
        const SELECTED_NOW = 1 << 9;
        /// The node's parent reference (B-1 PW-2). Part of `ALL`, never of `PRESENT`.
        const PARENT      = 1 << 10;
        /// Hint on `PropertiesChanged` only: the node just became visible
        /// (`state-changed:showing(1)`, M4b notifications). Never part of a fetch mask.
        const SHOWING_NOW = 1 << 11;
    }
}

impl PropMask {
    /// What the focus presenter needs (name, role, states, value, description).
    pub const PRESENT: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::VALUE)
        .union(PropMask::ROLE)
        .union(PropMask::STATES);

    /// Every field a snapshot can carry (no hint bits). `invalidate(k, ALL)` and the app-wide
    /// stale epoch mean this.
    pub const ALL: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::VALUE)
        .union(PropMask::ROLE)
        .union(PropMask::STATES)
        .union(PropMask::BOUNDS)
        .union(PropMask::ATTRS)
        .union(PropMask::RELATIONS)
        .union(PropMask::CHILDREN)
        .union(PropMask::PARENT);

    /// What a `Cache.GetItems` row carries (B-1 F-6): name, description, role, states, child
    /// count and parent. No value, relations, attributes or bounds.
    pub const CACHE_ITEM: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::ROLE)
        .union(PropMask::STATES)
        .union(PropMask::CHILDREN)
        .union(PropMask::PARENT);
}

bitflags! {
    /// The structural bits of a node's state (ADR-0022 section 8.1): whether it exists on the
    /// screen and can take the focus. Nothing spoken is in here; every spoken state is a group of
    /// the fused `SemEntry`. Navigation (object review, the review surface walk) reads these and
    /// nothing else of [`StateSet`].
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct StructFlags: u8 {
        const SHOWING   = 1 << 0;
        const VISIBLE   = 1 << 1;
        const DEFUNCT   = 1 << 2;
        const FOCUSABLE = 1 << 3;
        const FOCUSED   = 1 << 4;
        /// A multi-line text object (a geometry fact of the text layer, not a spoken state).
        const MULTILINE = 1 << 5;
        /// The window the user works in (AT-SPI `active`): structure of the window manager's
        /// view, not a spoken state.
        const ACTIVE    = 1 << 6;
    }
}

/// The structural state of a node, with whether the snapshot carried states at all. A snapshot
/// from a fetch that did not ask for states ([`PropMask::STATES`] clear) is `unknown`: navigation
/// treats such a node as present rather than hiding it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StructState {
    flags: StructFlags,
    known: bool,
}

impl StructState {
    /// The snapshot carried no states.
    pub const UNKNOWN: Self = Self {
        flags: StructFlags::empty(),
        known: false,
    };

    /// Structural state read from a snapshot that carried its states.
    pub const fn known(flags: StructFlags) -> Self {
        Self { flags, known: true }
    }

    /// The states were read (as opposed to never asked for).
    pub const fn is_known(self) -> bool {
        self.known
    }

    pub const fn contains(self, f: StructFlags) -> bool {
        self.flags.contains(f)
    }

    pub const fn is_defunct(self) -> bool {
        self.flags.contains(StructFlags::DEFUNCT)
    }

    pub const fn flags(self) -> StructFlags {
        self.flags
    }
}

/// What a node can do, as the source claimed it (ADR-0022 section 8.1: a capability is structure,
/// not a presented fact). A snapshot from a focus fetch or a child fetch does not say which
/// interfaces the node has; such a node is not `known`, and callers ask instead of assuming.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct NodeCaps {
    ifaces: Ifaces,
}

impl NodeCaps {
    /// Capabilities of a node whose source claimed `ifaces`.
    pub const fn new(ifaces: Ifaces) -> Self {
        Self { ifaces }
    }

    /// The source said which interfaces the node has (an empty claim is "not said").
    pub const fn is_known(self) -> bool {
        !self.ifaces.is_empty()
    }

    /// The node claims a text interface.
    pub const fn has_text(self) -> bool {
        self.ifaces.contains(Ifaces::TEXT)
    }

    /// The node claims an action interface (it can be activated).
    pub const fn has_action(self) -> bool {
        self.ifaces.contains(Ifaces::ACTION)
    }

    /// The node claims a value interface.
    pub const fn has_value(self) -> bool {
        self.ifaces.contains(Ifaces::VALUE)
    }
}

/// Interned attribute key. The interner lives in the connector; the core only compares keys.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AttrKey(pub u16);

/// Small attribute map (xml-roles, level, tag, live-*, ...).
pub type AttrMap = SmallVec<[(AttrKey, Arc<str>); 4]>;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RelationKind {
    LabelledBy,
    LabelFor,
    Controls,
    ControlledBy,
    FlowsTo,
    FlowsFrom,
    DescribedBy,
    MemberOf,
}

pub type Relations = SmallVec<[(RelationKind, NodeKey); 2]>;

/// Numeric range or text value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Range { current: f64, min: f64, max: f64 },
    Text(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildList {
    Known(SmallVec<[NodeKey; 8]>),
    Count(u32),
    Unknown,
}

/// The semantic carrier of a snapshot (M22a-C1b dual-carry, ADR-0022 section 2.11): beside the
/// legacy `role`/`states`/`attrs` fields a snapshot carries what the normaliser made of the same
/// raw data (`rec`, set by the connector) and what `FUSE` made of it (`entry`, set by the app
/// actor's writer, which drops `rec` once fused). Speech reads `entry` only through the writer's
/// `Ivm::entry_view`-backed stamping. Nothing speaks from it yet (M22b); the Reader's web overlay
/// reads the document mirror's entry (`NodeMeta::sem`) instead.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeSem {
    /// The normalised source record of the AT-SPI source (connector output).
    pub rec: Option<Arc<SrcRecord>>,
    /// The fused entry (app actor output).
    pub entry: Option<Arc<SemEntry>>,
}

impl NodeSem {
    /// Nothing carried.
    pub const NONE: NodeSem = NodeSem {
        rec: None,
        entry: None,
    };
}

/// The diagnostic names of what the connector decoded (the presentation log and the mirror
/// probes): the connector's role word and the bit set of the state names. No decision reads it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SrcDiag {
    role: &'static str,
    states: u16,
    /// The source states were read at all (the `sensitive` word needs them).
    states_read: bool,
}

/// The state names a snapshot logs, in a fixed order (the log sorts them).
const DIAG_STATES: &[(StateSet, &str)] = &[
    (StateSet::ACTIVE, "active"),
    (StateSet::CHECKED, "checked"),
    (StateSet::COLLAPSED, "collapsed"),
    (StateSet::DEFUNCT, "defunct"),
    (StateSet::ENABLED, "enabled"),
    (StateSet::EXPANDED, "expanded"),
    (StateSet::FOCUSABLE, "focusable"),
    (StateSet::FOCUSED, "focused"),
    (StateSet::PRESSED, "pressed"),
    (StateSet::SELECTABLE, "selectable"),
    (StateSet::SELECTED, "selected"),
    (StateSet::SHOWING, "showing"),
    (StateSet::TRANSIENT, "transient"),
    (StateSet::VISIBLE, "visible"),
    (StateSet::MANAGES_DESCENDANTS, "manages-descendants"),
];
/// Bit of the `sensitive` word in [`SrcDiag::states`]: the source did not say `disabled`.
const DIAG_SENSITIVE: u16 = 1 << 15;
/// Bit of the `selected` word, for a context that asserts it.
const DIAG_SELECTED: u16 = 1 << 10;

impl SrcDiag {
    /// What nothing was decoded for.
    pub const NONE: SrcDiag = SrcDiag {
        role: "unknown",
        states: 0,
        states_read: false,
    };

    /// The names of a decoded role and state set.
    pub fn of(role: Role, states: StateSet) -> SrcDiag {
        let mut bits = 0u16;
        for (i, (f, _)) in DIAG_STATES.iter().enumerate() {
            if states.contains(*f) {
                bits |= 1 << i;
            }
        }
        if !states.is_empty() && !states.contains(StateSet::DISABLED) {
            bits |= DIAG_SENSITIVE;
        }
        SrcDiag {
            role: role.log_name(),
            states: bits,
            states_read: true,
        }
    }
}

/// The structural flags of a decoded state set (see [`StructFlags`]).
pub fn struct_flags_of(states: StateSet) -> StructFlags {
    let mut f = StructFlags::empty();
    for (state, flag) in [
        (StateSet::SHOWING, StructFlags::SHOWING),
        (StateSet::VISIBLE, StructFlags::VISIBLE),
        (StateSet::DEFUNCT, StructFlags::DEFUNCT),
        (StateSet::FOCUSABLE, StructFlags::FOCUSABLE),
        (StateSet::FOCUSED, StructFlags::FOCUSED),
        (StateSet::MULTILINE, StructFlags::MULTILINE),
        (StateSet::ACTIVE, StructFlags::ACTIVE),
    ] {
        if states.contains(state) {
            f |= flag;
        }
    }
    f
}

/// Immutable node snapshot. Text is deliberately not part of it (see `TextQuery`).
///
/// The fields are private (ADR-0022 section 8.5): a snapshot is built with [`NodeSnapshot::new`]
/// and the `with_*` setters, and read through the accessors. It carries no legacy role, state set
/// or attribute map; what is presented is in the fused entry (`sem`), and the structure that
/// navigation reads is [`NodeSnapshot::struct_state`] and [`NodeSnapshot::caps`].
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSnapshot {
    key: NodeKey,
    name: Option<Arc<str>>,
    description: Option<Arc<str>>,
    value: Option<Value>,
    parent: Option<NodeKey>,
    children: ChildList,
    caps: NodeCaps,
    relations: Relations,
    bounds: Option<Rect>,
    have: PropMask,
    confirmed_seq: u64,
    fresh: Freshness,
    sem: NodeSem,
    structure: StructFlags,
    diag: SrcDiag,
    /// The source's own role is a password field (a mark only the connector sets, beside the
    /// source record's).
    native_secret: bool,
    /// The source role is a window, a dialog or the application (see `ends_ancestry`).
    chain_end: bool,
    /// Test support: the role and states `with_source` was given, so a fixture can describe a
    /// node by them and re-derive its source record (`testing` feature only).
    #[cfg(feature = "testing")]
    pub(crate) test_src: (Role, StateSet),
}

impl NodeSnapshot {
    /// An empty snapshot of `key`: it carries nothing (`have` is empty) and is as fresh as
    /// `fresh` says.
    pub fn new(key: NodeKey, fresh: Freshness) -> NodeSnapshot {
        NodeSnapshot {
            key,
            name: None,
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            caps: NodeCaps::default(),
            relations: Relations::default(),
            bounds: None,
            have: PropMask::empty(),
            confirmed_seq: 0,
            fresh,
            sem: NodeSem::NONE,
            structure: StructFlags::empty(),
            diag: SrcDiag::NONE,
            native_secret: false,
            chain_end: false,
            #[cfg(feature = "testing")]
            test_src: (Role::Unknown, StateSet::empty()),
        }
    }

    pub fn key(&self) -> NodeKey {
        self.key
    }
    pub fn name(&self) -> &Option<Arc<str>> {
        &self.name
    }
    pub fn description(&self) -> &Option<Arc<str>> {
        &self.description
    }
    pub fn value(&self) -> &Option<Value> {
        &self.value
    }
    pub fn parent(&self) -> Option<NodeKey> {
        self.parent
    }
    pub fn children(&self) -> &ChildList {
        &self.children
    }
    pub fn relations(&self) -> &Relations {
        &self.relations
    }
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }
    /// Fields this snapshot actually carries. A `GetItems` node has `PropMask::CACHE_ITEM`; a
    /// full fetch has what was asked for.
    pub fn have(&self) -> PropMask {
        self.have
    }
    /// Mirror write sequence at which the app confirmed this content: the `issued_at` of the
    /// request whose reply produced it (late-reply and re-dirty rules, B-1 5.4).
    pub fn confirmed_seq(&self) -> u64 {
        self.confirmed_seq
    }
    pub fn fresh(&self) -> Freshness {
        self.fresh
    }
    /// The semantic carriers (see [`NodeSem`]).
    pub fn sem(&self) -> &NodeSem {
        &self.sem
    }

    #[must_use]
    pub fn with_key(mut self, v: NodeKey) -> Self {
        self.key = v;
        self
    }
    #[must_use]
    pub fn with_name(mut self, v: Option<Arc<str>>) -> Self {
        self.name = v;
        self
    }
    #[must_use]
    pub fn with_description(mut self, v: Option<Arc<str>>) -> Self {
        self.description = v;
        self
    }
    #[must_use]
    pub fn with_value(mut self, v: Option<Value>) -> Self {
        self.value = v;
        self
    }
    #[must_use]
    pub fn with_parent(mut self, v: Option<NodeKey>) -> Self {
        self.parent = v;
        self
    }
    #[must_use]
    pub fn with_children(mut self, v: ChildList) -> Self {
        self.children = v;
        self
    }
    #[must_use]
    pub fn with_caps(mut self, v: NodeCaps) -> Self {
        self.caps = v;
        self
    }
    #[must_use]
    pub fn with_relations(mut self, v: Relations) -> Self {
        self.relations = v;
        self
    }
    #[must_use]
    pub fn with_bounds(mut self, v: Option<Rect>) -> Self {
        self.bounds = v;
        self
    }
    #[must_use]
    pub fn with_have(mut self, v: PropMask) -> Self {
        self.have = v;
        self
    }
    #[must_use]
    pub fn with_confirmed_seq(mut self, v: u64) -> Self {
        self.confirmed_seq = v;
        self
    }
    #[must_use]
    pub fn with_fresh(mut self, v: Freshness) -> Self {
        self.fresh = v;
        self
    }
    #[must_use]
    pub fn with_sem(mut self, v: NodeSem) -> Self {
        self.sem = v;
        self
    }

    /// The semantic carriers, for the writer that stamps the fused entry on a copy.
    pub fn sem_mut(&mut self) -> &mut NodeSem {
        &mut self.sem
    }
    /// The child list, for the test fixtures that edit one in place.
    pub fn children_mut(&mut self) -> &mut ChildList {
        &mut self.children
    }
    pub fn set_caps(&mut self, v: NodeCaps) {
        self.caps = v;
    }
    pub fn set_key(&mut self, v: NodeKey) {
        self.key = v;
    }
    pub fn set_name(&mut self, v: Option<Arc<str>>) {
        self.name = v;
    }
    pub fn set_value(&mut self, v: Option<Value>) {
        self.value = v;
    }
    pub fn set_parent(&mut self, v: Option<NodeKey>) {
        self.parent = v;
    }
    pub fn set_children(&mut self, v: ChildList) {
        self.children = v;
    }
    pub fn set_have(&mut self, v: PropMask) {
        self.have = v;
    }
    pub fn set_confirmed_seq(&mut self, v: u64) {
        self.confirmed_seq = v;
    }
    pub fn set_fresh(&mut self, v: Freshness) {
        self.fresh = v;
    }
    pub fn set_sem(&mut self, v: NodeSem) {
        self.sem = v;
    }
    pub fn set_description(&mut self, v: Option<Arc<str>>) {
        self.description = v;
    }
    pub fn set_relations(&mut self, v: Relations) {
        self.relations = v;
    }
    pub fn set_bounds(&mut self, v: Option<Rect>) {
        self.bounds = v;
    }

    /// The connector's decode of the node's role and states: the structural bits navigation
    /// reads, the diagnostic names, and the native password mark. The only place the connector's
    /// role and state types enter the record, and nothing reads them back.
    #[must_use]
    pub fn with_source(mut self, role: Role, states: StateSet) -> Self {
        self.structure = struct_flags_of(states);
        self.diag = SrcDiag::of(role, states);
        self.native_secret = crate::redact::is_sensitive(role);
        self.chain_end = matches!(role, Role::Window | Role::Dialog | Role::Application);
        #[cfg(feature = "testing")]
        {
            self.test_src = (role, states);
        }
        self
    }

    /// The role as the diagnostic logs name it (`check_box`): the connector's role, for the
    /// presentation log and the probes only, never for a decision.
    pub fn role_log_name(&self) -> &'static str {
        self.diag.role
    }

    /// The AT-SPI names of the states the connector decoded, plus `sensitive` (the core records
    /// its absence as `DISABLED`): for the presentation log and the mirror probes only, never for
    /// a decision.
    pub fn state_names(&self) -> SmallVec<[&'static str; 8]> {
        let mut out: SmallVec<[&'static str; 8]> = DIAG_STATES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.diag.states & (1 << i) != 0)
            .map(|(_, (_, name))| *name)
            .collect();
        if self.diag.states & DIAG_SENSITIVE != 0 {
            out.push("sensitive");
        }
        out.sort_unstable();
        out
    }

    /// A copy of this snapshot with the `selected` state set, for the presentation log of a
    /// context that asserts it; `None` when it already is (diagnostics only).
    pub fn with_selected_for_log(&self) -> Option<NodeSnapshot> {
        if self.diag.states & DIAG_SELECTED != 0 {
            return None;
        }
        let mut c = self.clone();
        c.diag.states |= DIAG_SELECTED;
        Some(c)
    }

    /// The snapshot of a document node from its fused `entry` (the document mirror's focus
    /// snapshot). `value` is the contents of a field.
    pub fn from_fused(
        key: NodeKey,
        entry: Arc<crate::SemEntry>,
        name: Arc<str>,
        value: Option<Value>,
        now: Instant,
    ) -> NodeSnapshot {
        use crate::sem::{Avail, Check, Expand, Invalid, Press, Select, SemFlags};
        let sem = &entry.sem;
        let mut have = PropMask::NAME | PropMask::ROLE | PropMask::STATES;
        if value.is_some() {
            have |= PropMask::VALUE;
        }
        // The names the log carries for this node: what the entry says, in the connector's words.
        let mut states = StateSet::empty();
        for (flag, state) in [
            (SemFlags::FOCUSABLE, StateSet::FOCUSABLE),
            (SemFlags::EDITABLE, StateSet::EDITABLE),
            (SemFlags::READ_ONLY, StateSet::READ_ONLY),
            (SemFlags::MODAL, StateSet::MODAL),
            (SemFlags::REQUIRED, StateSet::REQUIRED),
            (SemFlags::BUSY, StateSet::BUSY),
        ] {
            if sem.flags.contains(flag) {
                states |= state;
            }
        }
        if sem.select != Select::NotSelectable {
            states |= StateSet::SELECTABLE;
        }
        if sem.select == Select::On {
            states |= StateSet::SELECTED;
        }
        match sem.check {
            Check::On => states |= StateSet::CHECKED,
            Check::Mixed => states |= StateSet::MIXED,
            Check::Off | Check::NotCheckable => {}
        }
        if sem.press == Press::On {
            states |= StateSet::PRESSED;
        }
        match sem.expand {
            Expand::Expanded => states |= StateSet::EXPANDED,
            Expand::Collapsed => states |= StateSet::COLLAPSED,
            Expand::NotExpandable => {}
        }
        if sem.invalid != Invalid::No {
            states |= StateSet::INVALID;
        }
        if sem.avail != Avail::Disabled {
            states |= StateSet::ENABLED;
        }
        let mut diag = SrcDiag::of(Role::Unknown, states);
        diag.role = sem.role.name();
        let description = sem.text.as_deref().and_then(|t| t.description.clone());
        NodeSnapshot {
            key,
            name: Some(name),
            description,
            value,
            parent: None,
            children: ChildList::Unknown,
            caps: NodeCaps::default(),
            relations: Relations::default(),
            bounds: None,
            have,
            confirmed_seq: 0,
            fresh: Freshness::Stale {
                since: now,
                mask: PropMask::VALUE,
            },
            structure: struct_flags_of(states) & StructFlags::FOCUSABLE,
            diag,
            native_secret: false,
            chain_end: false,
            #[cfg(feature = "testing")]
            test_src: (Role::Unknown, states),
            sem: NodeSem {
                rec: None,
                entry: Some(entry),
            },
        }
    }

    /// The source says the node is a window, a dialog or the application: the end of an ancestor
    /// walk (the mirror's prefetch chain). Structure from the connector, as the states are.
    pub fn ends_ancestry(&self) -> bool {
        self.chain_end
    }

    /// The capabilities the source claimed for the node (see [`NodeCaps`]).
    pub fn caps(&self) -> NodeCaps {
        self.caps
    }

    /// The source claimed a password role (the native mark; the record and the entry are asked
    /// beside it, see [`NodeSnapshot::is_sensitive`]).
    pub(crate) fn native_secret(&self) -> bool {
        self.native_secret
    }

    /// The structural state of the node (see [`StructState`]): the only part of the source's
    /// state set that navigation reads.
    pub fn struct_state(&self) -> StructState {
        if !self.have.contains(PropMask::STATES) {
            return StructState::UNKNOWN;
        }
        StructState::known(self.structure)
    }
}

/// Result of a mirror read with the app-wide stale epoch applied (B-1 5.2).
#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub snap: Arc<NodeSnapshot>,
    /// Fields that are no longer trustworthy: the node's own stale mask, plus `ALL` when the
    /// node was confirmed before the app-wide stale epoch.
    pub stale: PropMask,
    /// The last confirmation is older than the app's TTL (B-1 2.2, evaluated on read): the node
    /// may still be presented, tagged `may_refine`, but speaking it is followed by a revalidation.
    pub aged: bool,
}

impl NodeView {
    /// A read is a hit for `need` when the snapshot carries it and none of it is stale.
    pub fn satisfies(&self, need: PropMask) -> bool {
        self.snap.have.contains(need) && !self.stale.intersects(need)
    }
}

/// A bulk cache fill (`AppPort::push_snapshot`, `MirrorWriter::apply`).
#[derive(Clone, Debug, Default)]
pub struct SnapshotBatch {
    pub nodes: Vec<Arc<NodeSnapshot>>,
    /// The producer stopped at a bound (`ChildSnapshots`, `Ancestry`, `Subtree`, `CacheDump`).
    pub truncated: bool,
}

impl SnapshotBatch {
    pub fn new(nodes: Vec<Arc<NodeSnapshot>>) -> Self {
        Self {
            nodes,
            truncated: false,
        }
    }
}

/// Result of a mirror lookup. I-3: `Gone` never panics and never blocks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lookup<T> {
    Hit(T),
    Miss,
    Gone,
}

impl<T> Lookup<T> {
    pub fn hit(self) -> Option<T> {
        match self {
            Lookup::Hit(v) => Some(v),
            Lookup::Miss | Lookup::Gone => None,
        }
    }
}

/// ADR-0001 section 4.4 (B-1 amendment): `Fetching` lives in the driver's in-flight table, not in
/// the published snapshot, which is immutable. `Stale` carries the fields an event said are no
/// longer trustworthy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// Confirmed by connector push or reply.
    Fresh { at: Instant },
    /// Invalidated by an event; still usable for presentation with care.
    Stale { since: Instant, mask: PropMask },
    /// Connector reported removal or the app is gone; a tombstone is kept briefly.
    Defunct,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FreshnessEvent {
    Invalidate {
        at: Instant,
        mask: PropMask,
    },
    /// A reply (or push) confirmed the content that was asked for.
    Confirm {
        at: Instant,
    },
    /// The content was re-dirtied by an event issued after the request that produced it.
    ConfirmRedirtied {
        at: Instant,
        mask: PropMask,
    },
    Removed,
}

impl Freshness {
    /// Transition function of the section 4.4 table. A defunct node never comes back under the
    /// same snapshot (I-1); revival is a new snapshot (LR-3).
    #[must_use]
    pub fn step(self, ev: FreshnessEvent) -> Freshness {
        match self {
            Freshness::Fresh { .. } => match ev {
                FreshnessEvent::Invalidate { at, mask } => Freshness::Stale { since: at, mask },
                FreshnessEvent::Confirm { at } => Freshness::Fresh { at },
                FreshnessEvent::ConfirmRedirtied { at, mask } => {
                    Freshness::Stale { since: at, mask }
                }
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Stale { since, mask } => match ev {
                FreshnessEvent::Invalidate { mask: m, .. }
                | FreshnessEvent::ConfirmRedirtied { mask: m, .. } => Freshness::Stale {
                    since,
                    mask: mask | m,
                },
                FreshnessEvent::Confirm { at } => Freshness::Fresh { at },
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Defunct => Freshness::Defunct,
        }
    }

    /// Fields known to be untrustworthy by this snapshot's own marking.
    pub fn stale_mask(self) -> PropMask {
        match self {
            Freshness::Fresh { .. } => PropMask::empty(),
            Freshness::Stale { mask, .. } => mask,
            Freshness::Defunct => PropMask::ALL,
        }
    }

    /// May the node be presented? Stale nodes may, with the `may_refine` tag.
    pub fn presentable(self) -> bool {
        match self {
            Freshness::Fresh { .. } | Freshness::Stale { .. } => true,
            Freshness::Defunct => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_snapshot_from_a_fused_entry_logs_what_the_entry_says() {
        use crate::sem::{Check, Expand, Select, SemEntry, SemFlags, SemRole};
        let mut e = SemEntry::default();
        e.sem.role = SemRole::Checkbox;
        e.sem.check = Check::On;
        e.sem.expand = Expand::Collapsed;
        e.sem.select = Select::Off;
        e.sem.flags |= SemFlags::FOCUSABLE;
        let key = NodeKey {
            app: crate::AppId {
                connector: crate::ConnectorId(1),
                serial: 1,
            },
            local: crate::LocalId(5),
        };
        let s =
            NodeSnapshot::from_fused(key, Arc::new(e), Arc::from("Wi-Fi"), None, Instant::now());
        assert_eq!(s.role_log_name(), "checkbox");
        let names = s.state_names();
        for st in ["checked", "collapsed", "selectable", "focusable", "enabled"] {
            assert!(names.contains(&st), "{st}");
        }
        assert!(!names.contains(&"selected"));
        assert!(
            s.have()
                .contains(PropMask::NAME | PropMask::ROLE | PropMask::STATES)
        );
        assert!(s.struct_state().contains(StructFlags::FOCUSABLE));
        assert!(s.sem().entry.is_some());
    }

    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn invalidate_accumulates_and_confirm_clears() {
        let t = t0();
        let s = Freshness::Fresh { at: t }.step(FreshnessEvent::Invalidate {
            at: t,
            mask: PropMask::NAME,
        });
        assert_eq!(
            s,
            Freshness::Stale {
                since: t,
                mask: PropMask::NAME
            }
        );
        let s = s.step(FreshnessEvent::Invalidate {
            at: t,
            mask: PropMask::STATES,
        });
        assert_eq!(s.stale_mask(), PropMask::NAME | PropMask::STATES);
        assert_eq!(
            s.step(FreshnessEvent::Confirm { at: t }),
            Freshness::Fresh { at: t }
        );
    }

    #[test]
    fn a_redirtied_confirmation_stays_stale() {
        let t = t0();
        let s = Freshness::Fresh { at: t }.step(FreshnessEvent::ConfirmRedirtied {
            at: t,
            mask: PropMask::NAME,
        });
        assert_eq!(s.stale_mask(), PropMask::NAME);
        assert!(s.presentable());
    }

    #[test]
    fn defunct_is_absorbing() {
        let t = t0();
        let evs = [
            FreshnessEvent::Invalidate {
                at: t,
                mask: PropMask::ALL,
            },
            FreshnessEvent::Confirm { at: t },
            FreshnessEvent::ConfirmRedirtied {
                at: t,
                mask: PropMask::ALL,
            },
            FreshnessEvent::Removed,
        ];
        for ev in evs {
            assert_eq!(Freshness::Defunct.step(ev), Freshness::Defunct);
        }
        assert!(!Freshness::Defunct.presentable());
        assert_eq!(Freshness::Defunct.stale_mask(), PropMask::ALL);
    }

    #[test]
    fn caps_say_what_the_source_claimed() {
        let none = NodeCaps::default();
        assert!(!none.is_known() && !none.has_text() && !none.has_action());
        let c = NodeCaps::new(Ifaces::TEXT | Ifaces::ACTION);
        assert!(c.is_known() && c.has_text() && c.has_action());
        let v = NodeCaps::new(Ifaces::VALUE);
        assert!(v.is_known() && !v.has_text() && !v.has_action());
    }

    #[test]
    fn present_mask_contains_name_and_role() {
        assert!(PropMask::PRESENT.contains(PropMask::NAME | PropMask::ROLE));
        assert!(!PropMask::PRESENT.contains(PropMask::BOUNDS));
        assert!(!PropMask::ALL.contains(PropMask::SELECTED_NOW));
        assert!(!PropMask::ALL.contains(PropMask::SHOWING_NOW));
        assert!(PropMask::ALL.contains(PropMask::PARENT));
        assert!(!PropMask::CACHE_ITEM.contains(PropMask::VALUE));
    }
}
