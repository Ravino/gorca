//! Reviewer probe (M6 review): hostile inputs against the ext-vte Session. The ext-api contract
//! says "an implementation never panics on any input and bounds every buffer it keeps".
//! Copy to extensions/ext-vte/tests/ and run `cargo test -p ext-vte --release --test probe_hostile -- --nocapture`.
use ext_vte::VteExtension;
use gorca_ext_api::terminal::{TermAction, TermConfig, TermEdit, TermInput, TermKey, TerminalHooks};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const ALPH: &[char] = &['a', 'b', ' ', '$', '#', '>', '\n', '\n', 'é', '中', '\u{301}', '🙂', 'x', '\t', '\r', '\0'];

fn rand_text(r: &mut Rng, max: u64) -> String {
    let n = r.below(max);
    (0..n).map(|_| ALPH[r.below(ALPH.len() as u64) as usize]).collect()
}

fn rand_off(r: &mut Rng) -> u32 {
    match r.below(6) {
        0 => u32::MAX,
        1 => u32::MAX - r.below(4) as u32,
        2 => r.below(100_000) as u32,
        _ => r.below(300) as u32,
    }
}

fn rand_input(r: &mut Rng) -> TermInput {
    match r.below(12) {
        0 | 1 => {
            let keys = [
                TermKey::Enter, TermKey::Backspace, TermKey::Delete, TermKey::Tab, TermKey::CharLeft,
                TermKey::CharRight, TermKey::WordLeft, TermKey::WordRight, TermKey::LineStart,
                TermKey::LineEnd, TermKey::LineUp, TermKey::LineDown, TermKey::Interrupt, TermKey::Other,
            ];
            TermInput::Key(keys[r.below(keys.len() as u64) as usize])
        }
        2 | 3 => TermInput::Key(TermKey::Typed(ALPH[r.below(ALPH.len() as u64) as usize])),
        4 => {
            let len = rand_off(r);
            let text = if r.below(2) == 0 { Some(rand_text(r, 40)) } else { None };
            TermInput::Edit(TermEdit::Insert { offset: rand_off(r), len, text })
        }
        5 => TermInput::Edit(TermEdit::Delete { offset: rand_off(r), len: rand_off(r) }),
        6 => TermInput::Edit(TermEdit::Replaced),
        7 => TermInput::Caret(rand_off(r)),
        8 | 9 => {
            let big = r.below(20) == 0;
            let text = if big { rand_text(r, 20_000) } else { rand_text(r, 400) };
            let caret = if r.below(4) == 0 { None } else { Some(rand_off(r)) };
            TermInput::Screen { text, caret }
        }
        10 => TermInput::ScreenFailed,
        _ => match r.below(3) {
            0 => TermInput::Focus,
            1 => TermInput::Blur,
            _ => TermInput::Tick,
        },
    }
}

#[test]
fn probe_hostile_inputs_never_panic() {
    let mut panics = 0u32;
    let mut worst_step = std::time::Duration::ZERO;
    let mut max_snapshot = 0usize;
    let mut first: Option<String> = None;
    for seed in 1..=400u64 {
        let res = std::panic::catch_unwind(|| {
            let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let mut cfg = TermConfig::default();
            if seed % 7 == 0 {
                cfg.prompt_regex = "(".to_owned(); // invalid
                cfg.secret_regex = "a{999999}".to_owned();
            }
            let mut e = VteExtension::new(&cfg);
            let mut out: Vec<TermAction> = Vec::new();
            let mut now = 0u64;
            let mut worst = std::time::Duration::ZERO;
            let mut maxs = 0usize;
            for _ in 0..3000 {
                now += r.below(50);
                let inp = rand_input(&mut r);
                out.clear();
                let t = Instant::now();
                e.step(now, inp, &mut out);
                worst = worst.max(t.elapsed());
                if let Some(s) = e.snapshot() {
                    maxs = maxs.max(s.text.len());
                }
            }
            (worst, maxs)
        });
        match res {
            Ok((w, m)) => {
                worst_step = worst_step.max(w);
                max_snapshot = max_snapshot.max(m);
            }
            Err(p) => {
                panics += 1;
                if first.is_none() {
                    let msg = p
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    first = Some(format!("seed {seed}: {msg}"));
                }
            }
        }
    }
    println!("panics={panics} first={first:?} worst_step={worst_step:?} max_snapshot_bytes={max_snapshot}");
    assert_eq!(panics, 0, "{first:?}");
}
