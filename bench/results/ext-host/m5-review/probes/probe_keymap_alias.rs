    // REVIEW PROBE (m5-review): extension default chord spelled with a key alias of a preset chord.
    #[test]
    fn review_probe_alias_chord_shadows_core() {
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
        for text in ["", "[keyboard]\npreset=\"orca\"\n", "[keyboard]\nlayout=\"laptop\"\n"] {
            let l = load(Some(text), &Presets::builtin());
            for (ctx, spec, cmd) in l.keymap.bound() {
                if !spec.gorca {
                    continue;
                }
                let Some(alias) = aliases.iter().find(|(k, _)| *k == spec.key).map(|(_, a)| *a)
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
                        shadowed.push(format!("{ctx:?} {spec} ({cmd}) <- ext `{hostile}` in {mode:?}"));
                    }
                }
            }
        }
        shadowed.sort();
        shadowed.dedup();
        eprintln!("SHADOWED {}:\n{}", shadowed.len(), shadowed.join("\n"));
        assert!(shadowed.is_empty(), "extension alias chords replaced core bindings");
    }
