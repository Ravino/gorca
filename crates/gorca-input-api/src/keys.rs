//! Keysyms, key names and the physical-key resolver (ADR-0005 K-8).
//!
//! Command chords match **physical keys**: a key name from the keymap (`t`, `f12`, `kp_home`)
//! is resolved to the XKB keycode of the key that carries it on a Latin layout, and the lane
//! compares keycodes. `gorca+T` therefore works in the Russian layout too. The static resolver
//! below knows the standard PC layout (evdev code + 8); a resolver built from the user's real
//! XKB keymap can replace it behind [`KeyResolver`].

use crate::keymap::KeyRef;

pub mod sym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const INSERT: u32 = 0xff63;
    pub const KP_INSERT: u32 = 0xff9e;
    pub const CAPS_LOCK: u32 = 0xffe5;
    pub const SHIFT_L: u32 = 0xffe1;
    pub const SHIFT_R: u32 = 0xffe2;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const CONTROL_R: u32 = 0xffe4;
    pub const ALT_L: u32 = 0xffe9;
    pub const ALT_R: u32 = 0xffea;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SUPER_R: u32 = 0xffec;
    pub const NUM_LOCK: u32 = 0xff7f;
    pub const ISO_LEVEL3_SHIFT: u32 = 0xfe03;
}

/// Keysym of a gorca modifier name from the settings (`Insert`, `KP_Insert`, `Caps_Lock`).
pub fn modifier_keysym(name: &str) -> Option<u32> {
    match name {
        "Insert" => Some(sym::INSERT),
        "KP_Insert" => Some(sym::KP_INSERT),
        "Caps_Lock" => Some(sym::CAPS_LOCK),
        _ => None,
    }
}

/// Shift, Control, Alt, Super, Meta, Hyper, level shifts and Num Lock: keys that never act alone.
pub fn is_modifier_keysym(ks: u32) -> bool {
    (0xffe1..=0xffee).contains(&ks)
        || ks == sym::NUM_LOCK
        || ks == sym::CAPS_LOCK
        || ks == sym::ISO_LEVEL3_SHIFT
}

pub fn is_shift(ks: u32) -> bool {
    ks == sym::SHIFT_L || ks == sym::SHIFT_R
}

pub fn is_control(ks: u32) -> bool {
    ks == sym::CONTROL_L || ks == sym::CONTROL_R
}

/// Resolves a keymap key name to a physical key.
pub trait KeyResolver: Send + Sync {
    /// `name` is lower-case, as normalised by `gorca-config`.
    fn resolve(&self, name: &str) -> Option<KeyRef>;
}

/// The standard PC layout (physical positions as labelled on a US keyboard).
#[derive(Copy, Clone, Debug, Default)]
pub struct StaticResolver;

impl KeyResolver for StaticResolver {
    fn resolve(&self, name: &str) -> Option<KeyRef> {
        static_keycode(name).map(KeyRef::Code)
    }
}

const ROW_Q: &str = "qwertyuiop";
const ROW_A: &str = "asdfghjkl";
const ROW_Z: &str = "zxcvbnm";

/// `(name, xkb keycode, keysym)` of the non-letter keys. XKB keycode = evdev code + 8.
const NAMED: &[(&str, u16, u32)] = &[
    ("escape", 9, 0xff1b),
    ("minus", 20, 0x2d),
    ("equal", 21, 0x3d),
    ("backspace", 22, 0xff08),
    ("tab", 23, 0xff09),
    ("bracketleft", 34, 0x5b),
    ("bracketright", 35, 0x5d),
    ("return", 36, 0xff0d),
    ("semicolon", 47, 0x3b),
    ("apostrophe", 48, 0x27),
    ("grave", 49, 0x60),
    ("backslash", 51, 0x5c),
    ("comma", 59, 0x2c),
    ("period", 60, 0x2e),
    ("slash", 61, 0x2f),
    ("kp_multiply", 63, 0xffaa),
    ("space", 65, 0x20),
    ("f11", 95, 0xffc8),
    ("f12", 96, 0xffc9),
    ("kp_home", 79, 0xff95),
    ("kp_up", 80, 0xff97),
    ("kp_prior", 81, 0xff9a),
    ("kp_subtract", 82, 0xffad),
    ("kp_left", 83, 0xff96),
    ("kp_begin", 84, 0xff9d),
    ("kp_right", 85, 0xff98),
    ("kp_add", 86, 0xffab),
    ("kp_end", 87, 0xff9c),
    ("kp_down", 88, 0xff99),
    ("kp_next", 89, 0xff9b),
    ("kp_insert", 90, 0xff9e),
    ("kp_delete", 91, 0xff9f),
    ("kp_enter", 104, 0xff8d),
    ("kp_divide", 106, 0xffaf),
    ("home", 110, 0xff50),
    ("up", 111, 0xff52),
    ("prior", 112, 0xff55),
    ("left", 113, 0xff51),
    ("right", 114, 0xff53),
    ("end", 115, 0xff57),
    ("down", 116, 0xff54),
    ("next", 117, 0xff56),
    ("insert", 118, 0xff63),
    ("delete", 119, 0xffff),
    ("menu", 135, 0xff67),
];

/// Aliases that name the same physical key.
const ALIASES: &[(&str, &str)] = &[
    ("enter", "return"),
    ("page_up", "prior"),
    ("pageup", "prior"),
    ("page_down", "next"),
    ("pagedown", "next"),
    ("esc", "escape"),
    ("kp_0", "kp_insert"),
    ("kp_1", "kp_end"),
    ("kp_2", "kp_down"),
    ("kp_3", "kp_next"),
    ("kp_4", "kp_left"),
    ("kp_5", "kp_begin"),
    ("kp_6", "kp_right"),
    ("kp_7", "kp_home"),
    ("kp_8", "kp_up"),
    ("kp_9", "kp_prior"),
    ("kp_decimal", "kp_delete"),
    ("plus", "equal"),
];

/// XKB keycode of the physical key named `name` (lower-case).
pub fn static_keycode(name: &str) -> Option<u16> {
    let name = ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map_or(name, |(_, n)| *n);
    if let Some((_, code, _)) = NAMED.iter().find(|(n, _, _)| *n == name) {
        return Some(*code);
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if let Some(i) = ROW_Q.find(c) {
            return Some(24 + u16::try_from(i).ok()?);
        }
        if let Some(i) = ROW_A.find(c) {
            return Some(38 + u16::try_from(i).ok()?);
        }
        if let Some(i) = ROW_Z.find(c) {
            return Some(52 + u16::try_from(i).ok()?);
        }
        if let Some(d) = c.to_digit(10) {
            // 1..9 are evdev 2..10, 0 is evdev 11.
            return Some(if d == 0 {
                19
            } else {
                9 + u16::try_from(d).ok()?
            });
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return match n {
            1..=10 => Some(66 + n),
            13..=24 => Some(178 + n),
            _ => None,
        };
    }
    None
}

/// The keysym a key name types on an unshifted US layout (for the keysym fallback, K-8).
pub fn static_keysym(name: &str) -> Option<u32> {
    let name = ALIASES
        .iter()
        .find(|(a, _)| *a == name)
        .map_or(name, |(_, n)| *n);
    if let Some((_, _, sym)) = NAMED.iter().find(|(n, _, _)| *n == name) {
        return Some(*sym);
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphanumeric()
    {
        return Some(u32::from(c));
    }
    name.strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (1..=35).contains(n))
        .map(|n| 0xffbd + n)
}

/// The keysym the physical key `code` types on an unshifted US layout with Num Lock off.
pub fn keysym_of_code(code: u16) -> Option<u32> {
    let letters = ROW_Q.chars().chain(ROW_A.chars()).chain(ROW_Z.chars());
    let digits = "1234567890".chars();
    let named = NAMED.iter().map(|(n, _, _)| *n);
    let names: Vec<String> = letters
        .chain(digits)
        .map(String::from)
        .chain(named.map(str::to_owned))
        .chain((1..=10).chain(13..=24).map(|n| format!("f{n}")))
        .collect();
    names
        .iter()
        .find(|n| static_keycode(n) == Some(code))
        .and_then(|n| static_keysym(n))
}

/// The keysyms a US keyboard types with Shift on the digit row `1..9`.
const US_SHIFTED_DIGITS: [u32; 9] = [0x21, 0x40, 0x23, 0x24, 0x25, 0x5e, 0x26, 0x2a, 0x28];

/// What the keys of the configured input sources type (K-8b). Mutter matches a grab against the
/// keysym the *active* layout produced, so a quick-navigation key grabbed as `h` never fires on
/// the Russian layout, where the same physical key types `Cyrillic_er`. gorca grabs the union of
/// the keysyms of every configured layout instead of tracking the active one.
///
/// Plain data: built by the platform layer (`gorca-input-xkb`), carried in `PlatformFacts`. A key
/// the table has no row for falls back to the US layout, so an empty table is exactly the
/// behaviour before layouts were considered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeysymTable {
    /// `(xkb keycode, shift)` to every keysym that key types in some configured layout.
    by_code: std::collections::BTreeMap<(u16, bool), Vec<u32>>,
}

impl KeysymTable {
    /// The US-only table (no rows).
    pub fn us() -> Self {
        Self::default()
    }

    /// Record that `code` types `sym` at the given shift level in some layout.
    pub fn insert(&mut self, code: u16, shift: bool, sym: u32) {
        if sym == 0 {
            return;
        }
        let row = self.by_code.entry((code, shift)).or_default();
        if !row.contains(&sym) {
            row.push(sym);
            row.sort_unstable();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }

    /// The US keysym of `code` at the given level (the baseline of every table).
    pub fn us_sym(code: u16, shift: bool) -> Option<u32> {
        let base = keysym_of_code(code)?;
        if !shift {
            return Some(base);
        }
        Some(match base {
            0x61..=0x7a => base - 0x20,
            0x31..=0x39 => US_SHIFTED_DIGITS[(base - 0x31) as usize],
            other => other,
        })
    }

    /// Every keysym `code` types at `shift`, the US one included, sorted, without duplicates.
    pub fn syms(&self, code: u16, shift: bool) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .by_code
            .get(&(code, shift))
            .cloned()
            .unwrap_or_default();
        if let Some(us) = Self::us_sym(code, shift)
            && !v.contains(&us)
        {
            v.push(us);
            v.sort_unstable();
        }
        v
    }

    /// Does some key other than `code` type `sym` at `shift`? Returns that key's code.
    pub fn typed_elsewhere(&self, code: u16, shift: bool, sym: u32) -> Option<u16> {
        self.by_code
            .iter()
            .find(|((c, sh), v)| *sh == shift && *c != code && v.contains(&sym))
            .map(|((c, _), _)| *c)
    }
}

/// The digit keysym a keypad navigation keysym types with Num Lock on (`KP_Home` -> `KP_7`).
/// `None` for keys whose keysym does not depend on Num Lock.
pub fn keypad_digit_keysym(nav: u32) -> Option<u32> {
    match nav {
        0xff9e => Some(0xffb0),
        0xff9c => Some(0xffb1),
        0xff99 => Some(0xffb2),
        0xff9b => Some(0xffb3),
        0xff96 => Some(0xffb4),
        0xff9d => Some(0xffb5),
        0xff98 => Some(0xffb6),
        0xff95 => Some(0xffb7),
        0xff97 => Some(0xffb8),
        0xff9a => Some(0xffb9),
        0xff9f => Some(0xffae),
        _ => None,
    }
}

/// Spoken name of a key, for learn mode: `T`, `F12`, `Page Up`.
pub fn spoken_key_name(keysym: u32, unichar: u32) -> String {
    let named = match keysym {
        0xff08 => "backspace",
        0xff09 | 0xfe20 => "tab",
        0xff0d => "enter",
        0xff1b => "escape",
        0x20 => "space",
        0xff50 => "home",
        0xff51 => "left",
        0xff52 => "up",
        0xff53 => "right",
        0xff54 => "down",
        0xff55 => "page up",
        0xff56 => "page down",
        0xff57 => "end",
        0xff63 => "insert",
        0xffff => "delete",
        0xff8d => "keypad enter",
        0xffab => "keypad plus",
        0xffad => "keypad minus",
        0xffaa => "keypad multiply",
        0xffaf => "keypad divide",
        0xff95..=0xff9f => return keypad(keysym),
        0xffe1 | 0xffe2 => "shift",
        0xffe3 | 0xffe4 => "control",
        0xffe9 | 0xffea => "alt",
        0xffeb | 0xffec => "super",
        0xffe5 => "caps lock",
        0xff67 => "menu",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    if (0xffbe..=0xffd9).contains(&keysym) {
        return format!("f {}", keysym - 0xffbd);
    }
    char::from_u32(unichar)
        .filter(|c| !c.is_control())
        .or_else(|| char::from_u32(keysym).filter(char::is_ascii_graphic))
        .map_or_else(
            || format!("key {keysym:x}"),
            |c| c.to_uppercase().to_string(),
        )
}

fn keypad(keysym: u32) -> String {
    let n = match keysym {
        0xff95 => "7",
        0xff96 => "4",
        0xff97 => "8",
        0xff98 => "6",
        0xff99 => "2",
        0xff9a => "9",
        0xff9b => "3",
        0xff9c => "1",
        0xff9d => "5",
        0xff9e => "0",
        0xff9f => "decimal",
        _ => "key",
    };
    format!("keypad {n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys_resolve() {
        assert_eq!(static_keycode("t"), Some(28));
        assert_eq!(static_keycode("a"), Some(38));
        assert_eq!(static_keycode("m"), Some(58));
        assert_eq!(static_keycode("1"), Some(10));
        assert_eq!(static_keycode("0"), Some(19));
        assert_eq!(static_keycode("f1"), Some(67));
        assert_eq!(static_keycode("f12"), Some(96));
        assert_eq!(static_keycode("f13"), Some(191));
        assert_eq!(static_keycode("f25"), None);
        assert_eq!(static_keycode("kp_7"), static_keycode("kp_home"));
        assert_eq!(static_keycode("nope"), None);
    }

    #[test]
    fn codes_are_unique_per_name() {
        let mut seen = std::collections::HashMap::new();
        let mut names: Vec<String> = ('a'..='z').map(String::from).collect();
        names.extend((0..=9).map(|d| d.to_string()));
        names.extend((1..=10).map(|n| format!("f{n}")));
        names.extend((13..=24).map(|n| format!("f{n}")));
        names.extend(NAMED.iter().map(|(n, _, _)| (*n).to_owned()));
        for n in names {
            let c = static_keycode(&n).unwrap_or_else(|| panic!("{n}"));
            if let Some(prev) = seen.insert(c, n.clone()) {
                panic!("{n} and {prev} share keycode {c}");
            }
        }
    }

    #[test]
    fn keysyms_follow_names() {
        assert_eq!(static_keysym("t"), Some(0x74));
        assert_eq!(static_keysym("f12"), Some(0xffc9));
        assert_eq!(static_keysym("tab"), Some(0xff09));
        assert_eq!(static_keysym("kp_enter"), Some(0xff8d));
    }

    #[test]
    fn modifier_names_map_to_keysyms() {
        assert_eq!(modifier_keysym("Insert"), Some(sym::INSERT));
        assert_eq!(modifier_keysym("Caps_Lock"), Some(sym::CAPS_LOCK));
        assert_eq!(modifier_keysym("Hyper"), None);
        assert!(is_modifier_keysym(sym::SHIFT_L));
        assert!(!is_modifier_keysym(sym::INSERT));
    }

    #[test]
    fn spoken_names() {
        assert_eq!(spoken_key_name(0x74, 0x74), "T");
        assert_eq!(spoken_key_name(0xffc9, 0), "f 12");
        assert_eq!(spoken_key_name(0xff55, 0), "page up");
        assert_eq!(spoken_key_name(0xff97, 0), "keypad 8");
    }
}

#[cfg(test)]
mod keysym_table_tests {
    use super::*;

    #[test]
    fn an_empty_table_is_the_us_layout() {
        let t = KeysymTable::us();
        assert_eq!(t.syms(43, false), vec![0x68]);
        assert_eq!(t.syms(43, true), vec![0x48]);
        assert_eq!(t.syms(10, true), vec![0x21]);
        assert_eq!(t.syms(15, true), vec![0x5e]);
    }

    #[test]
    fn rows_are_unioned_with_the_us_keysym() {
        let mut t = KeysymTable::us();
        t.insert(43, false, 0x6d2);
        t.insert(43, false, 0x6d2);
        assert_eq!(t.syms(43, false), vec![0x68, 0x6d2]);
        assert_eq!(t.typed_elsewhere(44, false, 0x6d2), Some(43));
        assert_eq!(t.typed_elsewhere(43, false, 0x6d2), None);
    }
}
