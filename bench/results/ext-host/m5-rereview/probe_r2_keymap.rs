
    // REVIEW PROBE r2 (m5-rereview): spelling variants and multi-press shadowing.
    #[test]
    fn review_probe_r2_spellings_and_presses() {
        use super::super::spec::ContextMask;
        let mut spelled = Vec::new();
        let mut pressed = Vec::new();
        let mut tried_spell = 0usize;
        let mut tried_press = 0usize;
        for text in ["", "[keyboard]\npreset=\"orca\"\n", "[keyboard]\nlayout=\"laptop\"\n"] {
            let l = load(Some(text), &Presets::builtin());
            let base = compile_with(&l, &PlatformFacts::default(), &StaticResolver, None);
            for (ctx, spec, cmd) in l.keymap.bound() {
                let Some(k) = chord_key(spec, &StaticResolver) else { continue };
                // spelling variants: upper case key, reversed modifier order, insert/caps_lock tokens
                let mut mods: Vec<String> = Vec::new();
                if spec.mods.super_ { mods.push("SUPER".into()); }
                if spec.mods.shift { mods.push("Shift".into()); }
                if spec.mods.alt { mods.push("ALT".into()); }
                if spec.mods.ctrl { mods.push("Control".into()); }
                if spec.gorca { mods.push("Caps_Lock".into()); }
                let mut variants = vec![format!("{}{}", if mods.is_empty() { String::new() } else { mods.join("+") + "+" }, spec.key.to_ascii_uppercase())];
                if spec.presses > 1 { for v in &mut variants { v.push_str(&format!("*{}", spec.presses)); } }
                for v in variants {
                    if !spec.gorca { continue; }
                    tried_spell += 1;
                    let mut r = ExtCommands::new();
                    r.register(ExtCmdSpec { name: "ext.evil.grab".into(), ext: "evil".into(), local: 1, help: "h".into(), contexts: ContextMask::ANY, max_press: 3, interrupts: true }).unwrap();
                    if r.bind(ctx, &v, "ext.evil.grab").is_err() { continue; }
                    let evil = r.by_name("ext.evil.grab").unwrap().0;
                    let c = compile_with(&l, &PlatformFacts::default(), &StaticResolver, Some(&r));
                    for mode in [ModeKind::Focus, ModeKind::Browse, ModeKind::Review] {
                        if c.keymap.lookup(mode, k, spec.presses).map(|b| b.cmd) == Some(evil) {
                            spelled.push(format!("{ctx:?} {spec} ({cmd}) <- `{v}` {mode:?}"));
                        }
                    }
                }
                // multi-press: core (k,1) with max_press>1 implicitly owns (k,2)/(k,3)
                if spec.presses != 1 || !spec.gorca { continue; }
                for mode in [ModeKind::Focus, ModeKind::Browse, ModeKind::Review] {
                    let Some(core) = base.keymap.lookup(mode, k, 1) else { continue };
                    if core.max_press < 2 { continue; }
                    for n in 2..=core.max_press {
                        if base.keymap.lookup(mode, k, n).is_some() { continue; }
                        tried_press += 1;
                        let mut r = ExtCommands::new();
                        r.register(ExtCmdSpec { name: "ext.evil.grab".into(), ext: "evil".into(), local: 1, help: "h".into(), contexts: ContextMask::ANY, max_press: 3, interrupts: true }).unwrap();
                        let v = format!("{spec}*{n}");
                        if r.bind(ctx, &v, "ext.evil.grab").is_err() { continue; }
                        let evil = r.by_name("ext.evil.grab").unwrap().0;
                        let c = compile_with(&l, &PlatformFacts::default(), &StaticResolver, Some(&r));
                        // the lane: explicit (chord, n) first, then (chord, 1) with a repeat count
                        if c.keymap.lookup(mode, k, n).map(|b| b.cmd) == Some(evil) {
                            pressed.push(format!("{ctx:?} {spec} ({cmd}, max_press {}) press {n} <- ext `{v}` in {mode:?}", core.max_press));
                        }
                    }
                }
            }
        }
        spelled.sort(); spelled.dedup(); pressed.sort(); pressed.dedup();
        eprintln!("PROBE spellings tried {tried_spell}, shadowed {}:\n{}", spelled.len(), spelled.join("\n"));
        eprintln!("PROBE multipress tried {tried_press}, shadowed {}:\n{}", pressed.len(), pressed.join("\n"));
    }
