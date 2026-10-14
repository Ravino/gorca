
    /// Reviewer probe F-1 r2: every keypad key x Num Lock x Shift x Caps, Mutter exact matching
    /// over the full wire set (modifier grabs included).
    #[test]
    fn probe_f1_every_keypad_key_both_num_lock_states() {
        // (nav keysym with Num Lock off, digit keysym with Num Lock on, name)
        let keys: [(u32, u32, &str); 16] = [
            (0xff9e, 0xffb0, "KP0"), (0xff9c, 0xffb1, "KP1"), (0xff99, 0xffb2, "KP2"),
            (0xff9b, 0xffb3, "KP3"), (0xff96, 0xffb4, "KP4"), (0xff9d, 0xffb5, "KP5"),
            (0xff98, 0xffb6, "KP6"), (0xff95, 0xffb7, "KP7"), (0xff97, 0xffb8, "KP8"),
            (0xff9a, 0xffb9, "KP9"), (0xff9f, 0xffae, "KPDecimal"),
            (0xffab, 0xffab, "KPAdd"), (0xffad, 0xffad, "KPSubtract"), (0xffaa, 0xffaa, "KPMultiply"),
            (0xffaf, 0xffaf, "KPDivide"), (0xff8d, 0xff8d, "KPEnter"),
        ];
        for preset in ["", "[keyboard]\npreset=\"orca\"\n", "[keyboard]\nlayout=\"laptop\"\n"] {
            let c = compiled(preset);
            let (plain, exact) = chord_grabs(&c.keymap, ModeKind::Focus);
            let l = load(Some(preset), &Presets::builtin()); let (mods, _) = gorca_modifiers(&l.settings, &PlatformFacts::default());
            let mut set = gorca_input_api::GrabSet::default();
            set.chords.extend(plain);
            set.exact.extend(exact);
            set.modifiers.extend(mods.iter().copied());
            let (wmods, strokes) = set.to_wire();
            let mut on_hits = Vec::new();
            let mut off_hits = Vec::new();
            for (nav, digit, name) in keys {
                for numlock in [false, true] {
                    for shift in [false, true] {
                        for caps in [false, true] {
                            let sym = if numlock && !shift { digit } else { nav };
                            let mut st = 0u32;
                            if shift { st |= Modifiers::SHIFT.bits(); }
                            if numlock { st |= Modifiers::MOD2.bits(); }
                            if caps { st |= Modifiers::LOCK.bits(); }
                            let swallowed = strokes.contains(&(sym, st)) || wmods.contains(&sym);
                            if swallowed {
                                let tag = format!("{name}{}{}", if shift {"+Shift"} else {""}, if caps {"+Caps"} else {""});
                                if numlock { on_hits.push(tag) } else { off_hits.push(tag) }
                            }
                        }
                    }
                }
            }
            eprintln!("PROBE f1 preset={preset:?}\n  NumLock OFF swallowed: {off_hits:?}\n  NumLock ON swallowed: {on_hits:?}");
        }
    }
