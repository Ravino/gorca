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
        // Inline spans have no accessible of their own in Gecko: the time's text lands under
        // the block that holds it.
        ("LastMessageMeta", Part::Time),
        ("last-message", Part::Preview),
        ("ChatBadge", Part::Badge),
        // Web K.
        ("dialog-avatar", Part::Avatar),
        ("peer-title", Part::Title),
        ("user-title", Part::Title),
        ("message-time", Part::Time),
        ("dialog-title-details", Part::Time),
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
pub fn chat_name(cells: &[Cell]) -> Option<Name> {
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
            .map(|v| {
                // A lone colon (Web A prints "sender" ":" "text" as three leaves) sticks to the
                // word before it: "Alex: hi", not "Alex : hi".
                let mut out = String::new();
                for t in v {
                    if t.trim() == ":" && !out.is_empty() {
                        out.push(':');
                    } else {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(t);
                    }
                }
                out
            })
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    let mut title = join(Part::Title);
    let titled_by_class = !title.is_empty();
    let mut preview = join(Part::Preview);
    let mut time = join(Part::Time);
    if title.is_empty() && part_of_cell.contains(&Some(Part::Avatar)) {
        // Chromium drops the inline spans of Web K's row: title, time and preview are bare text
        // directly under the link, in that order. Read them by position and shape.
        let loose: Vec<&str> = cells
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                c.parent == Some(0) && part_of_cell[*i].is_none() && !c.text.trim().is_empty()
            })
            .map(|(_, c)| c.text.trim())
            .collect();
        if let [merged] = loose.as_slice()
            && let Some((t, tm, pv)) = split_merged(merged)
        {
            // The runs of the row arrive as one text without separators: "Alice15:00see you soon".
            title = t;
            time = tm;
            preview = pv;
        } else if let Some((first, rest)) = loose.split_first() {
            title = (*first).to_owned();
            let mut others: Vec<&str> = Vec::new();
            for r in rest {
                if time.is_empty() && looks_like_time(r) {
                    time = (*r).to_owned();
                } else {
                    others.push(r);
                }
            }
            if preview.is_empty() {
                preview = others.join(" ");
            }
        }
    }
    if title.is_empty() {
        return None;
    }
    let mut unread = join(Part::Badge);
    if unread.is_empty() && titled_by_class {
        // Web A (2026-10) names the counter by a hashed CSS-module class (`NT-X2xr9 ...`): the
        // only stable mark is its place and shape, a bare count right under the row.
        unread = cells
            .iter()
            .enumerate()
            .find(|(i, c)| {
                // Its own cell or the run leaf under a hashed-class block, never under a part.
                part_of_cell[*i].is_none()
                    && looks_like_count(c.text.trim())
                    && under_row_only_unmarked(cells, &part_of_cell, *i)
            })
            .map(|(_, c)| c.text.trim().to_owned())
            .unwrap_or_default();
    }
    Some(Name {
        title,
        unread: (!unread.is_empty()).then_some(unread),
        preview,
        time,
    })
}

/// The parts of a chat row's name. The words between them ("unread", the order, the commas) are
/// the catalog's (`i18n/<lang>.msg`), so they follow the UI language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// The chat's title.
    pub title: String,
    /// The unread count as the page shows it.
    pub unread: Option<String>,
    /// The preview of the last message (empty: none).
    pub preview: String,
    /// The time of the last message (empty: none).
    pub time: String,
}

impl Name {
    /// The catalog key: `row-UPT`, one digit each for unread, preview and time present.
    pub fn key(&self) -> String {
        let d = |b: bool| if b { '1' } else { '0' };
        format!(
            "row-{}{}{}",
            d(self.unread.is_some()),
            d(!self.preview.is_empty()),
            d(!self.time.is_empty())
        )
    }

    /// The arguments of the message: `title`, `n`, `preview`, `time` (only the present ones).
    pub fn args(&self) -> Vec<(String, String)> {
        let mut v = vec![("title".to_owned(), self.title.clone())];
        if let Some(n) = &self.unread {
            v.push(("n".to_owned(), n.clone()));
        }
        if !self.preview.is_empty() {
            v.push(("preview".to_owned(), self.preview.clone()));
        }
        if !self.time.is_empty() {
            v.push(("time".to_owned(), self.time.clone()));
        }
        v
    }

    /// The English rendering (tests; the host renders the message from the catalog).
    #[cfg(test)]
    pub fn english(&self) -> String {
        let mut out = self.title.clone();
        if let Some(n) = &self.unread {
            out.push_str(&format!(", {n} unread"));
        }
        for s in [&self.preview, &self.time] {
            if !s.is_empty() {
                out.push_str(", ");
                out.push_str(s);
            }
        }
        out
    }
}

/// Split "Alice15:00see you soon" at the time: (title, time, preview). The title must not be
/// empty; the time is `H:MM` or `HH:MM` that does not follow a digit.
fn split_merged(s: &str) -> Option<(String, String, String)> {
    let b = s.as_bytes();
    for i in 1..b.len() {
        if !b[i].is_ascii_digit() || b[i - 1].is_ascii_digit() {
            continue;
        }
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=2).contains(&digits) || b.get(i + digits) != Some(&b':') {
            continue;
        }
        let mins = b[i + digits + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if mins < 2 {
            continue;
        }
        let end = i + digits + 1 + 2;
        let (title, rest) = (s[..i].trim(), s[end..].trim());
        if title.is_empty() {
            continue;
        }
        return Some((title.to_owned(), s[i..end].to_owned(), rest.to_owned()));
    }
    None
}

/// True when cell `i` reaches the row (cell 0) within three steps through cells that belong to no
/// part: it is the row's own loose text, not the text of a title, preview or time.
fn under_row_only_unmarked(cells: &[Cell], part_of_cell: &[Option<Part>], i: usize) -> bool {
    let mut cur = i;
    for _ in 0..3 {
        match cells[cur].parent {
            Some(0) => return true,
            Some(p) if part_of_cell.get(p).is_some_and(Option::is_none) => cur = p,
            _ => return false,
        }
    }
    false
}

/// An unread counter as Telegram prints it: `3`, `529`, `1,2K`, `182,6K`, `2M`.
fn looks_like_count(s: &str) -> bool {
    let body = s.strip_suffix(['K', 'k', 'M', 'm']).unwrap_or(s);
    let suffixed = body.len() != s.len();
    let mut parts = body.split(['.', ',']);
    let int_ok = parts
        .next()
        .is_some_and(|p| (1..=6).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()));
    let frac_ok = parts.next().is_none_or(|f| {
        suffixed && (1..=2).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit())
    });
    int_ok && frac_ok && parts.next().is_none()
}

/// `15:00`, `9:41`, `15:00:30` (a message time); not a date or a count.
fn looks_like_time(s: &str) -> bool {
    let mut parts = s.split(':');
    let ok = |p: Option<&str>, max: usize| {
        p.is_some_and(|p| (1..=max).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
    };
    ok(parts.next(), 2) && ok(parts.next(), 2) && parts.next().is_none_or(|x| ok(Some(x), 2))
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
            cell(Some(4), "time", ""),
            cell(Some(8), "", "15:00"),
            cell(Some(4), "last-message", ""),
            cell(Some(10), "", "see you soon"),
            cell(Some(4), "ChatBadge unread", ""),
            cell(Some(12), "", "1"),
        ];
        assert_eq!(
            chat_name(&cells).map(|n| n.english()).as_deref(),
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
            chat_name(&cells).map(|n| n.english()).as_deref(),
            Some("Alice, see you soon, 15:00")
        );
    }

    #[test]
    fn pruned_inline_spans_still_name_the_row() {
        // Firefox: only the blocks that have a frame are accessible; the time and the title sit
        // under `div.LastMessageMeta` / `span.user-title` (A) and `span.dialog-title-details` (K).
        let a = vec![
            cell(None, "ListItem-button", ""),
            cell(Some(0), "fullName", ""),
            cell(Some(1), "", "Alice"),
            cell(Some(0), "LastMessageMeta", ""),
            cell(Some(3), "", "15:00"),
            cell(Some(0), "last-message", ""),
            cell(Some(5), "", "see you soon"),
        ];
        assert_eq!(
            chat_name(&a).map(|n| n.english()).as_deref(),
            Some("Alice, see you soon, 15:00")
        );
        let k = vec![
            cell(None, "chatlist-chat", ""),
            cell(Some(0), "dialog-avatar", "Alice"),
            cell(Some(0), "user-title", ""),
            cell(Some(2), "", "Alice"),
            cell(Some(0), "dialog-title-details", ""),
            cell(Some(4), "", "15:00"),
            cell(Some(0), "user-last-message", ""),
            cell(Some(6), "", "see you soon"),
            cell(Some(0), "dialog-subtitle-badge-wrapper", ""),
            cell(Some(8), "dialog-subtitle-badge badge unread", ""),
            cell(Some(9), "", "3"),
        ];
        assert_eq!(
            chat_name(&k).map(|n| n.english()).as_deref(),
            Some("Alice, 3 unread, see you soon, 15:00")
        );
    }

    #[test]
    fn web_k_in_chromium_has_bare_text_under_the_link() {
        let cells = vec![
            cell(None, "chatlist-chat chatlist-chat-bigger", ""),
            cell(Some(0), "dialog-avatar", "Alice"),
            cell(Some(0), "", "Alice"),
            cell(Some(0), "", "15:00"),
            cell(Some(0), "", "see you soon"),
            cell(Some(0), "dialog-subtitle-badge badge unread", ""),
            cell(Some(5), "", "1"),
        ];
        assert_eq!(
            chat_name(&cells).map(|n| n.english()).as_deref(),
            Some("Alice, 1 unread, see you soon, 15:00")
        );
    }

    #[test]
    fn web_k_in_chromium_can_merge_the_runs_into_one_text() {
        let cells = vec![
            cell(None, "chatlist-chat", ""),
            cell(Some(0), "dialog-avatar", "Alice"),
            cell(Some(0), "", "Alice15:00see you soon"),
        ];
        assert_eq!(
            chat_name(&cells).map(|n| n.english()).as_deref(),
            Some("Alice, see you soon, 15:00")
        );
        assert_eq!(
            split_merged("Carol14:58lunch at noon?"),
            Some(("Carol".into(), "14:58".into(), "lunch at noon?".into()))
        );
        assert_eq!(split_merged("14:58 only time"), None);
        assert_eq!(split_merged("no time here"), None);
    }

    #[test]
    fn the_catalog_key_follows_which_parts_are_present() {
        let n = |u: Option<&str>, p: &str, t: &str| Name {
            title: "Alice".into(),
            unread: u.map(str::to_owned),
            preview: p.into(),
            time: t.into(),
        };
        assert_eq!(n(None, "", "").key(), "row-000");
        assert_eq!(n(None, "hi", "15:00").key(), "row-011");
        assert_eq!(n(Some("2"), "", "15:00").key(), "row-101");
        assert_eq!(n(Some("2"), "hi", "15:00").key(), "row-111");
        let names: Vec<String> = n(Some("2"), "hi", "15:00")
            .args()
            .into_iter()
            .map(|a| a.0)
            .collect();
        assert_eq!(names, ["title", "n", "preview", "time"]);
    }

    #[test]
    fn web_a_2026_10_row_with_a_hashed_badge_class_says_unread() {
        // Read from the live tree (Firefox ESR, Telegram Web A, 2026-10-25): the row is
        // `a.ListItem-button`, the badge has only hashed CSS-module classes.
        let cells = vec![
            cell(None, "ListItem-button", ""),
            cell(Some(0), "Avatar size-large peer-color-0 no-photo", ""),
            cell(Some(1), "inner", ""),
            cell(Some(2), "letters", "Q"),
            cell(Some(0), "fullName _8IVW0azL", "Q148"),
            cell(Some(0), "time", "22:28"),
            cell(Some(0), "last-message shared-canvas-container", ""),
            cell(Some(6), "sender-name", "Алексей"),
            cell(Some(6), "colon", ":"),
            cell(Some(6), "last-message-summary", "QBrailleXLWin.addon"),
            cell(Some(0), "NT-X2xr9 yf73Yld8 npoDLolY", "529"),
        ];
        let n = chat_name(&cells).expect("name");
        assert_eq!(n.unread.as_deref(), Some("529"));
        assert_eq!(n.title, "Q148");
        assert_eq!(n.time, "22:28");
        // The same row with the run leaf under the hashed block (Firefox splits container text).
        let runs = vec![
            cell(None, "ListItem-button", ""),
            cell(Some(0), "fullName", ""),
            cell(Some(1), "", "Q148"),
            cell(Some(0), "last-message", ""),
            cell(Some(3), "sender-name", "Алексей"),
            cell(Some(3), "colon", ":"),
            cell(Some(3), "last-message-summary", "hello"),
            cell(Some(0), "NT-X2xr9 yf73Yld8 npoDLolY", ""),
            cell(Some(7), "", "529"),
        ];
        let n = chat_name(&runs).expect("name");
        assert_eq!(n.unread.as_deref(), Some("529"));
        assert_eq!(n.preview, "Алексей: hello");
        // A read chat has no counter and a numeric preview is not one.
        let read = vec![
            cell(None, "ListItem-button", ""),
            cell(Some(0), "fullName", "Bob"),
            cell(Some(0), "last-message", "42"),
        ];
        assert_eq!(chat_name(&read).and_then(|n| n.unread), None);
    }

    #[test]
    fn count_shapes() {
        for t in ["3", "529", "182,6K", "1.2K", "2M"] {
            assert!(looks_like_count(t), "{t}");
        }
        for t in ["", "22:28", "Q148", "1,2", "12345678", "K", "1,234K"] {
            assert!(!looks_like_count(t), "{t}");
        }
    }

    #[test]
    fn time_shapes() {
        for t in ["15:00", "9:41", "15:00:30"] {
            assert!(looks_like_time(t), "{t}");
        }
        for t in ["Alice", "1", "see you soon", "15:", "2025-10-24", "123:45"] {
            assert!(!looks_like_time(t), "{t}");
        }
    }

    #[test]
    fn a_row_without_a_title_is_left_alone() {
        let cells = vec![cell(None, "ListItem-button", ""), cell(Some(0), "", "x")];
        assert_eq!(chat_name(&cells), None);
    }
}
