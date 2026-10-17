// SPDX-License-Identifier: GPL-3.0-or-later
//! Keymap compilation (ADR-0005 7.1, 7.5): chord and command names to O(1) lookup tables, plus
//! the platform facts that change which keys are usable (U-1, 8.1).

use gorca_config::{ChordSpec, ContextName, Diag, DiagMsg, Loaded, Settings, did_you_mean};
use gorca_input_api::{
    Bound, Chord, ChordKey, CmdId, CompiledKeymap, KeyRef, KeyResolver, KeysymTable, LaneSettings,
    ModeKind, Modifiers, TableKind, keypad_digit_keysym, keysym_of_code, modifier_keysym, sym,
};

use super::spec::CommandTable;
use super::table::CoreCmd;
use crate::ext::{ExtCmdSpec, ExtCommands};

/// Facts about the desktop that the daemon probes (ADR-0005 8.1). Plain values: nothing here
/// reads the environment or settings (the daemon's `main` does, and passes the results in).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformFacts {
    /// GNOME's keyboard repeat delay (Mutter's double-tap window).
    pub repeat_delay_ms: u32,
    /// `org.gnome.desktop.input-sources xkb-options`, comma separated.
    pub xkb_options: String,
    /// 24-hour clock (`org.gnome.desktop.interface clock-format`).
    pub clock_24h: bool,
    /// What the keys of the configured input sources type (K-8b). Empty means US only.
    pub keysyms: KeysymTable,
}

impl Default for PlatformFacts {
    fn default() -> Self {
        Self {
            repeat_delay_ms: 500,
            xkb_options: String::new(),
            clock_24h: true,
            keysyms: KeysymTable::us(),
        }
    }
}

impl PlatformFacts {
    /// An XKB option makes Caps Lock switch the layout (`grp:*caps*`) or turns the key into
    /// something else (`caps:*`), so it cannot also be the gorca key (U-1).
    pub fn caps_switches_layout(&self) -> bool {
        self.options().any(|o| {
            ((o.starts_with("grp:") || o.starts_with("ctrl:")) && o.contains("caps"))
                || o.starts_with("caps:")
        })
    }

    /// A lone Shift tap switches the layout, so it cannot also pause speech (4.6).
    pub fn shift_switches_layout(&self) -> bool {
        self.options().any(|o| {
            matches!(
                o,
                "grp:lshift_toggle" | "grp:rshift_toggle" | "grp:shifts_toggle"
            )
        })
    }

    fn options(&self) -> impl Iterator<Item = &str> {
        self.xkb_options
            .split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty())
    }
}

/// Everything compiled from one load.
#[derive(Debug)]
pub struct Compiled {
    pub keymap: CompiledKeymap,
    pub lane: LaneSettings,
    /// Problems found while mapping names (KC-8, KC-9, unknown keys).
    pub diags: Vec<Diag>,
    /// Caps Lock was dropped as a gorca key because it switches the layout (U-1): say it once.
    pub caps_lock_dropped: bool,
}

/// The gorca modifier keysyms in effect, and whether Caps Lock was dropped.
pub fn gorca_modifiers(settings: &Settings, facts: &PlatformFacts) -> (Vec<u32>, bool) {
    let drop_caps = settings.keyboard.caps_lock_layout_guard && facts.caps_switches_layout();
    let mut dropped = false;
    let mut out = Vec::new();
    for name in &settings.keyboard.modifiers {
        let Some(ks) = modifier_keysym(name) else {
            continue;
        };
        if drop_caps && ks == sym::CAPS_LOCK {
            dropped = true;
            continue;
        }
        if !out.contains(&ks) {
            out.push(ks);
        }
    }
    (out, dropped)
}

pub fn lane_settings(settings: &Settings, facts: &PlatformFacts) -> LaneSettings {
    use gorca_config::InterruptPolicy;
    LaneSettings {
        multi_press_ms: settings.keyboard.multi_press_ms,
        double_tap_guard_ms: settings.keyboard.double_tap_guard_ms,
        repeat_delay_ms: facts.repeat_delay_ms,
        interrupt_on_keys: settings.keyboard.interrupt == InterruptPolicy::NonModifier,
        stop_speech_on_control: settings.keyboard.stop_speech_on_control,
        pause_speech_on_shift: settings.keyboard.pause_speech_on_shift
            && !facts.shift_switches_layout(),
    }
}

fn table_kind(ctx: ContextName) -> TableKind {
    match ctx {
        ContextName::Global => TableKind::Global,
        ContextName::Focus => TableKind::Focus,
        ContextName::Browse => TableKind::Browse,
        ContextName::Review => TableKind::Review,
    }
}

fn chord_key(spec: &ChordSpec, resolver: &dyn KeyResolver) -> Option<ChordKey> {
    let key = resolver.resolve(&spec.key)?;
    let mut mods = Modifiers::empty();
    if spec.gorca {
        mods |= Modifiers::GORCA;
    }
    if spec.mods.ctrl {
        mods |= Modifiers::CONTROL;
    }
    if spec.mods.alt {
        mods |= Modifiers::MOD1;
    }
    if spec.mods.shift {
        mods |= Modifiers::SHIFT;
    }
    if spec.mods.super_ {
        mods |= Modifiers::MOD4;
    }
    Some(ChordKey { mods, key })
}

fn bound_of(cmd: CoreCmd) -> Bound {
    let s = cmd.spec();
    Bound {
        cmd: cmd.id(),
        active: cmd.is_implemented(),
        repeatable: cmd.repeatable(),
        max_press: s.max_press,
        bypass: s.input_help_bypass,
        interrupts: s.interrupts,
    }
}

fn ext_bound(id: CmdId, s: &ExtCmdSpec) -> Bound {
    Bound {
        cmd: id,
        active: true,
        repeatable: false,
        max_press: s.max_press,
        bypass: false,
        interrupts: s.interrupts,
    }
}

/// The chords taken so far, by resolved identity.
#[derive(Default)]
struct Mentioned {
    resolved: std::collections::HashSet<(ChordKey, u8)>,
    unresolved: std::collections::HashSet<ChordSpec>,
}

impl Mentioned {
    /// False when an equivalent chord was already present.
    fn insert(&mut self, spec: &ChordSpec, resolver: &dyn KeyResolver) -> bool {
        match chord_key(spec, resolver) {
            Some(k) => self.resolved.insert((k, spec.presses)),
            None => self.unresolved.insert(spec.clone()),
        }
    }

    /// Reserve the presses a binding owns beyond its own: a command with `max_press = m` bound at
    /// `(chord, p)` also answers presses `p + 1..=m` (the input lane looks an explicit
    /// `(chord, n)` up first and falls back to the lower press with a repeat count), so an
    /// extension chord at any of those presses would replace them.
    fn reserve_presses(&mut self, spec: &ChordSpec, max_press: u8, resolver: &dyn KeyResolver) {
        let Some(k) = chord_key(spec, resolver) else {
            return;
        };
        for n in spec.presses.saturating_add(1)..=max_press {
            self.resolved.insert((k, n));
        }
    }
}

/// Compile a loaded configuration for the input lane, with the extension commands installed for
/// the process (`ExtCommands::install`).
pub fn compile(loaded: &Loaded, facts: &PlatformFacts, resolver: &dyn KeyResolver) -> Compiled {
    compile_with(loaded, facts, resolver, ExtCommands::installed())
}

/// [`compile`] with an explicit extension registry (tests).
pub fn compile_with(
    loaded: &Loaded,
    facts: &PlatformFacts,
    resolver: &dyn KeyResolver,
    ext: Option<&ExtCommands>,
) -> Compiled {
    let table = CommandTable::get();
    // Chords the preset or the user mention in *any* context: a context table is consulted before
    // `global`, so an extension chord in `focus` would shadow a core `global` binding of the
    // same chord. Precedence is user > core preset > extension.
    //
    // Chords are compared by what the resolver makes of them (key, modifiers, press count), not
    // by the key name as written: `enter`/`return`, `page_up`/`prior`, `kp_1`/`kp_end` are one
    // key. A chord whose key name does not resolve cannot bind anything, so it is compared by
    // spelling only to keep the diagnostics deterministic.
    let mut mentioned = Mentioned::default();
    let (mods, caps_lock_dropped) = gorca_modifiers(&loaded.settings, facts);
    let mut km = CompiledKeymap::new(mods);
    let mut diags = Vec::new();
    for (ctx, chords) in &loaded.keymap.contexts {
        for (spec, binding) in chords {
            mentioned.insert(spec, resolver);
            let path = format!("keymap.{}.\"{spec}\"", ctx.name());
            let Some(key) = chord_key(spec, resolver) else {
                diags.push(Diag::error(
                    path,
                    None,
                    DiagMsg::BadChord {
                        chord: spec.to_string(),
                        reason: "unknown key name",
                    },
                ));
                continue;
            };
            let bound = match binding {
                None => None,
                Some(name) => {
                    let resolved = match table.by_name(name) {
                        Some(cmd) => Some((cmd.spec().contexts, bound_of(cmd))),
                        None => ext
                            .and_then(|e| e.by_name(name))
                            .map(|(id, s)| (s.contexts, ext_bound(id, s))),
                    };
                    let Some((contexts, bound)) = resolved else {
                        let mut candidates: Vec<&str> = table.names().collect();
                        candidates.extend(ext.into_iter().flat_map(ExtCommands::names));
                        diags.push(Diag::error(
                            path,
                            None,
                            DiagMsg::UnknownCommand {
                                command: name.clone(),
                                suggestion: did_you_mean(name, candidates),
                            },
                        ));
                        continue;
                    };
                    if !contexts.allows(*ctx) {
                        diags.push(Diag::error(
                            path,
                            None,
                            DiagMsg::WrongContext {
                                command: name.clone(),
                                context: ctx.name(),
                            },
                        ));
                        continue;
                    }
                    Some(bound)
                }
            };
            if let Some(b) = &bound {
                mentioned.reserve_presses(spec, b.max_press, resolver);
            }
            km.insert(table_kind(*ctx), key, spec.presses, bound);
        }
    }
    // Extension default chords (ADR-0001 section 10): never over a chord the preset or the user
    // mentioned, tombstones included.
    if let Some(ext) = ext {
        for (ctx, chord, id, spec) in ext.default_bindings() {
            if !mentioned.insert(chord, resolver) {
                // Taken by the preset, the user (a tombstone counts) or an earlier extension.
                tracing::warn!(
                    ext = %spec.ext,
                    command = %spec.name,
                    %chord,
                    "extension default chord not bound: the chord is already in use"
                );
                continue;
            }
            let Some(key) = chord_key(chord, resolver) else {
                diags.push(Diag::error(
                    format!("extension `{}` default chord `{chord}`", spec.ext),
                    None,
                    DiagMsg::BadChord {
                        chord: chord.to_string(),
                        reason: "unknown key name",
                    },
                ));
                continue;
            };
            km.insert(
                table_kind(ctx),
                key,
                chord.presses,
                Some(ext_bound(id, spec)),
            );
        }
    }
    km.stop = Some(CoreCmd::SpeechStop.id());
    km.pause = Some(CoreCmd::SpeechPauseToggle.id());
    km.learn_toggle = Some(CoreCmd::InputLearnModeToggle.id());
    Compiled {
        keymap: km,
        lane: lane_settings(&loaded.settings, facts),
        diags,
        caps_lock_dropped,
    }
}

/// The keystroke grabs the non-gorca chords of `mode` need (ADR-0005 K-8a, K-6a, K-8b; ADR-0006 11).
///
/// Chords with the gorca modifier need no grab of their own: Mutter swallows every key while a
/// grabbed modifier is held (KM-4). Every other implemented chord is compiled from
/// `(keycode, mods)` into the keysyms and state Mutter will see. Mutter matches the keysym the
/// *active layout* produced, so a physical key is grabbed under every keysym it types in any
/// configured layout (`table`, K-8b): `h` and `Cyrillic_er` for the quick-navigation key H. Returns
/// `(plain, exact)`: plain chords are expanded over the Caps Lock and Num Lock states on the wire;
/// keypad chords are exact, because the keysym of a keypad key depends on Num Lock and Mutter
/// matches the exact keysym and state (platform check V-1, 2026-10-14):
///
/// | Num Lock | plain `KP7` | `Shift+KP7` |
/// |---|---|---|
/// | off | `(KP_Home, 0)`, grabbed | `(KP_Home, Shift)`, grabbed |
/// | on | `(KP_7, Mod2)`, **not** grabbed: the digit reaches the app | `(KP_Home, Shift+Mod2)`, **not** grabbed |
///
/// Each grabbed variant also exists with Caps Lock set.
pub fn chord_grabs(
    km: &CompiledKeymap,
    mode: ModeKind,
    table: &KeysymTable,
) -> (Vec<Chord>, Vec<Chord>) {
    let g = grabs_where(km, mode, table, |_| true);
    (g.plain, g.exact)
}

/// The grabs of the browse keymap (browse design 7.4): `(plain, exact)` chords for every active
/// browse binding except Escape, which is grabbed only in auto focus mode.
pub fn browse_grabs(km: &CompiledKeymap, table: &KeysymTable) -> (Vec<Chord>, Vec<Chord>) {
    let esc = CoreCmd::BrowseEscape.id();
    let g = grabs_where(km, ModeKind::Browse, table, |b| b.cmd != esc);
    (g.plain, g.exact)
}

/// The bare single-letter navigation chords: the quick-navigation and container commands, which
/// `browse.single_letter_toggle` releases to the page.
pub fn letter_grabs(km: &CompiledKeymap, table: &KeysymTable) -> Vec<Chord> {
    let g = grabs_where(km, ModeKind::Browse, table, |b| {
        let name = CoreCmd::from_id(b.cmd).map_or("", |c| c.name());
        name.starts_with("quicknav.") || name.starts_with("browse.container_")
    });
    g.plain
}

/// The chords of `browse.tab_next` and `browse.tab_prev` (D-2): grabbed only while the browse
/// cursor has a focusable target in that direction (the Reader decides, from the mirror).
pub fn tab_grabs(km: &CompiledKeymap, table: &KeysymTable) -> (Vec<Chord>, Vec<Chord>) {
    let of = |name: &'static str| {
        grabs_where(km, ModeKind::Browse, table, |b| {
            CoreCmd::from_id(b.cmd).is_some_and(|c| c.name() == name)
        })
        .plain
    };
    (of("browse.tab_next"), of("browse.tab_prev"))
}

/// The chords bound to `browse.escape`, for the auto-focus-mode grab set.
pub fn escape_grabs(km: &CompiledKeymap, table: &KeysymTable) -> Vec<Chord> {
    let esc = CoreCmd::BrowseEscape.id();
    grabs_where(km, ModeKind::Browse, table, |b| b.cmd == esc).plain
}

/// Grabs the layout union left out because the keysym is also typed by a different, unbound
/// key of another layout (K-8b): grabbing it would swallow that key for the application.
pub fn grab_collisions(km: &CompiledKeymap, mode: ModeKind, table: &KeysymTable) -> Vec<Chord> {
    grabs_where(km, mode, table, |_| true).dropped
}

/// The result of one grab computation.
struct Grabs {
    plain: Vec<Chord>,
    exact: Vec<Chord>,
    dropped: Vec<Chord>,
}

fn grabs_where(
    km: &CompiledKeymap,
    mode: ModeKind,
    table: &KeysymTable,
    keep: impl Fn(&Bound) -> bool,
) -> Grabs {
    let mut plain: Vec<Chord> = Vec::new();
    let mut exact: Vec<Chord> = Vec::new();
    let mut dropped: Vec<Chord> = Vec::new();
    let active = km.active_chords(mode);
    // The physical keys this mode binds (with their Shift level): a keysym of another layout
    // that lands on one of them is safe to grab, the lane resolves it by keycode.
    let bound: std::collections::HashSet<(u16, bool)> = active
        .iter()
        .filter(|(ck, _, b)| !ck.mods.contains(Modifiers::GORCA) && keep(b))
        .filter_map(|(ck, _, _)| match ck.key {
            KeyRef::Code(c) => Some((c, ck.mods.without_locks().contains(Modifiers::SHIFT))),
            KeyRef::Sym(_) => None,
        })
        .collect();
    for (ck, _, bound_cmd) in active {
        if ck.mods.contains(Modifiers::GORCA) || !keep(&bound_cmd) {
            continue;
        }
        let mods = ck.mods.without_locks();
        let shift = mods.contains(Modifiers::SHIFT);
        let (code, code_sym, sym_direct) = match ck.key {
            KeyRef::Code(code) => (Some(code), keysym_of_code(code), None),
            KeyRef::Sym(s) => (None, None, Some(s)),
        };
        if let Some(s) = sym_direct {
            plain.push(Chord {
                keysym: s,
                modifiers: mods,
            });
            continue;
        }
        let Some(nav) = code_sym else { continue };
        let operator = (0xffaa..=0xffaf).contains(&nav) || nav == 0xff8d;
        if operator {
            // The keysym does not depend on Num Lock; K-6a still leaves the Num Lock-on states
            // alone, so the keypad types normally while Num Lock is on.
            for lock in [Modifiers::empty(), Modifiers::LOCK] {
                exact.push(Chord {
                    keysym: nav,
                    modifiers: mods | lock,
                });
            }
        } else if keypad_digit_keysym(nav).is_some() {
            // Num Lock off only (K-6a, KM-2): with Num Lock on the keypad types digits into the
            // application, so `(KP_7, Mod2)` and the like are never grabbed. Shift does not change
            // the keysym (V-1): `Shift+KP7` is `(KP_Home, Shift)`.
            for lock in [Modifiers::empty(), Modifiers::LOCK] {
                exact.push(Chord {
                    keysym: nav,
                    modifiers: mods | lock,
                });
            }
        } else {
            // What the key types with or without Shift in every configured layout; a letter with
            // Shift types the capital, the digit row its symbol. The lane still matches by keycode.
            let Some(code) = code else { continue };
            let baseline = KeysymTable::us_sym(code, shift).unwrap_or(nav);
            let mut syms = table.syms(code, shift);
            if !syms.contains(&baseline) {
                syms.push(baseline);
            }
            for keysym in syms {
                // Browse mode swallows the unbound keys it shares a keysym with: in browse mode
                // no key is typed text (the fields switch to focus mode, which grabs none of
                // these), and Shift+2 on the Russian layout (a double quote, also typed by
                // Shift+apostrophe on the US layout) must stay a quick-navigation key.
                if keysym != baseline
                    && mode != ModeKind::Browse
                    && let Some(other) = table.typed_elsewhere(code, shift, keysym)
                    && !bound.contains(&(other, shift))
                {
                    dropped.push(Chord {
                        keysym,
                        modifiers: mods,
                    });
                    continue;
                }
                plain.push(Chord {
                    keysym,
                    modifiers: mods,
                });
            }
        }
    }
    plain.sort_by_key(|c| (c.keysym, c.modifiers.bits()));
    plain.dedup();
    exact.sort_by_key(|c| (c.keysym, c.modifiers.bits()));
    exact.dedup();
    dropped.sort_by_key(|c| (c.keysym, c.modifiers.bits()));
    dropped.dedup();
    Grabs {
        plain,
        exact,
        dropped,
    }
}

/// The command a chord is bound to in a context, by name (tests and learn-mode helpers).
pub fn command_id(name: &str) -> Option<CmdId> {
    CommandTable::get().by_name(name).map(CoreCmd::id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_config::{Presets, load};
    use gorca_input_api::{Chord, KeyRef, ModeKind, StaticResolver, static_keycode};

    fn compiled(text: &str) -> Compiled {
        compile(
            &load(Some(text), &Presets::builtin()),
            &PlatformFacts::default(),
            &StaticResolver,
        )
    }

    fn ext_registry() -> ExtCommands {
        use super::super::spec::ContextMask;
        let mut r = ExtCommands::new();
        for (name, local) in [("ext.t.hello", 1), ("ext.t.other", 2)] {
            r.register(ExtCmdSpec {
                name: name.to_owned(),
                ext: "t".to_owned(),
                local,
                help: "h".to_owned(),
                contexts: ContextMask::of(&[ContextName::Focus]),
                max_press: 1,
                interrupts: true,
            })
            .unwrap();
        }
        r.bind(ContextName::Focus, "gorca+shift+f9", "ext.t.hello")
            .unwrap();
        // A default that collides with a chord the user binds elsewhere must lose.
        r.bind(ContextName::Focus, "gorca+f11", "ext.t.other")
            .unwrap();
        r
    }

    fn compiled_ext(text: &str, ext: &ExtCommands) -> Compiled {
        compile_with(
            &load(Some(text), &Presets::builtin()),
            &PlatformFacts::default(),
            &StaticResolver,
            Some(ext),
        )
    }

    #[test]
    fn extension_defaults_merge_without_overriding_the_user_or_the_preset() {
        let ext = ext_registry();
        let c = compiled_ext("", &ext);
        let hello = ext.by_name("ext.t.hello").unwrap().0;
        let b = c
            .keymap
            .lookup(ModeKind::Focus, key(true, Modifiers::SHIFT, "f9"), 1)
            .expect("the default chord is bound");
        assert_eq!(b.cmd, hello);
        assert!(b.active);
        assert!(c.diags.is_empty(), "{:?}", c.diags);
        // The user's tombstone for the same chord wins over the extension's default.
        let c = compiled_ext("[keymap.focus]\n\"gorca+shift+f9\" = \"\"\n", &ext);
        assert!(
            c.keymap
                .lookup(ModeKind::Focus, key(true, Modifiers::SHIFT, "f9"), 1)
                .is_none()
        );
    }

    #[test]
    fn an_extension_default_never_shadows_a_binding_of_another_context() {
        let ext = ext_registry();
        // The user binds gorca+f11 in `global`; the extension default is in `focus`, which is
        // consulted first, so it would shadow the user's binding. It must not be installed.
        let c = compiled_ext("[keymap.global]\n\"gorca+f11\" = \"speech.stop\"\n", &ext);
        let b = c
            .keymap
            .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "f11"), 1)
            .expect("the user's binding stays");
        assert_eq!(b.cmd, CoreCmd::SpeechStop.id());
    }

    #[test]
    fn two_extensions_claiming_one_chord_first_wins() {
        use super::super::spec::ContextMask;
        let mut r = ExtCommands::new();
        for (name, ext) in [("ext.a.go", "a"), ("ext.b.go", "b")] {
            r.register(ExtCmdSpec {
                name: name.to_owned(),
                ext: ext.to_owned(),
                local: 1,
                help: "h".to_owned(),
                contexts: ContextMask::of(&[ContextName::Focus]),
                max_press: 1,
                interrupts: true,
            })
            .unwrap();
            r.bind(ContextName::Focus, "gorca+f8", name).unwrap();
        }
        let c = compiled_ext("", &r);
        let first = r.by_name("ext.a.go").unwrap().0;
        assert_eq!(
            c.keymap
                .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "f8"), 1)
                .map(|b| b.cmd),
            Some(first)
        );
    }

    #[test]
    fn users_bind_extension_commands_by_name_and_typos_get_a_suggestion() {
        let ext = ext_registry();
        let c = compiled_ext("[keymap.focus]\n\"gorca+f10\" = \"ext.t.other\"\n", &ext);
        let other = ext.by_name("ext.t.other").unwrap().0;
        assert_eq!(
            c.keymap
                .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "f10"), 1)
                .map(|b| b.cmd),
            Some(other)
        );
        let c = compiled_ext("[keymap.focus]\n\"gorca+f10\" = \"ext.t.helo\"\n", &ext);
        let text = c.diags.iter().map(ToString::to_string).collect::<String>();
        assert!(text.contains("ext.t.hello"), "{text}");
        // Not in this context.
        let c = compiled_ext("[keymap.review]\n\"gorca+f10\" = \"ext.t.hello\"\n", &ext);
        assert!(!c.diags.is_empty());
    }

    fn key(gorca: bool, mods: Modifiers, name: &str) -> ChordKey {
        ChordKey {
            mods: mods
                | if gorca {
                    Modifiers::GORCA
                } else {
                    Modifiers::empty()
                },
            key: KeyRef::Code(static_keycode(name).unwrap()),
        }
    }

    /// Every shipped preset maps to commands without a single diagnostic.
    #[test]
    fn every_preset_compiles_clean() {
        for preset in ["nvda", "orca"] {
            for layout in ["desktop", "laptop"] {
                let c = compiled(&format!(
                    "[keyboard]\npreset=\"{preset}\"\nlayout=\"{layout}\"\n"
                ));
                assert!(c.diags.is_empty(), "{preset} {layout}: {:?}", c.diags);
                assert!(c.keymap.len() > 100);
            }
        }
    }

    /// Ctrl+C and the other clipboard and undo shortcuts of applications are never grabbed, in
    /// any mode, preset, layout or keyboard layout (a Cyrillic keysym included): the grab set
    /// would swallow them before the application sees them. Ctrl alone is never a grabbed
    /// modifier either (it is only watched for "stop speech"). Learn mode (`grab_all`) is the
    /// one deliberate exception and is not a chord set.
    #[test]
    fn app_clipboard_shortcuts_are_never_grabbed_and_ctrl_is_not_a_grab_modifier() {
        let keys = ["c", "x", "v", "z", "y", "insert", "delete"];
        let table = us_ru();
        let mut protected: Vec<u32> = Vec::new();
        for name in keys {
            let code = static_keycode(name).expect(name);
            for shift in [false, true] {
                protected.extend(table.syms(code, shift));
                protected.extend(KeysymTable::us_sym(code, shift));
            }
        }
        // The Russian layout types Cyrillic on the C and V keys (es, em): the keysym tables of
        // a real session do, so cover them explicitly as well.
        for (code, lower, upper) in [(46, 0x6d3, 0x6f3), (47, 0x6d7, 0x6f7)] {
            let mut t = KeysymTable::us();
            t.insert(code, false, lower);
            t.insert(code, true, upper);
            for shift in [false, true] {
                protected.extend(t.syms(code, shift));
            }
        }
        for preset in ["nvda", "orca"] {
            for layout in ["desktop", "laptop"] {
                let c = compiled(&format!(
                    "[keyboard]\npreset=\"{preset}\"\nlayout=\"{layout}\"\n"
                ));
                let loaded = load(
                    Some(&format!(
                        "[keyboard]\npreset=\"{preset}\"\nlayout=\"{layout}\"\n"
                    )),
                    &Presets::builtin(),
                );
                let (mods, _) = gorca_modifiers(&loaded.settings, &PlatformFacts::default());
                // Control_L, Control_R
                assert!(
                    !mods.iter().any(|m| matches!(*m, 0xffe3 | 0xffe4)),
                    "Ctrl must not be a gorca modifier: {mods:x?}"
                );
                for mode in [ModeKind::Focus, ModeKind::Review, ModeKind::Browse] {
                    let g = grabs_where(&c.keymap, mode, &table, |_| true);
                    for ch in g.plain.iter().chain(g.exact.iter()) {
                        let m = ch.modifiers.without_locks();
                        let ctrl_only = m.contains(Modifiers::CONTROL)
                            && !m.intersects(Modifiers::MOD1 | Modifiers::MOD4);
                        assert!(
                            !(ctrl_only && protected.contains(&ch.keysym)),
                            "{preset} {layout} {mode:?}: grabs {:#x} with {m:?}, an application \
                             shortcut",
                            ch.keysym
                        );
                    }
                }
            }
        }
    }

    /// The preset oracle (docs/design/modes.md 5): both presets bind every M1 command that has a
    /// chord in the source screen reader. Watched gestures (stop, pause) have none.
    #[test]
    fn presets_bind_every_m1_chord_command() {
        let chordless = [CoreCmd::SpeechStop, CoreCmd::SpeechPauseToggle];
        let direct = [
            CoreCmd::SpeechRateUp,
            CoreCmd::SpeechRateDown,
            CoreCmd::SpeechPitchUp,
            CoreCmd::SpeechPitchDown,
            CoreCmd::SpeechVolumeUp,
            CoreCmd::SpeechVolumeDown,
        ];
        for text in ["", "[keyboard]\npreset=\"orca\"\n"] {
            let l = load(Some(text), &Presets::builtin());
            let bound: std::collections::HashSet<&str> =
                l.keymap.bound().map(|(_, _, c)| c).collect();
            for cmd in CoreCmd::ALL {
                if !cmd.is_implemented() || chordless.contains(&cmd) {
                    continue;
                }
                if direct.contains(&cmd)
                    || (text.contains("orca") && cmd.name().starts_with("speech.ring_"))
                {
                    // Unbound in the presets by design (modes.md 4.2, 5).
                    assert!(!bound.contains(cmd.name()), "{}", cmd.name());
                    continue;
                }
                // Commands the source screen reader has no chord for (modes.md 4.3, 5): NVDA has
                // no review toggle, Orca has no mode switch, focus move or flow moves.
                // M3: both source readers read the character, word and line under the caret
                // with the review keys, so the `text.say_*` and `text.spell_*` commands have no
                // chord of their own; Orca has no punctuation cycle or current-line chord either.
                if cmd.name().starts_with("text.say_c")
                    || cmd.name().starts_with("text.say_w")
                    || cmd.name().starts_with("text.say_l")
                    || cmd.name().starts_with("text.spell_")
                {
                    continue;
                }
                let by_design: &[&str] = if text.contains("orca") {
                    &[
                        "info.current_line",
                        "speech.punctuation_cycle",
                        "review.simple_toggle",
                        "review.mode_next",
                        "review.mode_prev",
                        "review.to_focus",
                        "review.move_focus",
                        "review.start_of_line",
                        "object.next_in_flow",
                        "object.prev_in_flow",
                        // Orca has no key for frames.
                        "quicknav.next.frame",
                        "quicknav.prev.frame",
                        // Neither source reader binds articles by default.
                        "quicknav.next.article",
                        "quicknav.prev.article",
                        // NVDA-only browse keys: Orca has no virtual buffer (spec D-10).
                        "quicknav.next.not_link_block",
                        "quicknav.prev.not_link_block",
                        "quicknav.next.annotation",
                        "quicknav.prev.annotation",
                        "quicknav.next.error",
                        "quicknav.prev.error",
                        "quicknav.next.paragraph",
                        "quicknav.prev.paragraph",
                        "browse.container_start",
                        "browse.container_end",
                        "browse.single_letter_toggle",
                        "browse.cursor_next_paragraph",
                        "browse.cursor_prev_paragraph",
                        "browse.cursor_next_sentence",
                        "browse.cursor_prev_sentence",
                        "browse.select_next_char",
                        "browse.select_prev_char",
                        "browse.select_next_word",
                        "browse.select_prev_word",
                        "browse.select_next_line",
                        "browse.select_prev_line",
                        "browse.select_line_start",
                        "browse.select_line_end",
                        "browse.select_top",
                        "browse.select_bottom",
                        "browse.select_page_up",
                        "browse.select_page_down",
                        "browse.select_all",
                        // Orca leaves Tab to the application and has no native selection mode.
                        "browse.tab_next",
                        "browse.tab_prev",
                        "browse.native_selection_toggle",
                        // Orca moves cell by cell and reads a row or a column; it has no
                        // first/last row or column and no say-all of a row.
                        "table.first_row",
                        "table.last_row",
                        "table.first_column",
                        "table.last_column",
                        "table.say_all_row",
                        "table.say_all_column",
                    ]
                } else {
                    &[
                        "review.simple_toggle",
                        "review.toggle",
                        "quicknav.next.article",
                        "quicknav.prev.article",
                    ]
                };
                if by_design.contains(&cmd.name()) {
                    continue;
                }
                assert!(
                    bound.contains(cmd.name()),
                    "{} unbound in {text:?}",
                    cmd.name()
                );
            }
        }
    }

    #[test]
    fn nvda_desktop_where_am_i_is_gorca_tab_and_planned_commands_are_inert() {
        let c = compiled("");
        let b = c
            .keymap
            .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "tab"), 1)
            .unwrap();
        assert_eq!(b.cmd, CoreCmd::InfoWhereAmI.id());
        assert!(b.active);
        let planned = c
            .keymap
            .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "b"), 1)
            .unwrap();
        assert_eq!(planned.cmd, CoreCmd::InfoReadWindow.id());
        assert!(!planned.active, "M4 command: bound but inert (KC-10)");
        // Only implemented chords are grabbed (K-1).
        assert!(
            c.keymap
                .active_chords(ModeKind::Focus)
                .iter()
                .all(|(_, _, b)| b.active)
        );
    }

    #[test]
    fn user_bindings_unknown_names_and_wrong_contexts_are_diagnosed() {
        let c = compiled(
            "[keymap.global]\n\"gorca+w\" = \"info.window_titel\"\n\"gorca+u\" = \"quicknav.next.heading\"\n\"gorca+F9\" = \"info.time_date\"\n",
        );
        assert_eq!(c.diags.len(), 2, "{:?}", c.diags);
        assert!(
            c.diags[0]
                .to_string()
                .contains("did you mean `info.window_title`")
                || c.diags[1]
                    .to_string()
                    .contains("did you mean `info.window_title`")
        );
        let b = c
            .keymap
            .lookup(ModeKind::Focus, key(true, Modifiers::empty(), "f9"), 1)
            .unwrap();
        assert_eq!(b.cmd, CoreCmd::InfoTimeDate.id());
    }

    #[test]
    fn caps_lock_is_dropped_when_it_switches_the_layout() {
        let l = load(None, &Presets::builtin());
        let (mods, dropped) = gorca_modifiers(&l.settings, &PlatformFacts::default());
        assert_eq!(mods, vec![sym::INSERT, sym::KP_INSERT, sym::CAPS_LOCK]);
        assert!(!dropped);
        let facts = PlatformFacts {
            xkb_options: "ctrl:nocaps, grp:caps_toggle".into(),
            ..PlatformFacts::default()
        };
        let (mods, dropped) = gorca_modifiers(&l.settings, &facts);
        assert_eq!(mods, vec![sym::INSERT, sym::KP_INSERT]);
        assert!(dropped);
        // The guard can be switched off.
        let l2 = load(
            Some("[keyboard]\ncaps_lock_layout_guard = false\n"),
            &Presets::builtin(),
        );
        assert_eq!(gorca_modifiers(&l2.settings, &facts).0.len(), 3);
        // Caps remapped to something else is no gorca key either.
        let remap = PlatformFacts {
            xkb_options: "caps:ctrl_modifier".into(),
            ..PlatformFacts::default()
        };
        assert!(remap.caps_switches_layout());
        assert!(
            PlatformFacts {
                xkb_options: "ctrl:nocaps".into(),
                ..PlatformFacts::default()
            }
            .caps_switches_layout()
        );
        assert!(
            !PlatformFacts {
                xkb_options: "lv3:ralt_switch".into(),
                ..PlatformFacts::default()
            }
            .caps_switches_layout()
        );
    }

    #[test]
    fn shift_tap_pause_is_disabled_by_shift_layout_toggles() {
        let s = Settings::default();
        assert!(lane_settings(&s, &PlatformFacts::default()).pause_speech_on_shift);
        let facts = PlatformFacts {
            xkb_options: "grp:shifts_toggle".into(),
            ..PlatformFacts::default()
        };
        assert!(!lane_settings(&s, &facts).pause_speech_on_shift);
        let alt = PlatformFacts {
            xkb_options: "grp:alt_shift_toggle".into(),
            ..PlatformFacts::default()
        };
        assert!(lane_settings(&s, &alt).pause_speech_on_shift);
    }

    fn grabs(text: &str) -> (Vec<Chord>, Vec<Chord>) {
        let c = compiled(text);
        chord_grabs(&c.keymap, ModeKind::Focus, &KeysymTable::us())
    }

    const KP_HOME: u32 = 0xff95;
    const KP_7: u32 = 0xffb7;
    const KP_UP: u32 = 0xff97;
    const KP_8: u32 = 0xffb8;
    const KP_ADD: u32 = 0xffab;

    fn has(set: &[Chord], keysym: u32, mods: Modifiers) -> bool {
        set.contains(&Chord {
            keysym,
            modifiers: mods,
        })
    }

    /// V-1 (2026-10-14): Shift does not change a keypad keysym; Num Lock does. Mutter matches the
    /// exact keysym and state. F-1 (review): only the Num Lock off pairs are grabbed, so the digits
    /// reach applications while Num Lock is on (K-6a, KM-2).
    #[test]
    fn desktop_keypad_review_keys_are_grabbed_for_num_lock_off_only() {
        let (plain, exact) = grabs("");
        // KP7 = review.prev_line.
        assert!(has(&exact, KP_HOME, Modifiers::empty()));
        assert!(has(&exact, KP_HOME, Modifiers::LOCK));
        // Shift+KP7 = review.top: KP_Home with Shift.
        assert!(has(&exact, KP_HOME, Modifiers::SHIFT));
        // F-1: nothing with Num Lock on (Mod2) is grabbed, whatever the keysym.
        assert!(
            !exact.iter().any(|c| c.modifiers.contains(Modifiers::MOD2)),
            "Num Lock on must type digits: {exact:?}"
        );
        assert!(!exact.iter().any(|c| c.keysym == KP_7 || c.keysym == KP_8));
        assert!(has(&exact, KP_UP, Modifiers::empty()));
        // M4b commands stay ungrabbed (K-1): KP_Add is review.say_all, not implemented yet.
        assert!(!has(&plain, KP_ADD, Modifiers::empty()));
        assert!(!exact.iter().any(|c| c.keysym == KP_ADD));
        // gorca chords need no grab of their own.
        assert!(plain.is_empty(), "{plain:?}");
    }

    /// F-1 end to end through the Mutter model: with Num Lock on, `KP7` is `(KP_7, Mod2)`, which is
    /// not grabbed, so it is not swallowed and reaches the application; with Num Lock off it is
    /// grabbed.
    #[test]
    fn a_digit_typed_with_num_lock_on_reaches_the_application() {
        use gorca_input_api::{Ev, Replica};
        let (plain, exact) = grabs("");
        let mut set = gorca_input_api::GrabSet::default();
        set.chords.extend(plain);
        set.exact.extend(exact);
        let ev = |keysym, state: Modifiers| Ev {
            keysym,
            pressed: true,
            repeat: false,
            state,
            time_ms: 1_000,
        };
        let mut r = Replica::default();
        assert!(
            !r.step(&ev(KP_7, Modifiers::MOD2), &set, false).swallowed,
            "the digit reaches the app"
        );
        assert!(
            !r.step(
                &ev(KP_HOME, Modifiers::SHIFT | Modifiers::MOD2),
                &set,
                false
            )
            .swallowed,
            "Home reaches the app"
        );
        let mut r = Replica::default();
        assert!(
            r.step(&ev(KP_HOME, Modifiers::empty()), &set, false)
                .swallowed
        );
    }

    #[test]
    fn the_wire_form_of_a_keypad_chord_has_only_num_lock_off_pairs() {
        let (plain, exact) = grabs("");
        let mut set = gorca_input_api::GrabSet::default();
        set.chords.extend(plain);
        set.exact.extend(exact);
        let (_, strokes) = set.to_wire();
        let shift = Modifiers::SHIFT.bits();
        let mod2 = Modifiers::MOD2.bits();
        assert!(strokes.contains(&(KP_HOME, 0)));
        assert!(strokes.contains(&(KP_HOME, shift)));
        assert!(
            !strokes.iter().any(|(_, m)| m & mod2 != 0),
            "no Num Lock on grab on the wire"
        );
        assert!(set.contains(KP_HOME, Modifiers::empty()));
        assert!(
            !set.contains(KP_7, Modifiers::MOD2),
            "KP_7 with Num Lock on is the app's"
        );
    }

    /// F-6: a held review key keeps moving; the lane only runs repeats of `repeatable` commands.
    #[test]
    fn held_review_moves_repeat_and_one_shot_commands_do_not() {
        let c = compiled("");
        let b = c
            .keymap
            .lookup(
                ModeKind::Focus,
                key(false, Modifiers::empty(), "kp_prior"),
                1,
            )
            .unwrap();
        assert_eq!(b.cmd, CoreCmd::ReviewNextLine.id());
        assert!(b.repeatable && b.active);
        let top = c
            .keymap
            .lookup(ModeKind::Focus, key(false, Modifiers::SHIFT, "kp_prior"), 1)
            .unwrap();
        assert!(!top.repeatable, "review.bottom is one-shot");
        assert!(!CoreCmd::ObjectCurrent.repeatable());
        assert!(!CoreCmd::ReviewActivate.repeatable());
    }

    #[test]
    fn laptop_layouts_and_gorca_chords_grab_nothing_extra() {
        let (plain, exact) = grabs("[keyboard]\nlayout=\"laptop\"\n");
        assert!(plain.is_empty() && exact.is_empty());
    }

    #[test]
    fn the_orca_desktop_preset_grabs_its_bare_keypad_keys() {
        let (_, exact) = grabs("[keyboard]\npreset=\"orca\"\n");
        // review.toggle is KP_Subtract; KP_Home is review.prev_line.
        assert!(exact.iter().any(|c| c.keysym == KP_HOME));
    }

    #[test]
    fn explicit_press_counts_are_compiled() {
        let c = compiled("[keyboard]\npreset=\"orca\"\n");
        let k = ChordKey {
            mods: Modifiers::GORCA,
            key: KeyRef::Code(static_keycode("kp_enter").unwrap()),
        };
        assert_eq!(
            c.keymap.lookup(ModeKind::Focus, k, 1).unwrap().cmd,
            CoreCmd::InfoWindowTitle.id()
        );
        assert_eq!(
            c.keymap.lookup(ModeKind::Focus, k, 2).unwrap().cmd,
            CoreCmd::InfoStatusBar.id()
        );
    }

    #[test]
    fn browse_grabs_cover_letters_shifted_digits_and_leave_escape_to_focus_mode() {
        let c = compiled("[keyboard]\npreset=\"nvda\"\nlayout=\"desktop\"\n");
        let (plain, _exact) = browse_grabs(&c.keymap, &KeysymTable::us());
        let has =
            |sym: u32, m: Modifiers| plain.iter().any(|ch| ch.keysym == sym && ch.modifiers == m);
        assert!(has(0x68, Modifiers::empty()), "h");
        assert!(has(0x48, Modifiers::SHIFT), "Shift+h arrives as H");
        assert!(has(0x32, Modifiers::empty()), "2");
        assert!(has(0x40, Modifiers::SHIFT), "Shift+2 arrives as @");
        assert!(has(0xff54, Modifiers::empty()), "Down");
        assert!(
            has(0xff54, Modifiers::CONTROL) || has(0xff53, Modifiers::CONTROL),
            "Ctrl+arrows"
        );
        assert!(
            !has(0xff1b, Modifiers::empty()),
            "Escape only in focus mode"
        );
        assert!(
            escape_grabs(&c.keymap, &KeysymTable::us())
                .iter()
                .any(|ch| ch.keysym == 0xff1b && ch.modifiers.is_empty())
        );
    }

    /// A table as libxkbcommon would build it for `us` + `ru` (the letters of the home rows and
    /// the digit row that the tests need).
    fn us_ru() -> KeysymTable {
        let mut t = KeysymTable::us();
        // Physical H, J, K: Cyrillic er, o, el and their capitals.
        for (code, lower, upper) in [(43, 0x6d2, 0x6f2), (44, 0x6cf, 0x6ef), (45, 0x6cc, 0x6ec)] {
            t.insert(code, false, lower);
            t.insert(code, true, upper);
        }
        // Russian Shift+6 types a colon, Shift+4 a semicolon.
        t.insert(15, true, 0x3a);
        t.insert(13, true, 0x3b);
        t
    }

    #[test]
    fn russian_layout_grabs_the_cyrillic_keysyms_of_the_quick_nav_keys() {
        let c = compiled("[keyboard]\npreset=\"nvda\"\nlayout=\"desktop\"\n");
        let (plain, _) = browse_grabs(&c.keymap, &us_ru());
        let has =
            |sym: u32, m: Modifiers| plain.iter().any(|ch| ch.keysym == sym && ch.modifiers == m);
        assert!(has(0x68, Modifiers::empty()), "h stays grabbed (US layout)");
        assert!(has(0x6d2, Modifiers::empty()), "Cyrillic_er on the H key");
        assert!(
            has(0x6f2, Modifiers::SHIFT),
            "Shift+H on the Russian layout"
        );
        assert!(has(0x48, Modifiers::SHIFT), "and on the US layout");
        // Shift+6 (heading level 6 backwards) follows the layout: ^ on US, : on Russian.
        assert!(has(0x5e, Modifiers::SHIFT));
        assert!(has(0x3a, Modifiers::SHIFT));
        // Without a table nothing but the US keysyms is grabbed.
        let (us_only, _) = browse_grabs(&c.keymap, &KeysymTable::us());
        assert!(!us_only.iter().any(|ch| ch.keysym == 0x6d2));
        assert!(
            plain.len() > us_only.len(),
            "the union is larger than the US set"
        );
    }

    #[test]
    fn browse_mode_keeps_a_keysym_that_another_layouts_unbound_key_also_types() {
        let c = compiled("[keyboard]\npreset=\"nvda\"\nlayout=\"desktop\"\n");
        let mut t = us_ru();
        // A made-up layout types Cyrillic_er on physical Y too; Y is not a browse binding.
        // Browse mode keeps the grab (Y is swallowed there, which costs nothing: browse mode
        // types no text), so Shift+2 on the Russian layout can be a quick-navigation key.
        t.insert(29, false, 0x6d2);
        let (plain, _) = browse_grabs(&c.keymap, &t);
        assert!(
            plain
                .iter()
                .any(|ch| ch.keysym == 0x6d2 && ch.modifiers.is_empty())
        );
        assert!(grab_collisions(&c.keymap, ModeKind::Browse, &t).is_empty());
    }

    #[test]
    fn container_keys_and_the_letter_subset_are_grabbed() {
        let c = compiled("[keyboard]\npreset=\"nvda\"\nlayout=\"desktop\"\n");
        let (plain, _) = browse_grabs(&c.keymap, &KeysymTable::us());
        let has = |v: &[Chord], sym: u32, m: Modifiers| {
            v.iter().any(|ch| ch.keysym == sym && ch.modifiers == m)
        };
        assert!(has(&plain, 0x2c, Modifiers::empty()), "comma");
        assert!(has(&plain, 0x3c, Modifiers::SHIFT), "Shift+comma is less");
        let letters = letter_grabs(&c.keymap, &KeysymTable::us());
        assert!(has(&letters, 0x68, Modifiers::empty()), "h is a letter key");
        assert!(has(&letters, 0x2c, Modifiers::empty()));
        assert!(
            !letters.iter().any(|ch| ch.keysym == 0xff54),
            "Down stays grabbed"
        );
    }

    /// Regression for review finding H-1: an extension default chord spelled with a key alias of a
    /// preset chord must not replace the core binding (reviewer's probe, 0 replacements expected).
    #[test]
    fn extension_alias_chords_never_shadow_core_chords() {
        use super::super::spec::ContextMask;
        let aliases: &[(&str, &str)] = &[
            ("return", "enter"),
            ("prior", "page_up"),
            ("next", "page_down"),
            ("escape", "esc"),
            ("kp_end", "kp_1"),
            ("kp_home", "kp_7"),
            ("kp_prior", "kp_9"),
            ("kp_next", "kp_3"),
            ("kp_insert", "kp_0"),
            ("kp_down", "kp_2"),
            ("kp_up", "kp_8"),
            ("kp_left", "kp_4"),
            ("kp_right", "kp_6"),
            ("kp_begin", "kp_5"),
            ("kp_delete", "kp_decimal"),
            ("equal", "plus"),
        ];
        let mut shadowed = Vec::new();
        for text in [
            "",
            "[keyboard]\npreset=\"orca\"\n",
            "[keyboard]\nlayout=\"laptop\"\n",
        ] {
            let l = load(Some(text), &Presets::builtin());
            for (ctx, spec, cmd) in l.keymap.bound() {
                if !spec.gorca {
                    continue;
                }
                let Some(alias) = aliases
                    .iter()
                    .find(|(k, _)| *k == spec.key)
                    .map(|(_, a)| *a)
                else {
                    continue;
                };
                let mut hostile = spec.clone();
                hostile.key = alias.to_owned();
                let mut r = ExtCommands::new();
                r.register(ExtCmdSpec {
                    name: "ext.evil.grab".to_owned(),
                    ext: "evil".to_owned(),
                    local: 1,
                    help: "h".to_owned(),
                    contexts: ContextMask::ANY,
                    max_press: 3,
                    interrupts: true,
                })
                .unwrap();
                r.bind(ctx, &hostile.to_string(), "ext.evil.grab").unwrap();
                let evil = r.by_name("ext.evil.grab").unwrap().0;
                let c = compile_with(&l, &PlatformFacts::default(), &StaticResolver, Some(&r));
                let k = chord_key(spec, &StaticResolver).unwrap();
                for mode in [ModeKind::Focus, ModeKind::Browse, ModeKind::Review] {
                    if c.keymap.lookup(mode, k, spec.presses).map(|b| b.cmd) == Some(evil) {
                        shadowed.push(format!(
                            "{ctx:?} {spec} ({cmd}) <- ext `{hostile}` in {mode:?}"
                        ));
                    }
                }
            }
        }
        shadowed.sort();
        shadowed.dedup();
        assert!(
            shadowed.is_empty(),
            "extension alias chords replaced {} core bindings:\n{}",
            shadowed.len(),
            shadowed.join("\n")
        );
    }

    /// A core command with `max_press > 1` owns presses `2..=max_press` of its chord: an extension
    /// default chord at those presses must lose (regression for review finding N-1).
    #[test]
    fn extension_chords_never_take_over_core_multi_press() {
        use super::super::spec::ContextMask;
        let mut shadowed = Vec::new();
        let mut tried = 0usize;
        for text in [
            "",
            "[keyboard]\npreset=\"orca\"\n",
            "[keyboard]\nlayout=\"laptop\"\n",
        ] {
            let l = load(Some(text), &Presets::builtin());
            let base = compile_with(&l, &PlatformFacts::default(), &StaticResolver, None);
            for (ctx, spec, cmd) in l.keymap.bound() {
                let Some(k) = chord_key(spec, &StaticResolver) else {
                    continue;
                };
                if spec.presses != 1 {
                    continue;
                }
                for mode in [ModeKind::Focus, ModeKind::Browse, ModeKind::Review] {
                    let Some(core) = base.keymap.lookup(mode, k, 1) else {
                        continue;
                    };
                    for n in 2..=core.max_press {
                        if base.keymap.lookup(mode, k, n).is_some() {
                            continue;
                        }
                        tried += 1;
                        let mut r = ExtCommands::new();
                        r.register(ExtCmdSpec {
                            name: "ext.evil.grab".into(),
                            ext: "evil".into(),
                            local: 1,
                            help: "h".into(),
                            contexts: ContextMask::ANY,
                            max_press: 3,
                            interrupts: true,
                        })
                        .unwrap();
                        let v = format!("{spec}*{n}");
                        if r.bind(ctx, &v, "ext.evil.grab").is_err() {
                            continue;
                        }
                        let evil = r.by_name("ext.evil.grab").unwrap().0;
                        let c =
                            compile_with(&l, &PlatformFacts::default(), &StaticResolver, Some(&r));
                        if c.keymap.lookup(mode, k, n).map(|b| b.cmd) == Some(evil) {
                            shadowed.push(format!("{ctx:?} {spec} ({cmd}) press {n} in {mode:?}"));
                        }
                    }
                }
            }
        }
        assert!(
            tried > 20,
            "the probe must exercise the multi-press chords, tried {tried}"
        );
        assert!(shadowed.is_empty(), "shadowed: {shadowed:#?}");
    }
}
