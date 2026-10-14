//! Seeded property tests for segmentation, stepping, selection diffing, edit coalescing, echo
//! dedupe and offset conversion. Every failure message carries the seed and the input, so a
//! failing case replays exactly.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::single_range_in_vec_init,
    clippy::range_plus_one
)]

use std::ops::Range;

use gorca_model::TextUnit;
use gorca_text::coalesce::{EditCoalescer, Splice, compose};
use gorca_text::echo::{Consume, ExpectRing};
use gorca_text::space::{CharsSpace, Dir, Lookup, Step, TextSpace, Unit};
use gorca_text::{seg, sel, units};
use unicode_segmentation::UnicodeSegmentation;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

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

const ALPHABET: &[char] = &[
    'a',
    'b',
    'Z',
    'я',
    'Ж',
    'ё',
    'д',
    'é',
    '5',
    '-',
    '_',
    ' ',
    ' ',
    ' ',
    '.',
    ',',
    '!',
    '?',
    '\n',
    '\r',
    '\u{308}',
    '\u{301}',
    '\u{1F600}',
    '\u{1F468}',
    '\u{200D}',
    '\u{1F469}',
    '\u{1F1E9}',
    '\u{1F1EA}',
    '\u{FE0F}',
    '\u{1F3FD}',
];

fn random_text(rng: &mut Rng, max: usize) -> Vec<char> {
    let n = rng.below(max + 1);
    (0..n)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect()
}

fn s(chars: &[char]) -> String {
    chars.iter().collect()
}

const CASES: u64 = 3000;

#[test]
fn char_range_equals_unicode_graphemes() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed);
        let t = random_text(&mut rng, 40);
        let text = s(&t);
        let mut expect: Vec<Range<usize>> = Vec::new();
        let mut at = 0;
        for g in text.graphemes(true) {
            let n = g.chars().count();
            expect.push(at..at + n);
            at += n;
        }
        for r in &expect {
            for i in r.clone() {
                assert_eq!(seg::char_range(&t, i), *r, "seed {seed} i {i} {text:?}");
            }
        }
        if !t.is_empty() {
            let last = expect.last().cloned().unwrap_or(0..0);
            assert_eq!(seg::char_range(&t, t.len() + 9), last, "clamp, seed {seed}");
        }
    }
}

#[test]
fn word_and_sentence_ranges_are_consistent_partitions() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0xABCD);
        let t = random_text(&mut rng, 60);
        // Words are defined inside one hard line (callers clamp); sentences span the text.
        let lines: Vec<Range<usize>> = seg::soft_lines(&t, 0).collect();
        for line in &lines {
            let l = &t[line.clone()];
            let l = if l.last() == Some(&'\n') {
                &l[..l.len() - 1]
            } else {
                l
            };
            let mut at = 0;
            while at < l.len() {
                let r = seg::word_range(l, at);
                assert!(
                    r.start <= at && at < r.end && r.end <= l.len(),
                    "word contains: seed {seed} at {at} r {r:?} {:?}",
                    s(l)
                );
                for j in r.clone() {
                    assert_eq!(
                        seg::word_range(l, j),
                        r,
                        "word consistency: seed {seed} j {j} {:?}",
                        s(l)
                    );
                }
                // Ranges start and end on grapheme boundaries.
                assert_eq!(seg::char_range(l, r.start).start, r.start, "seed {seed}");
                assert_eq!(
                    seg::char_range(l, r.end - 1).end,
                    r.end,
                    "seed {seed} {:?}",
                    s(l)
                );
                at = r.end;
            }
        }
        for (name, f) in [(
            "sentence",
            seg::sentence_range as fn(&[char], usize) -> Range<usize>,
        )] {
            let mut at = 0;
            while at < t.len() {
                let r = f(&t, at);
                assert!(
                    r.start <= at && at < r.end && r.end <= t.len(),
                    "{name} contains: seed {seed} at {at} r {r:?} {:?}",
                    s(&t)
                );
                for j in r.clone() {
                    assert_eq!(
                        f(&t, j),
                        r,
                        "{name} consistency: seed {seed} j {j} {:?}",
                        s(&t)
                    );
                }
                at = r.end;
            }
            assert!(f(&t, t.len() + 5).end <= t.len());
        }
    }
}

#[test]
fn soft_lines_tile_and_respect_the_limit() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0x1234);
        let t = random_text(&mut rng, 80);
        let max = 1 + rng.below(12) as u16;
        let lines: Vec<_> = seg::soft_lines(&t, max).collect();
        let mut at = 0;
        for (k, r) in lines.iter().enumerate() {
            assert_eq!(r.start, at, "gap: seed {seed} {:?}", s(&t));
            let content = r.len()
                - usize::from(t.get(r.end.wrapping_sub(1)) == Some(&'\n') && r.end > r.start);
            assert!(
                content <= usize::from(max) || r.len() == 1,
                "line too long: seed {seed} max {max} r {r:?} {:?}",
                s(&t)
            );
            assert!(
                !r.is_empty() || k == lines.len() - 1,
                "empty line mid-text: seed {seed}"
            );
            at = r.end;
        }
        assert_eq!(at, t.len(), "coverage: seed {seed} {:?}", s(&t));
    }
}

fn collect_units(space: &CharsSpace<'_>, unit: Unit, dir: Dir, start: u32) -> Vec<Range<u32>> {
    let mut out = Vec::new();
    let mut at = start;
    loop {
        let Some(Lookup::Ready(Step::Moved(r))) = Some(space.step(at, unit, dir)) else {
            return out;
        };
        out.push(r.clone());
        at = r.start;
        assert!(
            out.len() < 10_000,
            "no progress {unit:?} {dir:?} text {:?} last {:?}",
            s(space.chars()),
            &out[out.len() - 3..]
        );
    }
}

#[test]
fn stepping_next_then_prev_walks_the_same_units() {
    for seed in 0..1000u64 {
        let mut rng = Rng::new(seed ^ 0x7777);
        let t = random_text(&mut rng, 60);
        let sp = CharsSpace::new(&t, 1 + rng.below(20) as u16);
        for unit in [
            Unit::Char,
            Unit::Word,
            Unit::Line,
            Unit::Sentence,
            Unit::Paragraph,
        ] {
            let first = sp.unit_at(0, unit).ready().unwrap_or(0..0);
            let mut fwd = vec![first.clone()];
            fwd.extend(collect_units(&sp, unit, Dir::Next, 0));
            // The units tile the text.
            let mut at = 0;
            for r in &fwd {
                assert_eq!(r.start, at, "{unit:?} tiling: seed {seed} {:?}", s(&t));
                at = r.end;
            }
            let end_ok = at == t.len() as u32 || t.is_empty();
            assert!(end_ok, "{unit:?} coverage: seed {seed} {:?}", s(&t));
            // Walking back from the start of the last unit visits the earlier ones in reverse.
            let Some(last) = fwd.last().cloned() else {
                continue;
            };
            if last.is_empty() {
                continue;
            }
            let mut back = vec![last.clone()];
            back.extend(collect_units(&sp, unit, Dir::Prev, last.start));
            back.reverse();
            let nonempty: Vec<_> = fwd.iter().filter(|r| !r.is_empty()).cloned().collect();
            assert_eq!(back, nonempty, "{unit:?} back: seed {seed} {:?}", s(&t));
        }
    }
}

fn bits(set: &[Range<u32>]) -> u128 {
    let mut b = 0u128;
    for r in set {
        for i in r.clone() {
            b |= 1 << i;
        }
    }
    b
}

fn random_ranges(rng: &mut Rng) -> Vec<Range<u32>> {
    (0..rng.below(5))
        .map(|_| {
            let a = rng.below(64) as u32;
            let b = rng.below(64) as u32;
            // Includes empty and inverted ranges on purpose.
            a..b
        })
        .collect()
}

#[test]
fn selection_diff_matches_a_bitset_model() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0x5E1);
        let old = random_ranges(&mut rng);
        let new = random_ranges(&mut rng);
        let d = sel::diff(&old, &new);
        let (o, n) = (bits(&old), bits(&new));
        assert_eq!(
            bits(&d.added),
            n & !o,
            "added: seed {seed} {old:?} -> {new:?}"
        );
        assert_eq!(
            bits(&d.removed),
            o & !n,
            "removed: seed {seed} {old:?} -> {new:?}"
        );
        assert_eq!(bits(&d.added) & bits(&d.removed), 0);
        assert_eq!(
            (o & !bits(&d.removed)) | bits(&d.added),
            n,
            "apply: seed {seed}"
        );
        for set in [&d.added, &d.removed] {
            assert!(
                set.windows(2).all(|w| w[0].end < w[1].start),
                "sorted and disjoint: seed {seed} {set:?}"
            );
            assert!(set.iter().all(|r| r.start < r.end));
        }
        assert_eq!(sel::diff(&new, &new), sel::SelDelta::default());
    }
}

fn apply(text: &[u32], sp: Splice, fresh: &mut u32) -> Vec<u32> {
    let at = (sp.at as usize).min(text.len());
    let end = (at + sp.removed as usize).min(text.len());
    let mut out = text[..at].to_vec();
    for _ in 0..sp.inserted {
        *fresh += 1;
        out.push(*fresh);
    }
    out.extend_from_slice(&text[end..]);
    out
}

/// `net` explains the change from `orig` to `fin`: everything outside it is untouched.
fn explains(orig: &[u32], fin: &[u32], net: Splice) -> bool {
    let at = net.at as usize;
    let tail = at + net.removed as usize;
    let fin_tail = at + net.inserted as usize;
    tail <= orig.len()
        && fin_tail <= fin.len()
        && orig[..at] == fin[..at]
        && orig[tail..] == fin[fin_tail..]
}

fn random_splice(rng: &mut Rng, len: u32) -> Splice {
    let at = rng.below(len as usize + 1) as u32;
    let removed = rng.below(4).min((len - at) as usize) as u32;
    Splice {
        at,
        removed,
        inserted: rng.below(4) as u32,
    }
}

#[test]
fn compose_is_a_correct_net_splice() {
    let mut composed = 0u32;
    for seed in 0..CASES * 3 {
        let mut rng = Rng::new(seed ^ 0xC0);
        let orig: Vec<u32> = (0..20).collect();
        let mut fresh = 1000;
        let a = random_splice(&mut rng, orig.len() as u32);
        let mid = apply(&orig, a, &mut fresh);
        let b = random_splice(&mut rng, mid.len() as u32);
        let fin = apply(&mid, b, &mut fresh);
        if let Some(net) = compose(a, b) {
            composed += 1;
            assert!(
                explains(&orig, &fin, net),
                "seed {seed}: {a:?} then {b:?} gave {net:?}"
            );
            assert_eq!(net.new_len(orig.len() as u32), fin.len() as u32);
        } else {
            // Apart: b really does not touch what a wrote.
            let (p, i) = (a.at, a.inserted);
            assert!(
                b.at > p + i || b.at + b.removed < p,
                "seed {seed}: {a:?} {b:?}"
            );
        }
    }
    assert!(
        composed > 1000,
        "the generator must exercise composition: {composed}"
    );
}

#[test]
fn coalescer_folds_a_walking_edit_stream_into_one_splice() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0xFEED);
        let orig: Vec<u32> = (0..30).collect();
        let mut text = orig.clone();
        let mut fresh = 5000;
        let mut c = EditCoalescer::new();
        let mut caret = 10u32;
        for _ in 0..1 + rng.below(12) {
            // Typing, Backspace or Delete at the caret, so every edit touches the previous.
            let sp = match rng.below(3) {
                0 => Splice::insert(caret, 1),
                1 if caret > 0 => Splice::delete(caret - 1, 1),
                _ if (caret as usize) < text.len() => Splice::delete(caret, 1),
                _ => Splice::insert(caret, 1),
            };
            text = apply(&text, sp, &mut fresh);
            caret = match sp.kind() {
                gorca_text::coalesce::SpliceKind::Insert => caret + 1,
                gorca_text::coalesce::SpliceKind::Delete => sp.at,
                _ => caret,
            };
            assert_eq!(c.push(sp), None, "seed {seed}: touching edits must fold");
        }
        let net = c.take().unwrap_or(Splice::insert(0, 0));
        assert!(explains(&orig, &text, net), "seed {seed}: {net:?}");
    }
}

#[test]
fn echo_dedupe_consumes_exactly_what_was_typed() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0xEC40);
        let typed: Vec<char> = (0..1 + rng.below(RING_TEST))
            .map(|_| ['а', 'я', 'x', 'Y', ' ', '1', 'ё'][rng.below(7)])
            .collect();
        let mut ring = ExpectRing::new();
        for (i, c) in typed.iter().enumerate() {
            ring.push(*c, i as u64);
        }
        // The app delivers the keys as a few coalesced inserts.
        let mut at = 0;
        while at < typed.len() {
            let n = 1 + rng.below(typed.len() - at);
            let chunk = s(&typed[at..at + n]);
            assert_eq!(
                ring.consume_insert(&chunk, 50),
                Consume::Matched { n },
                "seed {seed} {chunk:?}"
            );
            at += n;
        }
        assert!(ring.is_empty());
        // A paste that is not the typed text consumes nothing.
        ring.push('q', 60);
        assert_eq!(ring.consume_insert("qq", 61), Consume::Unmatched);
        assert_eq!(ring.len(), 1);
    }
}

const RING_TEST: usize = 16;

#[test]
fn offset_conversion_roundtrips() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0x0FF);
        let t = random_text(&mut rng, 30);
        let text = s(&t);
        // Utf8: every scalar boundary round-trips.
        for sc in 0..=t.len() as u32 {
            let raw = units::scalar_to_raw(&text, sc, TextUnit::Utf8);
            assert_eq!(
                raw.and_then(|r| units::raw_to_scalar(&text, r, TextUnit::Utf8)),
                Some(sc),
                "utf8 seed {seed}"
            );
        }
        // Utf16Bom: build the application's string with a phantom after each astral scalar.
        let mut app = String::new();
        for c in &t {
            app.push(*c);
            if u32::from(*c) > 0xFFFF {
                app.push('\u{FEFF}');
            }
        }
        for sc in 0..=t.len() as u32 {
            let raw = units::scalar_to_raw(&app, sc, TextUnit::Utf16Bom);
            assert_eq!(
                raw.and_then(|r| units::raw_to_scalar(&app, r, TextUnit::Utf16Bom)),
                Some(sc),
                "utf16 seed {seed} sc {sc} {text:?}"
            );
        }
        // Every raw offset maps into range and never to a mid-pair position.
        let raw_len = app.chars().count() as u32;
        for raw in 0..=raw_len {
            let sc = units::raw_to_scalar(&app, raw, TextUnit::Utf16Bom);
            assert!(
                sc.is_some_and(|v| v as usize <= t.len()),
                "seed {seed} raw {raw}"
            );
        }
        assert_eq!(
            units::raw_to_scalar(&app, raw_len + 1, TextUnit::Utf16Bom),
            None
        );
    }
}
