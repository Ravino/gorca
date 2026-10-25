// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed values over the [`SETTINGS`] table: [`KeyId`], [`Value`] and the sparse
//! [`SettingsPatch`] that generalises [`crate::Overlay`] (ADR-0016 P1).
//!
//! Every layer of the profile stack (rows, the user base, each profile, each unsaved delta) is a
//! `SettingsPatch`; folding layers is `apply` in order. Pure: no I/O.

use std::collections::BTreeMap;

use crate::settings::{
    Backend, BoundaryMode, Caps, Confirmations, IfCoveredPolicy, InterruptPolicy, ItemClick,
    Layout, ModeDefault, PluginIdleLease, PluginPlacement, PluginScheduler, Preset, Punct,
    SETTINGS, SettingDef, SettingType, Settings, Verbosity, WordLevel,
};

/// Index of a row of [`SETTINGS`]: the dense key id every resolver uses.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct KeyId(u16);

impl KeyId {
    /// The key at `index` of [`SETTINGS`], if there is one.
    pub fn from_index(index: usize) -> Option<Self> {
        if index < SETTINGS.len() {
            u16::try_from(index).ok().map(Self)
        } else {
            None
        }
    }

    /// The key `section.key`, if the schema has it.
    pub fn of(section: &str, key: &str) -> Option<Self> {
        SETTINGS
            .iter()
            .position(|d| d.section == section && d.key == key)
            .and_then(Self::from_index)
    }

    /// Parse `section.key`.
    pub fn parse(path: &str) -> Option<Self> {
        let (s, k) = path.split_once('.')?;
        Self::of(s, k)
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// The schema row. `from_index` bounds the index, so the fallback is unreachable.
    pub fn def(self) -> &'static SettingDef {
        SETTINGS.get(self.index()).unwrap_or(&SETTINGS[0])
    }

    /// Every key, in table order.
    pub fn all() -> impl Iterator<Item = KeyId> {
        (0..SETTINGS.len()).filter_map(Self::from_index)
    }
}

/// A typed setting value. Enumerations are carried by their canonical file name, lists of
/// independent options (`speech.caps`, `editing.boundary`) as their option names.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Str(String),
    StrList(Vec<String>),
    Map(BTreeMap<String, bool>),
}

impl Value {
    /// Same variant as the schema type `ty` demands (the range is not checked).
    pub fn fits(&self, ty: SettingType) -> bool {
        match (self, ty) {
            (Value::Bool(_), SettingType::Bool)
            | (Value::Int(_), SettingType::Int { .. })
            | (Value::Str(_), SettingType::Enum(_) | SettingType::Str)
            | (Value::StrList(_), SettingType::StrList)
            | (Value::Map(_), SettingType::Map(_)) => true,
            (
                Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::StrList(_) | Value::Map(_),
                _,
            ) => false,
        }
    }
}

fn int_u8(v: &Value) -> Option<u8> {
    match v {
        Value::Int(n) => u8::try_from(*n).ok(),
        Value::Bool(_) | Value::Str(_) | Value::StrList(_) | Value::Map(_) => None,
    }
}

fn int_u32(v: &Value) -> Option<u32> {
    match v {
        Value::Int(n) => u32::try_from(*n).ok(),
        Value::Bool(_) | Value::Str(_) | Value::StrList(_) | Value::Map(_) => None,
    }
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Int(_) | Value::Str(_) | Value::StrList(_) | Value::Map(_) => None,
    }
}

fn text(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s),
        Value::Bool(_) | Value::Int(_) | Value::StrList(_) | Value::Map(_) => None,
    }
}

fn list(v: &Value) -> Option<&[String]> {
    match v {
        Value::StrList(l) => Some(l),
        Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::Map(_) => None,
    }
}

/// The canonical enum name for `text` (case as the file spells it), per key.
pub(crate) fn enum_name(key: KeyId, text: &str) -> Option<&'static str> {
    let d = key.def();
    match (d.section, d.key) {
        ("keyboard", "preset") => Preset::parse(text).map(Preset::name),
        ("keyboard", "layout") => Layout::parse(text).map(Layout::name),
        ("keyboard", "interrupt") => InterruptPolicy::parse(text).map(InterruptPolicy::name),
        ("speech", "backend") => Backend::parse(text).map(Backend::name),
        ("speech", "punctuation") => Punct::parse(text).map(Punct::name),
        ("editing", "word") => WordLevel::parse(text).map(WordLevel::name),
        ("mouse", "item_click") => ItemClick::parse(text).map(ItemClick::name),
        ("plugins", "scheduler") => PluginScheduler::parse(text).map(PluginScheduler::name),
        ("plugins", "placement") => PluginPlacement::parse(text).map(PluginPlacement::name),
        ("plugins", "idle_lease") => PluginIdleLease::parse(text).map(PluginIdleLease::name),
        ("mouse", "if_covered") => IfCoveredPolicy::parse(text).map(IfCoveredPolicy::name),
        ("browse", "mode_default") => ModeDefault::parse(text).map(ModeDefault::name),
        ("verbosity", "level") => Verbosity::parse(text).map(Verbosity::name),
        ("verbosity", "confirmations") => Confirmations::parse(text).map(Confirmations::name),
        _ => None,
    }
}

/// The value of `key` in `s`.
#[allow(clippy::too_many_lines)] // one arm per schema row, kept flat on purpose
pub fn get_value(s: &Settings, key: KeyId) -> Value {
    let d = key.def();
    let names = |v: Vec<&'static str>| Value::StrList(v.into_iter().map(str::to_owned).collect());
    match (d.section, d.key) {
        ("keyboard", "preset") => Value::Str(s.keyboard.preset.name().to_owned()),
        ("keyboard", "layout") => Value::Str(s.keyboard.layout.name().to_owned()),
        ("keyboard", "modifiers") => Value::StrList(s.keyboard.modifiers.clone()),
        ("keyboard", "caps_lock_layout_guard") => Value::Bool(s.keyboard.caps_lock_layout_guard),
        ("keyboard", "multi_press_ms") => Value::Int(i64::from(s.keyboard.multi_press_ms)),
        ("keyboard", "double_tap_guard_ms") => {
            Value::Int(i64::from(s.keyboard.double_tap_guard_ms))
        }
        ("keyboard", "interrupt") => Value::Str(s.keyboard.interrupt.name().to_owned()),
        ("keyboard", "stop_speech_on_control") => Value::Bool(s.keyboard.stop_speech_on_control),
        ("keyboard", "pause_speech_on_shift") => Value::Bool(s.keyboard.pause_speech_on_shift),
        ("keymap", "replace_preset") => Value::Bool(s.keyboard.replace_preset),
        ("speech", "rate") => Value::Int(i64::from(s.speech.rate)),
        ("speech", "pitch") => Value::Int(i64::from(s.speech.pitch)),
        ("speech", "volume") => Value::Int(i64::from(s.speech.volume)),
        ("speech", "rate_step") => Value::Int(i64::from(s.speech.rate_step)),
        ("speech", "voice") => Value::Str(s.speech.voice.clone()),
        ("speech", "language") => Value::Str(s.speech.language.clone()),
        ("speech", "output_module") => Value::Str(s.speech.output_module.clone()),
        ("speech", "backend") => Value::Str(s.speech.backend.name().to_owned()),
        ("speech", "punctuation") => Value::Str(s.speech.punctuation.name().to_owned()),
        ("speech", "caps") => names(s.speech.caps.names()),
        ("speech", "caps_pitch_change") => Value::Int(i64::from(s.speech.caps_pitch_change)),
        ("plugins", "enabled") => Value::Bool(s.plugins.enabled),
        ("plugins", "scheduler") => Value::Str(s.plugins.scheduler.name().to_owned()),
        ("plugins", "placement") => Value::Str(s.plugins.placement.name().to_owned()),
        ("plugins", "idle_lease") => Value::Str(s.plugins.idle_lease.name().to_owned()),
        ("mouse", "enabled") => Value::Bool(s.mouse.enabled),
        ("mouse", "item_click") => Value::Str(s.mouse.item_click.name().to_owned()),
        ("mouse", "if_covered") => Value::Str(s.mouse.if_covered.name().to_owned()),
        ("mouse", "announce_move") => Value::Bool(s.mouse.announce_move),
        ("echo", "typed_characters") => Value::Bool(s.echo.typed_characters),
        ("echo", "typed_words") => Value::Bool(s.echo.typed_words),
        ("echo", "command_keys") => Value::Bool(s.echo.command_keys),
        ("echo", "inserted_text") => Value::Bool(s.echo.inserted_text),
        ("editing", "announce_clipboard") => Value::Bool(s.editing.announce_clipboard),
        ("editing", "boundary") => names(s.editing.boundary.names()),
        ("editing", "word") => Value::Str(s.editing.word.name().to_owned()),
        ("text", "say_all_moves_caret") => Value::Bool(s.text.say_all_moves_caret),
        ("text", "caret_edge_ms") => Value::Int(i64::from(s.text.caret_edge_ms)),
        ("text", "caret_late_ms") => Value::Int(i64::from(s.text.caret_late_ms)),
        ("browse", "say_all_on_load") => Value::Bool(s.browse.say_all_on_load),
        ("browse", "description_pause") => Value::Bool(s.browse.description_pause),
        ("browse", "hedge_inferred") => Value::Bool(s.browse.hedge_inferred),
        ("browse", "enabled") => Value::Bool(s.browse.enabled),
        ("browse", "mode_default") => Value::Str(s.browse.mode_default.name().to_owned()),
        ("terminal", "settle_ms") => Value::Int(i64::from(s.terminal.settle_ms)),
        ("terminal", "burst_lines") => Value::Int(i64::from(s.terminal.burst_lines)),
        ("terminal", "prompt_markers") => Value::StrList(s.terminal.prompt_markers.clone()),
        ("terminal", "prompt_regex") => Value::Str(s.terminal.prompt_regex.clone()),
        ("terminal", "secret_regex") => Value::Str(s.terminal.secret_regex.clone()),
        ("verbosity", "level") => Value::Str(s.verbosity.level.name().to_owned()),
        ("verbosity", "confirmations") => Value::Str(s.verbosity.confirmations.name().to_owned()),
        ("profiles", "rows") => Value::Map(s.profiles.rows.clone()),
        // A schema row without an arm is caught by `every_row_round_trips`.
        _ => Value::Bool(false),
    }
}

/// Set `key` in `s` to `v`. `false` (and `s` untouched) when the value does not fit the key:
/// wrong variant, out of the integer range, unknown enum name or option.
#[allow(clippy::too_many_lines)] // one arm per schema row, kept flat on purpose
pub fn set_value(s: &mut Settings, key: KeyId, v: &Value) -> bool {
    let d = key.def();
    if !v.fits(d.ty) {
        return false;
    }
    if let (Value::Int(n), SettingType::Int { min, max }) = (v, d.ty)
        && !(min..=max).contains(n)
    {
        return false;
    }
    macro_rules! put {
        ($slot:expr, $val:expr) => {{
            match $val {
                Some(x) => {
                    $slot = x;
                    true
                }
                None => false,
            }
        }};
    }
    match (d.section, d.key) {
        ("keyboard", "preset") => put!(s.keyboard.preset, text(v).and_then(Preset::parse)),
        ("keyboard", "layout") => put!(s.keyboard.layout, text(v).and_then(Layout::parse)),
        ("keyboard", "modifiers") => put!(s.keyboard.modifiers, list(v).map(<[String]>::to_vec)),
        ("keyboard", "caps_lock_layout_guard") => {
            put!(s.keyboard.caps_lock_layout_guard, boolean(v))
        }
        ("keyboard", "multi_press_ms") => put!(s.keyboard.multi_press_ms, int_u32(v)),
        ("keyboard", "double_tap_guard_ms") => put!(s.keyboard.double_tap_guard_ms, int_u32(v)),
        ("keyboard", "interrupt") => {
            put!(
                s.keyboard.interrupt,
                text(v).and_then(InterruptPolicy::parse)
            )
        }
        ("keyboard", "stop_speech_on_control") => {
            put!(s.keyboard.stop_speech_on_control, boolean(v))
        }
        ("keyboard", "pause_speech_on_shift") => {
            put!(s.keyboard.pause_speech_on_shift, boolean(v))
        }
        ("keymap", "replace_preset") => put!(s.keyboard.replace_preset, boolean(v)),
        ("speech", "rate") => put!(s.speech.rate, int_u8(v)),
        ("speech", "pitch") => put!(s.speech.pitch, int_u8(v)),
        ("speech", "volume") => put!(s.speech.volume, int_u8(v)),
        ("speech", "rate_step") => put!(s.speech.rate_step, int_u8(v)),
        ("speech", "voice") => put!(s.speech.voice, text(v).map(str::to_owned)),
        ("speech", "language") => put!(s.speech.language, text(v).map(str::to_owned)),
        ("speech", "output_module") => put!(s.speech.output_module, text(v).map(str::to_owned)),
        ("speech", "backend") => put!(s.speech.backend, text(v).and_then(Backend::parse)),
        ("speech", "punctuation") => put!(s.speech.punctuation, text(v).and_then(Punct::parse)),
        ("speech", "caps") => put!(s.speech.caps, list(v).and_then(caps_of)),
        ("speech", "caps_pitch_change") => put!(s.speech.caps_pitch_change, int_u8(v)),
        ("plugins", "enabled") => put!(s.plugins.enabled, boolean(v)),
        ("plugins", "scheduler") => {
            put!(
                s.plugins.scheduler,
                text(v).and_then(PluginScheduler::parse)
            )
        }
        ("plugins", "placement") => {
            put!(
                s.plugins.placement,
                text(v).and_then(PluginPlacement::parse)
            )
        }
        ("plugins", "idle_lease") => {
            put!(
                s.plugins.idle_lease,
                text(v).and_then(PluginIdleLease::parse)
            )
        }
        ("mouse", "enabled") => put!(s.mouse.enabled, boolean(v)),
        ("mouse", "item_click") => put!(s.mouse.item_click, text(v).and_then(ItemClick::parse)),
        ("mouse", "if_covered") => {
            put!(s.mouse.if_covered, text(v).and_then(IfCoveredPolicy::parse))
        }
        ("mouse", "announce_move") => put!(s.mouse.announce_move, boolean(v)),
        ("echo", "typed_characters") => put!(s.echo.typed_characters, boolean(v)),
        ("echo", "typed_words") => put!(s.echo.typed_words, boolean(v)),
        ("echo", "command_keys") => put!(s.echo.command_keys, boolean(v)),
        ("echo", "inserted_text") => put!(s.echo.inserted_text, boolean(v)),
        ("editing", "announce_clipboard") => put!(s.editing.announce_clipboard, boolean(v)),
        ("editing", "boundary") => put!(s.editing.boundary, list(v).and_then(boundary_of)),
        ("editing", "word") => put!(s.editing.word, text(v).and_then(WordLevel::parse)),
        ("text", "say_all_moves_caret") => put!(s.text.say_all_moves_caret, boolean(v)),
        ("text", "caret_edge_ms") => put!(s.text.caret_edge_ms, int_u32(v)),
        ("text", "caret_late_ms") => put!(s.text.caret_late_ms, int_u32(v)),
        ("browse", "say_all_on_load") => put!(s.browse.say_all_on_load, boolean(v)),
        ("browse", "description_pause") => put!(s.browse.description_pause, boolean(v)),
        ("browse", "hedge_inferred") => put!(s.browse.hedge_inferred, boolean(v)),
        ("browse", "enabled") => put!(s.browse.enabled, boolean(v)),
        ("browse", "mode_default") => {
            put!(s.browse.mode_default, text(v).and_then(ModeDefault::parse))
        }
        ("terminal", "settle_ms") => put!(s.terminal.settle_ms, int_u32(v)),
        ("terminal", "burst_lines") => put!(s.terminal.burst_lines, int_u32(v)),
        ("terminal", "prompt_markers") => {
            put!(s.terminal.prompt_markers, list(v).map(<[String]>::to_vec))
        }
        ("terminal", "prompt_regex") => put!(s.terminal.prompt_regex, text(v).map(str::to_owned)),
        ("terminal", "secret_regex") => put!(s.terminal.secret_regex, text(v).map(str::to_owned)),
        ("verbosity", "level") => put!(s.verbosity.level, text(v).and_then(Verbosity::parse)),
        ("verbosity", "confirmations") => {
            put!(
                s.verbosity.confirmations,
                text(v).and_then(Confirmations::parse)
            )
        }
        ("profiles", "rows") => match v {
            Value::Map(m) => {
                s.profiles.rows.clone_from(m);
                true
            }
            Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::StrList(_) => false,
        },
        _ => false,
    }
}

fn caps_of(names: &[String]) -> Option<Caps> {
    let mut c = Caps::OFF;
    names.iter().all(|n| c.add(n)).then_some(c)
}

fn boundary_of(names: &[String]) -> Option<BoundaryMode> {
    let mut m = BoundaryMode::OFF;
    names.iter().all(|n| m.add(n)).then_some(m)
}

/// UB-1: `v` is the default of `key` in `base` (`Settings::default()`, or L0 plus the preset's
/// defaults), so a full dump of the defaults is not an override.
pub fn is_default(base: &Settings, key: KeyId, v: &Value) -> bool {
    get_value(base, key) == *v
}

/// A sparse set of key values: one layer, one unsaved delta, one profile body.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettingsPatch {
    values: BTreeMap<KeyId, Value>,
}

impl SettingsPatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn get(&self, key: KeyId) -> Option<&Value> {
        self.values.get(&key)
    }

    pub fn contains(&self, key: KeyId) -> bool {
        self.values.contains_key(&key)
    }

    /// Set `key`; `false` (nothing stored) when the value does not fit the key.
    pub fn set(&mut self, key: KeyId, v: Value) -> bool {
        let mut probe = Settings::default();
        if !set_value(&mut probe, key, &v) {
            return false;
        }
        self.values.insert(key, v);
        true
    }

    pub fn remove(&mut self, key: KeyId) -> Option<Value> {
        self.values.remove(&key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (KeyId, &Value)> {
        self.values.iter().map(|(k, v)| (*k, v))
    }

    /// `base` with this patch applied, key by key (table order).
    #[must_use]
    pub fn apply(&self, base: &Settings) -> Settings {
        let mut s = base.clone();
        for (k, v) in &self.values {
            // Stored values were validated by `set`, so this cannot fail.
            let _ = set_value(&mut s, *k, v);
        }
        s
    }

    /// HR-4: after a reload, drop the entries whose value in the file changed (the user's
    /// file edit wins); the others survive.
    pub fn drop_changed(&mut self, old: &Settings, new: &Settings) {
        self.values
            .retain(|k, _| get_value(old, *k) == get_value(new, *k));
    }

    /// Entries of `other` over this patch (the later layer wins per key).
    pub fn merge_over(&mut self, other: &SettingsPatch) {
        for (k, v) in &other.values {
            self.values.insert(*k, v.clone());
        }
    }
}

impl From<&crate::Overlay> for SettingsPatch {
    /// The façade: the old eight-field overlay as a patch.
    fn from(o: &crate::Overlay) -> Self {
        let mut p = SettingsPatch::new();
        let mut put = |path: &str, v: Value| {
            if let Some(k) = KeyId::parse(path) {
                let _ = p.set(k, v);
            }
        };
        if let Some(v) = o.rate {
            put("speech.rate", Value::Int(i64::from(v)));
        }
        if let Some(v) = o.pitch {
            put("speech.pitch", Value::Int(i64::from(v)));
        }
        if let Some(v) = o.volume {
            put("speech.volume", Value::Int(i64::from(v)));
        }
        if let Some(v) = o.typed_characters {
            put("echo.typed_characters", Value::Bool(v));
        }
        if let Some(v) = o.typed_words {
            put("echo.typed_words", Value::Bool(v));
        }
        if let Some(v) = o.command_keys {
            put("echo.command_keys", Value::Bool(v));
        }
        if let Some(v) = o.verbosity {
            put("verbosity.level", Value::Str(v.name().to_owned()));
        }
        if let Some(v) = o.punctuation {
            put("speech.punctuation", Value::Str(v.name().to_owned()));
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Overlay;

    /// A value different from the default that fits the row.
    fn other_than_default(key: KeyId, base: &Settings) -> Value {
        let d = key.def();
        let cur = get_value(base, key);
        match (d.ty, &cur) {
            (SettingType::Bool, Value::Bool(b)) => Value::Bool(!b),
            (SettingType::Int { min, max }, Value::Int(n)) => {
                Value::Int(if *n == min { max } else { min })
            }
            (SettingType::Enum(_), _) => {
                let candidates: &[&str] = match (d.section, d.key) {
                    ("keyboard", "preset") => &["orca", "nvda"],
                    ("keyboard", "layout") => &["laptop", "desktop"],
                    ("keyboard", "interrupt") => &["never", "non-modifier"],
                    ("speech", "backend") => &["ssip", "auto"],
                    ("speech", "punctuation") => &["all", "none"],
                    ("editing", "word") => &["coarse", "auto"],
                    ("mouse", "item_click") => &["pointer", "action_first"],
                    ("plugins", "scheduler") => &["sps", "labels"],
                    ("plugins", "placement" | "idle_lease") => &["off", "auto"],
                    ("mouse", "if_covered") => &["click", "refuse"],
                    ("browse", "mode_default") => &["focus", "browse"],
                    ("verbosity", "level") => &["brief", "normal"],
                    ("verbosity", "confirmations") => &["brief", "full"],
                    _ => &[],
                };
                let pick = candidates
                    .iter()
                    .find(|c| Value::Str((**c).to_owned()) != cur)
                    .copied()
                    .unwrap_or("");
                Value::Str(pick.to_owned())
            }
            (SettingType::Str, _) => Value::Str("x".to_owned()),
            (SettingType::StrList, _) => match (d.section, d.key) {
                ("keyboard", "modifiers") => Value::StrList(vec!["Insert".to_owned()]),
                ("speech", "caps") => Value::StrList(vec!["word".to_owned()]),
                ("editing", "boundary") => Value::StrList(vec!["say".to_owned()]),
                _ => Value::StrList(vec!["q".to_owned()]),
            },
            (SettingType::Map(_), _) => Value::Map(BTreeMap::from([("a/b".to_owned(), true)])),
            (SettingType::Bool | SettingType::Int { .. }, _) => cur,
        }
    }

    #[test]
    fn every_row_round_trips() {
        let base = Settings::default();
        for key in KeyId::all() {
            let d = key.def();
            // get -> set on a fresh settings is the identity.
            let v = get_value(&base, key);
            assert!(
                v.fits(d.ty),
                "{} default value has the wrong type",
                d.path()
            );
            let mut s = Settings::default();
            assert!(set_value(&mut s, key, &v), "{}", d.path());
            assert_eq!(s, base, "{}", d.path());
            // a different value sticks and reads back.
            let other = other_than_default(key, &base);
            assert_ne!(other, v, "{} has no alternative in the test", d.path());
            let mut s = Settings::default();
            assert!(set_value(&mut s, key, &other), "{}", d.path());
            assert_ne!(s, base, "{} did not change Settings", d.path());
            assert_eq!(get_value(&s, key), other, "{}", d.path());
            // the patch route gives the same result.
            let mut p = SettingsPatch::new();
            assert!(p.set(key, other.clone()));
            assert_eq!(p.apply(&base), s, "{}", d.path());
        }
    }

    #[test]
    fn the_default_text_of_every_row_is_the_default_value() {
        // `SETTINGS.default` (TOML text) and `Settings::default()` must agree, so UB-1's
        // default-equality is the same whichever side is asked.
        let text = crate::settings::defaults_toml();
        let l = crate::load::load(Some(&text), &crate::Presets::builtin());
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert_eq!(l.settings, Settings::default());
    }

    #[test]
    fn unfit_values_are_refused() {
        let mut s = Settings::default();
        let rate = KeyId::of("speech", "rate").unwrap();
        assert!(!set_value(&mut s, rate, &Value::Int(101)));
        assert!(!set_value(&mut s, rate, &Value::Int(-1)));
        assert!(!set_value(&mut s, rate, &Value::Bool(true)));
        let punct = KeyId::of("speech", "punctuation").unwrap();
        assert!(!set_value(&mut s, punct, &Value::Str("lots".to_owned())));
        let caps = KeyId::of("speech", "caps").unwrap();
        assert!(!set_value(
            &mut s,
            caps,
            &Value::StrList(vec!["loud".to_owned()])
        ));
        assert_eq!(s, Settings::default());
        let mut p = SettingsPatch::new();
        assert!(!p.set(rate, Value::Int(500)));
        assert!(p.is_empty());
    }

    #[test]
    fn map_values_parse_and_print() {
        let rows = KeyId::of("profiles", "rows").unwrap();
        let mut p = SettingsPatch::new();
        let m = BTreeMap::from([("gorca/vscode".to_owned(), false), ("x/y".to_owned(), true)]);
        assert!(p.set(rows, Value::Map(m.clone())));
        assert_eq!(p.apply(&Settings::default()).profiles.rows, m);
        assert!(!p.set(rows, Value::Bool(true)));
    }

    #[test]
    fn ub1_default_equality_per_type() {
        let base = Settings::default();
        for key in KeyId::all() {
            let v = get_value(&base, key);
            assert!(is_default(&base, key, &v), "{}", key.def().path());
            let o = other_than_default(key, &base);
            assert!(!is_default(&base, key, &o), "{}", key.def().path());
        }
        // The base may be a preset-implied default instead of L0.
        let mut b2 = Settings::default();
        b2.speech.rate = 70;
        let rate = KeyId::of("speech", "rate").unwrap();
        assert!(is_default(&b2, rate, &Value::Int(70)));
        assert!(!is_default(&b2, rate, &Value::Int(50)));
    }

    #[test]
    fn drop_changed_is_per_key() {
        let old = Settings::default();
        let mut new = old.clone();
        new.speech.rate = 30;
        let mut p = SettingsPatch::new();
        p.set(KeyId::of("speech", "rate").unwrap(), Value::Int(80));
        p.set(KeyId::of("speech", "pitch").unwrap(), Value::Int(70));
        p.drop_changed(&old, &new);
        assert!(!p.contains(KeyId::of("speech", "rate").unwrap()));
        assert!(p.contains(KeyId::of("speech", "pitch").unwrap()));
    }

    #[test]
    fn the_overlay_facade_equals_the_old_behaviour() {
        let o = Overlay {
            rate: Some(65),
            pitch: Some(10),
            volume: Some(80),
            typed_characters: Some(false),
            typed_words: Some(true),
            command_keys: Some(true),
            verbosity: Some(Verbosity::Brief),
            punctuation: Some(Punct::All),
        };
        let base = Settings::default();
        assert_eq!(SettingsPatch::from(&o).apply(&base), o.apply(&base));
        assert_eq!(SettingsPatch::from(&o).len(), 8);
        assert!(SettingsPatch::from(&Overlay::default()).is_empty());
        // drop_changed agrees with the overlay's.
        let mut new = base.clone();
        new.speech.rate = 11;
        new.echo.typed_words = true;
        let mut ov = o.clone();
        ov.drop_changed(&base, &new);
        let mut pa = SettingsPatch::from(&o);
        pa.drop_changed(&base, &new);
        assert_eq!(pa, SettingsPatch::from(&ov));
    }

    #[test]
    fn scope_table_matches_the_adr() {
        use crate::settings::KeyScope::{App, Base, Doc, Focus};
        let sc = |p: &str| KeyId::parse(p).unwrap().def();
        assert_eq!(sc("keyboard.preset").scope, Base);
        assert_eq!(sc("keymap.replace_preset").scope, Base);
        assert_eq!(sc("speech.backend").scope, Base);
        assert_eq!(sc("profiles.rows").scope, Base);
        assert_eq!(sc("speech.rate").scope, Focus);
        assert!(!sc("speech.rate").vendor);
        assert!(sc("speech.punctuation").vendor);
        assert!(sc("echo.typed_words").privacy && !sc("echo.typed_words").vendor);
        assert!(sc("editing.announce_clipboard").privacy);
        assert!(sc("editing.word").vendor && !sc("editing.word").privacy);
        assert_eq!(sc("browse.say_all_on_load").scope, Doc);
        assert_eq!(sc("terminal.settle_ms").scope, App);
        assert!(sc("terminal.secret_regex").privacy && !sc("terminal.secret_regex").vendor);
        for k in KeyId::all() {
            let d = k.def();
            if d.scope == Base {
                assert!(!d.vendor, "{}", d.path());
            }
            if d.privacy {
                assert!(!d.vendor, "{}", d.path());
            }
        }
    }
}
