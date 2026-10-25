// SPDX-License-Identifier: MIT OR Apache-2.0
//! First-party plugin P-3, Telegram Web A and K (ADR-0026 section 2.8).
//!
//! * A chat row gets a computed name (title, unread count, preview, time) without the avatar's
//!   initials or its doubled label (study findings T2, T3).
//! * A Web A message (`div.Message`, no role) gets the role `article` (finding T4).
//!
//! Both are facts about nodes the host calls `derive` for (the `[[watch]]` selectors of the
//! manifest); the result is a function of the rows handed over, nothing else (PF-1).

mod chat;

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({ path: "../../wit/gorca-ext-0.10", world: "app-module" });

    use super::chat::{Cell, chat_name};
    use exports::gorca::ext::{commands, reactor, rules};
    use gorca::ext::{facts, model, types};

    /// Registry codes (`gorca-model`): group `role` is 0, `name` is 1; role `article` is 5.
    const GROUP_ROLE: u16 = 0;
    const GROUP_NAME: u16 = 1;
    const ROLE_ARTICLE: u16 = 5;

    const WATCH_CHAT_A: u16 = 1;
    const WATCH_CHAT_K: u16 = 2;
    const WATCH_MESSAGE_A: u16 = 3;

    struct Plugin;

    fn chat_row(n: u64) {
        let nodes = model::subtree(n, 6, 64);
        let mut cells: Vec<Cell> = Vec::with_capacity(nodes.len());
        for vn in &nodes {
            let class = model::attr(vn.node, "class").unwrap_or_default();
            let leaf = vn.child_count == 0;
            let text = if leaf {
                vn.name.clone().or_else(|| vn.text.clone()).unwrap_or_default()
            } else {
                String::new()
            };
            cells.push(Cell {
                parent: vn.parent_index.map(usize::from),
                class,
                text,
            });
        }
        if let Some(name) = chat_name(&cells, "unread") {
            facts::assert(
                facts::FactTarget::Node(n),
                GROUP_NAME,
                &types::PropValue::Text(name),
                facts::Layer::Patch,
                900,
                None,
            );
        }
    }

    fn message_row(n: u64) {
        facts::assert(
            facts::FactTarget::Node(n),
            GROUP_ROLE,
            &types::PropValue::Code(ROLE_ARTICLE),
            facts::Layer::Patch,
            900,
            None,
        );
    }

    impl rules::Guest for Plugin {
        fn attach(_ctx: rules::RuleCtx) {}

        fn derive(_ctx: rules::RuleCtx, batch: Vec<rules::Dirty>) {
            for d in batch {
                match d.watch {
                    WATCH_CHAT_A | WATCH_CHAT_K => chat_row(d.node),
                    WATCH_MESSAGE_A => message_row(d.node),
                    _ => {}
                }
            }
        }

        fn removed(_nodes: Vec<types::NodeRef>) {}
    }

    impl commands::Guest for Plugin {
        fn run(_id: u32, _ctx: commands::CmdCtx) {}
    }

    impl reactor::Guest for Plugin {
        fn deliver(_id: u32, _r: reactor::Reply) {}
        fn tick(_token: u32) {}
        fn settings_changed(_keys: Vec<String>) {}
    }

    export!(Plugin);
}
