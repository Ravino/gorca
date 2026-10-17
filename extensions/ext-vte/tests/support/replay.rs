// SPDX-License-Identifier: GPL-3.0-or-later
//! Replay of a REAL gnome-terminal 3.56 / VTE 0.80 session through the terminal policy.
//!
//! `tests/fixtures/gnome-terminal-3.56-vte-0.80-session.jsonl` was recorded by
//! `tests/term/vte_probe.py` in a private headless session (see `tests/term/README.md`): the
//! AT-SPI events of a real `gnome-terminal-server`, the keys the probe pressed through a Mutter
//! RemoteDesktop session, and `GetText(0, -1)` snapshots after each phase. The test rebuilds the
//! screen from the events (and checks it against every recorded snapshot, so the replay is
//! faithful), feeds the extension exactly what the host would, answers its screen reads with the
//! rebuilt screen, and compares what it says with what a user should hear.
//!
//! Two deliveries are replayed: the raw events, and the events as the ingress T-1 collapse hands
//! them to the Reader (a delete followed at once by an insert becomes one `Replaced`).

#![allow(
    clippy::if_not_else,
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::match_same_arms
)]

use gorca_ext_api::terminal::{
    Ms, TermAction, TermEdit, TermInput, TermKey, TermSnapshot, TerminalHooks, Unit, Urgency,
};

pub const SESSION: &str = include_str!("../fixtures/gnome-terminal-3.56-vte-0.80-session.jsonl");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub at: Ms,
    pub text: String,
    pub unit: Unit,
    pub urgency: Urgency,
}

pub struct Replay {
    pub ext: Box<dyn TerminalHooks>,
    now: Ms,
    screen: Vec<char>,
    caret: u32,
    tick_due: Option<Ms>,
    read_due: Option<Ms>,
    pub said: Vec<Said>,
    /// Every action the extension answered, with the time (the parity tests compare all of them).
    pub actions: Vec<(Ms, TermAction)>,
    /// The extension's snapshot after every input.
    pub snaps: Vec<Option<TermSnapshot>>,
    /// Marker name and the index into `said` where it started.
    pub marks: Vec<(String, usize)>,
}

fn key_of(name: &str) -> TermKey {
    match name {
        "backspace" => TermKey::Backspace,
        "tab" => TermKey::Tab,
        "enter" => TermKey::Enter,
        "left" => TermKey::CharLeft,
        "right" => TermKey::CharRight,
        "up" => TermKey::LineUp,
        "down" => TermKey::LineDown,
        "home" => TermKey::LineStart,
        "end" => TermKey::LineEnd,
        "ctrl-c" => TermKey::Interrupt,
        "ctrl-a" => TermKey::LineStart,
        "ctrl-e" => TermKey::LineEnd,
        "ctrl-u" | "ctrl-l" | "ctrl-left" => TermKey::Other,
        c if c.chars().count() == 1 => TermKey::Typed(c.chars().next().unwrap_or(' ')),
        other => panic!("unknown key {other}"),
    }
}

impl Replay {
    fn new(make: &dyn Fn() -> Box<dyn TerminalHooks>) -> Self {
        Self {
            ext: make(),
            now: 0,
            screen: Vec::new(),
            caret: 0,
            tick_due: None,
            read_due: None,
            said: Vec::new(),
            actions: Vec::new(),
            snaps: Vec::new(),
            marks: Vec::new(),
        }
    }

    fn deliver(&mut self, input: TermInput) {
        let mut out = Vec::new();
        self.ext.step(self.now, input, &mut out);
        self.snaps.push(self.ext.snapshot());
        for a in out {
            self.actions.push((self.now, a.clone()));
            match a {
                TermAction::Say {
                    text,
                    unit,
                    urgency,
                    sensitive: _,
                } => self.said.push(Said {
                    at: self.now,
                    text,
                    unit,
                    urgency,
                }),
                TermAction::FetchScreen => self.read_due = Some(self.now + 2),
                TermAction::ArmTimer(t) => {
                    self.tick_due = Some(self.tick_due.map_or(t, |d| d.min(t)));
                }
                TermAction::ClaimKey => {}
            }
        }
    }

    fn text(&self) -> String {
        self.screen.iter().collect()
    }

    /// Run the clock to `t`, delivering timer ticks and screen reads on the way.
    fn advance_to(&mut self, t: Ms) {
        while self.now < t {
            self.now += 1;
            if self.read_due.is_some_and(|d| d <= self.now) {
                self.read_due = None;
                let input = TermInput::Screen {
                    text: self.text(),
                    caret: Some(self.caret),
                };
                self.deliver(input);
            }
            if self.tick_due.is_some_and(|d| d <= self.now) {
                self.tick_due = None;
                self.deliver(TermInput::Tick);
            }
        }
    }

    fn apply_insert(&mut self, off: usize, text: &str) {
        let at = off.min(self.screen.len());
        for (i, c) in text.chars().enumerate() {
            self.screen.insert(at + i, c);
        }
    }

    fn apply_delete(&mut self, off: usize, len: usize) {
        let end = (off + len).min(self.screen.len());
        let start = off.min(end);
        self.screen.drain(start..end);
    }
}

pub fn run(session: &str, collapse: bool, make: &dyn Fn() -> Box<dyn TerminalHooks>) -> Replay {
    let mut r = Replay::new(make);
    let mut pending_delete: Option<(u32, u32, Ms)> = None;
    let mut started = false;
    for line in session.lines() {
        let e: serde_json::Value = serde_json::from_str(line).expect("fixture line");
        let t = e["t"].as_f64().unwrap_or(0.0) as Ms;
        r.advance_to(t);
        // A delete is held back for a possible insert right behind it (T-1).
        if let Some((off, len, at)) = pending_delete {
            let next_is_insert = e["ev"] == "text-changed" && e["kind"] == "insert";
            if !(collapse && next_is_insert && t.saturating_sub(at) <= 3) {
                if collapse {
                    r.deliver(TermInput::Edit(TermEdit::Delete { offset: off, len }));
                }
                pending_delete = None;
            }
        }
        match e["ev"].as_str().unwrap_or("") {
            "snapshot" => {
                let text = e["text"].as_str().unwrap_or("");
                if !started {
                    r.screen = text.chars().collect();
                    r.caret = u32::try_from(e["caret"].as_u64().unwrap_or(0)).unwrap_or(0);
                    r.deliver(TermInput::Focus);
                    started = true;
                } else {
                    // The replay is faithful only if the rebuilt screen is the recorded one.
                    r.advance_to(t + 100);
                    assert_eq!(r.text(), text, "snapshot {:?} at {t} ms", e["name"]);
                    assert_eq!(u64::from(r.caret), e["caret"].as_u64().unwrap_or(0));
                }
            }
            "marker" => r
                .marks
                .push((e["name"].as_str().unwrap_or("").to_owned(), r.said.len())),
            "key" => {
                let k = key_of(e["name"].as_str().unwrap_or(""));
                r.deliver(TermInput::Key(k));
            }
            "caret" => {
                r.caret = u32::try_from(e["off"].as_u64().unwrap_or(0)).unwrap_or(0);
                r.deliver(TermInput::Caret(r.caret));
            }
            "text-changed" => {
                let off = e["off"].as_u64().unwrap_or(0) as usize;
                let len = e["len"].as_u64().unwrap_or(0) as usize;
                let text = e["text"].as_str().unwrap_or("").to_owned();
                if e["kind"] == "insert" {
                    r.apply_insert(off, &text);
                    let edit = if collapse && pending_delete.take().is_some() {
                        TermEdit::Replaced
                    } else {
                        TermEdit::Insert {
                            offset: off as u32,
                            len: len as u32,
                            text: Some(text),
                        }
                    };
                    r.deliver(TermInput::Edit(edit));
                } else {
                    // The payload of a delete is the text that went.
                    assert_eq!(
                        text.chars().count(),
                        len,
                        "delete payload is the deleted text"
                    );
                    r.apply_delete(off, len);
                    if collapse {
                        pending_delete = Some((off as u32, len as u32, t));
                    } else {
                        r.deliver(TermInput::Edit(TermEdit::Delete {
                            offset: off as u32,
                            len: len as u32,
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    r.advance_to(r.now + 2000);
    r
}

/// What was said between the marker `name` and the next one.
pub fn during<'a>(r: &'a Replay, name: &str) -> Vec<&'a Said> {
    let i = r
        .marks
        .iter()
        .position(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no marker {name}"));
    let from = r.marks[i].1;
    let to = r.marks.get(i + 1).map_or(r.said.len(), |(_, s)| *s);
    r.said[from..to].iter().collect()
}

pub fn texts(v: &[&Said]) -> Vec<String> {
    v.iter().map(|s| s.text.clone()).collect()
}
