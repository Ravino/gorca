// SPDX-License-Identifier: MIT OR Apache-2.0
//! Name <-> code registries of the semantic model: a **generated copy** of the tables in
//! `gorca-model::sem::registry` (the source of truth). This crate cannot depend on `gorca-model`;
//! the equality test below (dev-dependency) and the checked-in append-only snapshot
//! `codes.snapshot` keep the two in step. Codes are never reused or renumbered; a new entry is an
//! append to the model, to these tables and to the snapshot.

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
];

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
];
