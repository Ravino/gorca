// SPDX-License-Identifier: GPL-3.0-or-later
//! The facts of a plugin follow the UI language: a message fact is resolved from the plugin's own
//! catalog (`i18n/<lang>.msg`) on the plugin's worker thread, in the process language. This test
//! sets the process language, so it lives in a binary of its own.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{FactOp, PluginBridge, PluginService, Row, ScopeSpec, Tree};
use gorca_daemon::plugins::{PluginOptions, PluginRuntime, parse_pins};
use gorca_model::sem::SemRole;

struct Flat(HashMap<u32, Vec<u32>>);
impl Tree for Flat {
    fn parent(&self, n: u32) -> Option<u32> {
        self.0.iter().find(|(_, c)| c.contains(&n)).map(|(p, _)| *p)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.get(&n).cloned().unwrap_or_default()
    }
}

/// (index, parent, tag, class, text)
const CHAT: &[(u32, Option<u32>, &str, &str, &str)] = &[
    (0, None, "div", "", ""),
    (1, Some(0), "div", "ListItem-button", ""),
    (2, Some(1), "h3", "fullName", ""),
    (3, Some(2), "", "", "Alice"),
    (4, Some(1), "div", "last-message", ""),
    (5, Some(4), "", "", "see you soon"),
    (6, Some(1), "div", "ChatBadge unread", ""),
    (7, Some(6), "", "", "1"),
];

fn names_for(unread: &str) -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        Command::new(root.join("tools/build-plugins.sh"))
            .status()
            .unwrap()
            .success()
    );
    let pins = parse_pins(&std::fs::read_to_string(root.join("plugins/pins.txt")).unwrap());
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(root.join("plugins/dist")),
            ..PluginOptions::default()
        },
        &pins,
    );
    let scope = ScopeSpec {
        app: Arc::from("firefox-esr"),
        url: Some(Arc::from("https://web.telegram.org/a/")),
        engine: 1,
        verified: true,
        doc: true,
    };
    let runner = rt.open(&scope, Arc::new(|| {})).expect("scope");
    let mut b = PluginBridge::new(runner, rt.governor(), scope);
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, p, tag, class, text) in CHAT {
        if let Some(p) = p {
            kids.entry(*p).or_default().push(*i);
        }
        let text = if *i == 7 { unread } else { *text };
        b.upsert(
            *i,
            Row {
                role: if text.is_empty() {
                    SemRole::Generic.code()
                } else {
                    SemRole::StaticText.code()
                },
                name: (!text.is_empty()).then(|| Arc::from(text)),
                tag: (!tag.is_empty()).then(|| Arc::from(*tag)),
                class: (!class.is_empty()).then(|| Arc::from(*class)),
                ..Row::default()
            },
        );
    }
    let tree = Flat(kids);
    let end = Instant::now() + Duration::from_secs(5);
    let mut out = Vec::new();
    while Instant::now() < end && out.is_empty() {
        b.begin_turn();
        b.background(&tree);
        for op in b.take_ops() {
            if let FactOp::Patch {
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                ..
            } = op
            {
                out.push(n.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    out
}

#[test]
fn the_row_name_follows_the_process_language_from_the_plugins_catalog() {
    gorca_i18n::set_language(gorca_i18n::Lang::En);
    assert_eq!(names_for("1"), ["Alice, 1 unread, see you soon"]);
    // Russian plural forms: 1 непрочитанное, 2 and 5 непрочитанных.
    gorca_i18n::set_language(gorca_i18n::Lang::Ru);
    assert_eq!(names_for("1"), ["Alice, 1 непрочитанное, see you soon"]);
    assert_eq!(names_for("5"), ["Alice, 5 непрочитанных, see you soon"]);
    gorca_i18n::set_language(gorca_i18n::Lang::En);
}
