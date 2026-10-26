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

    use super::chat::{Cell, chat_name, has_class};
    use exports::gorca::ext::{commands, reactor, rules};
    use gorca::ext::{facts, model, types};

    /// Registry codes (`gorca-model`): groups `role` 0, `name` 1, `level` 12; roles `heading` 28,
    /// `list` 32, `listitem` 34.
    const GROUP_ROLE: u16 = 0;
    const GROUP_NAME: u16 = 1;
    const GROUP_DESCRIPTION: u16 = 2;
    const GROUP_LEVEL: u16 = 12;
    const ROLE_HEADING: u16 = 28;
    const ROLE_LIST: u16 = 32;
    const ROLE_LISTITEM: u16 = 34;
    const ROLE_NAVIGATION: u16 = 46;
    const ROLE_REGION: u16 = 53;

    const WATCH_CHAT_A: u16 = 1;
    const WATCH_CHAT_K: u16 = 2;
    const WATCH_MESSAGE_A: u16 = 3;
    const WATCH_MESSAGES_A: u16 = 4;
    const WATCH_HEADER_A: u16 = 5;
    const WATCH_DATE_A: u16 = 6;
    const WATCH_DIVIDER_A: u16 = 7;
    const WATCH_DIVIDER_A2: u16 = 8;
    const WATCH_CHATS_A: u16 = 9;
    const WATCH_COMPOSER_A: u16 = 10;
    const WATCH_CHAT_ITEM_A: u16 = 11;

    struct Plugin;

    fn chat_row(n: u64) {
        let nodes = model::subtree(n, 6, 64);
        let mut cells: Vec<Cell> = Vec::with_capacity(nodes.len());
        for vn in &nodes {
            let class = model::attr(vn.node, "class").unwrap_or_default();
            let leaf = vn.child_count == 0;
            let text = if leaf {
                vn.name
                    .clone()
                    .or_else(|| vn.text.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            cells.push(Cell {
                parent: vn.parent_index.map(usize::from),
                class,
                text,
            });
        }
        if let Some(name) = chat_name(&cells) {
            // A message of the plugin's catalog (`i18n/*.msg`): the host resolves it in the
            // UI language, so "unread" is not an English literal.
            let msg = types::MsgRef {
                id: name.key(),
                args: name.args(),
            };
            facts::assert(
                facts::FactTarget::Node(n),
                GROUP_NAME,
                &types::PropValue::Message(msg),
                facts::Layer::Patch,
                900,
                None,
            );
        }
    }

    fn assert_node(n: u64, group: u16, value: types::PropValue) {
        facts::assert(
            facts::FactTarget::Node(n),
            group,
            &value,
            facts::Layer::Patch,
            900,
            None,
        );
    }

    fn heading(n: u64, level: u32) {
        assert_node(n, GROUP_ROLE, types::PropValue::Code(ROLE_HEADING));
        assert_node(n, GROUP_LEVEL, types::PropValue::Number(level.into()));
    }

    /// A message of the open chat is an item of the message list; an outgoing one (`own`) says so
    /// in its description, from the catalog.
    fn message_row(n: u64) {
        assert_node(n, GROUP_ROLE, types::PropValue::Code(ROLE_LISTITEM));
        let class = model::attr(n, "class").unwrap_or_default();
        if has_class(&class, "own") {
            assert_node(
                n,
                GROUP_DESCRIPTION,
                types::PropValue::Message(types::MsgRef {
                    id: "own-message".to_owned(),
                    args: Vec::new(),
                }),
            );
        }
    }

    /// A landmark: `role` with a name from the catalog.
    fn landmark(n: u64, role: u16, name: &str) {
        assert_node(n, GROUP_ROLE, types::PropValue::Code(role));
        assert_node(
            n,
            GROUP_NAME,
            types::PropValue::Message(types::MsgRef {
                id: name.to_owned(),
                args: Vec::new(),
            }),
        );
    }

    /// The chat list is a list; its parent (a block with nothing else in it) is the "Chats"
    /// navigation landmark.
    fn chat_list(n: u64) {
        assert_node(n, GROUP_ROLE, types::PropValue::Code(ROLE_LIST));
        assert_node(
            n,
            GROUP_NAME,
            types::PropValue::Message(types::MsgRef {
                id: "chat-list".to_owned(),
                args: Vec::new(),
            }),
        );
        if let Some(area) = model::parent(n) {
            landmark(area, ROLE_NAVIGATION, "chats");
        }
    }

    /// The message list is a list; its parent (the message area: the list and, below it, the
    /// composer's footer) is the "Messages" region.
    fn message_list(n: u64) {
        assert_node(n, GROUP_ROLE, types::PropValue::Code(ROLE_LIST));
        assert_node(
            n,
            GROUP_NAME,
            types::PropValue::Message(types::MsgRef {
                id: "messages-list".to_owned(),
                args: Vec::new(),
            }),
        );
        if let Some(area) = model::parent(n) {
            landmark(area, ROLE_REGION, "messages");
        }
    }

    /// The chat's title in the header (an `h3.fullName` that is a direct child of the chat view,
    /// with `role=button` instead of a heading role) is the page's first-level heading. The
    /// chat-list rows' titles are deeper, below `a.ListItem-button`, and are never touched here.
    fn chat_header(n: u64) {
        for vn in model::subtree(n, 1, 24) {
            let class = model::attr(vn.node, "class").unwrap_or_default();
            if has_class(&class, "fullName") {
                heading(vn.node, 1);
                return;
            }
        }
    }

    impl rules::Guest for Plugin {
        fn attach(_ctx: rules::RuleCtx) {}

        fn derive(_ctx: rules::RuleCtx, batch: Vec<rules::Dirty>) {
            for d in batch {
                match d.watch {
                    WATCH_CHAT_A | WATCH_CHAT_K => chat_row(d.node),
                    WATCH_MESSAGE_A => message_row(d.node),
                    WATCH_MESSAGES_A => message_list(d.node),
                    WATCH_CHATS_A => chat_list(d.node),
                    WATCH_CHAT_ITEM_A => {
                        assert_node(d.node, GROUP_ROLE, types::PropValue::Code(ROLE_LISTITEM))
                    }
                    WATCH_COMPOSER_A => landmark(d.node, ROLE_REGION, "composer"),
                    WATCH_HEADER_A => chat_header(d.node),
                    WATCH_DATE_A | WATCH_DIVIDER_A | WATCH_DIVIDER_A2 => heading(d.node, 2),
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
