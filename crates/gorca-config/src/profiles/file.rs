// SPDX-License-Identifier: GPL-3.0-or-later
//! Profile files and rows: the format, PK-1 validation and the key rules (ADR-0016 sections
//! 2.3, 2.5 PK-1, 2.7 and 2.8).
//!
//! Pure text in, [`Profile`] and diagnostics out. A bad key never fails a whole profile (NS-2);
//! a file that does not parse yields no profile (the caller keeps its last good parse, PC-0).

use std::collections::{BTreeMap, BTreeSet};

use toml_edit::{Document, Item, TableLike};

use crate::diag::{Diag, DiagMsg, did_you_mean, fnv1a};
use crate::glob::Glob;
use crate::load::{MAX_FILE_BYTES, parse_value};
use crate::patch::{KeyId, SettingsPatch, Value};
use crate::settings::{KeyScope, SCHEMA, SETTINGS, sections};

use super::naming::{display_name, is_valid_file_id};
use super::trigger::{AppMatcher, Trigger, TriggerKind, parse_trigger};

/// PC-3.
pub const MAX_PROFILE_FILES: usize = 256;
/// PF-6.
pub const MAX_ROWS_PER_EXT: usize = 8;
pub const MAX_KEYS_PER_ROW: usize = 64;
pub const MAX_TRIGGERS_PER_ROW: usize = 16;
/// PC-1: no profile or row may set `speech.volume` below this.
pub const VOLUME_FLOOR: i64 = 20;
/// PF-4: a row pattern needs this many literal characters.
pub const MIN_ROW_LITERALS: usize = 3;

/// The type of a key of an installed extension's schema (PK-1: same type and range).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtType {
    Bool,
    Int { min: i64, max: i64 },
    Str,
    StrList,
    Enum(Vec<String>),
}

/// The keys the installed extensions' schemas define, `ext.<id>.<key>`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtSchema {
    exts: BTreeMap<String, BTreeMap<String, ExtType>>,
}

impl ExtSchema {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an installed extension and its keys (replaces an earlier registration).
    pub fn add(&mut self, ext: &str, keys: impl IntoIterator<Item = (String, ExtType)>) {
        self.exts.insert(ext.to_owned(), keys.into_iter().collect());
    }

    pub fn installed(&self, ext: &str) -> bool {
        self.exts.contains_key(ext)
    }

    pub fn key(&self, ext: &str, key: &str) -> Option<&ExtType> {
        self.exts.get(ext)?.get(key)
    }

    pub fn ext_ids(&self) -> impl Iterator<Item = &str> {
        self.exts.keys().map(String::as_str)
    }

    /// Is the dotted path `ext.<id>.<key>` a key global settings can hold (SG-3)?
    pub fn holds(&self, path: &str) -> bool {
        let Some(rest) = path.strip_prefix("ext.") else {
            return false;
        };
        let Some((id, key)) = rest.split_once('.') else {
            return false;
        };
        self.key(id, key).is_some()
    }
}

/// Where a profile or row comes from; decides its layer (ADR-0016 section 2.1).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ProfileSource {
    /// L6: a file in the user's `profiles/` directory.
    User,
    /// L4: a product row compiled into the binary; on in safe mode and under suspend.
    Product,
    /// L4: a row of a first-party extension of this repository.
    FirstParty { ext: String },
    /// L3: a row of an installed third-party extension; off until the user enables it.
    ThirdParty { ext: String },
}

impl ProfileSource {
    /// Fold order: L3 < L4 < L6.
    pub fn layer_rank(&self) -> u8 {
        match self {
            ProfileSource::ThirdParty { .. } => 0,
            ProfileSource::Product | ProfileSource::FirstParty { .. } => 1,
            ProfileSource::User => 2,
        }
    }

    pub fn is_row(&self) -> bool {
        match self {
            ProfileSource::User => false,
            ProfileSource::Product
            | ProfileSource::FirstParty { .. }
            | ProfileSource::ThirdParty { .. } => true,
        }
    }
}

/// One profile (a user file) or row (product or add-on), validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    /// File stem for user profiles; `gorca/<id>` or `<ext>/<id>` for rows.
    pub id: String,
    pub name: String,
    /// Rows: why the row exists (spoken by `settings.explain_current`).
    pub why: String,
    pub source: ProfileSource,
    /// `enabled` in the file; rows start from their layer's default (PF-5).
    pub enabled: bool,
    /// Never triggered, reachable only through a pin.
    pub manual: bool,
    pub triggers: Vec<Trigger>,
    /// Core keys.
    pub values: SettingsPatch,
    /// Keys `ext.<id>.<key>` of installed extensions, by dotted path.
    pub ext: BTreeMap<String, Value>,
    /// Dotted paths of keys of extensions that are not installed: kept in the file, not applied.
    pub inactive: Vec<String>,
    /// FNV-1a of the file text (0 for rows).
    pub hash: u64,
}

impl Profile {
    /// An empty profile with the given identity (the base of auto-created app profiles).
    pub fn new(id: &str, name: &str, source: ProfileSource, triggers: Vec<Trigger>) -> Self {
        Profile {
            id: id.to_owned(),
            name: name.to_owned(),
            why: String::new(),
            source,
            enabled: true,
            manual: false,
            triggers,
            values: SettingsPatch::new(),
            ext: BTreeMap::new(),
            inactive: Vec::new(),
            hash: 0,
        }
    }

    /// Does this profile hold the key (core or extension path)?
    pub fn holds(&self, key: &KeyRef) -> bool {
        match key {
            KeyRef::Core(k) => self.values.contains(*k),
            KeyRef::Ext(p) => self.ext.contains_key(p),
        }
    }

    /// Does the profile have a trigger of `kind`?
    pub fn has_trigger(&self, kind: TriggerKind) -> bool {
        self.triggers.iter().any(|t| t.kind() == kind)
    }

    pub fn key_count(&self) -> usize {
        self.values.len() + self.ext.len()
    }
}

/// A key of the global schema: a core row or `ext.<id>.<key>`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum KeyRef {
    Core(KeyId),
    Ext(String),
}

impl KeyRef {
    /// The dotted path (`speech.rate`, `ext.foo.bar`).
    pub fn path(&self) -> String {
        match self {
            KeyRef::Core(k) => k.def().path(),
            KeyRef::Ext(p) => p.clone(),
        }
    }
}

/// ADR-0016 NS-1: may a trigger of `kind` set a key of `scope`?
pub fn matrix_allows(kind: TriggerKind, scope: KeyScope) -> bool {
    match (kind, scope) {
        (_, KeyScope::Base)
        | (TriggerKind::Site, KeyScope::App)
        | (TriggerKind::Window | TriggerKind::Context, KeyScope::App | KeyScope::Doc) => false,
        (TriggerKind::App, KeyScope::App | KeyScope::Doc | KeyScope::Focus)
        | (TriggerKind::Site, KeyScope::Doc | KeyScope::Focus)
        | (TriggerKind::Window | TriggerKind::Context, KeyScope::Focus) => true,
    }
}

/// Who writes the keys: decides PF-1 and the extension namespace rule.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Origin<'a> {
    User,
    /// A product row: vendor keys only, any `ext.*` key of an installed extension.
    Product,
    /// A row of extension `ext`: vendor keys, and `ext.<ext>.*` only.
    ExtRow {
        ext: &'a str,
    },
}

/// Why `triggers` may not set core key `key`, or `Ok`.
pub fn admit_core(
    key: KeyId,
    triggers: &[Trigger],
    origin: Origin<'_>,
) -> Result<(), &'static str> {
    let d = key.def();
    match d.scope {
        KeyScope::Base => return Err("a Base key belongs to the global settings only"),
        KeyScope::App | KeyScope::Doc | KeyScope::Focus => {}
    }
    match origin {
        Origin::User => {}
        Origin::Product | Origin::ExtRow { .. } => {
            if d.privacy {
                return Err("a privacy key cannot be set by a row");
            }
            if !d.vendor {
                return Err("a row cannot set this key (voice, rate and volume stay the user's)");
            }
        }
    }
    for t in triggers {
        if !matrix_allows(t.kind(), d.scope) {
            return Err(match t.kind() {
                TriggerKind::Context => "a context trigger may set only Focus keys",
                TriggerKind::Window => "a window trigger may set only Focus keys",
                TriggerKind::Site => "a site trigger may set only Doc and Focus keys",
                TriggerKind::App => "key not allowed under an app trigger",
            });
        }
    }
    Ok(())
}

/// Why `triggers` may not set extension key `path` (`ext.<id>.<key>`), or `Ok`. Extension keys
/// are `Focus` keys; NS-3 forbids them under `Context{BrowseMode|FocusMode}`.
pub fn admit_ext(
    triggers: &[Trigger],
    origin: Origin<'_>,
    ext_id: &str,
) -> Result<(), &'static str> {
    if let Origin::ExtRow { ext } = origin
        && ext != ext_id
    {
        return Err("a row may set only the keys of its own extension");
    }
    for t in triggers {
        if let Trigger::Context { ctx } = t
            && ctx.is_mode()
        {
            return Err("extension keys cannot be set under a browse or focus mode context");
        }
        if !matrix_allows(t.kind(), KeyScope::Focus) {
            return Err("key not allowed under this trigger");
        }
    }
    Ok(())
}

fn span(item: &Item) -> Option<(u32, u32)> {
    item.span().map(|r| {
        (
            u32::try_from(r.start).unwrap_or(u32::MAX),
            u32::try_from(r.end).unwrap_or(u32::MAX),
        )
    })
}

fn parse_ext_value(ty: &ExtType, v: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Value> {
    let wrong = |expected: &'static str, diags: &mut Vec<Diag>| {
        diags.push(Diag::error(path, span(v), DiagMsg::WrongType { expected }));
    };
    match ty {
        ExtType::Bool => {
            let b = v.as_bool();
            if b.is_none() {
                wrong("true or false", diags);
            }
            b.map(Value::Bool)
        }
        ExtType::Int { min, max } => {
            let Some(n) = v.as_integer() else {
                wrong("a whole number", diags);
                return None;
            };
            let used = n.clamp(*min, *max);
            if used != n {
                diags.push(Diag::error(
                    path,
                    span(v),
                    DiagMsg::OutOfRange {
                        value: n,
                        min: *min,
                        max: *max,
                        used,
                    },
                ));
            }
            Some(Value::Int(used))
        }
        ExtType::Str => {
            let s = v.as_str();
            if s.is_none() {
                wrong("a string", diags);
            }
            s.map(|x| Value::Str(x.to_owned()))
        }
        ExtType::StrList => {
            let Some(arr) = v.as_array() else {
                wrong("a list of strings", diags);
                return None;
            };
            let mut out = Vec::new();
            for e in arr {
                if let Some(s) = e.as_str() {
                    out.push(s.to_owned());
                } else {
                    wrong("a list of strings", diags);
                    return None;
                }
            }
            Some(Value::StrList(out))
        }
        ExtType::Enum(names) => {
            let Some(s) = v.as_str() else {
                wrong("a string", diags);
                return None;
            };
            if names.iter().any(|n| n == s) {
                Some(Value::Str(s.to_owned()))
            } else {
                diags.push(Diag::error(
                    path,
                    span(v),
                    DiagMsg::UnknownEnum {
                        value: s.to_owned(),
                        allowed: "a value of the extension's schema",
                    },
                ));
                None
            }
        }
    }
}

struct ReadCtx<'a> {
    triggers: &'a [Trigger],
    origin: Origin<'a>,
    ext: &'a ExtSchema,
}

#[derive(Default)]
struct Values {
    core: SettingsPatch,
    ext: BTreeMap<String, Value>,
    inactive: Vec<String>,
}

impl Values {
    fn count(&self) -> usize {
        self.core.len() + self.ext.len()
    }
}

fn read_ext_section(item: &Item, cx: &ReadCtx<'_>, out: &mut Values, diags: &mut Vec<Diag>) {
    let Some(t) = item.as_table_like() else {
        diags.push(Diag::error(
            "ext",
            span(item),
            DiagMsg::WrongType {
                expected: "a table",
            },
        ));
        return;
    };
    let mut warned: BTreeSet<String> = BTreeSet::new();
    for (id, body) in t.iter() {
        let Some(keys) = body.as_table_like() else {
            diags.push(Diag::error(
                format!("ext.{id}"),
                span(body),
                DiagMsg::WrongType {
                    expected: "a table",
                },
            ));
            continue;
        };
        for (k, v) in keys.iter() {
            let path = format!("ext.{id}.{k}");
            if !cx.ext.installed(id) {
                // PK-1: kept in the file, inactive; one diagnostic per extension.
                out.inactive.push(path);
                if warned.insert(id.to_owned()) {
                    diags.push(Diag::warning(
                        format!("ext.{id}"),
                        span(body),
                        DiagMsg::InactiveExtension { ext: id.to_owned() },
                    ));
                }
                continue;
            }
            let Some(ty) = cx.ext.key(id, k) else {
                let known: Vec<&str> = cx
                    .ext
                    .exts
                    .get(id)
                    .map(|m| m.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                diags.push(Diag::warning(
                    path,
                    span(v),
                    DiagMsg::UnknownKey {
                        suggestion: did_you_mean(k, known),
                    },
                ));
                continue;
            };
            if let Err(why) = admit_ext(cx.triggers, cx.origin, id) {
                diags.push(Diag::error(path, span(v), DiagMsg::ProfileKey { why }));
                continue;
            }
            if let Some(val) = parse_ext_value(ty, v, &path, diags) {
                out.ext.insert(path, val);
            }
        }
    }
}

fn read_values(
    table: &dyn TableLike,
    cx: &ReadCtx<'_>,
    skip: &[&str],
    diags: &mut Vec<Diag>,
) -> Values {
    let mut out = Values::default();
    for (section, item) in table.iter() {
        if skip.contains(&section) {
            continue;
        }
        if section == "ext" {
            read_ext_section(item, cx, &mut out, diags);
            continue;
        }
        if !sections().any(|s| s == section) {
            diags.push(Diag::warning(
                section,
                span(item),
                DiagMsg::UnknownKey {
                    suggestion: did_you_mean(section, sections().chain(["ext"])),
                },
            ));
            continue;
        }
        let Some(t) = item.as_table_like() else {
            diags.push(Diag::error(
                section,
                span(item),
                DiagMsg::WrongType {
                    expected: "a table",
                },
            ));
            continue;
        };
        for (key, v) in t.iter() {
            let path = format!("{section}.{key}");
            let Some(id) = KeyId::of(section, key) else {
                diags.push(Diag::warning(
                    path,
                    span(v),
                    DiagMsg::UnknownKey {
                        suggestion: did_you_mean(
                            key,
                            SETTINGS
                                .iter()
                                .filter(|d| d.section == section)
                                .map(|d| d.key),
                        ),
                    },
                ));
                continue;
            };
            if let Err(why) = admit_core(id, cx.triggers, cx.origin) {
                diags.push(Diag::error(path, span(v), DiagMsg::ProfileKey { why }));
                continue;
            }
            let Some(val) = parse_value(id, v, &path, diags) else {
                continue;
            };
            if path == "speech.volume"
                && let Value::Int(n) = val
                && n < VOLUME_FLOOR
            {
                diags.push(Diag::error(
                    path,
                    span(v),
                    DiagMsg::VolumeFloor {
                        value: n,
                        floor: VOLUME_FLOOR,
                    },
                ));
                continue;
            }
            let _ = out.core.set(id, val);
        }
    }
    out
}

/// Result of parsing one profile file.
#[derive(Debug)]
pub struct ParsedProfile {
    /// `None` when the file is unreadable (too large or not TOML): keep the last good one.
    pub profile: Option<Profile>,
    pub diags: Vec<Diag>,
    /// The file could not be parsed at all (PC-0 applies).
    pub failed: bool,
}

fn syntax_failure(diags: Vec<Diag>) -> ParsedProfile {
    ParsedProfile {
        profile: None,
        diags,
        failed: true,
    }
}

/// Parse a user profile file `<id>.toml` (ADR-0016 section 2.8). `id` is the file stem.
pub fn parse_user_profile(id: &str, text: &str, ext: &ExtSchema) -> ParsedProfile {
    let mut diags = Vec::new();
    if text.len() > MAX_FILE_BYTES {
        diags.push(Diag::error(
            id,
            None,
            DiagMsg::TooLarge {
                limit: MAX_FILE_BYTES,
            },
        ));
        return syntax_failure(diags);
    }
    if !is_valid_file_id(id) {
        diags.push(Diag::error(
            id,
            None,
            DiagMsg::ProfileInvalid {
                what: "profile id",
                why: "file names are [a-z0-9._-], at most 100 characters".to_owned(),
            },
        ));
        return syntax_failure(diags);
    }
    let doc = match Document::parse(text) {
        Ok(d) => d,
        Err(e) => {
            let sp = e.span().map(|r| {
                (
                    u32::try_from(r.start).unwrap_or(u32::MAX),
                    u32::try_from(r.end).unwrap_or(u32::MAX),
                )
            });
            diags.push(Diag::error("", sp, DiagMsg::Syntax(e.message().to_owned())));
            return syntax_failure(diags);
        }
    };
    let root = doc.as_table();
    if let Some(n) = root.get("schema").and_then(Item::as_integer)
        && n > i64::from(SCHEMA)
    {
        diags.push(Diag::warning(
            "schema",
            None,
            DiagMsg::NewerSchema {
                found: n,
                supported: SCHEMA,
            },
        ));
    }
    let mut profile = Profile::new(id, id, ProfileSource::User, Vec::new());
    profile.hash = fnv1a(text.as_bytes());
    if let Some(head) = root.get("profile") {
        read_header(head, "profile", &mut profile, &mut diags);
    }
    let cx = ReadCtx {
        triggers: &profile.triggers,
        origin: Origin::User,
        ext,
    };
    let v = read_values(root, &cx, &["schema", "profile"], &mut diags);
    profile.values = v.core;
    profile.ext = v.ext;
    profile.inactive = v.inactive;
    if profile.triggers.is_empty() && !profile.manual {
        diags.push(Diag::warning(
            "profile",
            None,
            DiagMsg::ProfileInvalid {
                what: "profile",
                why: "no trigger and not manual: it never applies".to_owned(),
            },
        ));
    }
    ParsedProfile {
        profile: Some(profile),
        diags,
        failed: false,
    }
}

fn read_header(head: &Item, path: &str, p: &mut Profile, diags: &mut Vec<Diag>) {
    let Some(t) = head.as_table_like() else {
        diags.push(Diag::error(
            path,
            span(head),
            DiagMsg::WrongType {
                expected: "a table",
            },
        ));
        return;
    };
    for (k, v) in t.iter() {
        let kp = format!("{path}.{k}");
        match k {
            "name" => match v.as_str() {
                Some(s) => {
                    let n = display_name(s);
                    if !n.is_empty() {
                        p.name = n;
                    }
                }
                None => diags.push(Diag::error(
                    kp,
                    span(v),
                    DiagMsg::WrongType {
                        expected: "a string",
                    },
                )),
            },
            "why" => {
                if let Some(s) = v.as_str() {
                    p.why = display_name(s);
                }
            }
            "enabled" => match v.as_bool() {
                Some(b) => p.enabled = b,
                None => diags.push(Diag::error(
                    kp,
                    span(v),
                    DiagMsg::WrongType {
                        expected: "true or false",
                    },
                )),
            },
            "manual" => match v.as_bool() {
                Some(b) => p.manual = b,
                None => diags.push(Diag::error(
                    kp,
                    span(v),
                    DiagMsg::WrongType {
                        expected: "true or false",
                    },
                )),
            },
            "trigger" => read_triggers(v, &kp, &mut p.triggers, diags),
            _ => diags.push(Diag::warning(
                kp,
                span(v),
                DiagMsg::UnknownKey {
                    suggestion: did_you_mean(k, ["name", "enabled", "manual", "trigger"]),
                },
            )),
        }
    }
}

fn read_triggers(v: &Item, path: &str, out: &mut Vec<Trigger>, diags: &mut Vec<Diag>) {
    let mut items: Vec<&Item> = Vec::new();
    if let Some(aot) = v.as_array_of_tables() {
        items.extend(aot.iter().map(|_| v));
        // `[[profile.trigger]]`: one table per entry.
        items.clear();
        for (i, t) in aot.iter().enumerate() {
            let _ = i;
            let item = Item::Table(t.clone());
            if out.len() >= MAX_TRIGGERS_PER_ROW {
                diags.push(Diag::error(
                    path,
                    span(v),
                    DiagMsg::ProfileLimit {
                        what: "triggers",
                        limit: MAX_TRIGGERS_PER_ROW,
                    },
                ));
                return;
            }
            if let Some(t) = parse_trigger(&item, &format!("{path}[{i}]"), diags) {
                out.push(t);
            }
        }
        return;
    }
    if let Some(arr) = v.as_array() {
        for (i, e) in arr.iter().enumerate() {
            if out.len() >= MAX_TRIGGERS_PER_ROW {
                diags.push(Diag::error(
                    path,
                    span(v),
                    DiagMsg::ProfileLimit {
                        what: "triggers",
                        limit: MAX_TRIGGERS_PER_ROW,
                    },
                ));
                return;
            }
            let item = Item::Value(e.clone());
            if let Some(t) = parse_trigger(&item, &format!("{path}[{i}]"), diags) {
                out.push(t);
            }
        }
        return;
    }
    if let Some(t) = parse_trigger(v, path, diags) {
        out.push(t);
    }
}

/// Where a set of rows comes from, with the manifest's `app_name` patterns for add-ons.
#[derive(Copy, Clone, Debug)]
pub enum RowOrigin<'a> {
    Product,
    FirstParty { ext: &'a str, app_names: &'a [Glob] },
    ThirdParty { ext: &'a str, app_names: &'a [Glob] },
}

/// Result of parsing a row file.
#[derive(Debug, Default)]
pub struct ParsedRows {
    pub rows: Vec<Profile>,
    pub diags: Vec<Diag>,
}

/// PF-4: a literal covered by one manifest pattern, or a wildcard pattern written identically.
fn covered(app_names: &[Glob], g: &Glob) -> bool {
    match g.literal() {
        Some(l) => app_names.iter().any(|m| m.matches(&l)),
        None => app_names
            .iter()
            .any(|m| m.as_str().eq_ignore_ascii_case(g.as_str())),
    }
}

/// PF-4 for add-on rows: every trigger names applications, patterns are at least
/// [`MIN_ROW_LITERALS`] literal characters, and (L3, L4) the `name` patterns are covered by the
/// manifest's `app_name` list. Returns the reason on failure.
fn confine(triggers: &[Trigger], app_names: &[Glob]) -> Result<(), String> {
    if triggers.is_empty() {
        return Err("a row needs at least one trigger".to_owned());
    }
    let check = |m: &AppMatcher| -> Result<(), String> {
        if m.name.is_empty() {
            return Err("an add-on row trigger must name applications (name patterns)".to_owned());
        }
        for g in m.globs() {
            if g.literal_chars() < MIN_ROW_LITERALS {
                return Err(format!(
                    "pattern `{}` has fewer than {MIN_ROW_LITERALS} literal characters",
                    g.as_str()
                ));
            }
        }
        for g in &m.name {
            if !covered(app_names, g) {
                return Err(format!(
                    "pattern `{}` is not covered by the extension's app_name list",
                    g.as_str()
                ));
            }
        }
        Ok(())
    };
    for t in triggers {
        match t.app() {
            Some(m) => check(m)?,
            None => return Err("a context-only row is invalid for add-ons".to_owned()),
        }
    }
    Ok(())
}

fn valid_row_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Parse `[[profile]]` rows (ADR-0016 section 2.7) from a product file or an add-on manifest.
pub fn parse_rows(text: &str, origin: RowOrigin<'_>, ext: &ExtSchema) -> ParsedRows {
    let mut out = ParsedRows::default();
    let diags = &mut out.diags;
    if text.len() > MAX_FILE_BYTES {
        diags.push(Diag::error(
            "",
            None,
            DiagMsg::TooLarge {
                limit: MAX_FILE_BYTES,
            },
        ));
        return out;
    }
    let doc = match Document::parse(text) {
        Ok(d) => d,
        Err(e) => {
            diags.push(Diag::error(
                "",
                None,
                DiagMsg::Syntax(e.message().to_owned()),
            ));
            return out;
        }
    };
    let (prefix, source, key_origin, app_names): (
        &str,
        ProfileSource,
        Origin<'_>,
        Option<&[Glob]>,
    ) = match origin {
        RowOrigin::Product => ("gorca", ProfileSource::Product, Origin::Product, None),
        RowOrigin::FirstParty { ext, app_names } => (
            ext,
            ProfileSource::FirstParty {
                ext: ext.to_owned(),
            },
            Origin::ExtRow { ext },
            Some(app_names),
        ),
        RowOrigin::ThirdParty { ext, app_names } => (
            ext,
            ProfileSource::ThirdParty {
                ext: ext.to_owned(),
            },
            Origin::ExtRow { ext },
            Some(app_names),
        ),
    };
    if let Some(names) = app_names
        && names.is_empty()
    {
        diags.push(Diag::error(
            "profile",
            None,
            DiagMsg::ProfileInvalid {
                what: "rows",
                why: "an extension with no app_name list has no rows".to_owned(),
            },
        ));
        return out;
    }
    let Some(aot) = doc
        .as_table()
        .get("profile")
        .and_then(Item::as_array_of_tables)
    else {
        if doc.as_table().get("profile").is_some() {
            diags.push(Diag::error(
                "profile",
                None,
                DiagMsg::WrongType {
                    expected: "a list of [[profile]] tables",
                },
            ));
        }
        return out;
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, row) in aot.iter().enumerate() {
        let path = format!("profile[{i}]");
        if out.rows.len() >= MAX_ROWS_PER_EXT && app_names.is_some() {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileLimit {
                    what: "rows per extension",
                    limit: MAX_ROWS_PER_EXT,
                },
            ));
            break;
        }
        let Some(rid) = row
            .get("id")
            .and_then(Item::as_str)
            .filter(|s| valid_row_id(s))
        else {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid {
                    what: "row",
                    why: "a row needs a stable id of [a-z0-9._-]".to_owned(),
                },
            ));
            continue;
        };
        let full_id = format!("{prefix}/{rid}");
        if !seen.insert(full_id.clone()) {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid {
                    what: "row",
                    why: format!("duplicate row id `{full_id}`"),
                },
            ));
            continue;
        }
        let mut p = Profile::new(
            &full_id,
            &display_name(row.get("name").and_then(Item::as_str).unwrap_or(rid)),
            source.clone(),
            Vec::new(),
        );
        p.why = display_name(row.get("why").and_then(Item::as_str).unwrap_or(""));
        if let Some(b) = row.get("enabled").and_then(Item::as_bool) {
            p.enabled = b;
        }
        let Some(tr) = row.get("triggers") else {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid {
                    what: "row",
                    why: "a row needs triggers".to_owned(),
                },
            ));
            continue;
        };
        let mut trig = Vec::new();
        let before = diags.len();
        read_triggers(tr, &format!("{path}.triggers"), &mut trig, diags);
        if diags.len() > before {
            // A malformed trigger drops the row: a row with fewer triggers than written would
            // apply more widely than its author meant.
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid {
                    what: "row",
                    why: format!("`{full_id}` dropped: its triggers are invalid"),
                },
            ));
            continue;
        }
        if let Some(names) = app_names
            && let Err(why) = confine(&trig, names)
        {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid { what: "row", why },
            ));
            continue;
        }
        p.triggers = trig;
        let Some(vals) = row.get("values").and_then(Item::as_table_like) else {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileInvalid {
                    what: "row",
                    why: "a row needs a [profile.values] table".to_owned(),
                },
            ));
            continue;
        };
        let cx = ReadCtx {
            triggers: &p.triggers,
            origin: key_origin,
            ext,
        };
        let v = read_values(vals, &cx, &[], diags);
        if v.count() > MAX_KEYS_PER_ROW {
            diags.push(Diag::error(
                &path,
                None,
                DiagMsg::ProfileLimit {
                    what: "keys in one row",
                    limit: MAX_KEYS_PER_ROW,
                },
            ));
            continue;
        }
        p.values = v.core;
        p.ext = v.ext;
        p.inactive = v.inactive;
        out.rows.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn user(text: &str) -> ParsedProfile {
        parse_user_profile("t", text, &ExtSchema::new())
    }

    fn ext_schema() -> ExtSchema {
        let mut e = ExtSchema::new();
        e.add(
            "ext-foo",
            [
                ("level".to_owned(), ExtType::Int { min: 0, max: 10 }),
                (
                    "mode".to_owned(),
                    ExtType::Enum(vec!["a".into(), "b".into()]),
                ),
                ("on".to_owned(), ExtType::Bool),
            ],
        );
        e
    }

    const APP: &str =
        "[profile]\nname = \"Firefox\"\n[[profile.trigger]]\napp = { name = [\"firefox*\"] }\n";

    #[test]
    fn a_user_profile_parses_with_header_triggers_and_values() {
        let text = format!(
            "schema = 1\n{APP}[speech]\npunctuation = \"most\"\nrate = 70\n[browse]\nsay_all_on_load = false\n"
        );
        let r = user(&text);
        assert!(r.diags.is_empty(), "{:?}", r.diags);
        let p = r.profile.unwrap();
        assert_eq!(p.name, "Firefox");
        assert!(p.enabled && !p.manual);
        assert_eq!(p.triggers.len(), 1);
        assert_eq!(p.values.len(), 3);
        assert_eq!(p.hash, fnv1a(text.as_bytes()));
    }

    #[test]
    fn pk1_unknown_keys_sections_and_bad_values_are_rejected_not_fatal() {
        let r = user(&format!(
            "{APP}[speech]\nrte = 5\nrate = \"x\"\npitch = 60\n[nope]\nx = 1\n[speach]\nrate = 1\n"
        ));
        let p = r.profile.unwrap();
        assert_eq!(p.values.len(), 1, "only pitch survives");
        let unknown: Vec<_> = r
            .diags
            .iter()
            .filter_map(|d| match &d.msg {
                DiagMsg::UnknownKey { suggestion } => Some((d.path.clone(), suggestion.clone())),
                _ => None,
            })
            .collect();
        assert!(
            unknown.contains(&("speech.rte".to_owned(), Some("rate".to_owned()))),
            "{unknown:?}"
        );
        assert!(
            unknown
                .iter()
                .any(|(p, s)| p == "speach" && s.as_deref() == Some("speech"))
        );
        assert!(
            r.diags
                .iter()
                .any(|d| matches!(d.msg, DiagMsg::WrongType { .. }))
        );
    }

    #[test]
    fn base_keys_never_enter_a_profile() {
        let r = user(&format!(
            "{APP}[keyboard]\npreset = \"orca\"\n[speech]\nbackend = \"ssip\"\n[profiles]\nrows = {{}}\n"
        ));
        let p = r.profile.unwrap();
        assert!(p.values.is_empty());
        assert_eq!(
            r.diags
                .iter()
                .filter(|d| matches!(d.msg, DiagMsg::ProfileKey { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn ns1_matrix_is_exhaustive() {
        use KeyScope::{App, Base, Doc, Focus};
        use TriggerKind as T;
        // trigger kind x scope, via real keys of every scope.
        let keys = [
            ("speech", "rate", KeyScope::Focus),
            ("browse", "say_all_on_load", KeyScope::Doc),
            ("terminal", "settle_ms", KeyScope::App),
            ("keyboard", "preset", KeyScope::Base),
        ];
        let triggers = [
            ("{ app = { name = [\"abc\"] } }", TriggerKind::App),
            ("{ window = { title = \"x\" } }", TriggerKind::Window),
            ("{ context = \"terminal\" }", TriggerKind::Context),
            ("{ site = { host = \"x\" } }", TriggerKind::Site),
        ];
        for (ttext, kind) in triggers {
            for (s, k, scope) in keys {
                let text = format!(
                    "[profile]\ntrigger = [{ttext}]\n[{s}]\n{k} = {}\n",
                    match (s, k) {
                        ("speech", _) => "40",
                        ("browse", _) => "false",
                        ("terminal", _) => "50",
                        _ => "\"orca\"",
                    }
                );
                let r = user(&text);
                let applied = r.profile.is_some_and(|p| !p.values.is_empty());
                assert_eq!(applied, matrix_allows(kind, scope), "{kind:?} x {scope:?}");
            }
        }
        // The table itself.
        for (kind, scope, want) in [
            (T::App, App, true),
            (T::App, Doc, true),
            (T::App, Focus, true),
            (T::App, Base, false),
            (T::Site, App, false),
            (T::Site, Doc, true),
            (T::Site, Focus, true),
            (T::Site, Base, false),
            (T::Window, App, false),
            (T::Window, Doc, false),
            (T::Window, Focus, true),
            (T::Window, Base, false),
            (T::Context, App, false),
            (T::Context, Doc, false),
            (T::Context, Focus, true),
            (T::Context, Base, false),
        ] {
            assert_eq!(matrix_allows(kind, scope), want, "{kind:?} x {scope:?}");
        }
    }

    #[test]
    fn a_profile_with_several_triggers_is_held_to_the_strictest() {
        let r = user(
            "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }, { context = \"terminal\" }]\n[terminal]\nsettle_ms = 50\n[speech]\npitch = 1\n",
        );
        let p = r.profile.unwrap();
        assert_eq!(
            p.values.len(),
            1,
            "settle_ms needs every trigger to allow App keys"
        );
    }

    #[test]
    fn extension_keys_inactive_and_active() {
        let text = format!(
            "{APP}[ext.ext-foo]\nlevel = 3\nmode = \"b\"\nlevl = 1\n[ext.ext-gone]\nx = 1\ny = 2\n"
        );
        let r = parse_user_profile("t", &text, &ext_schema());
        let p = r.profile.unwrap();
        assert_eq!(p.ext.get("ext.ext-foo.level"), Some(&Value::Int(3)));
        assert_eq!(p.ext.get("ext.ext-foo.mode"), Some(&Value::Str("b".into())));
        assert_eq!(p.inactive, ["ext.ext-gone.x", "ext.ext-gone.y"]);
        // One diagnostic per uninstalled extension; the typo gets a suggestion.
        assert_eq!(
            r.diags
                .iter()
                .filter(|d| matches!(d.msg, DiagMsg::InactiveExtension { .. }))
                .count(),
            1
        );
        assert!(r.diags.iter().any(
            |d| matches!(&d.msg, DiagMsg::UnknownKey { suggestion: Some(s) } if s == "level")
        ));
        // Same file, extension installed later: the keys are active again.
        let mut e2 = ext_schema();
        e2.add(
            "ext-gone",
            [
                ("x".to_owned(), ExtType::Int { min: 0, max: 9 }),
                ("y".to_owned(), ExtType::Int { min: 0, max: 9 }),
            ],
        );
        let p2 = parse_user_profile("t", &text, &e2).profile.unwrap();
        assert!(p2.inactive.is_empty());
        assert_eq!(p2.ext.get("ext.ext-gone.y"), Some(&Value::Int(2)));
        // Wrong type or range for an installed key.
        let r = parse_user_profile(
            "t",
            &format!("{APP}[ext.ext-foo]\nlevel = 99\nmode = \"z\"\non = 3\n"),
            &ext_schema(),
        );
        let p = r.profile.unwrap();
        assert_eq!(
            p.ext.get("ext.ext-foo.level"),
            Some(&Value::Int(10)),
            "clamped like the global file"
        );
        assert!(!p.ext.contains_key("ext.ext-foo.mode"));
        assert!(!p.ext.contains_key("ext.ext-foo.on"));
    }

    #[test]
    fn ns3_extension_keys_are_rejected_under_mode_contexts() {
        for (ctx, ok) in [
            ("browse-mode", false),
            ("focus-mode", false),
            ("terminal", true),
            ("document", true),
        ] {
            let text = format!(
                "[profile]\ntrigger = [{{ context = \"{ctx}\" }}]\n[ext.ext-foo]\nlevel = 1\n"
            );
            let p = parse_user_profile("t", &text, &ext_schema())
                .profile
                .unwrap();
            assert_eq!(!p.ext.is_empty(), ok, "{ctx}");
        }
    }

    #[test]
    fn pc1_volume_floor() {
        let r = user(&format!("{APP}[speech]\nvolume = 10\npitch = 5\n"));
        let p = r.profile.unwrap();
        assert_eq!(p.values.len(), 1);
        assert!(r.diags.iter().any(|d| matches!(
            d.msg,
            DiagMsg::VolumeFloor {
                value: 10,
                floor: 20
            }
        )));
        let p = user(&format!("{APP}[speech]\nvolume = 20\n"))
            .profile
            .unwrap();
        assert_eq!(p.values.len(), 1);
    }

    #[test]
    fn pc0_unparsable_and_oversized_files_yield_no_profile() {
        let r = user("[speech\n");
        assert!(r.profile.is_none() && r.failed);
        let big = "#".repeat(MAX_FILE_BYTES + 1);
        let r = user(&big);
        assert!(r.profile.is_none() && r.failed);
        assert!(matches!(r.diags[0].msg, DiagMsg::TooLarge { .. }));
        let r = parse_user_profile("../x", "", &ExtSchema::new());
        assert!(r.profile.is_none());
    }

    #[test]
    fn manual_and_trigger_less_profiles() {
        let p = user("[profile]\nmanual = true\n[speech]\nrate = 30\n");
        assert!(p.diags.is_empty(), "{:?}", p.diags);
        assert!(p.profile.unwrap().manual);
        let p = user("[speech]\nrate = 30\n");
        assert!(
            p.diags
                .iter()
                .any(|d| matches!(d.msg, DiagMsg::ProfileInvalid { .. }))
        );
    }

    #[test]
    fn newer_schema_warns_and_still_reads() {
        let r = user(&format!("schema = 9\n{APP}[speech]\nrate = 30\n"));
        assert!(
            r.diags
                .iter()
                .any(|d| matches!(d.msg, DiagMsg::NewerSchema { .. }))
        );
        assert_eq!(r.profile.unwrap().values.len(), 1);
    }

    const ROWS: &str = r#"
schema = 1
[[profile]]
id = "vscode"
name = "Visual Studio Code"
why = "editor built from text widgets"
triggers = [{ app = { name = ["code", "visual studio code", "code - oss"] } }]
[profile.values]
browse.say_all_on_load = false
"#;

    #[test]
    fn product_rows_parse_with_dotted_values() {
        let r = parse_rows(ROWS, RowOrigin::Product, &ExtSchema::new());
        assert!(r.diags.is_empty(), "{:?}", r.diags);
        assert_eq!(r.rows.len(), 1);
        let row = &r.rows[0];
        assert_eq!(row.id, "gorca/vscode");
        assert_eq!(row.source, ProfileSource::Product);
        assert_eq!(row.values.len(), 1);
        assert_eq!(row.why, "editor built from text widgets");
    }

    #[test]
    fn pf1_rows_cannot_set_privacy_or_voice_keys() {
        let text = r#"
[[profile]]
id = "x"
triggers = [{ app = { name = ["firefox"] } }]
[profile.values]
echo.typed_words = true
speech.rate = 90
speech.volume = 90
editing.announce_clipboard = false
terminal.secret_regex = "x"
speech.punctuation = "all"
editing.word = "coarse"
"#;
        let r = parse_rows(text, RowOrigin::Product, &ExtSchema::new());
        assert_eq!(r.rows.len(), 1);
        let keys: Vec<_> = r.rows[0]
            .values
            .iter()
            .map(|(k, _)| k.def().path())
            .collect();
        assert_eq!(keys, ["speech.punctuation", "editing.word"]);
        assert_eq!(
            r.diags
                .iter()
                .filter(|d| matches!(d.msg, DiagMsg::ProfileKey { .. }))
                .count(),
            5
        );
    }

    fn app_names(list: &[&str]) -> Vec<Glob> {
        list.iter().map(|s| Glob::new(s).unwrap()).collect()
    }

    #[test]
    fn pf4_confinement() {
        let names = app_names(&["firefox*", "slack"]);
        let ok = |trig: &str| -> usize {
            let text = format!(
                "[[profile]]\nid = \"r\"\ntriggers = [{trig}]\n[profile.values]\nspeech.punctuation = \"all\"\n"
            );
            parse_rows(
                &text,
                RowOrigin::ThirdParty {
                    ext: "ext-x",
                    app_names: &names,
                },
                &ExtSchema::new(),
            )
            .rows
            .len()
        };
        assert_eq!(ok("{ app = { name = [\"firefox*\"] } }"), 1, "same pattern");
        assert_eq!(
            ok("{ app = { name = [\"firefox-esr\"] } }"),
            1,
            "literal covered"
        );
        assert_eq!(ok("{ app = { name = [\"slack\"] } }"), 1);
        assert_eq!(ok("{ app = { name = [\"signal\"] } }"), 0, "not covered");
        assert_eq!(
            ok("{ app = { name = [\"*\"] } }"),
            0,
            "too short and uncovered"
        );
        assert_eq!(
            ok("{ app = { name = [\"fi*\"] } }"),
            0,
            "wildcard not written in the manifest"
        );
        assert_eq!(ok("{ context = \"terminal\" }"), 0, "context-only");
        assert_eq!(ok("{ window = { title = \"x\" } }"), 0, "no app named");
        assert_eq!(
            ok("{ app = { exe = [\"firefox\"] } }"),
            0,
            "exe alone cannot be covered"
        );
        assert_eq!(
            ok("{ app = { name = [\"slack\"], exe = [\"slack\"] } }"),
            1,
            "exe narrows"
        );
        assert_eq!(
            ok("{ app = { name = [\"sl\"] } }"),
            0,
            "under 3 literal characters"
        );
        // Empty manifest list: no rows at all.
        let r = parse_rows(
            "[[profile]]\nid = \"r\"\ntriggers = [{ app = { name = [\"abc\"] } }]\n[profile.values]\nspeech.punctuation = \"all\"\n",
            RowOrigin::FirstParty {
                ext: "ext-y",
                app_names: &[],
            },
            &ExtSchema::new(),
        );
        assert!(r.rows.is_empty() && !r.diags.is_empty());
        // The product may use context rows.
        let text = "[[profile]]\nid = \"r\"\ntriggers = [{ context = \"terminal\" }]\n[profile.values]\nspeech.punctuation = \"all\"\n";
        assert_eq!(
            parse_rows(text, RowOrigin::Product, &ExtSchema::new())
                .rows
                .len(),
            1
        );
    }

    #[test]
    fn rows_may_set_only_their_own_extension_keys() {
        let mut e = ext_schema();
        e.add("ext-bar", [("k".to_owned(), ExtType::Bool)]);
        let names = app_names(&["slack"]);
        let text = "[[profile]]\nid = \"r\"\ntriggers = [{ app = { name = [\"slack\"] } }]\n[profile.values]\next.ext-foo.on = true\next.ext-bar.k = true\n";
        let r = parse_rows(
            text,
            RowOrigin::ThirdParty {
                ext: "ext-foo",
                app_names: &names,
            },
            &e,
        );
        assert_eq!(r.rows[0].ext.keys().collect::<Vec<_>>(), ["ext.ext-foo.on"]);
    }

    #[test]
    fn pf6_limits() {
        let names = app_names(&["slack"]);
        let mut text = String::new();
        for i in 0..10 {
            let _ = write!(
                text,
                "[[profile]]\nid = \"r{i}\"\ntriggers = [{{ app = {{ name = [\"slack\"] }} }}]\n[profile.values]\nspeech.punctuation = \"all\"\n"
            );
        }
        let r = parse_rows(
            &text,
            RowOrigin::FirstParty {
                ext: "ext-x",
                app_names: &names,
            },
            &ExtSchema::new(),
        );
        assert_eq!(r.rows.len(), MAX_ROWS_PER_EXT);
        assert!(
            r.diags
                .iter()
                .any(|d| matches!(d.msg, DiagMsg::ProfileLimit { .. }))
        );
        // Over 16 triggers drops the row; duplicates and bad ids are diagnosed.
        let many: Vec<String> = (0..17)
            .map(|_| "{ app = { name = [\"slack\"] } }".to_owned())
            .collect();
        let text = format!(
            "[[profile]]\nid = \"big\"\ntriggers = [{}]\n[profile.values]\nspeech.punctuation = \"all\"\n",
            many.join(",")
        );
        let r = parse_rows(
            &text,
            RowOrigin::FirstParty {
                ext: "ext-x",
                app_names: &names,
            },
            &ExtSchema::new(),
        );
        assert!(r.rows.is_empty());
        let text = "[[profile]]\nid = \"a\"\ntriggers = [{ app = { name = [\"abc\"] } }]\n[profile.values]\n[[profile]]\nid = \"a\"\ntriggers = [{ app = { name = [\"abc\"] } }]\n[[profile]]\nid = \"B d\"\n";
        let r = parse_rows(text, RowOrigin::Product, &ExtSchema::new());
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.diags.len(), 2);
    }

    #[test]
    fn a_row_with_a_broken_trigger_is_dropped_whole() {
        let text = "[[profile]]\nid = \"a\"\ntriggers = [{ app = { name = [\"abc\"] } }, { app = { name = [] } }]\n[profile.values]\nspeech.punctuation = \"all\"\n";
        let r = parse_rows(text, RowOrigin::Product, &ExtSchema::new());
        assert!(r.rows.is_empty());
    }

    #[test]
    fn ext_schema_holds() {
        let e = ext_schema();
        assert!(e.holds("ext.ext-foo.level"));
        assert!(!e.holds("ext.ext-foo.zzz"));
        assert!(!e.holds("ext.ext-bar.level"));
        assert!(!e.holds("speech.rate"));
    }
}
