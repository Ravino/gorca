// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Telegram-specific knowledge, as pure functions of the rows the host hands over: which
//! part of a chat row is which (Telegram Web A and K), and the name a chat row should have.
//!
//! Study: docs/reports/telegram-web-a-study.md, findings T2 (initials and time in the name, no
//! "unread" word), T3 (K: the link name is doubled by the avatar label).

use std::collections::HashMap;

/// The parts of a chat row that matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    /// The avatar: its initials (A) or its label (K) are not part of the name.
    Avatar,
    /// The chat's title.
    Title,
    /// The time of the last message.
    Time,
    /// The preview of the last message.
    Preview,
    /// The unread counter.
    Badge,
}

/// CSS class tokens of both clients and the part they mark.
fn table() -> HashMap<&'static str, Part> {
    HashMap::from([
        // Web A.
        ("Avatar", Part::Avatar),
        ("avatar-wrapper", Part::Avatar),
        ("fullName", Part::Title),
        ("time", Part::Time),
        ("last-message", Part::Preview),
        ("ChatBadge", Part::Badge),
        // Web K.
        ("dialog-avatar", Part::Avatar),
        ("peer-title", Part::Title),
        ("message-time", Part::Time),
        ("user-last-message", Part::Preview),
        ("dialog-subtitle-badge", Part::Badge),
    ])
}

/// The part a class attribute marks (`None`: none of the known tokens).
pub fn part_of(class: &str, t: &HashMap<&'static str, Part>) -> Option<Part> {
    class
        .split_ascii_whitespace()
        .find_map(|tok| t.get(tok).copied())
}

/// One row of the chat row's subtree.
#[derive(Clone, Debug)]
pub struct Cell {
    /// Index of the parent cell, `None` for the root.
    pub parent: Option<usize>,
    /// The `class` attribute.
    pub class: String,
    /// Text of a leaf (its accessible name), empty for a container.
    pub text: String,
}

/// The name of a chat row: title, unread count, preview and time, without the avatar's
/// initials. `None` when the row has no title (nothing to improve).
pub fn chat_name(cells: &[Cell], unread_word: &str) -> Option<String> {
    let t = table();
    let mut part_of_cell: Vec<Option<Part>> = Vec::with_capacity(cells.len());
    let mut texts: HashMap<Part, Vec<&str>> = HashMap::new();
    for (i, c) in cells.iter().enumerate() {
        // A cell belongs to its own part, else to its parent's (the cells arrive parents first).
        let own = part_of(&c.class, &t);
        let inherited = c
            .parent
            .filter(|p| *p < i)
            .and_then(|p| part_of_cell.get(p).copied().flatten());
        let part = own.or(inherited);
        part_of_cell.push(part);
        if let Some(p) = part
            && !c.text.is_empty()
        {
            texts.entry(p).or_default().push(c.text.as_str());
        }
    }
    let join = |p: Part| -> String {
        texts
            .get(&p)
            .map(|v| v.join(" "))
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let title = join(Part::Title);
    if title.is_empty() {
        return None;
    }
    let mut out = title;
    let badge = join(Part::Badge);
    if !badge.is_empty() {
        out.push_str(", ");
        out.push_str(&badge);
        out.push(' ');
        out.push_str(unread_word);
    }
    for p in [Part::Preview, Part::Time] {
        let s = join(p);
        if !s.is_empty() {
            out.push_str(", ");
            out.push_str(&s);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(parent: Option<usize>, class: &str, text: &str) -> Cell {
        Cell {
            parent,
            class: class.into(),
            text: text.into(),
        }
    }

    #[test]
    fn web_a_row_drops_the_initials_and_says_unread() {
        let cells = vec![
            cell(None, "ListItem-button", ""),
            cell(Some(0), "status", ""),
            cell(Some(1), "Avatar", ""),
            cell(Some(2), "", "AL"),
            cell(Some(0), "info", ""),
            cell(Some(4), "title", ""),
            cell(Some(5), "fullName", ""),
            cell(Some(6), "", "Alice"),
            cell(Some(4), "time", "") ,
            cell(Some(8), "", "15:00"),
            cell(Some(4), "last-message", ""),
            cell(Some(10), "", "see you soon"),
            cell(Some(4), "ChatBadge unread", ""),
            cell(Some(12), "", "1"),
        ];
        assert_eq!(
            chat_name(&cells, "unread").as_deref(),
            Some("Alice, 1 unread, see you soon, 15:00")
        );
    }

    #[test]
    fn web_k_row_is_not_doubled_by_the_avatar_label() {
        let cells = vec![
            cell(None, "chatlist-chat chatlist-chat-bigger", ""),
            cell(Some(0), "dialog-avatar", "Alice"),
            cell(Some(0), "user-caption", ""),
            cell(Some(2), "dialog-title", ""),
            cell(Some(3), "peer-title", ""),
            cell(Some(4), "", "Alice"),
            cell(Some(3), "message-time", ""),
            cell(Some(6), "", "15:00"),
            cell(Some(2), "user-last-message", ""),
            cell(Some(8), "", "see you soon"),
        ];
        assert_eq!(
            chat_name(&cells, "unread").as_deref(),
            Some("Alice, see you soon, 15:00")
        );
    }

    #[test]
    fn a_row_without_a_title_is_left_alone() {
        let cells = vec![cell(None, "ListItem-button", ""), cell(Some(0), "", "x")];
        assert_eq!(chat_name(&cells, "unread"), None);
    }
}
