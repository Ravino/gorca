//! Keymap compilation (ADR-0005 7.1, 7.5): chord and command names to O(1) lookup tables, plus
//! the platform facts that change which keys are usable (U-1, 8.1).

use gorca_config::{ChordSpec, ContextName, Diag, DiagMsg, Loaded, Settings, did_you_mean};
use gorca_input_api::{
    Bound, Chord, ChordKey, CmdId, CompiledKeymap, KeyRef, KeyResolver, LaneSettings, ModeKind,
    Modifiers, TableKind, keypad_digit_keysym, keysym_of_code, modifier_keysym, sym,
};

use super::spec::CommandTable;
use super::table::CoreCmd;

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
}

impl Default for PlatformFacts {
    fn default() -> Self {
        Self {
            repeat_delay_ms: 500,
            xkb_options: String::new(),
            clock_24h: true,
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

/// Compile a loaded configuration for the input lane.
pub fn compile(loaded: &Loaded, facts: &PlatformFacts, resolver: &dyn KeyResolver) -> Compiled {
    let table = CommandTable::get();
    let (mods, caps_lock_dropped) = gorca_modifiers(&loaded.settings, facts);
    let mut km = CompiledKeymap::new(mods);
    let mut diags = Vec::new();
    for (ctx, chords) in &loaded.keymap.contexts {
        for (spec, binding) in chords {
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
                    let Some(cmd) = table.by_name(name) else {
                        diags.push(Diag::error(
                            path,
                            None,
                            DiagMsg::UnknownCommand {
                                command: name.clone(),
                                suggestion: did_you_mean(name, table.names()),
                            },
                        ));
                        continue;
                    };
                    if !cmd.spec().contexts.allows(*ctx) {
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
                    Some(bound_of(cmd))
                }
            };
            km.insert(table_kind(*ctx), key, spec.presses, bound);
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

/// The keystroke grabs the non-gorca chords of `mode` need (ADR-0005 K-8a, K-6a; ADR-0006 11).
///
/// Chords with the gorca modifier need no grab of their own: Mutter swallows every key while a
/// grabbed modifier is held (KM-4). Every other implemented chord is compiled from
/// `(keycode, mods)` into the keysym and state Mutter will see. Returns `(plain, exact)`:
/// plain chords are expanded over the Caps Lock and Num Lock states on the wire; keypad chords
/// are exact, because the keysym of a keypad key depends on Num Lock and Mutter matches the
/// exact keysym and state (platform check V-1, 2026-10-14):
///
/// | Num Lock | plain `KP7` | `Shift+KP7` |
/// |---|---|---|
/// | off | `(KP_Home, 0)` | `(KP_Home, Shift)` |
/// | on | `(KP_7, Mod2)` | `(KP_Home, Shift+Mod2)` |
///
/// Each variant also exists with Caps Lock set.
pub fn chord_grabs(km: &CompiledKeymap, mode: ModeKind) -> (Vec<Chord>, Vec<Chord>) {
    let mut plain: Vec<Chord> = Vec::new();
    let mut exact: Vec<Chord> = Vec::new();
    for (ck, _, _) in km.active_chords(mode) {
        if ck.mods.contains(Modifiers::GORCA) {
            continue;
        }
        let mods = ck.mods.without_locks();
        let shift = mods.contains(Modifiers::SHIFT);
        let (code_sym, sym_direct) = match ck.key {
            KeyRef::Code(code) => (keysym_of_code(code), None),
            KeyRef::Sym(s) => (None, Some(s)),
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
        } else if let Some(digit) = keypad_digit_keysym(nav) {
            let on = if shift { nav } else { digit };
            for (keysym, extra) in [(nav, Modifiers::empty()), (on, Modifiers::MOD2)] {
                for lock in [Modifiers::empty(), Modifiers::LOCK] {
                    exact.push(Chord {
                        keysym,
                        modifiers: mods | extra | lock,
                    });
                }
            }
        } else {
            // A letter with Shift types the capital.
            let keysym = if shift && (0x61..=0x7a).contains(&nav) {
                nav - 0x20
            } else {
                nav
            };
            plain.push(Chord {
                keysym,
                modifiers: mods,
            });
        }
    }
    plain.sort_by_key(|c| (c.keysym, c.modifiers.bits()));
    plain.dedup();
    exact.sort_by_key(|c| (c.keysym, c.modifiers.bits()));
    exact.dedup();
    (plain, exact)
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
                let by_design: &[&str] = if text.contains("orca") {
                    &[
                        "review.simple_toggle",
                        "review.mode_next",
                        "review.mode_prev",
                        "review.to_focus",
                        "review.move_focus",
                        "review.start_of_line",
                        "object.next_in_flow",
                        "object.prev_in_flow",
                    ]
                } else {
                    &["review.simple_toggle", "review.toggle"]
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
        chord_grabs(&c.keymap, ModeKind::Focus)
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
    /// exact keysym and state, so every variant is registered with the keysym it will see.
    #[test]
    fn desktop_keypad_review_keys_are_grabbed_with_the_keysym_each_num_lock_state_produces() {
        let (plain, exact) = grabs("");
        // KP7 = review.prev_line: Num Lock off, and on (where the keypad types KP_7).
        assert!(has(&exact, KP_HOME, Modifiers::empty()));
        assert!(has(&exact, KP_HOME, Modifiers::LOCK));
        assert!(has(&exact, KP_7, Modifiers::MOD2));
        assert!(has(&exact, KP_7, Modifiers::MOD2 | Modifiers::LOCK));
        // Shift+KP7 = review.top: KP_Home with Shift, and KP_Home with Shift+Mod2.
        assert!(has(&exact, KP_HOME, Modifiers::SHIFT));
        assert!(has(&exact, KP_HOME, Modifiers::SHIFT | Modifiers::MOD2));
        // Never the digit keysym under Shift, and never a bare-Mod2 navigation keysym.
        assert!(!has(&exact, KP_7, Modifiers::SHIFT | Modifiers::MOD2));
        assert!(!has(&exact, KP_HOME, Modifiers::MOD2));
        // Keys with one keysym are exact too only if they are keypad navigation keys.
        assert!(has(&exact, KP_UP, Modifiers::empty()) && has(&exact, KP_8, Modifiers::MOD2));
        // M4b commands stay ungrabbed (K-1): KP_Add is review.say_all, not implemented yet.
        assert!(!has(&plain, KP_ADD, Modifiers::empty()));
        assert!(!exact.iter().any(|c| c.keysym == KP_ADD));
        // gorca chords need no grab of their own.
        assert!(plain.is_empty(), "{plain:?}");
    }

    #[test]
    fn the_wire_form_of_a_keypad_chord_has_no_blind_variants() {
        let (plain, exact) = grabs("");
        let mut set = gorca_input_api::GrabSet::default();
        set.chords.extend(plain);
        set.exact.extend(exact);
        let (_, strokes) = set.to_wire();
        let shift = Modifiers::SHIFT.bits();
        let mod2 = Modifiers::MOD2.bits();
        assert!(strokes.contains(&(KP_HOME, 0)));
        assert!(strokes.contains(&(KP_7, mod2)));
        assert!(strokes.contains(&(KP_HOME, shift | mod2)));
        assert!(
            !strokes.contains(&(KP_HOME, mod2)),
            "Mutter would never match it"
        );
        assert!(set.contains(KP_HOME, Modifiers::empty()));
        assert!(set.contains(KP_7, Modifiers::MOD2));
        assert!(
            !set.contains(KP_7, Modifiers::empty()),
            "Num Lock off never types KP_7"
        );
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
}
