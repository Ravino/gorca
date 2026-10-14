//! Per-keystroke cost of the text layer. Target: well under 50 us per keystroke (the gorca-side
//! budget for keypress to speech start is 15 ms, ADR-0007 C1/E1).
//!
//! Run: `cargo bench -p gorca-text --bench keystroke [-- --samples N]`
//!
//! Each scenario is timed per call with `Instant`; the table shows p50, p99 and max in
//! nanoseconds. Timer overhead is about 20 to 40 ns per sample.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::single_range_in_vec_init,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use std::hint::black_box;
use std::time::Instant;

use gorca_text::caret::{Say, present};
use gorca_text::coalesce::{EditCoalescer, Splice};
use gorca_text::echo::{ExpectRing, InsertSource, on_insert, word_before};
use gorca_text::sel;
use gorca_text::space::CharsSpace;
use gorca_text::{Dir, TextSpace, Unit, seg};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn text(words: &[&str], n: usize, rng: &mut Rng) -> Vec<char> {
    let mut s = String::new();
    while s.chars().count() < n {
        s.push_str(words[rng.below(words.len())]);
        s.push(match rng.below(12) {
            0 => '\n',
            1 => '.',
            _ => ' ',
        });
    }
    s.chars().collect()
}

fn run(name: &str, samples: usize, mut f: impl FnMut(usize)) {
    for i in 0..samples / 10 {
        f(i);
    }
    let mut t = Vec::with_capacity(samples);
    for i in 0..samples {
        let a = Instant::now();
        f(i);
        t.push(a.elapsed().as_nanos() as u64);
    }
    t.sort_unstable();
    let p = |q: f64| t[((t.len() - 1) as f64 * q) as usize];
    println!(
        "| {name:<44} | {:>7} | {:>7} | {:>9} |",
        p(0.5),
        p(0.99),
        t[t.len() - 1]
    );
}

fn main() {
    let samples = std::env::args()
        .skip_while(|a| a != "--samples")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000usize);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let latin = text(
        &[
            "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog",
        ],
        4000,
        &mut rng,
    );
    let cyr = text(
        &[
            "съешь",
            "ещё",
            "этих",
            "мягких",
            "французских",
            "булок",
            "да",
            "выпей",
            "чаю",
        ],
        4000,
        &mut rng,
    );
    let long_line: Vec<char> = "слово ".repeat(400).chars().collect();

    println!("| scenario (per call)                          |     p50 |     p99 |       max |");
    println!("|----------------------------------------------|---------|---------|-----------|");

    run("typed char echo: ring push + dedupe match", samples, |i| {
        let mut ring = ExpectRing::new();
        let c = if i % 2 == 0 { 'a' } else { 'ж' };
        ring.push(c, i as u64);
        let mut buf = [0u8; 4];
        let t = c.encode_utf8(&mut buf);
        black_box(on_insert(
            &mut ring,
            t,
            i as u64,
            InsertSource::Unknown,
            true,
        ));
    });
    run("char_range, Latin 4k text", samples, |i| {
        black_box(seg::char_range(&latin, (i * 7) % latin.len()));
    });
    run("char_range, Cyrillic 4k text", samples, |i| {
        black_box(seg::char_range(&cyr, (i * 7) % cyr.len()));
    });
    let sp_cyr = CharsSpace::new(&cyr, 100);
    run("caret Right: present(Char), Cyrillic", samples, |i| {
        black_box(present(&sp_cyr, ((i * 7) % cyr.len()) as u32, Say::Char));
    });
    run("caret Ctrl+Right: present(Word), Cyrillic", samples, |i| {
        black_box(present(&sp_cyr, ((i * 7) % cyr.len()) as u32, Say::Word));
    });
    run("caret Down: present(Line), Cyrillic 4k", samples, |i| {
        black_box(present(&sp_cyr, ((i * 7) % cyr.len()) as u32, Say::Line));
    });
    let sp_long = CharsSpace::new(&long_line, 100);
    run("caret Down: Line in a 2400-char hard line", samples, |i| {
        black_box(present(
            &sp_long,
            ((i * 13) % long_line.len()) as u32,
            Say::Line,
        ));
    });
    run("step(Word, Next), Latin", samples, |i| {
        let sp = CharsSpace::new(&latin, 100);
        black_box(sp.step(((i * 7) % latin.len()) as u32, Unit::Word, Dir::Next));
    });
    run("sentence_range, one paragraph", samples / 10, |i| {
        black_box(seg::sentence_range(&cyr[..600], (i * 7) % 600));
    });
    run("selection diff (Shift+Right extends by 1)", samples, |i| {
        let a = i as u32 % 3000;
        black_box(sel::diff(&[a..a + 40], &[a..a + 41]));
    });
    run("word_before (typed word on separator)", samples, |i| {
        black_box(word_before(&latin, 10 + i % 3000));
    });
    run("edit coalescer: push typing splice", samples, |i| {
        let mut c = EditCoalescer::new();
        black_box(c.push(Splice::insert(i as u32, 1)));
        black_box(c.push(Splice::insert(i as u32 + 1, 1)));
    });
    run("per keystroke total (Cyrillic typing)", samples, |i| {
        let at = ((i * 7) % (cyr.len() - 1)) as u32;
        let mut ring = ExpectRing::new();
        ring.push('ж', i as u64);
        black_box(on_insert(
            &mut ring,
            "ж",
            i as u64,
            InsertSource::Unknown,
            true,
        ));
        black_box(present(&sp_cyr, at, Say::Char));
        black_box(word_before(&cyr, at as usize));
        let mut c = EditCoalescer::new();
        black_box(c.push(Splice::insert(at, 1)));
        black_box(sel::diff(&[], &[]));
    });
}
