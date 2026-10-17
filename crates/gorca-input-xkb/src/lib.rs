// SPDX-License-Identifier: GPL-3.0-or-later
//! The keysym table of the configured input sources (K-8b), read through libxkbcommon.
//!
//! Mutter matches a key grab against the keysym the active layout produced, so a quick-navigation
//! key grabbed as `h` does nothing on the Russian layout. [`build_table`] asks libxkbcommon what
//! every physical key types, at level 0 (no Shift) and level 1 (Shift), in each configured layout,
//! and returns the union as plain data ([`KeysymTable`]); the keymap compiler grabs all of them.
//!
//! libxkbcommon is loaded at run time (`dlopen("libxkbcommon.so.0")`): a machine without it gets
//! the US-only table, never a build or start-up failure. This is the only `unsafe` in the crate;
//! every block says why it is sound. Nothing here runs on a lane: the daemon calls it from its
//! config task.

#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::ptr;

use gorca_input_api::KeysymTable;

/// One input source: an XKB layout name and an optional variant (`us+dvorak`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct LayoutSpec {
    pub layout: String,
    pub variant: String,
}

impl LayoutSpec {
    /// `ru`, `us+dvorak`, `de+nodeadkeys`. `None` for an empty or malformed name.
    pub fn parse(id: &str) -> Option<Self> {
        let id = id.trim();
        let (layout, variant) = id.split_once('+').unwrap_or((id, ""));
        let ok = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        (!layout.is_empty() && ok(layout) && ok(variant)).then(|| Self {
            layout: layout.to_owned(),
            variant: variant.to_owned(),
        })
    }
}

/// Keycodes XKB defines (evdev + 8); the table covers all of them.
const MIN_KEYCODE: u32 = 8;
const MAX_KEYCODE: u32 = 255;

type CtxNew = unsafe extern "C" fn(c_uint) -> *mut c_void;
type Unref = unsafe extern "C" fn(*mut c_void);
type KeymapNew = unsafe extern "C" fn(*mut c_void, *const RuleNames, c_uint) -> *mut c_void;
type NumLayouts = unsafe extern "C" fn(*mut c_void) -> c_uint;
type NumLevels = unsafe extern "C" fn(*mut c_void, c_uint, c_uint) -> c_uint;
type GetSyms = unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, *mut *const u32) -> c_int;

/// `struct xkb_rule_names`.
#[repr(C)]
struct RuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

/// The entry points of libxkbcommon this crate calls.
struct Lib {
    handle: *mut c_void,
    ctx_new: CtxNew,
    ctx_unref: Unref,
    keymap_new: KeymapNew,
    keymap_unref: Unref,
    num_layouts: NumLayouts,
    num_levels: NumLevels,
    get_syms: GetSyms,
}

impl std::fmt::Debug for Lib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lib")
    }
}

/// `dlsym` of a NUL-terminated name, as a function pointer type `T`.
///
/// # Safety
/// `T` must be the function-pointer type the symbol really has.
unsafe fn sym<T: Copy>(handle: *mut c_void, name: &'static [u8]) -> Option<T> {
    debug_assert!(name.ends_with(&[0]));
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    // SAFETY: `handle` is a live dlopen handle and `name` is NUL-terminated.
    let p = unsafe { libc::dlsym(handle, name.as_ptr().cast()) };
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller promises `T` matches the symbol; sizes were checked above.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
}

impl Lib {
    fn open() -> Option<Self> {
        for name in [c"libxkbcommon.so.0", c"libxkbcommon.so"] {
            // SAFETY: `name` is a valid C string; dlopen has no other preconditions.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                continue;
            }
            // SAFETY: each type below is the documented C signature of the named function
            // (xkbcommon.h); `handle` is live.
            let lib = unsafe {
                (|| {
                    Some(Self {
                        handle,
                        ctx_new: sym(handle, b"xkb_context_new\0")?,
                        ctx_unref: sym(handle, b"xkb_context_unref\0")?,
                        keymap_new: sym(handle, b"xkb_keymap_new_from_names\0")?,
                        keymap_unref: sym(handle, b"xkb_keymap_unref\0")?,
                        num_layouts: sym(handle, b"xkb_keymap_num_layouts\0")?,
                        num_levels: sym(handle, b"xkb_keymap_num_levels_for_key\0")?,
                        get_syms: sym(handle, b"xkb_keymap_key_get_syms_by_level\0")?,
                    })
                })()
            };
            if lib.is_none() {
                // SAFETY: `handle` came from dlopen above and is not used afterwards.
                unsafe { libc::dlclose(handle) };
            }
            return lib;
        }
        None
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        // SAFETY: `handle` came from dlopen and every pointer taken from it is gone with `self`.
        unsafe { libc::dlclose(self.handle) };
    }
}

/// Is libxkbcommon present on this machine?
pub fn available() -> bool {
    Lib::open().is_some()
}

/// What the keys of `layouts` type, as the union table. `options` is the comma-separated
/// `xkb-options` setting (it moves keys, e.g. `ctrl:nocaps`). Layouts libxkbcommon refuses are
/// skipped; without the library the result is the US-only table. `us` is always part of the
/// result (the table falls back to it).
pub fn build_table(layouts: &[LayoutSpec], options: &str) -> KeysymTable {
    build_table_cancellable(layouts, options, &std::sync::atomic::AtomicBool::new(false))
}

/// [`build_table`] that stops before the next layout once `cancel` is set (a caller whose
/// deadline passed). A keymap compile already running cannot be interrupted.
pub fn build_table_cancellable(
    layouts: &[LayoutSpec],
    options: &str,
    cancel: &std::sync::atomic::AtomicBool,
) -> KeysymTable {
    let mut table = KeysymTable::us();
    let Some(lib) = Lib::open() else {
        tracing::warn!("libxkbcommon is not available: quick-navigation grabs are US-only");
        return table;
    };
    let Ok(options) = CString::new(options) else {
        return table;
    };
    // SAFETY: flags 0 is XKB_CONTEXT_NO_FLAGS.
    let ctx = unsafe { (lib.ctx_new)(0) };
    if ctx.is_null() {
        return table;
    }
    for spec in layouts {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let (Ok(layout), Ok(variant)) = (
            CString::new(spec.layout.as_str()),
            CString::new(spec.variant.as_str()),
        ) else {
            continue;
        };
        let names = RuleNames {
            rules: ptr::null(),
            model: ptr::null(),
            layout: layout.as_ptr(),
            variant: variant.as_ptr(),
            options: options.as_ptr(),
        };
        // SAFETY: `ctx` is live, `names` and the strings it points to outlive the call, and
        // NULL members mean "use the default".
        let map = unsafe { (lib.keymap_new)(ctx, &raw const names, 0) };
        if map.is_null() {
            tracing::warn!(layout = %spec.layout, variant = %spec.variant, "xkb refused the layout");
            continue;
        }
        add_keymap(&lib, map, &mut table);
        // SAFETY: `map` is live and not used afterwards.
        unsafe { (lib.keymap_unref)(map) };
    }
    // SAFETY: `ctx` is live and every keymap made from it is released.
    unsafe { (lib.ctx_unref)(ctx) };
    table
}

/// Level 0 and level 1 of every key of group 0 of `map`.
fn add_keymap(lib: &Lib, map: *mut c_void, table: &mut KeysymTable) {
    // SAFETY: `map` is a live keymap.
    if unsafe { (lib.num_layouts)(map) } == 0 {
        return;
    }
    for key in MIN_KEYCODE..=MAX_KEYCODE {
        // SAFETY: `map` is live; any keycode may be asked about (unknown keys have no levels).
        let levels = unsafe { (lib.num_levels)(map, key, 0) };
        for level in 0..levels.min(2) {
            let mut syms: *const u32 = ptr::null();
            // SAFETY: `map` is live and `syms` is a valid out pointer.
            let n = unsafe { (lib.get_syms)(map, key, 0, level, &raw mut syms) };
            let Ok(n) = usize::try_from(n) else { continue };
            if n == 0 || syms.is_null() {
                continue;
            }
            // SAFETY: libxkbcommon returned `n` keysyms at `syms`, owned by the keymap, which
            // is alive until after this loop iteration.
            let found = unsafe { std::slice::from_raw_parts(syms, n) };
            let Ok(code) = u16::try_from(key) else {
                continue;
            };
            for s in found {
                table.insert(code, level == 1, *s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_H: u16 = 43;
    const KEY_1: u16 = 10;
    const KEY_6: u16 = 15;
    const CYRILLIC_ER: u32 = 0x6d2; // XKB keysym for Cyrillic_er (р)

    fn have_xkb() -> bool {
        // A machine without libxkbcommon or without xkeyboard-config data skips, loudly.
        let ok = available()
            && build_table(&[LayoutSpec::parse("ru").unwrap()], "")
                .syms(KEY_H, false)
                .len()
                > 1;
        if !ok {
            eprintln!("SKIPPED: libxkbcommon or the ru layout data is missing");
        }
        ok
    }

    fn table(ids: &[&str]) -> KeysymTable {
        let specs: Vec<_> = ids.iter().filter_map(|i| LayoutSpec::parse(i)).collect();
        build_table(&specs, "")
    }

    #[test]
    fn layout_names_parse() {
        assert_eq!(
            LayoutSpec::parse("us+dvorak"),
            Some(LayoutSpec {
                layout: "us".into(),
                variant: "dvorak".into()
            })
        );
        assert_eq!(
            LayoutSpec::parse("ru").map(|l| l.variant),
            Some(String::new())
        );
        assert_eq!(LayoutSpec::parse(""), None);
        assert_eq!(LayoutSpec::parse("us;rm -rf"), None);
    }

    #[test]
    fn a_missing_library_still_gives_the_us_table() {
        // Whatever the machine has, an empty layout list is the US table.
        assert_eq!(build_table(&[], "").syms(KEY_H, false), vec![0x68]);
    }

    #[test]
    fn russian_h_key_types_cyrillic_er_and_keeps_h() {
        if !have_xkb() {
            return;
        }
        let t = table(&["us", "ru"]);
        assert_eq!(t.syms(KEY_H, false), vec![0x68, CYRILLIC_ER]);
        // Shift gives the capital letters of both.
        let up = t.syms(KEY_H, true);
        assert!(up.contains(&0x48) && up.len() == 2, "{up:x?}");
    }

    #[test]
    fn shift_digit_row_follows_the_layout() {
        if !have_xkb() {
            return;
        }
        let us = table(&["us"]);
        assert_eq!(us.syms(KEY_1, true), vec![0x21]);
        assert_eq!(us.syms(KEY_6, true), vec![0x5e]);
        // Russian: Shift+6 types a colon (0x3a), Shift+1 still an exclamation mark.
        let ru = table(&["ru"]);
        assert!(ru.syms(KEY_6, true).contains(&0x3a));
        assert!(ru.syms(KEY_1, true).contains(&0x21));
        // German: Shift+6 is ampersand, Shift+1 exclamation mark.
        let de = table(&["de"]);
        assert!(de.syms(KEY_6, true).contains(&0x26));
        // French: the digit row types symbols unshifted and digits with Shift.
        let fr = table(&["fr"]);
        assert!(fr.syms(KEY_6, true).contains(&0x36));
        assert!(fr.syms(KEY_6, false).contains(&0x2d));
    }

    #[test]
    fn dvorak_moves_letters() {
        if !have_xkb() {
            return;
        }
        let t = table(&["us+dvorak"]);
        // On Dvorak the physical H key types 'd'.
        assert!(t.syms(KEY_H, false).contains(&0x64));
    }

    #[test]
    fn a_refused_layout_is_skipped() {
        if !available() {
            return;
        }
        let t = table(&["us", "zz_no_such_layout"]);
        assert_eq!(t.syms(KEY_H, false), vec![0x68]);
    }
}
