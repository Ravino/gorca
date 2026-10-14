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
/// | off | `(KP_Home, 0)`, grabbed | `(KP_Home, Shift)`, grabbed |
/// | on | `(KP_7, Mod2)`, **not** grabbed: the digit reaches the app | `(KP_Home, Shift+Mod2)`, **not** grabbed |
///
/// Each grabbed variant also exists with Caps Lock set.
pub fn chord_grabs(km: &CompiledKeymap, mode: ModeKind) -> (Vec<Chord>, Vec<Chord>) {
    grabs_where(km, mode, |_| true)
}

/// The grabs of the browse keymap (browse design 7.4): `(plain, exact)` chords for every active
/// browse binding except Escape, which is grabbed only in auto focus mode.
pub fn browse_grabs(km: &CompiledKeymap) -> (Vec<Chord>, Vec<Chord>) {
    let esc = CoreCmd::BrowseEscape.id();
    grabs_where(km, ModeKind::Browse, |b| b.cmd != esc)
}

/// The chords bound to `browse.escape`, for the auto-focus-mode grab set.
pub fn escape_grabs(km: &CompiledKeymap) -> Vec<Chord> {
    let esc = CoreCmd::BrowseEscape.id();
    grabs_where(km, ModeKind::Browse, |b| b.cmd == esc).0
}

fn grabs_where(
    km: &CompiledKeymap,
    mode: ModeKind,
    keep: impl Fn(&Bound) -> bool,
) -> (Vec<Chord>, Vec<Chord>) {
    let mut plain: Vec<Chord> = Vec::new();
    let mut exact: Vec<Chord> = Vec::new();
    for (ck, _, bound) in km.active_chords(mode) {
        if ck.mods.contains(Modifiers::GORCA) || !keep(&bound) {
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
            // A letter with Shift types the capital.
            let keysym = if shift && (0x61..=0x7a).contains(&nav) {
                nav - 0x20
            } else if shift && (0x31..=0x39).contains(&nav) {
                // Shift on the digit row types the symbol, not the digit (US layout: the
                // static keymap this compiler knows); the lane still matches by keycode.
                const SHIFTED: [u32; 9] = [0x21, 0x40, 0x23, 0x24, 0x25, 0x5e, 0x26, 0x2a, 0x28];
                SHIFTED[(nav - 0x31) as usize]
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
        let (plain, _exact) = browse_grabs(&c.keymap);
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
            escape_grabs(&c.keymap)
                .iter()
                .any(|ch| ch.keysym == 0xff1b && ch.modifiers.is_empty())
        );
    }
}
