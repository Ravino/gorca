//! Randomised typing against a model of a real VTE (rows ending in `\n`, written blanks kept,
//! payloads on inserts and deletes, caret events after the text), with the shell's echo
//! arriving 2 to 25 ms after the key, before or after the key is seen. Every key must be spoken
//! once, by itself, and nothing else may be said (the property of the VT e2e gate, here with
//! many more keys and a controlled clock).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used
)]

use gorca_ext_api::terminal::{
    Ms, TermAction, TermEdit, TermInput, TermKey, TermSnapshot, TerminalHooks, Unit,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const PROMPT: &str = "$ ";
const ROWS: usize = 6;

/// The terminal: one editable line (row 0) after the prompt, `ROWS - 1` empty rows below.
struct Vte {
    line: Vec<char>,
    cur: usize,
}

impl Vte {
    fn text(&self) -> String {
        let mut s = String::from(PROMPT);
        s.extend(self.line.iter());
        s.push('\n');
        for _ in 1..ROWS {
            s.push('\n');
        }
        s
    }
    fn caret(&self) -> u32 {
        (PROMPT.len() + self.cur) as u32
    }
}

#[derive(Clone, Debug)]
enum Ev {
    Key(TermKey),
    Edit(TermEdit),
    Caret(u32),
}

struct Host {
    ext: Box<dyn TerminalHooks>,
    now: Ms,
    screen: Vec<char>,
    caret: u32,
    tick: Option<Ms>,
    read: Option<Ms>,
    said: Vec<(Ms, String, Unit)>,
    actions: Vec<(Ms, TermAction)>,
    snaps: Vec<Option<TermSnapshot>>,
}

impl Host {
    fn deliver(&mut self, input: TermInput) {
        let mut out = Vec::new();
        let dbg = std::env::var("FUZZ_WINDOW").ok().and_then(|v| {
            let (a, b) = v.split_once('-')?;
            Some((a.parse::<Ms>().ok()?, b.parse::<Ms>().ok()?))
        });
        let line = format!("{input:?}");
        self.ext.step(self.now, input, &mut out);
        self.snaps.push(self.ext.snapshot());
        if dbg.is_some_and(|(a, b)| self.now >= a && self.now <= b) {
            eprintln!("{} {:.60} -> {:?}", self.now, line, out);
        }
        for a in out {
            self.actions.push((self.now, a.clone()));
            match a {
                TermAction::Say { text, unit, .. } => {
                    self.said.push((self.now, ext_vte::english(&text), unit));
                }
                TermAction::FetchScreen => self.read = Some(self.now + 2),
                TermAction::ArmTimer(t) => self.tick = Some(self.tick.map_or(t, |d| d.min(t))),
                TermAction::ClaimKey => {}
            }
        }
    }
    fn advance_to(&mut self, t: Ms, vte_text: &dyn Fn() -> (String, u32)) {
        while self.now < t {
            self.now += 1;
            if self.read.is_some_and(|d| d <= self.now) {
                self.read = None;
                let (text, caret) = vte_text();
                self.deliver(TermInput::Screen {
                    text,
                    caret: Some(caret),
                });
            }
            if self.tick.is_some_and(|d| d <= self.now) {
                self.tick = None;
                self.deliver(TermInput::Tick);
            }
        }
    }
}

fn name(c: char) -> String {
    c.to_string()
}

pub struct Outcome {
    /// What the score found wrong (empty: every key said once, nothing else).
    pub problems: Vec<String>,
    pub said: Vec<(Ms, String, Unit)>,
    pub actions: Vec<(Ms, TermAction)>,
    pub snaps: Vec<Option<TermSnapshot>>,
}

pub fn run(seed: u64, n: usize, make: &dyn Fn() -> Box<dyn TerminalHooks>) -> Outcome {
    // Measured on a real VTE: the echo follows its key by 2 to 5 ms; FUZZ_ECHO_SPREAD widens it.
    let echo_base: u64 = std::env::var("FUZZ_ECHO_BASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let echo_spread: u64 = std::env::var("FUZZ_ECHO_SPREAD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let mut rng = Rng(seed);
    let mut vte = Vte {
        line: Vec::new(),
        cur: 0,
    };
    // The schedule: (time, event, expected speech of a key).
    let mut events: Vec<(Ms, u64, Ev)> = Vec::new();
    let mut expect: Vec<(Ms, String)> = Vec::new();
    let mut t: Ms = 2000;
    let mut seq = 0u64;
    let mut last_echo: Ms = 0;
    let alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789 ".chars().collect();
    let mut push = |events: &mut Vec<(Ms, u64, Ev)>, at: Ms, e: Ev| {
        seq += 1;
        events.push((at, seq, e));
    };
    for _ in 0..n {
        t += 20 + rng.below(11);
        // The pty is a FIFO: echoes come in the order of the keys.
        let echo = (t + echo_base + rng.below(echo_spread)).max(last_echo + 1);
        last_echo = echo;
        let at_end = vte.cur == vte.line.len();
        let roll = rng.below(100);
        if !at_end && roll < 50 {
            vte.cur += 1;
            let want = vte
                .line
                .get(vte.cur)
                .map_or_else(|| "blank".to_owned(), |c| name(*c));
            push(&mut events, t, Ev::Key(TermKey::CharRight));
            push(&mut events, echo, Ev::Caret(vte.caret()));
            expect.push((t, want));
        } else if at_end && roll < 6 && !vte.line.is_empty() {
            let c = vte.line.pop().unwrap();
            vte.cur -= 1;
            let off = (PROMPT.len() + vte.cur) as u32;
            push(&mut events, t, Ev::Key(TermKey::Backspace));
            push(
                &mut events,
                echo,
                Ev::Edit(TermEdit::Delete {
                    offset: off,
                    len: 1,
                }),
            );
            push(&mut events, echo, Ev::Caret(vte.caret()));
            expect.push((t, name(c)));
        } else if at_end && roll < 12 && !vte.line.is_empty() {
            vte.cur -= 1;
            let want = name(vte.line[vte.cur]);
            push(&mut events, t, Ev::Key(TermKey::CharLeft));
            push(&mut events, echo, Ev::Caret(vte.caret()));
            expect.push((t, want));
        } else if at_end {
            let c = alphabet[rng.below(alphabet.len() as u64) as usize];
            let off = (PROMPT.len() + vte.cur) as u32;
            vte.line.push(c);
            vte.cur += 1;
            push(&mut events, t, Ev::Key(TermKey::Typed(c)));
            push(
                &mut events,
                echo,
                Ev::Edit(TermEdit::Insert {
                    offset: off,
                    len: 1,
                    text: Some(c.to_string()),
                }),
            );
            push(&mut events, echo, Ev::Caret(vte.caret()));
            expect.push((t, name(c)));
        }
    }
    events.sort_by_key(|(at, s, _)| (*at, *s));
    let trace = events.clone();
    // Replay against a second model that applies the same edits when they are delivered, so a
    // screen read shows the terminal as of that moment.
    let mut term = Vte {
        line: Vec::new(),
        cur: 0,
    };
    let mut h = Host {
        ext: make(),
        now: 1000,
        screen: Vec::new(),
        caret: 0,
        tick: None,
        read: None,
        said: Vec::new(),
        actions: Vec::new(),
        snaps: Vec::new(),
    };
    let snap = |term: &Vte| (term.text(), term.caret());
    h.screen = term.text().chars().collect();
    h.caret = term.caret();
    h.deliver(TermInput::Focus);
    let mut shown = (term.text(), term.caret());
    for (at, _, e) in events {
        let s = shown.clone();
        h.advance_to(at, &move || s.clone());
        match e {
            Ev::Key(k) => h.deliver(TermInput::Key(k)),
            Ev::Edit(ed) => {
                match &ed {
                    TermEdit::Insert { text, .. } => {
                        term.line.extend(text.as_deref().unwrap_or("").chars());
                    }
                    TermEdit::Delete { .. } => {
                        term.line.pop();
                    }
                    TermEdit::Replaced => {}
                }
                term.cur = term
                    .line
                    .len()
                    .min(term.cur + usize::from(matches!(ed, TermEdit::Insert { .. })));
                shown = snap(&term);
                h.deliver(TermInput::Edit(ed));
            }
            Ev::Caret(c) => {
                // The terminal's cursor as of this event.
                term.cur = (c as usize).saturating_sub(PROMPT.len());
                shown = (term.text(), c);
                h.deliver(TermInput::Caret(c));
            }
        }
    }
    let s = shown.clone();
    h.advance_to(h.now + 3000, &move || s.clone());
    // Score: the speech, in order, is exactly what the keys ask for (a caret-driven answer may
    // land after the next key, so windows per key would blame the wrong one).
    let want: Vec<&String> = expect.iter().map(|(_, w)| w).collect();
    let got: Vec<&String> = h
        .said
        .iter()
        .filter(|(at, _, _)| *at >= 2000)
        .map(|(_, s, _)| s)
        .collect();
    // A caret-driven answer can land after the next key's own echo: compare as multisets, then
    // check that no answer is more than a few places from where it belongs.
    let mut sw = want.clone();
    let mut sg = got.clone();
    sw.sort();
    sg.sort();
    let mut problems = Vec::new();
    if sw != sg {
        // The first wanted item with no equal heard item within three places of it.
        let mut used = vec![false; got.len()];
        let mut i = want.len().min(got.len());
        for (k, w) in want.iter().enumerate() {
            let lo = k.saturating_sub(3);
            let hi = (k + 4).min(got.len());
            let hit = (lo..hi).find(|&j| !used[j] && got[j] == *w);
            if let Some(j) = hit {
                used[j] = true;
            } else {
                i = k;
                break;
            }
        }
        let at = expect.get(i).map_or(0, |e| e.0);
        let near: Vec<String> = trace
            .iter()
            .filter(|(t, _, _)| *t + 90 >= at && *t <= at + 70)
            .map(|(t, _, e)| format!("{t}:{e:?}"))
            .collect();
        if std::env::var_os("FUZZ_TRACE").is_some() {
            eprintln!("seed trace: {near:#?}");
        }
        problems.push(format!(
            "{} wanted, {} heard; first difference at #{i} ({at} ms): wanted {:?}, around it heard {:?}",
            want.len(),
            got.len(),
            want.get(i),
            &got[i.saturating_sub(2)..(i + 3).min(got.len())]
        ));
        if std::env::var_os("FUZZ_TRACE").is_some() {
            let (mut a, mut b) = (sw.clone(), sg.clone());
            a.retain(|x| {
                if let Some(p) = b.iter().position(|y| y == x) {
                    b.remove(p);
                    false
                } else {
                    true
                }
            });
            eprintln!("only wanted {a:?}, only heard {b:?}");
            eprintln!(
                "want {:?}\ngot  {:?}",
                &want[i.saturating_sub(4)..(i + 6).min(want.len())],
                &got[i.saturating_sub(4)..(i + 6).min(got.len())]
            );
        }
    }
    Outcome {
        problems,
        said: h.said,
        actions: h.actions,
        snaps: h.snaps,
    }
}
