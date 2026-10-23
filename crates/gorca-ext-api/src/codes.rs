// SPDX-License-Identifier: MIT OR Apache-2.0
//! Name <-> code registries of the semantic model: a **generated copy** of the tables in
//! `gorca-model::sem::registry` (the source of truth). This crate cannot depend on `gorca-model`;
//! the equality test (dev-dependency) and the checked-in append-only snapshot
//! `codes.snapshot` keep the two in step. Codes are never reused or renumbered; a new entry is an
//! append to the model, to these tables and to the snapshot (`tools/gen-sem-codes.py`).

/// One registry row: registry name and wire code.
pub type Row = (&'static str, u32);

/// Fused semantic groups (code = slot).
pub const GROUPS: &[Row] = &[
    ("role", 0),
    ("name", 1),
    ("description", 2),
    ("check", 3),
    ("press", 4),
    ("expand", 5),
    ("select", 6),
    ("current", 7),
    ("invalid", 8),
    ("popup", 9),
    ("avail", 10),
    ("flags", 11),
    ("level", 12),
    ("setpos", 13),
    ("grid", 14),
    ("live", 15),
    ("orientation", 16),
    ("sort", 17),
    ("roledescription", 18),
    ("placeholder", 19),
    ("keyshortcuts", 20),
    ("valuetext", 21),
    ("namefrom", 22),
];

/// Evidence kinds (bit 15 set).
pub const EVIDENCE: &[Row] = &[
    ("hint.icon_class", 32768),
    ("hint.src_name", 32769),
    ("hint.title_attr", 32770),
    ("visual.ocr_text", 32771),
    ("visual.label_guess", 32772),
    ("layout.box", 32773),
    ("layout.css_hidden", 32774),
];

/// Provenance codes (4 bits in a provenance tag; no `Fallback`).
pub const PROV_CODES: &[Row] = &[
    ("Default", 0),
    ("Derived", 1),
    ("DerivedTainted", 2),
    ("Inferred", 3),
    ("ObservedPrimary", 4),
    ("ObservedSecondary", 5),
    ("Patch", 6),
    ("User", 7),
];

/// Confidence classes.
pub const CONF_CLASSES: &[Row] = &[
    ("Asserted", 0),
    ("High", 1),
    ("Probable", 2),
    ("Possible", 3),
];

/// Binding qualities.
pub const BIND_QUALITIES: &[Row] = &[
    ("Unbound", 0),
    ("Geometric", 1),
    ("Heuristic", 2),
    ("Unique", 3),
    ("Exact", 4),
];

/// Source classes.
pub const SOURCE_CLASSES: &[Row] = &[
    ("Primary", 0),
    ("Secondary", 1),
    ("Inferred", 2),
    ("Annotation", 3),
];

/// Trust tiers.
pub const TRUST_TIERS: &[Row] = &[("T0", 0), ("T1", 1), ("T2", 2), ("T3", 3)];

/// Semantic roles.
pub const ROLES: &[Row] = &[
    ("unknown", 0),
    ("generic", 1),
    ("alert", 2),
    ("alertdialog", 3),
    ("application", 4),
    ("article", 5),
    ("banner", 6),
    ("blockquote", 7),
    ("button", 8),
    ("caption", 9),
    ("cell", 10),
    ("checkbox", 11),
    ("code", 12),
    ("columnheader", 13),
    ("combobox", 14),
    ("complementary", 15),
    ("contentinfo", 16),
    ("definition", 17),
    ("deletion", 18),
    ("dialog", 19),
    ("document", 20),
    ("document-web", 21),
    ("feed", 22),
    ("figure", 23),
    ("form", 24),
    ("grid", 25),
    ("gridcell", 26),
    ("group", 27),
    ("heading", 28),
    ("img", 29),
    ("insertion", 30),
    ("link", 31),
    ("list", 32),
    ("listbox", 33),
    ("listitem", 34),
    ("log", 35),
    ("main", 36),
    ("mark", 37),
    ("marquee", 38),
    ("math", 39),
    ("menu", 40),
    ("menubar", 41),
    ("menuitem", 42),
    ("menuitemcheckbox", 43),
    ("menuitemradio", 44),
    ("meter", 45),
    ("navigation", 46),
    ("note", 47),
    ("option", 48),
    ("paragraph", 49),
    ("progressbar", 50),
    ("radio", 51),
    ("radiogroup", 52),
    ("region", 53),
    ("row", 54),
    ("rowgroup", 55),
    ("rowheader", 56),
    ("scrollbar", 57),
    ("search", 58),
    ("searchbox", 59),
    ("separator", 60),
    ("slider", 61),
    ("spinbutton", 62),
    ("status", 63),
    ("switch", 64),
    ("tab", 65),
    ("table", 66),
    ("tablist", 67),
    ("tabpanel", 68),
    ("term", 69),
    ("textbox", 70),
    ("passwordbox", 71),
    ("timer", 72),
    ("toolbar", 73),
    ("tooltip", 74),
    ("tree", 75),
    ("treegrid", 76),
    ("treeitem", 77),
    ("terminal", 78),
    ("window", 79),
    ("popover", 80),
    ("statictext", 81),
    ("comment", 82),
    ("suggestion", 83),
    ("details", 84),
    ("summary", 85),
    ("descriptionlist", 86),
    ("time", 87),
    ("iframe", 88),
    ("emphasis", 89),
    ("strong", 90),
    ("subscript", 91),
    ("superscript", 92),
    ("presentation", 93),
];

/// `aria-checked` states.
pub const CHECK: &[Row] = &[("na", 0), ("off", 1), ("on", 2), ("mixed", 3)];

/// `aria-pressed` states.
pub const PRESS: &[Row] = &[("na", 0), ("off", 1), ("on", 2), ("mixed", 3)];

/// `aria-expanded` states.
pub const EXPAND: &[Row] = &[("na", 0), ("collapsed", 1), ("expanded", 2)];

/// `aria-selected` states.
pub const SELECT: &[Row] = &[("na", 0), ("off", 1), ("on", 2)];

/// `aria-current` values.
pub const CURRENT: &[Row] = &[
    ("no", 0),
    ("true", 1),
    ("page", 2),
    ("step", 3),
    ("location", 4),
    ("date", 5),
    ("time", 6),
];

/// `aria-invalid` values.
pub const INVALID: &[Row] = &[("no", 0), ("true", 1), ("grammar", 2), ("spelling", 3)];

/// `aria-haspopup` values.
pub const POPUP: &[Row] = &[
    ("no", 0),
    ("menu", 1),
    ("listbox", 2),
    ("tree", 3),
    ("grid", 4),
    ("dialog", 5),
];

/// Availability values.
pub const AVAIL: &[Row] = &[("na", 0), ("enabled", 1), ("disabled", 2)];

/// Name sources.
pub const NAME_FROM: &[Row] = &[
    ("none", 0),
    ("content", 1),
    ("author", 2),
    ("placeholder", 3),
];

/// Orientation values.
pub const ORIENTATION: &[Row] = &[("na", 0), ("horizontal", 1), ("vertical", 2)];

/// Sort values.
pub const SORT: &[Row] = &[
    ("none", 0),
    ("ascending", 1),
    ("descending", 2),
    ("other", 3),
];

/// Set-position sources.
pub const POS_SRC: &[Row] = &[("author", 0), ("engine", 1), ("computed", 2)];

/// `SemFlags` bit values (code = bit mask).
pub const SEM_FLAGS: &[Row] = &[
    ("REQUIRED", 1),
    ("READ_ONLY", 2),
    ("BUSY", 4),
    ("MODAL", 8),
    ("MULTISELECTABLE", 16),
    ("MULTILINE", 32),
    ("VISITED", 64),
    ("FOCUSABLE", 128),
    ("EDITABLE", 256),
    ("HAS_DETAILS", 512),
    ("HAS_ERROR_MESSAGE", 1024),
    ("ACTIVE_DESCENDANT_HOST", 2048),
    ("ATOMIC_PRESENTATION", 4096),
    ("LAYOUT_TABLE", 8192),
    ("SELECTION_FOLLOWS_FOCUS", 16384),
    ("SYNTHETIC", 32768),
];

/// `Relevant` bit values.
pub const RELEVANT: &[Row] = &[("ADDITIONS", 1), ("REMOVALS", 2), ("TEXT", 4)];

/// Every registry as `(registry, rows)`, in snapshot order.
pub const REGISTRIES: &[(&str, &[Row])] = &[
    ("groups", GROUPS),
    ("evidence", EVIDENCE),
    ("prov", PROV_CODES),
    ("conf", CONF_CLASSES),
    ("bind", BIND_QUALITIES),
    ("source_class", SOURCE_CLASSES),
    ("tier", TRUST_TIERS),
    ("role", ROLES),
    ("check", CHECK),
    ("press", PRESS),
    ("expand", EXPAND),
    ("select", SELECT),
    ("current", CURRENT),
    ("invalid", INVALID),
    ("popup", POPUP),
    ("avail", AVAIL),
    ("name_from", NAME_FROM),
    ("orientation", ORIENTATION),
    ("sort", SORT),
    ("pos_src", POS_SRC),
    ("sem_flags", SEM_FLAGS),
    ("relevant", RELEVANT),
];
