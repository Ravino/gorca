//! The pure state of one B-1 fake application: the tree, its mutations and the events and truth
//! lines they cause (SPEC 8.1, H-2).
//!
//! No transport lives here. The D-Bus fake app (`tests/harness/b1/fakeapp`) serves a [`World`]
//! over AT-SPI; the in-process reference reader ([`super::sim`]) consumes the very same
//! [`Effect`]s, so a conformance case behaves identically against both.
//!
//! Time is an argument (`now_ns`), never read here: a seed reproduces a run exactly.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value as Json, json};

use super::proto::{
    Cmd, CmdReply, DestroyEvents, ModelSpec, PathStyle, ReorderMode, Shape, TruthLine,
};
use super::service::Rng;

pub type NodeId = u32;

/// `Atspi.Role` numbers (verified against libatspi).
pub mod role {
    pub const CHECK_BOX: u32 = 7;
    pub const COMBO_BOX: u32 = 11;
    pub const FILLER: u32 = 20;
    pub const FRAME: u32 = 23;
    pub const ICON: u32 = 26;
    pub const IMAGE: u32 = 27;
    pub const LABEL: u32 = 29;
    pub const LIST: u32 = 31;
    pub const LIST_ITEM: u32 = 32;
    pub const MENU_ITEM: u32 = 35;
    pub const PANEL: u32 = 39;
    pub const PUSH_BUTTON: u32 = 43;
    pub const SCROLL_PANE: u32 = 49;
    pub const SLIDER: u32 = 51;
    pub const SPIN_BUTTON: u32 = 52;
    pub const TERMINAL: u32 = 60;
    pub const TEXT: u32 = 61;
    pub const TOGGLE_BUTTON: u32 = 62;
    pub const TOOL_BAR: u32 = 63;
    pub const APPLICATION: u32 = 75;
    pub const ENTRY: u32 = 79;
    pub const DOCUMENT_WEB: u32 = 95;
}

/// `Atspi.StateType` numbers.
pub mod state {
    pub const ACTIVE: u32 = 1;
    pub const CHECKED: u32 = 4;
    pub const COLLAPSED: u32 = 5;
    pub const DEFUNCT: u32 = 6;
    pub const ENABLED: u32 = 8;
    pub const EXPANDED: u32 = 10;
    pub const FOCUSABLE: u32 = 11;
    pub const FOCUSED: u32 = 12;
    pub const PRESSED: u32 = 20;
    pub const SELECTABLE: u32 = 22;
    pub const SELECTED: u32 = 23;
    pub const SENSITIVE: u32 = 24;
    pub const SHOWING: u32 = 25;
    pub const TRANSIENT: u32 = 28;
    pub const VISIBLE: u32 = 30;
    pub const MANAGES_DESCENDANTS: u32 = 31;

    const NAMES: &[(&str, u32)] = &[
        ("active", ACTIVE),
        ("checked", CHECKED),
        ("collapsed", COLLAPSED),
        ("defunct", DEFUNCT),
        ("enabled", ENABLED),
        ("expanded", EXPANDED),
        ("focusable", FOCUSABLE),
        ("focused", FOCUSED),
        ("pressed", PRESSED),
        ("selectable", SELECTABLE),
        ("selected", SELECTED),
        ("sensitive", SENSITIVE),
        ("showing", SHOWING),
        ("transient", TRANSIENT),
        ("visible", VISIBLE),
        ("manages-descendants", MANAGES_DESCENDANTS),
    ];

    pub fn by_name(name: &str) -> Option<u32> {
        NAMES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }

    /// Names of the set bits as the run-directory contract wants them: sorted, without the focus
    /// ring (`focused`, `focusable`), which is not a mutation.
    pub fn names_of(states: u64) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = NAMES
            .iter()
            .filter(|(n, bit)| states & (1u64 << bit) != 0 && !matches!(*n, "focused" | "focusable"))
            .map(|(n, _)| *n)
            .collect();
        v.sort_unstable();
        v
    }

    pub fn name_of(bit: u32) -> &'static str {
        NAMES
            .iter()
            .find(|(_, v)| *v == bit)
            .map_or("unknown", |(n, _)| n)
    }

    /// Bits whose changes are tracked by the truth log and compared by the verdict classifier.
    pub const TRACKED: u64 = (1 << CHECKED)
        | (1 << EXPANDED)
        | (1 << COLLAPSED)
        | (1 << PRESSED)
        | (1 << SELECTED)
        | (1 << SENSITIVE)
        | (1 << SHOWING)
        | (1 << ENABLED);
}

/// Interface bits of a node (`Accessible` is implied).
pub mod iface {
    pub const COMPONENT: u8 = 1;
    pub const VALUE: u8 = 2;
    pub const TEXT: u8 = 4;
    pub const ACTION: u8 = 8;

    pub fn names(mask: u8) -> Vec<&'static str> {
        let mut v = vec!["org.a11y.atspi.Accessible"];
        if mask & COMPONENT != 0 {
            v.push("org.a11y.atspi.Component");
        }
        if mask & ACTION != 0 {
            v.push("org.a11y.atspi.Action");
        }
        if mask & VALUE != 0 {
            v.push("org.a11y.atspi.Value");
        }
        if mask & TEXT != 0 {
            v.push("org.a11y.atspi.Text");
        }
        v
    }
}

pub const BASE_PATH: &str = "/org/a11y/atspi/accessible";
pub const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub id: NodeId,
    pub path: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub role: u32,
    pub name: String,
    pub desc: String,
    pub states: u64,
    pub ifaces: u8,
    pub value: f64,
    /// This node's name is derived from that node's name and follows it silently (G-1, S-1).
    pub name_from: Option<NodeId>,
    pub alive: bool,
    pub hidden: bool,
    pub cache_invisible: bool,
    pub lease_expired: bool,
}

impl Node {
    pub fn has_state(&self, bit: u32) -> bool {
        self.states & (1u64 << bit) != 0
    }
}

/// Read-only copy of what a `Fetch` of the node returns.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub path: String,
    pub name: String,
    pub desc: String,
    pub role: u32,
    pub states: u64,
    pub value: Option<f64>,
    pub ifaces: u8,
    pub child_count: usize,
    pub parent: Option<String>,
}

/// A `Cache` item: `((so)(so)(so)iiassusau)` without the bus names.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: String,
    pub parent: Option<String>,
    pub index: i32,
    pub child_count: i32,
    pub ifaces: Vec<&'static str>,
    pub name: String,
    pub role: u32,
    pub desc: String,
    pub states: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PropKind {
    Name,
    Description,
    Value,
}

impl PropKind {
    pub fn atspi_kind(self) -> &'static str {
        match self {
            PropKind::Name => "accessible-name",
            PropKind::Description => "accessible-description",
            PropKind::Value => "accessible-value",
        }
    }
}

/// A signal the app emits because of a mutation, in emission order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Prop {
        path: String,
        kind: PropKind,
    },
    State {
        path: String,
        state: u32,
        on: bool,
    },
    Children {
        parent: String,
        add: bool,
        index: i32,
        child: String,
    },
    Window {
        path: String,
        kind: &'static str,
    },
    Focus {
        path: String,
    },
    /// `Cache.AddAccessible`.
    CacheAdd(Item),
    /// `Cache.RemoveAccessible`.
    CacheRemove {
        path: String,
    },
}

const WORDS: &[&str] = &[
    "Wi-Fi", "Bluetooth", "Network", "Display", "Sound", "Power", "Mouse", "Keyboard", "Privacy",
    "Sharing", "Users", "About", "Date", "Region", "Apps", "Notifications", "Search", "Multitasking",
    "Wallpaper", "Accessibility", "Online", "Printers", "Storage", "Color", "Thunderbolt",
];

const CONTROL_ROLES: &[u32] = &[
    role::PUSH_BUTTON,
    role::CHECK_BOX,
    role::TOGGLE_BUTTON,
    role::SLIDER,
    role::ENTRY,
    role::COMBO_BOX,
    role::SPIN_BUTTON,
    role::LIST_ITEM,
    role::MENU_ITEM,
];

const BASE_STATES: u64 = (1 << state::ENABLED)
    | (1 << state::SENSITIVE)
    | (1 << state::SHOWING)
    | (1 << state::VISIBLE);

/// `Node.states` bits as the `au` words of `GetState` / cache items.
pub fn state_words(states: u64) -> [u32; 2] {
    // Intentional truncation: low and high 32 bits.
    #[allow(clippy::cast_possible_truncation)]
    [states as u32, (states >> 32) as u32]
}

/// One fake application.
#[derive(Debug)]
pub struct World {
    pub spec: ModelSpec,
    pub label: String,
    nodes: Vec<Node>,
    by_path: HashMap<String, NodeId>,
    landmarks: BTreeMap<String, Vec<NodeId>>,
    rng: Rng,
    counter: u64,
    focus: Option<NodeId>,
    selected: Option<NodeId>,
    list_offset: u32,
    truth: Vec<TruthLine>,
    doc_root: Option<NodeId>,
    realised: HashSet<NodeId>,
}

fn kib(i: u32) -> String {
    format!("{} KB", (u64::from(i) * 7) % 900 + 1)
}

fn item_name(i: u32) -> String {
    format!("File {i:05}.txt")
}

impl World {
    pub fn new(spec: ModelSpec, label: &str) -> Self {
        let rng = Rng::new(spec.seed);
        let mut w = Self {
            spec,
            label: label.to_owned(),
            nodes: Vec::new(),
            by_path: HashMap::new(),
            landmarks: BTreeMap::new(),
            rng,
            counter: 0,
            focus: None,
            selected: None,
            list_offset: 0,
            truth: Vec::new(),
            doc_root: None,
            realised: HashSet::new(),
        };
        w.build();
        w
    }

    // ---- construction --------------------------------------------------------------------

    fn mint_path(&mut self) -> String {
        match self.spec.path_style {
            PathStyle::Counter => {
                self.counter += 1;
                format!("{BASE_PATH}/{}", self.counter)
            }
            PathStyle::Uuid => {
                let hi = self.rng.next_u64();
                let lo = self.rng.next_u64();
                format!("{BASE_PATH}/{hi:016x}{lo:016x}")
            }
        }
    }

    fn add_node(&mut self, parent: Option<NodeId>, r: u32, name: &str, states: u64, ifc: u8) -> NodeId {
        let id = NodeId::try_from(self.nodes.len()).unwrap_or(NodeId::MAX);
        let path = if parent.is_none() {
            ROOT_PATH.to_owned()
        } else {
            self.mint_path()
        };
        self.nodes.push(Node {
            id,
            path: path.clone(),
            parent,
            children: Vec::new(),
            role: r,
            name: name.to_owned(),
            desc: String::new(),
            states,
            ifaces: ifc | iface::COMPONENT,
            value: 0.0,
            name_from: None,
            alive: true,
            hidden: false,
            cache_invisible: false,
            lease_expired: false,
        });
        self.by_path.insert(path, id);
        if let Some(p) = parent
            && let Some(pn) = self.nodes.get_mut(p as usize)
        {
            pn.children.push(id);
        }
        id
    }

    fn container(&mut self, parent: NodeId, r: u32, name: &str) -> NodeId {
        self.add_node(Some(parent), r, name, BASE_STATES, 0)
    }

    fn control(&mut self, parent: NodeId, r: u32, name: &str) -> NodeId {
        let mut ifc = iface::ACTION;
        if matches!(r, role::SLIDER | role::SPIN_BUTTON) {
            ifc |= iface::VALUE;
        }
        if r == role::ENTRY {
            ifc |= iface::TEXT;
        }
        let id = self.add_node(
            Some(parent),
            r,
            name,
            BASE_STATES | (1 << state::FOCUSABLE),
            ifc,
        );
        if ifc & iface::VALUE != 0
            && let Some(n) = self.nodes.get_mut(id as usize)
        {
            n.value = 50.0;
        }
        id
    }

    fn word(&mut self) -> &'static str {
        self.rng.pick(WORDS).copied().unwrap_or("Item")
    }

    /// Spend about `budget` nodes below `parent` (subtrees of panels, controls, labels).
    fn fill(&mut self, parent: NodeId, budget: u32, depth_left: u32) -> u32 {
        let mut used = 0;
        while used < budget {
            let left = budget - used;
            if depth_left > 0 && left > 8 && self.rng.chance(0.45) {
                let share = u32::try_from(self.rng.range(4, u64::from(left.min(60)))).unwrap_or(4);
                let name = format!("{} group", self.word());
                let c = self.container(parent, role::PANEL, &name);
                used += 1 + self.fill(c, share - 1, depth_left - 1);
                continue;
            }
            used += self.leaf(parent);
        }
        used
    }

    /// One leaf (or a labelled control with a derived name); returns the nodes created.
    fn leaf(&mut self, parent: NodeId) -> u32 {
        let w = self.word();
        match self.rng.range(0, 99) {
            0..=29 => {
                self.add_node(Some(parent), role::LABEL, &format!("{w} label"), BASE_STATES, 0);
                1
            }
            30..=44 => {
                // A row-like control whose name derives from its label child (G-1, S-1).
                let c = self.control(parent, role::CHECK_BOX, &format!("{w} setting"));
                let l = self.add_node(
                    Some(c),
                    role::LABEL,
                    &format!("{w} setting"),
                    BASE_STATES,
                    0,
                );
                if let Some(n) = self.nodes.get_mut(c as usize) {
                    n.name_from = Some(l);
                }
                2
            }
            45..=59 => {
                self.control(parent, role::PUSH_BUTTON, &format!("{w} button"));
                1
            }
            60..=66 => {
                self.control(parent, role::TOGGLE_BUTTON, &format!("{w} switch"));
                1
            }
            67..=71 => {
                self.control(parent, role::SLIDER, &format!("{w} level"));
                1
            }
            72..=76 => {
                self.control(parent, role::ENTRY, &format!("{w} entry"));
                1
            }
            77..=83 => {
                self.control(parent, role::COMBO_BOX, &format!("{w} choice"));
                1
            }
            _ => {
                self.add_node(Some(parent), role::IMAGE, &format!("{w} icon"), BASE_STATES, 0);
                1
            }
        }
    }

    fn lm(&mut self, name: &str, ids: Vec<NodeId>) {
        self.landmarks.insert(name.to_owned(), ids);
    }

    fn depth_target(&mut self) -> u32 {
        u32::try_from(self.rng.range(
            u64::from(self.spec.depth_min),
            u64::from(self.spec.depth_max),
        ))
        .unwrap_or(10)
    }

    /// Nested fillers to reach the depth target; returns the innermost.
    fn pad_chain(&mut self, from: NodeId, extra: u32) -> NodeId {
        let mut cur = from;
        for i in 0..extra {
            cur = self.container(cur, role::FILLER, &format!("box {i}"));
        }
        cur
    }

    fn build(&mut self) {
        let root = self.add_node(None, role::APPLICATION, &self.spec.app_name.clone(), BASE_STATES, 0);
        self.lm("root", vec![root]);
        let win = self.add_node(
            Some(root),
            role::FRAME,
            &format!("{} window", self.spec.app_name),
            BASE_STATES | (1 << state::ACTIVE),
            0,
        );
        self.lm("window", vec![win]);
        let depth = self.depth_target();
        let idle = self.spec.nodes_idle;
        match self.spec.shape {
            Shape::Generic => {
                let inner = self.pad_chain(win, depth.saturating_sub(3));
                let budget = idle.saturating_sub(self.live_count());
                self.fill(inner, budget, 3);
            }
            Shape::Storm => {
                let t = self.add_node(
                    Some(win),
                    role::TERMINAL,
                    "terminal",
                    BASE_STATES | (1 << state::FOCUSABLE),
                    iface::TEXT,
                );
                self.lm("terminal", vec![t]);
            }
            Shape::Settings => self.build_settings(win, depth),
            Shape::Files => self.build_files(win, depth),
            Shape::Gtk3 => self.build_gtk3(win, depth),
            Shape::Shell => self.build_shell(win, depth),
            Shape::FfChrome => self.build_ff(win, depth),
        }
        self.refresh_landmarks();
    }

    fn build_headerbar(&mut self, win: NodeId, buttons: &[&str]) {
        let hb = self.container(win, role::TOOL_BAR, "header bar");
        for b in buttons {
            self.control(hb, role::PUSH_BUTTON, b);
        }
        self.add_node(Some(hb), role::LABEL, "title", BASE_STATES, 0);
    }

    fn build_settings(&mut self, win: NodeId, depth: u32) {
        self.build_headerbar(win, &["Search", "Menu", "Back"]);
        let inner = self.pad_chain(win, depth.saturating_sub(7));
        let split = self.container(inner, role::PANEL, "split view");
        let sb_scroll = self.container(split, role::SCROLL_PANE, "sidebar scroller");
        let sb = self.container(sb_scroll, role::LIST, "sidebar");
        self.lm("sidebar_list", vec![sb]);
        for i in 0..self.spec.sidebar_rows.max(1) {
            let w = WORDS[i as usize % WORDS.len()];
            let row = self.control(sb, role::LIST_ITEM, w);
            let l = self.add_node(Some(row), role::LABEL, w, BASE_STATES, 0);
            if let Some(n) = self.nodes.get_mut(row as usize) {
                n.name_from = Some(l);
            }
        }
        let content = self.container(split, role::SCROLL_PANE, "content scroller");
        let panel = self.container(content, role::PANEL, "panel");
        self.lm("panel", vec![panel]);
        let budget = self.spec.nodes_idle.saturating_sub(self.live_count());
        self.fill(panel, budget, 4);
    }

    fn build_files(&mut self, win: NodeId, depth: u32) {
        self.build_headerbar(win, &["Back", "Forward", "Menu", "View"]);
        let inner = self.pad_chain(win, depth.saturating_sub(8));
        let split = self.container(inner, role::PANEL, "split view");
        let sb = self.container(split, role::LIST, "places");
        for w in ["Recent", "Home", "Documents", "Downloads", "Music", "Pictures", "Videos", "Trash"] {
            self.control(sb, role::LIST_ITEM, w);
        }
        let main = self.container(split, role::PANEL, "main");
        let path = self.container(main, role::PANEL, "path bar");
        for w in ["Home", "Documents", "Projects"] {
            self.control(path, role::PUSH_BUTTON, w);
        }
        let scroller = self.container(main, role::SCROLL_PANE, "list scroller");
        let list = self.add_node(
            Some(scroller),
            role::LIST,
            "files",
            BASE_STATES | (1 << state::MANAGES_DESCENDANTS),
            0,
        );
        self.lm("list", vec![list]);
        let rows = self.spec.list_rows.max(1);
        for i in 0..rows {
            let row = self.control(list, role::LIST_ITEM, &item_name(i));
            let icon = self.add_node(Some(row), role::IMAGE, "icon", BASE_STATES, 0);
            let label = self.add_node(Some(row), role::LABEL, &item_name(i), BASE_STATES, 0);
            self.add_node(Some(row), role::LABEL, &kib(i), BASE_STATES, 0);
            self.add_node(Some(row), role::LABEL, "Today", BASE_STATES, 0);
            let check = self.add_node(Some(row), role::CHECK_BOX, "select", BASE_STATES, 0);
            let _ = (icon, check);
            if let Some(n) = self.nodes.get_mut(row as usize) {
                n.name_from = Some(label);
            }
        }
        let budget = self.spec.nodes_idle.saturating_sub(self.live_count());
        let chrome = self.container(main, role::PANEL, "status");
        self.fill(chrome, budget, 3);
    }

    /// GTK3 (gedit-like): menus, a text view and a `MANAGES_DESCENDANTS` list holding most nodes.
    fn build_gtk3(&mut self, win: NodeId, depth: u32) {
        self.build_headerbar(win, &["Open", "Save", "Menu"]);
        let inner = self.pad_chain(win, depth.saturating_sub(5));
        let list = self.add_node(
            Some(inner),
            role::LIST,
            "documents",
            BASE_STATES | (1 << state::MANAGES_DESCENDANTS),
            0,
        );
        self.lm("list", vec![list]);
        let hidden_rows = (self.spec.nodes_idle * 63 / 100).max(1);
        for i in 0..hidden_rows {
            self.control(list, role::LIST_ITEM, &format!("Row {i}"));
        }
        let ed = self.add_node(
            Some(inner),
            role::TEXT,
            "editor",
            BASE_STATES | (1 << state::FOCUSABLE),
            iface::TEXT,
        );
        self.lm("editor", vec![ed]);
        let panel = self.container(inner, role::PANEL, "panel");
        self.lm("panel", vec![panel]);
        let budget = self.spec.nodes_idle.saturating_sub(self.live_count());
        self.fill(panel, budget, 3);
    }

    fn build_shell(&mut self, win: NodeId, depth: u32) {
        let top = self.container(win, role::PANEL, "top bar");
        self.control(top, role::PUSH_BUTTON, "Activities");
        let clock = self.add_node(Some(top), role::LABEL, "Tue 13 Oct 10:41", BASE_STATES, 0);
        self.lm("clock", vec![clock]);
        self.control(top, role::PUSH_BUTTON, "Quick settings");
        let inner = self.pad_chain(win, depth.saturating_sub(4));
        let overview = self.container(inner, role::PANEL, "overview");
        self.lm("overview", vec![overview]);
        let search = self.control(overview, role::ENTRY, "Type to search");
        self.lm("search", vec![search]);
        let results = self.container(overview, role::LIST, "search results");
        self.lm("results_root", vec![results]);
        let grid = self.container(overview, role::PANEL, "app grid");
        let sw = self.container(inner, role::LIST, "switcher");
        self.lm("switcher_root", vec![sw]);
        let budget = self.spec.nodes_idle.saturating_sub(self.live_count());
        self.fill(grid, budget, 4);
    }

    fn build_ff(&mut self, win: NodeId, depth: u32) {
        let inner = self.pad_chain(win, depth.saturating_sub(4));
        let chrome = self.container(inner, role::TOOL_BAR, "navigation");
        let budget = self.spec.nodes_idle.saturating_sub(60);
        self.fill(chrome, budget, 3);
        let doc = self.add_node(Some(inner), role::DOCUMENT_WEB, "page", BASE_STATES, 0);
        self.doc_root = Some(doc);
        self.lm("document", vec![doc]);
        for i in 0..50 {
            self.add_node(Some(doc), role::LABEL, &format!("paragraph {i}"), BASE_STATES, 0);
        }
    }

    /// Recompute the landmarks that depend on dynamic structure.
    fn refresh_landmarks(&mut self) {
        let kids = |w: &Self, key: &str| -> Vec<NodeId> {
            w.landmarks
                .get(key)
                .and_then(|v| v.first())
                .and_then(|id| w.nodes.get(*id as usize))
                .map(|n| n.children.clone())
                .unwrap_or_default()
        };
        let sidebar = kids(self, "sidebar_list");
        if !sidebar.is_empty() {
            self.lm("sidebar", sidebar);
        }
        let rows = kids(self, "list");
        if !rows.is_empty() {
            let labels = rows
                .iter()
                .filter_map(|r| self.nodes.get(*r as usize).and_then(|n| n.name_from))
                .collect();
            self.lm("rows", rows);
            self.lm("row_labels", labels);
        }
        if self.landmarks.contains_key("switcher_root") {
            let s = kids(self, "switcher_root");
            self.lm("switcher", s);
            let r = kids(self, "results_root");
            self.lm("results", r);
        }
        if let Some(panel) = self.landmarks.get("panel").and_then(|v| v.first()).copied() {
            let mut controls = Vec::new();
            let mut stack = vec![panel];
            while let Some(id) = stack.pop() {
                let Some(n) = self.nodes.get(id as usize) else { continue };
                if id != panel && CONTROL_ROLES.contains(&n.role) {
                    controls.push(id);
                }
                for c in n.children.iter().rev() {
                    stack.push(*c);
                }
            }
            let pick = |w: &Self, f: &dyn Fn(&Node) -> bool| -> Vec<NodeId> {
                controls
                    .iter()
                    .copied()
                    .filter(|id| w.nodes.get(*id as usize).is_some_and(f))
                    .collect()
            };
            let switches = pick(self, &|n| matches!(n.role, role::TOGGLE_BUTTON | role::CHECK_BOX));
            let sliders = pick(self, &|n| n.role == role::SLIDER);
            let derived = pick(self, &|n| n.name_from.is_some());
            let derived_labels = derived
                .iter()
                .filter_map(|d| self.nodes.get(*d as usize).and_then(|n| n.name_from))
                .collect();
            self.lm("controls", controls);
            self.lm("switches", switches);
            self.lm("slider", sliders);
            self.lm("derived", derived);
            self.lm("derived_labels", derived_labels);
        }
    }

    // ---- queries -------------------------------------------------------------------------

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id as usize)
    }

    /// Alive nodes (those that answer calls).
    #[allow(clippy::cast_possible_truncation)]
    pub fn live_count(&self) -> u32 {
        self.by_path.len() as u32
    }

    /// Resolve a node reference: a landmark (`controls[3]`, `panel`), `id:N` or a path.
    pub fn resolve(&self, r: &str) -> Option<NodeId> {
        if let Some(id) = r.strip_prefix("id:") {
            return id.parse().ok();
        }
        if r.starts_with('/') {
            return self.by_path.get(r).copied();
        }
        let (name, idx) = match r.split_once('[') {
            Some((n, rest)) => (n, rest.strip_suffix(']')?.parse::<usize>().ok()?),
            None => (r, 0),
        };
        self.landmarks.get(name)?.get(idx).copied()
    }

    /// Path of a reference, also for dead nodes that were once resolved.
    pub fn path_of(&self, r: &str) -> Option<String> {
        self.resolve(r)
            .and_then(|id| self.node(id))
            .map(|n| n.path.clone())
    }

    pub fn landmark_paths(&self) -> BTreeMap<String, Vec<String>> {
        self.landmarks
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter()
                        .filter_map(|id| self.node(*id))
                        .map(|n| n.path.clone())
                        .collect(),
                )
            })
            .collect()
    }

    /// The path answers calls (not unknown, not dead, lease not expired).
    pub fn is_alive_path(&self, path: &str) -> bool {
        self.by_path(path).is_some()
    }

    /// The node answering at `path`, `None` when the path is unknown or dead (`UnknownObject`).
    pub fn by_path(&self, path: &str) -> Option<&Node> {
        self.by_path
            .get(path)
            .and_then(|id| self.node(*id))
            .filter(|n| !n.lease_expired)
    }

    pub fn view(&self, path: &str) -> Option<NodeView> {
        let n = self.by_path(path)?;
        Some(NodeView {
            path: n.path.clone(),
            name: n.name.clone(),
            desc: n.desc.clone(),
            role: n.role,
            states: n.states,
            value: (n.ifaces & iface::VALUE != 0).then_some(n.value),
            ifaces: n.ifaces,
            child_count: n.children.len(),
            parent: n
                .parent
                .and_then(|p| self.node(p))
                .map(|p| p.path.clone()),
        })
    }

    pub fn child_paths(&self, path: &str) -> Option<Vec<String>> {
        let n = self.by_path(path)?;
        Some(
            n.children
                .iter()
                .filter_map(|c| self.node(*c))
                .map(|c| c.path.clone())
                .collect(),
        )
    }

    /// True when `path` lies strictly below a `Document*` node (IC-20).
    pub fn below_document(&self, path: &str) -> bool {
        let Some(doc) = self.doc_root else { return false };
        let mut cur = self.by_path.get(path).copied();
        while let Some(id) = cur {
            let Some(n) = self.node(id) else { return false };
            if n.parent == Some(doc) {
                return true;
            }
            cur = n.parent;
        }
        false
    }

    pub fn item(&self, id: NodeId) -> Option<Item> {
        let n = self.node(id)?;
        let parent = n.parent.and_then(|p| self.node(p));
        let index = parent
            .and_then(|p| p.children.iter().position(|c| *c == id))
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(-1);
        Some(Item {
            path: n.path.clone(),
            parent: parent.map(|p| p.path.clone()),
            index,
            child_count: if n.has_state(state::MANAGES_DESCENDANTS) {
                -1
            } else {
                i32::try_from(n.children.len()).unwrap_or(i32::MAX)
            },
            ifaces: iface::names(n.ifaces),
            name: n.name.clone(),
            role: n.role,
            desc: n.desc.clone(),
            states: n.states,
        })
    }

    fn in_cache(&self, n: &Node) -> bool {
        if !n.alive || n.lease_expired || n.cache_invisible {
            return false;
        }
        if n.hidden && self.spec.quirks.hidden_skips_cache {
            return false;
        }
        if self.spec.quirks.dump_realised_only
            && n.id > 1
            && !self.realised.contains(&n.id)
        {
            return false;
        }
        if self.spec.quirks.dump_omits_transient {
            if n.has_state(state::TRANSIENT) {
                return false;
            }
            if let Some(p) = n.parent.and_then(|p| self.node(p))
                && p.has_state(state::MANAGES_DESCENDANTS)
            {
                return false;
            }
        }
        // GetItems does not return document nodes (ADR-0001 5.4).
        !(self.doc_root.is_some_and(|d| n.id == d) || self.below_document(&n.path))
    }

    /// What `Cache.GetItems` returns now, capped at `max` items.
    pub fn dump(&self, max: usize) -> Vec<Item> {
        self.nodes
            .iter()
            .filter(|n| self.in_cache(n))
            .take(max)
            .filter_map(|n| self.item(n.id))
            .collect()
    }

    pub fn focus_path(&self) -> Option<String> {
        self.focus.and_then(|f| self.node(f)).map(|n| n.path.clone())
    }

    pub fn take_truth(&mut self) -> Vec<TruthLine> {
        std::mem::take(&mut self.truth)
    }

    pub fn truth(&self) -> &[TruthLine] {
        &self.truth
    }

    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    // ---- mutations -----------------------------------------------------------------------

    fn tl(
        &mut self,
        now_ns: u64,
        path: &str,
        field: &str,
        old: impl Into<Json>,
        new: impl Into<Json>,
        evented: bool,
    ) {
        self.truth.push(TruthLine {
            t_ns: now_ns,
            app: self.label.clone(),
            path: path.to_owned(),
            field: field.to_owned(),
            old: old.into(),
            new: new.into(),
            evented,
            focus: false,
        });
    }

    fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            out.push(i);
            if let Some(n) = self.node(i) {
                for c in n.children.iter().rev() {
                    stack.push(*c);
                }
            }
        }
        out
    }

    fn kill(&mut self, id: NodeId) {
        for d in self.descendants(id) {
            if let Some(n) = self.node_mut(d) {
                n.alive = false;
                let p = n.path.clone();
                self.by_path.remove(&p);
            }
            if self.focus == Some(d) {
                self.focus = None;
            }
        }
    }

    fn detach(&mut self, id: NodeId) -> Option<(NodeId, i32)> {
        let parent = self.node(id)?.parent?;
        let pn = self.node_mut(parent)?;
        let idx = pn.children.iter().position(|c| *c == id)?;
        pn.children.remove(idx);
        Some((parent, i32::try_from(idx).unwrap_or(0)))
    }

    fn cache_signalled(&self, n: &Node) -> bool {
        !(n.hidden && self.spec.quirks.hidden_skips_cache) && !n.cache_invisible
    }

    fn effects_remove_subtree(&self, id: NodeId, out: &mut Vec<Effect>) {
        if self.spec.quirks.never_remove_accessible {
            return; // P-9 H1: GTK4 never sends RemoveAccessible for widget removal
        }
        let mut ids = self.descendants(id);
        ids.reverse();
        for d in ids {
            if let Some(n) = self.node(d)
                && self.cache_signalled(n)
            {
                out.push(Effect::CacheRemove {
                    path: n.path.clone(),
                });
            }
        }
    }

    fn effects_add_subtree(&self, id: NodeId, out: &mut Vec<Effect>) {
        for d in self.descendants(id) {
            if let Some(n) = self.node(d)
                && self.cache_signalled(n)
                && let Some(item) = self.item(d)
            {
                out.push(Effect::CacheAdd(item));
            }
        }
    }

    fn name_dependents(&self, src: NodeId) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|n| n.alive && n.name_from == Some(src))
            .map(|n| n.id)
            .collect()
    }

    fn set_name(&mut self, id: NodeId, name: &str, evented: bool, now_ns: u64, fx: &mut Vec<Effect>) {
        let Some(n) = self.node_mut(id) else { return };
        let old = std::mem::replace(&mut n.name, name.to_owned());
        let path = n.path.clone();
        self.tl(now_ns, &path, "name", old, name.to_owned(), evented);
        if evented {
            fx.push(Effect::Prop {
                path,
                kind: PropKind::Name,
            });
            self.spurious_parent_add(id, fx);
        }
        for dep in self.name_dependents(id) {
            let silent = self.spec.quirks.silent_derived_names;
            self.set_name(dep, name, !silent, now_ns, fx);
        }
    }

    /// P9-2: a child update also announces `ChildrenChanged:add` on the parent.
    fn spurious_parent_add(&self, id: NodeId, fx: &mut Vec<Effect>) {
        if !self.spec.quirks.spurious_parent_add {
            return;
        }
        let Some(n) = self.node(id) else { return };
        let Some(p) = n.parent.and_then(|p| self.node(p)) else { return };
        fx.push(Effect::Children {
            parent: p.path.clone(),
            add: true,
            index: p.children.iter().position(|c| *c == id).and_then(|i| i32::try_from(i).ok()).unwrap_or(0),
            child: n.path.clone(),
        });
    }

    /// A client read this node (or listed its children): GTK4 realises its context (P-9).
    pub fn mark_realised(&mut self, path: &str, with_children: bool) {
        let Some(id) = self.by_path.get(path).copied() else { return };
        self.realised.insert(id);
        if with_children {
            let kids = self.node(id).map(|n| n.children.clone()).unwrap_or_default();
            self.realised.extend(kids);
        }
    }

    fn set_state_bit(
        &mut self,
        id: NodeId,
        bit: u32,
        on: bool,
        evented: bool,
        now_ns: u64,
        fx: &mut Vec<Effect>,
    ) {
        let Some(n) = self.node_mut(id) else { return };
        let before = n.states;
        if on {
            n.states |= 1u64 << bit;
        } else {
            n.states &= !(1u64 << bit);
        }
        let after = n.states;
        let path = n.path.clone();
        if bit != state::FOCUSED && before != after {
            self.tl(
                now_ns,
                &path,
                "states",
                json!(state::names_of(before)),
                json!(state::names_of(after)),
                evented,
            );
        }
        if evented {
            fx.push(Effect::State {
                path,
                state: bit,
                on,
            });
            if bit != state::FOCUSED {
                self.spurious_parent_add(id, fx);
            }
        }
    }

    /// Apply one world command. Commands that are about the transport (`Hang`, `DelayReplies`,
    /// `Storm`, `Stats`, `Kill`, `Quit`) are handled by the caller and answered `ok` here.
    #[allow(clippy::too_many_lines)]
    pub fn apply(&mut self, cmd: &Cmd, now_ns: u64) -> (CmdReply, Vec<Effect>) {
        let mut fx = Vec::new();
        let reply = match cmd {
            Cmd::Focus { node } => self.do_focus(node, now_ns, &mut fx),
            Cmd::Rename {
                node,
                name,
                evented,
            } => match self.resolve(node) {
                Some(id) => {
                    let old = self.node(id).map(|n| n.name.clone()).unwrap_or_default();
                    self.set_name(id, name, *evented, now_ns, &mut fx);
                    CmdReply::ok(json!({"path": self.path_of(node), "old": old, "new": name}))
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::SetState {
                node,
                state: sname,
                on,
                evented,
            } => match (self.resolve(node), state::by_name(sname)) {
                (Some(id), Some(bit)) => {
                    self.set_state_bit(id, bit, *on, *evented, now_ns, &mut fx);
                    CmdReply::ok(json!({"path": self.path_of(node)}))
                }
                _ => CmdReply::err(format!("bad node or state: {node} {sname}")),
            },
            Cmd::SetValue {
                node,
                value,
                evented,
            } => match self.resolve(node) {
                Some(id) => {
                    let Some(n) = self.node_mut(id) else {
                        return (CmdReply::err("dead"), fx);
                    };
                    if n.ifaces & iface::VALUE == 0 {
                        return (CmdReply::err("node has no Value"), fx);
                    }
                    let old = std::mem::replace(&mut n.value, *value);
                    let path = n.path.clone();
                    self.tl(now_ns, &path, "value", old, *value, *evented);
                    if *evented {
                        fx.push(Effect::Prop {
                            path: path.clone(),
                            kind: PropKind::Value,
                        });
                    }
                    CmdReply::ok(json!({"path": path}))
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::ReplaceSubtree {
                root,
                nodes,
                remove_events,
                add_events,
            } => self.do_replace(root, *nodes, *remove_events, *add_events, now_ns, &mut fx),
            Cmd::Scroll { steps } => self.do_scroll(*steps, now_ns, &mut fx),
            Cmd::Hide { node } => match self.resolve(node) {
                Some(id) => {
                    if let Some(n) = self.node_mut(id) {
                        n.hidden = true;
                    }
                    if !self.spec.quirks.hidden_ghost {
                        self.set_state_bit(id, state::SHOWING, false, true, now_ns, &mut fx);
                    }
                    CmdReply::ok(json!({"path": self.path_of(node)}))
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::Destroy { node, events } => match self.resolve(node) {
                Some(id) => self.do_destroy(id, *events, now_ns, &mut fx),
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::Reorder { parent, mode } => self.do_reorder(parent, *mode, now_ns, &mut fx),
            Cmd::Add {
                parent,
                name,
                during_dump,
                silent,
            } => self.do_add(parent, name, *during_dump, *silent, now_ns, &mut fx),
            Cmd::ExpireLease { node } => match self.resolve(node) {
                Some(id) => {
                    if let Some(n) = self.node_mut(id) {
                        n.lease_expired = true;
                        let p = n.path.clone();
                        self.tl(now_ns, &p, "lease", "alive", "expired", false);
                    }
                    CmdReply::ok(json!({"path": self.nodes.get(id as usize).map(|n| n.path.clone())}))
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::RebuildSwitcher { items } => self.do_rebuild_switcher(*items, now_ns, &mut fx),
            Cmd::Select { node } => match self.resolve(node) {
                Some(id) => {
                    if let Some(old) = self.selected.filter(|o| *o != id) {
                        self.set_state_bit(old, state::SELECTED, false, true, now_ns, &mut fx);
                    }
                    self.set_state_bit(id, state::SELECTED, true, true, now_ns, &mut fx);
                    self.selected = Some(id);
                    CmdReply::ok(json!({"path": self.path_of(node)}))
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::WindowDestroy { node } => match self.resolve(node) {
                Some(id) => {
                    let path = self.node(id).map(|n| n.path.clone()).unwrap_or_default();
                    fx.push(Effect::Window {
                        path,
                        kind: "destroy",
                    });
                    self.do_destroy(id, DestroyEvents::Full, now_ns, &mut fx)
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::WindowActivate { node } => match self.resolve(node) {
                Some(id) => {
                    self.set_state_bit(id, state::ACTIVE, true, true, now_ns, &mut fx);
                    fx.push(Effect::Window {
                        path: self.node(id).map(|n| n.path.clone()).unwrap_or_default(),
                        kind: "activate",
                    });
                    CmdReply::ok(Json::Null)
                }
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::Get { node } => match self.resolve(node).and_then(|i| self.node(i)) {
                Some(n) => CmdReply::ok(json!({
                    "path": n.path, "name": n.name, "desc": n.desc, "role": n.role,
                    "states": n.states, "value": n.value, "alive": n.alive && !n.lease_expired,
                    "children": n.children.len(),
                    "parent": n.parent.and_then(|p| self.node(p)).map(|p| p.path.clone()),
                })),
                None => CmdReply::err(format!("no node {node}")),
            },
            Cmd::Landmarks => {
                CmdReply::ok(json!({"landmarks": self.landmark_paths(), "live": self.live_count()}))
            }
            Cmd::Hang { .. }
            | Cmd::DelayReplies { .. }
            | Cmd::Storm { .. }
            | Cmd::ClosePeers
            | Cmd::RegisterListener
            | Cmd::Malform { .. }
            | Cmd::PeerAcceptDelay { .. }
            | Cmd::Stats
            | Cmd::Kill
            | Cmd::Quit => CmdReply::ok(Json::Null),
        };
        self.refresh_landmarks();
        (reply, fx)
    }

    fn do_focus(&mut self, node: &str, now_ns: u64, fx: &mut Vec<Effect>) -> CmdReply {
        let Some(id) = self.resolve(node) else {
            return CmdReply::err(format!("no node {node}"));
        };
        let Some(path) = self.node(id).map(|n| n.path.clone()) else {
            return CmdReply::err("dead");
        };
        // The toolkit can announce focus on an object whose lease expired; a dead one it cannot.
        if !self.by_path.contains_key(&path) {
            return CmdReply::err(format!("{node} is not alive"));
        }
        if let Some(old) = self.focus.filter(|o| *o != id) {
            self.set_state_bit(old, state::FOCUSED, false, true, now_ns, fx);
        }
        self.set_state_bit(id, state::FOCUSED, true, true, now_ns, fx);
        self.focus = Some(id);
        fx.push(Effect::Focus { path: path.clone() });
        self.truth.push(TruthLine {
            t_ns: now_ns,
            app: self.label.clone(),
            path: path.clone(),
            focus: true,
            ..TruthLine::default()
        });
        CmdReply::ok(json!({"path": path}))
    }

    fn do_replace(
        &mut self,
        root: &str,
        count: u32,
        remove_events: bool,
        add_events: bool,
        now_ns: u64,
        fx: &mut Vec<Effect>,
    ) -> CmdReply {
        let Some(rid) = self.resolve(root) else {
            return CmdReply::err(format!("no node {root}"));
        };
        let old_children: Vec<NodeId> = self.node(rid).map(|n| n.children.clone()).unwrap_or_default();
        let mut n_old = 0u32;
        let root_path = self.node(rid).map(|n| n.path.clone()).unwrap_or_default();
        for (i, c) in old_children.iter().enumerate() {
            n_old += u32::try_from(self.descendants(*c).len()).unwrap_or(u32::MAX);
            if remove_events {
                if let Some(cn) = self.node(*c) {
                    fx.push(Effect::Children {
                        parent: root_path.clone(),
                        add: false,
                        index: i32::try_from(i).unwrap_or(0),
                        child: cn.path.clone(),
                    });
                }
                self.effects_remove_subtree(*c, fx);
            }
            if self.focus.is_some_and(|f| self.descendants(*c).contains(&f)) {
                self.focus = None;
            }
            self.kill(*c);
        }
        if let Some(n) = self.node_mut(rid) {
            n.children.clear();
        }
        let before = self.nodes.len();
        let used = self.fill(rid, count, 4);
        let new_top: Vec<NodeId> = self.node(rid).map(|n| n.children.clone()).unwrap_or_default();
        if add_events {
            for (i, c) in new_top.iter().enumerate() {
                if let Some(cn) = self.node(*c) {
                    fx.push(Effect::Children {
                        parent: root_path.clone(),
                        add: true,
                        index: i32::try_from(i).unwrap_or(0),
                        child: cn.path.clone(),
                    });
                }
                self.effects_add_subtree(*c, fx);
            }
        }
        let _ = before;
        self.tl(
            now_ns,
            &root_path,
            "subtree",
            n_old,
            used,
            remove_events || add_events,
        );
        let top: Vec<String> = new_top
            .iter()
            .filter_map(|c| self.node(*c))
            .map(|n| n.path.clone())
            .collect();
        CmdReply::ok(json!({"root": root_path, "old": n_old, "new": used, "children": top}))
    }

    fn do_scroll(&mut self, steps: i32, now_ns: u64, fx: &mut Vec<Effect>) -> CmdReply {
        let rows = self.landmarks.get("rows").cloned().unwrap_or_default();
        if rows.is_empty() {
            return CmdReply::err("no list rows");
        }
        let rows_n = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        let max_off = self.spec.list_items.max(rows_n) - rows_n;
        let off = (i64::from(self.list_offset) + i64::from(steps)).clamp(0, i64::from(max_off));
        let off = u32::try_from(off).unwrap_or(0);
        if off == self.list_offset {
            return CmdReply::ok(json!({"offset": off, "changed": 0}));
        }
        self.list_offset = off;
        let evented = self.spec.quirks.recycle_evented;
        let mut changed = 0u32;
        for (i, row) in rows.iter().enumerate() {
            let item = off + u32::try_from(i).unwrap_or(0);
            let kids: Vec<NodeId> = self.node(*row).map(|n| n.children.clone()).unwrap_or_default();
            // children: icon, label, size, date, check
            if let Some(label) = kids.get(1).copied() {
                self.set_name(label, &item_name(item), evented, now_ns, fx);
                changed += 1;
            }
            if let Some(size) = kids.get(2).copied() {
                self.set_name(size, &kib(item), evented, now_ns, fx);
            }
        }
        CmdReply::ok(json!({"offset": off, "changed": changed}))
    }

    fn do_destroy(
        &mut self,
        id: NodeId,
        events: DestroyEvents,
        now_ns: u64,
        fx: &mut Vec<Effect>,
    ) -> CmdReply {
        let Some(path) = self.node(id).map(|n| n.path.clone()) else {
            return CmdReply::err("dead");
        };
        if events != DestroyEvents::Silent {
            fx.push(Effect::State {
                path: path.clone(),
                state: state::DEFUNCT,
                on: true,
            });
        }
        if events == DestroyEvents::Full {
            self.effects_remove_subtree(id, fx);
        }
        let detached = self.detach(id);
        if events == DestroyEvents::Full
            && let Some((parent, idx)) = detached
            && let Some(pp) = self.node(parent).map(|p| p.path.clone())
        {
            fx.push(Effect::Children {
                parent: pp,
                add: false,
                index: idx,
                child: path.clone(),
            });
        }
        self.kill(id);
        self.tl(
            now_ns,
            &path,
            "destroyed",
            "alive",
            "dead",
            events != DestroyEvents::Silent,
        );
        CmdReply::ok(json!({"path": path}))
    }

    fn do_reorder(&mut self, parent: &str, mode: ReorderMode, now_ns: u64, fx: &mut Vec<Effect>) -> CmdReply {
        let Some(pid) = self.resolve(parent) else {
            return CmdReply::err(format!("no node {parent}"));
        };
        let Some(pn) = self.node(pid) else {
            return CmdReply::err("dead");
        };
        let old: Vec<NodeId> = pn.children.clone();
        if old.len() < 2 {
            return CmdReply::err("fewer than two children");
        }
        let ppath = pn.path.clone();
        let mut new = old.clone();
        // Rotate by a seeded amount, guaranteed to differ from the identity.
        let k = 1 + usize::try_from(self.rng.next_u64() % (new.len() as u64 - 1)).unwrap_or(0);
        new.rotate_left(k);
        if let Some(n) = self.node_mut(pid) {
            n.children.clone_from(&new);
        }
        let paths = |w: &Self, v: &[NodeId]| -> String {
            v.iter()
                .filter_map(|i| w.node(*i))
                .map(|n| n.path.rsplit('/').next().unwrap_or("").to_owned())
                .collect::<Vec<_>>()
                .join(",")
        };
        let (o, n) = (paths(self, &old), paths(self, &new));
        self.tl(now_ns, &ppath, "order", o, n, mode != ReorderMode::Silent);
        match mode {
            ReorderMode::Silent => {}
            ReorderMode::ChildrenOnly => {
                for (i, c) in new.iter().enumerate() {
                    if let Some(cn) = self.node(*c) {
                        fx.push(Effect::Children {
                            parent: ppath.clone(),
                            add: false,
                            index: i32::try_from(i).unwrap_or(0),
                            child: cn.path.clone(),
                        });
                        fx.push(Effect::Children {
                            parent: ppath.clone(),
                            add: true,
                            index: i32::try_from(i).unwrap_or(0),
                            child: cn.path.clone(),
                        });
                    }
                }
            }
            ReorderMode::RemoveAdd => {
                for (i, c) in new.iter().enumerate() {
                    let idx = i32::try_from(i).unwrap_or(0);
                    if let Some(cn) = self.node(*c) {
                        fx.push(Effect::Children {
                            parent: ppath.clone(),
                            add: false,
                            index: idx,
                            child: cn.path.clone(),
                        });
                        if self.cache_signalled(cn) && !self.spec.quirks.never_remove_accessible {
                            fx.push(Effect::CacheRemove {
                                path: cn.path.clone(),
                            });
                        }
                    }
                    if let Some(cn) = self.node(*c) {
                        if self.cache_signalled(cn)
                            && let Some(item) = self.item(*c)
                        {
                            fx.push(Effect::CacheAdd(item));
                        }
                        fx.push(Effect::Children {
                            parent: ppath.clone(),
                            add: true,
                            index: idx,
                            child: cn.path.clone(),
                        });
                    }
                }
            }
        }
        CmdReply::ok(json!({"parent": ppath}))
    }

    fn do_add(
        &mut self,
        parent: &str,
        name: &str,
        during_dump: bool,
        silent: bool,
        now_ns: u64,
        fx: &mut Vec<Effect>,
    ) -> CmdReply {
        let Some(pid) = self.resolve(parent) else {
            return CmdReply::err(format!("no node {parent}"));
        };
        let id = self.control(pid, role::PUSH_BUTTON, name);
        let invisible = during_dump && self.spec.quirks.add_during_dump_invisible;
        if let Some(n) = self.node_mut(id) {
            n.cache_invisible = invisible;
        }
        let ppath = self.node(pid).map(|n| n.path.clone()).unwrap_or_default();
        let path = self.node(id).map(|n| n.path.clone()).unwrap_or_default();
        if !silent {
            let idx = self
                .node(pid)
                .map_or(0, |p| i32::try_from(p.children.len()).unwrap_or(0) - 1);
            fx.push(Effect::Children {
                parent: ppath,
                add: true,
                index: idx,
                child: path.clone(),
            });
            if !invisible && let Some(item) = self.item(id) {
                fx.push(Effect::CacheAdd(item));
            }
        }
        self.tl(now_ns, &path, "added", Json::Null, name.to_owned(), !silent);
        CmdReply::ok(json!({"path": path, "cache_invisible": invisible}))
    }

    fn do_rebuild_switcher(&mut self, items: u32, now_ns: u64, fx: &mut Vec<Effect>) -> CmdReply {
        let Some(root) = self.landmarks.get("switcher_root").and_then(|v| v.first()).copied() else {
            return CmdReply::err("no switcher");
        };
        let rpath = self.node(root).map(|n| n.path.clone()).unwrap_or_default();
        let old: Vec<NodeId> = self.node(root).map(|n| n.children.clone()).unwrap_or_default();
        for (i, c) in old.iter().enumerate() {
            if let Some(cn) = self.node(*c) {
                fx.push(Effect::Children {
                    parent: rpath.clone(),
                    add: false,
                    index: i32::try_from(i).unwrap_or(0),
                    child: cn.path.clone(),
                });
            }
            self.kill(*c);
        }
        if let Some(n) = self.node_mut(root) {
            n.children.clear();
        }
        self.selected = None;
        for i in 0..items {
            let id = self.control(root, role::LIST_ITEM, &format!("Window {i}"));
            // Event-only objects: never in the cache (A-2).
            if let Some(n) = self.node_mut(id) {
                n.cache_invisible = true;
                n.states |= 1 << state::TRANSIENT;
            }
            if let Some(cn) = self.node(id) {
                fx.push(Effect::Children {
                    parent: rpath.clone(),
                    add: true,
                    index: i32::try_from(i).unwrap_or(0),
                    child: cn.path.clone(),
                });
            }
        }
        self.tl(now_ns, &rpath, "subtree", old.len(), items, true);
        CmdReply::ok(json!({"root": rpath, "items": items}))
    }

    /// A text flood event target and its path, for `Storm`.
    pub fn storm_target(&self) -> Option<String> {
        self.landmarks
            .get("terminal")
            .and_then(|v| v.first())
            .and_then(|i| self.node(*i))
            .map(|n| n.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::b1::proto::{P2pMode, Quirks};

    fn settings() -> ModelSpec {
        ModelSpec {
            name: "settings".into(),
            app_name: "gnome-control-center".into(),
            shape: Shape::Settings,
            nodes_idle: 800,
            nodes_peak: 2000,
            depth_min: 15,
            depth_max: 25,
            sidebar_rows: 45,
            p2p: P2pMode::SameBus,
            quirks: Quirks {
                // The tests below count effects; the P-9 flags have their own tests.
                never_remove_accessible: false,
                spurious_parent_add: false,
                ..Quirks::pessimistic_gtk4()
            },
            ..ModelSpec::default()
        }
    }

    fn files() -> ModelSpec {
        ModelSpec {
            name: "files".into(),
            app_name: "nautilus".into(),
            shape: Shape::Files,
            nodes_idle: 900,
            list_rows: 60,
            list_items: 10_000,
            depth_min: 15,
            depth_max: 25,
            ..ModelSpec::default()
        }
    }

    fn depth_of(w: &World, id: NodeId) -> u32 {
        let mut d = 0;
        let mut cur = w.node(id).and_then(|n| n.parent);
        while let Some(p) = cur {
            d += 1;
            cur = w.node(p).and_then(|n| n.parent);
        }
        d
    }

    fn max_depth(w: &World) -> u32 {
        w.nodes.iter().filter(|n| n.alive).map(|n| depth_of(w, n.id)).max().unwrap_or(0)
    }

    #[test]
    fn same_seed_same_tree() {
        let a = World::new(settings(), "s");
        let b = World::new(settings(), "s");
        let pa: Vec<_> = a.nodes.iter().map(|n| (n.path.clone(), n.name.clone(), n.role)).collect();
        let pb: Vec<_> = b.nodes.iter().map(|n| (n.path.clone(), n.name.clone(), n.role)).collect();
        assert_eq!(pa, pb);
        let mut other = settings();
        other.seed = 2;
        let c = World::new(other, "s");
        assert_ne!(a.nodes[5].path, c.nodes[5].path);
    }

    #[test]
    fn settings_model_has_the_scripted_landmarks_and_size() {
        let w = World::new(settings(), "s");
        assert!((780..=860).contains(&w.live_count()), "{}", w.live_count());
        assert_eq!(w.landmarks["sidebar"].len(), 45);
        assert!(w.landmarks["controls"].len() >= 20);
        assert!(!w.landmarks["switches"].is_empty());
        assert!(!w.landmarks["slider"].is_empty());
        assert!(!w.landmarks["derived"].is_empty());
        let d = max_depth(&w);
        assert!((12..=30).contains(&d), "depth {d}");
    }

    #[test]
    fn files_model_has_realised_rows_of_six_nodes() {
        let w = World::new(files(), "f");
        assert_eq!(w.landmarks["rows"].len(), 60);
        let row = w.landmarks["rows"][0];
        assert_eq!(w.descendants(row).len(), 6);
        assert!(w.live_count() >= 880, "{}", w.live_count());
    }

    #[test]
    fn counter_paths_are_monotonic_and_uuid_paths_are_not_small() {
        let mut spec = settings();
        spec.path_style = PathStyle::Counter;
        let w = World::new(spec, "s");
        assert_eq!(w.nodes[1].path, format!("{BASE_PATH}/1"));
        assert_eq!(w.nodes[0].path, ROOT_PATH);
        let u = World::new(settings(), "s");
        assert_eq!(u.nodes[1].path.rsplit('/').next().map(str::len), Some(32));
    }

    #[test]
    fn rename_evented_and_silent_are_logged_with_the_flag() {
        let mut w = World::new(settings(), "s");
        let (r, fx) = w.apply(
            &Cmd::Rename { node: "controls[0]".into(), name: "New".into(), evented: true },
            10,
        );
        assert!(r.ok);
        assert!(matches!(fx.first(), Some(Effect::Prop { kind: PropKind::Name, .. })));
        let (_, fx) = w.apply(
            &Cmd::Rename { node: "controls[1]".into(), name: "Quiet".into(), evented: false },
            20,
        );
        assert!(fx.is_empty());
        let t = w.take_truth();
        assert_eq!(t.len(), 2);
        assert!(t[0].evented && !t[1].evented);
        assert_eq!((t[1].new.as_str(), t[1].t_ns), (Some("Quiet"), 20));
    }

    #[test]
    fn derived_names_follow_their_label_silently() {
        let mut w = World::new(settings(), "s");
        let label = w.landmarks["derived_labels"][0];
        let ctrl = w.landmarks["derived"][0];
        let (_, fx) = w.apply(
            &Cmd::Rename { node: format!("id:{label}"), name: "Renamed".into(), evented: true },
            1,
        );
        assert_eq!(fx.len(), 1, "only the label announces (G-1)");
        assert_eq!(w.node(ctrl).unwrap().name, "Renamed");
        let t = w.take_truth();
        assert_eq!(t.len(), 2);
        assert!(!t[1].evented);
    }

    #[test]
    fn replace_subtree_kills_old_paths_and_creates_new_ones() {
        let mut w = World::new(settings(), "s");
        let old = w.path_of("controls[2]").unwrap();
        assert!(w.is_alive_path(&old));
        let (r, fx) = w.apply(
            &Cmd::ReplaceSubtree { root: "panel".into(), nodes: 300, remove_events: true, add_events: true },
            1,
        );
        assert!(r.ok, "{r:?}");
        assert!(!w.is_alive_path(&old));
        assert!(fx.iter().any(|e| matches!(e, Effect::Children { add: false, .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::Children { add: true, .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::CacheRemove { .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::CacheAdd(_))));
        let new = w.path_of("controls[2]").unwrap();
        assert_ne!(new, old);
    }

    #[test]
    fn replace_subtree_without_events_is_silent() {
        let mut w = World::new(settings(), "s");
        let (_, fx) = w.apply(
            &Cmd::ReplaceSubtree { root: "panel".into(), nodes: 100, remove_events: false, add_events: false },
            1,
        );
        assert!(fx.is_empty());
    }

    #[test]
    fn hidden_nodes_skip_the_cache_under_the_gtk4_quirk() {
        let mut w = World::new(settings(), "s");
        let p = w.path_of("controls[0]").unwrap();
        let n = w.dump(usize::MAX).len();
        w.apply(&Cmd::Hide { node: "controls[0]".into() }, 1);
        assert_eq!(w.dump(usize::MAX).len(), n - 1);
        let (_, fx) = w.apply(
            &Cmd::Destroy { node: "controls[0]".into(), events: DestroyEvents::Full },
            2,
        );
        assert!(!fx.iter().any(|e| matches!(e, Effect::CacheRemove { path } if *path == p)), "G-2");
        let mut spec = settings();
        spec.quirks = Quirks::default();
        let mut w = World::new(spec, "s");
        let n = w.dump(usize::MAX).len();
        w.apply(&Cmd::Hide { node: "controls[0]".into() }, 1);
        assert_eq!(w.dump(usize::MAX).len(), n);
    }

    #[test]
    fn add_during_dump_is_invisible_to_the_cache() {
        let mut w = World::new(settings(), "s");
        let (r, fx) = w.apply(
            &Cmd::Add { parent: "panel".into(), name: "late".into(), during_dump: true, silent: false },
            1,
        );
        assert!(r.ok);
        assert!(fx.iter().any(|e| matches!(e, Effect::Children { add: true, .. })));
        assert!(!fx.iter().any(|e| matches!(e, Effect::CacheAdd(_))), "G-3");
        let path = r.str("path").unwrap().to_owned();
        assert!(w.dump(usize::MAX).iter().all(|i| i.path != path));
        assert!(w.is_alive_path(&path));
    }

    #[test]
    fn scroll_recycles_rows_keeping_their_paths() {
        let mut w = World::new(files(), "f");
        let row = w.path_of("rows[0]").unwrap();
        let before = w.view(&w.path_of("row_labels[0]").unwrap()).unwrap().name;
        let (r, fx) = w.apply(&Cmd::Scroll { steps: 7 }, 1);
        assert!(r.ok);
        assert!(fx.is_empty(), "H4 pessimistic: recycling is silent");
        assert_eq!(w.path_of("rows[0]").unwrap(), row);
        let after = w.view(&w.path_of("row_labels[0]").unwrap()).unwrap().name;
        assert_ne!(before, after);
        assert_eq!(after, item_name(7));
        // The row name follows its label (derived).
        assert_eq!(w.view(&row).unwrap().name, item_name(7));
    }

    #[test]
    fn reorder_changes_the_order_and_keeps_the_paths() {
        let mut w = World::new(settings(), "s");
        let before = w.child_paths(&w.path_of("sidebar_list").unwrap()).unwrap();
        let (r, fx) = w.apply(
            &Cmd::Reorder { parent: "sidebar_list".into(), mode: ReorderMode::RemoveAdd },
            1,
        );
        assert!(r.ok);
        let after = w.child_paths(&w.path_of("sidebar_list").unwrap()).unwrap();
        assert_ne!(before, after);
        let mut a = before.clone();
        let mut b = after.clone();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(fx.iter().filter(|e| matches!(e, Effect::CacheAdd(_))).count(), 45);
    }

    #[test]
    fn lease_expiry_makes_the_path_unknown_without_an_event() {
        let mut w = World::new(settings(), "s");
        let p = w.path_of("controls[1]").unwrap();
        let (_, fx) = w.apply(&Cmd::ExpireLease { node: "controls[1]".into() }, 1);
        assert!(fx.is_empty());
        assert!(w.view(&p).is_none());
    }

    #[test]
    fn switcher_rebuild_mints_new_paths_and_skips_the_cache() {
        let mut spec = ModelSpec {
            shape: Shape::Shell,
            path_style: PathStyle::Counter,
            nodes_idle: 400,
            ..ModelSpec::default()
        };
        spec.quirks.switcher_rebuild = true;
        spec.quirks.spurious_parent_add = false;
        let mut w = World::new(spec, "shell");
        w.apply(&Cmd::RebuildSwitcher { items: 4 }, 1);
        let first = w.path_of("switcher[0]").unwrap();
        let (_, fx) = w.apply(&Cmd::RebuildSwitcher { items: 4 }, 2);
        let second = w.path_of("switcher[0]").unwrap();
        assert_ne!(first, second);
        assert!(!fx.iter().any(|e| matches!(e, Effect::CacheAdd(_))));
        assert!(w.dump(usize::MAX).iter().all(|i| i.path != second));
        let (_, fx) = w.apply(&Cmd::Select { node: "switcher[1]".into() }, 3);
        assert_eq!(fx.len(), 1);
        let (_, fx) = w.apply(&Cmd::Select { node: "switcher[2]".into() }, 4);
        assert_eq!(fx.len(), 2, "off on the old item, on on the new one");
    }

    #[test]
    fn ff_chrome_dump_omits_the_document_and_calls_below_it_are_detected() {
        let spec = ModelSpec {
            shape: Shape::FfChrome,
            nodes_idle: 300,
            ..ModelSpec::default()
        };
        let w = World::new(spec, "ff");
        let doc = w.path_of("document").unwrap();
        let below = w.child_paths(&doc).unwrap();
        assert_eq!(below.len(), 50);
        assert!(w.below_document(&below[0]));
        assert!(!w.below_document(&doc));
        let items = w.dump(usize::MAX);
        assert!(items.iter().all(|i| i.path != doc && !below.contains(&i.path)));
    }

    #[test]
    fn focus_moves_and_is_logged() {
        let mut w = World::new(settings(), "s");
        w.apply(&Cmd::Focus { node: "controls[0]".into() }, 5);
        let (_, fx) = w.apply(&Cmd::Focus { node: "controls[1]".into() }, 6);
        assert_eq!(fx.len(), 3, "unfocus old, focus new, Focus event");
        let t = w.take_truth();
        assert_eq!(t.iter().filter(|l| l.focus).count(), 2);
        assert_eq!(w.focus_path(), w.path_of("controls[1]"));
    }

    #[test]
    fn p9_quirks_never_remove_accessible_and_spurious_parent_add() {
        let mut spec = settings();
        spec.quirks.never_remove_accessible = true;
        spec.quirks.spurious_parent_add = true;
        let mut w = World::new(spec, "s");
        let (_, fx) = w.apply(
            &Cmd::ReplaceSubtree { root: "panel".into(), nodes: 100, remove_events: true, add_events: true },
            1,
        );
        assert!(!fx.iter().any(|e| matches!(e, Effect::CacheRemove { .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::CacheAdd(_))));
        let (_, fx) = w.apply(
            &Cmd::Rename { node: "controls[0]".into(), name: "x".into(), evented: true },
            2,
        );
        assert!(matches!(fx[0], Effect::Prop { .. }));
        assert!(matches!(fx[1], Effect::Children { add: true, .. }), "P9-2: {fx:?}");
    }

    #[test]
    fn dump_realised_only_grows_when_a_client_reads() {
        let mut spec = settings();
        spec.quirks.dump_realised_only = true;
        let mut w = World::new(spec, "s");
        assert_eq!(w.dump(usize::MAX).len(), 2, "a fresh GTK4 app dumps 2 items");
        let root = w.path_of("root").unwrap();
        w.mark_realised(&root, true);
        let win = w.path_of("window").unwrap();
        w.mark_realised(&win, true);
        assert!(w.dump(usize::MAX).len() >= 2);
    }

    #[test]
    fn hidden_ghost_keeps_showing_and_stays_silent() {
        let mut spec = settings();
        spec.quirks.hidden_ghost = true;
        spec.quirks.hidden_skips_cache = false;
        let mut w = World::new(spec, "s");
        let n = w.dump(usize::MAX).len();
        let (_, fx) = w.apply(&Cmd::Hide { node: "controls[0]".into() }, 1);
        assert!(fx.is_empty());
        assert_eq!(w.dump(usize::MAX).len(), n);
        let v = w.view(&w.path_of("controls[0]").unwrap()).unwrap();
        assert!(v.states & (1 << state::SHOWING) != 0);
    }

    #[test]
    fn gtk3_dump_omits_the_managed_list_and_reports_minus_one() {
        let mut spec = ModelSpec {
            shape: Shape::Gtk3,
            nodes_idle: 488,
            depth_min: 10,
            depth_max: 18,
            p2p: P2pMode::Peer,
            ..ModelSpec::default()
        };
        spec.quirks = Quirks { dump_omits_transient: true, ..Quirks::default() };
        let w = World::new(spec, "gedit");
        let live = w.live_count();
        let dump = w.dump(usize::MAX);
        assert!((470..=530).contains(&live), "{live}");
        let frac = dump.len() as f64 / f64::from(live);
        assert!((0.30..=0.45).contains(&frac), "dump {} of {live}", dump.len());
        let list = w.path_of("list").unwrap();
        assert_eq!(dump.iter().find(|i| i.path == list).unwrap().child_count, -1);
    }

    #[test]
    fn stray_references_are_errors_not_panics() {
        let mut w = World::new(settings(), "s");
        for c in [
            Cmd::Rename { node: "nope".into(), name: "x".into(), evented: true },
            Cmd::Focus { node: "controls[9999]".into() },
            Cmd::SetValue { node: "controls[0]".into(), value: 1.0, evented: true },
            Cmd::Scroll { steps: 1 },
        ] {
            let (r, _) = w.apply(&c, 1);
            assert!(!r.ok, "{c:?}");
        }
    }
}
