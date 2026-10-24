// SPDX-License-Identifier: GPL-3.0-or-later
//! M22b determinism property P4: `PresIr` survives its wire form exactly, and kinds this build
//! does not know pass through unchanged.
//!
//! `gorca-present-ir` defines the *shape* of the wire (codes, catalog-key strings, language tags)
//! but ships no byte codec; the codec lives at the plugin boundary. This file holds a small
//! reference codec built **only from the public code-based API** (`Reason::code/from_code`,
//! `Content::code/msg_key/from_wire_msg`, `ArgVal::code`, `FieldKind(u16)`, `LangTags`). If the
//! API is lossless, the reference codec round-trips; a value the API cannot carry shows up as a
//! counterexample here. See `docs/reports/m22b-determinism-properties.md` (P4).
//!
//! `proptest` is not vendored in the project-local cargo home, so the generator is the same
//! seeded xorshift used by the fusion property tests; every failure message carries the seed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gorca_i18n::MsgId;
use gorca_model::sem::GroupMask;
use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};
use gorca_present_ir::{
    Arg, ArgVal, Content, Field, FieldFlags, FieldKind, FlatSpan, IrFlags, LangId, LangTags,
    Origin, PRES_IR_VERSION, PresIr, Reason, RowId,
};
use smallvec::SmallVec;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
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
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn u16(&mut self) -> u16 {
        u16::try_from(self.next() & 0xFFFF).unwrap()
    }
    fn u32(&mut self) -> u32 {
        u32::try_from(self.next() & 0xFFFF_FFFF).unwrap()
    }
}

// ---- reference codec ---------------------------------------------------------------------------

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(u32::try_from(s.len()).unwrap());
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .map(|b| i64::from_le_bytes(b.try_into().unwrap()))
    }
    fn str(&mut self) -> Option<String> {
        let n = usize::try_from(self.u32()?).ok()?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn enc_args(w: &mut W, args: &[Arg]) {
    w.u32(u32::try_from(args.len()).unwrap());
    for a in args {
        w.str(&a.name);
        w.u16(a.val.code());
        match &a.val {
            ArgVal::Num(n) => w.i64(*n),
            ArgVal::Text(t) => w.str(t),
            ArgVal::Opaque { .. } => {}
        }
    }
}

fn dec_args(r: &mut R<'_>) -> Option<Vec<Arg>> {
    let n = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..n {
        let name = r.str()?;
        let val = match r.u16()? {
            0 => ArgVal::Num(r.i64()?),
            1 => ArgVal::Text(Arc::from(r.str()?)),
            code => ArgVal::opaque(code)?,
        };
        out.push(Arg {
            name: Arc::from(name),
            val,
        });
    }
    Some(out)
}

fn encode(ir: &PresIr, tags: &LangTags) -> Vec<u8> {
    let mut w = W::default();
    w.u32(PRES_IR_VERSION);
    w.u16(ir.reason.code());
    w.u8(u8::from(matches!(
        ir.reason,
        Reason::BrowseLine { entered: true }
    )));
    w.u32(match ir.reason {
        Reason::Change { changed } => changed.0,
        _ => 0,
    });
    w.u16(ir.node.app.connector.0);
    w.u32(ir.node.app.serial);
    w.u64(ir.node.local.0);
    w.u32(ir.sem_rev);
    w.u8(u8::from(ir.may_refine));
    w.u16(ir.flags.0);
    w.u32(u32::try_from(ir.fields.len()).unwrap());
    for f in &ir.fields {
        w.u16(f.kind.0);
        w.u16(f.flags.0);
        // A language travels as its tag, never as the process-local number.
        match f.lang.and_then(|l| tags.tag(l)) {
            Some(t) => {
                w.u8(1);
                w.str(t);
            }
            None => w.u8(0),
        }
        w.u16(f.content.code());
        match &f.content {
            Content::Text(t) => w.str(t),
            Content::Msg { id, args } => {
                w.str(id.key());
                enc_args(&mut w, args);
            }
            Content::Ext(x) => {
                w.str(&x.ns);
                w.str(&x.key);
                enc_args(&mut w, &x.args);
            }
            Content::UnknownMsg(u) => {
                w.str(&u.key);
                enc_args(&mut w, &u.args);
            }
            Content::Opaque { .. } => {}
        }
        w.u16(f.origin.row.0);
        w.u8(f.origin.group);
        w.u32(f.origin.via);
        match f.span {
            Some(s) => {
                w.u8(1);
                w.u32(s.start);
                w.u32(s.end);
            }
            None => w.u8(0),
        }
    }
    w.0
}

fn decode(bytes: &[u8], tags: &mut LangTags) -> Option<PresIr> {
    let mut r = R(bytes);
    if r.u32()? != PRES_IR_VERSION {
        return None;
    }
    let (code, entered, changed) = (r.u16()?, r.u8()? != 0, r.u32()?);
    let reason = Reason::from_code(code, entered, changed);
    let node = NodeKey {
        app: AppId {
            connector: ConnectorId(r.u16()?),
            serial: r.u32()?,
        },
        local: LocalId(r.u64()?),
    };
    let sem_rev = r.u32()?;
    let may_refine = r.u8()? != 0;
    let flags = IrFlags(r.u16()?);
    let nfields = r.u32()?;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    for _ in 0..nfields {
        let kind = FieldKind(r.u16()?);
        let fflags = FieldFlags(r.u16()?);
        let lang = if r.u8()? == 1 {
            tags.intern(&r.str()?)
        } else {
            None
        };
        let content = match r.u16()? {
            0 => Content::Text(Arc::from(r.str()?)),
            1 => {
                let key = r.str()?;
                Content::from_wire_msg(&key, dec_args(&mut r)?)
            }
            2 => {
                let (ns, key) = (r.str()?, r.str()?);
                Content::ext(&ns, &key, dec_args(&mut r)?)
            }
            code => Content::opaque(code)?,
        };
        let origin = Origin {
            row: RowId(r.u16()?),
            group: r.u8()?,
            via: r.u32()?,
        };
        let span = if r.u8()? == 1 {
            Some(FlatSpan {
                start: r.u32()?,
                end: r.u32()?,
            })
        } else {
            None
        };
        let mut f = Field::new(kind, content, origin).with_flags(fflags);
        f.lang = lang;
        f.span = span;
        fields.push(f);
    }
    if !r.0.is_empty() {
        return None;
    }
    let mut ir = PresIr::new(reason, node, sem_rev, fields);
    ir.may_refine = may_refine;
    ir.flags = flags;
    Some(ir)
}

// ---- generators --------------------------------------------------------------------------------

const TEXTS: &[&str] = &[
    "",
    "OK",
    "Save as...",
    "ünïcödé \u{1F600}",
    "a\u{0}b",
    "line\nbreak",
    "Сохранить",
];

fn text(r: &mut Rng) -> Arc<str> {
    Arc::from(TEXTS[r.below(TEXTS.len())])
}

/// `canonical` = only values the API's own decoders can produce (no `Unknown(c)` with a known
/// code, no `Opaque` that collides with a kind code).
fn gen_arg(r: &mut Rng, canonical: bool) -> Arg {
    let name: Arc<str> = Arc::from(["n", "l", "word", "x"][r.below(4)]);
    let val = match r.below(4) {
        0 => ArgVal::Num(i64::try_from(r.next() >> 1).unwrap() * if r.chance(50) { -1 } else { 1 }),
        1 | 2 => ArgVal::Text(text(r)),
        _ => ArgVal::opaque(if canonical {
            2 + r.u16().max(1) / 2 + 1
        } else {
            r.u16() % 4
        })
        .unwrap_or(ArgVal::Num(0)),
    };
    Arg { name, val }
}

fn gen_args(r: &mut Rng, canonical: bool) -> Vec<Arg> {
    (0..r.below(4)).map(|_| gen_arg(r, canonical)).collect()
}

fn gen_content(r: &mut Rng, canonical: bool) -> Content {
    match r.below(10) {
        0..=3 => Content::Text(text(r)),
        4..=6 => Content::msg(
            MsgId::ALL[r.below(MsgId::ALL.len())],
            gen_args(r, canonical),
        ),
        7 | 8 => Content::ext(
            ["ns.a", "ns.b"][r.below(2)],
            ["unread", "k"][r.below(2)],
            gen_args(r, canonical),
        ),
        _ => Content::opaque(if canonical {
            // never 0, 1, 2 (kind codes)
            3 + r.u16() % 0xFFF0
        } else {
            r.u16() % 6
        })
        .unwrap_or_else(|| Content::Text(text(r))),
    }
}

fn gen_reason(r: &mut Rng, canonical: bool) -> Reason {
    match r.below(14) {
        0 => Reason::Focus,
        1 => Reason::FocusAncestor,
        2 => Reason::BrowseLine {
            entered: r.chance(50),
        },
        3 => Reason::QuickNav,
        4 => Reason::SayAll,
        5 => Reason::Review,
        6 => Reason::Change {
            changed: GroupMask(r.u32()),
        },
        7 => Reason::ListEntry,
        8 => Reason::WhereAmI,
        9 => Reason::Window,
        10 => Reason::Notice,
        _ => Reason::unknown(if canonical {
            11 + r.u16() % 0xFFF0
        } else {
            r.u16() % 14
        }),
    }
}

/// A language table with a fixed interning order, so `LangId` numbers are comparable.
fn lang_table() -> LangTags {
    let mut t = LangTags::new();
    for tag in ["de", "fr-ca", "ru", "zh-hant"] {
        t.intern(tag).unwrap();
    }
    t
}

fn gen_ir(seed: u64, canonical: bool) -> PresIr {
    let mut r = Rng::new(seed);
    let kinds: [u16; 9] = [1, 2, 3, 4, 5, 13, 14, 0x7FFF, 0x8001];
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    for _ in 0..r.below(9) {
        let kind = FieldKind(if r.chance(85) {
            kinds[r.below(kinds.len())]
        } else {
            r.u16()
        });
        let origin = Origin {
            row: RowId(r.u16()),
            group: u8::try_from(r.below(256)).unwrap(),
            via: r.u32(),
        };
        let mut f = Field::new(kind, gen_content(&mut r, canonical), origin)
            // Reserved bits are preserved by editors: use all 16.
            .with_flags(FieldFlags(r.u16()));
        if r.chance(30) {
            f = f.with_lang(LangId(u16::try_from(1 + r.below(4)).unwrap()));
        }
        if r.chance(30) {
            f.span = Some(FlatSpan {
                start: r.u32(),
                end: r.u32(),
            });
        }
        fields.push(f);
    }
    let node = NodeKey {
        app: AppId {
            connector: ConnectorId(r.u16()),
            serial: r.u32(),
        },
        local: LocalId(r.next()),
    };
    let mut ir = PresIr::new(gen_reason(&mut r, canonical), node, r.u32(), fields);
    ir.may_refine = r.chance(50);
    ir.flags = IrFlags(r.u16());
    ir
}

fn trials() -> u64 {
    std::env::var("GORCA_PROP_TRIALS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000)
}

// ---- properties --------------------------------------------------------------------------------

/// P4.1 decode(encode(x)) == x for every canonical IR (all 16 flag bits, unregistered field kinds,
/// extension kinds, every `MsgId`, numeric arguments at the i64 extremes of the generator, empty
/// and NUL-bearing text, spans, languages).
#[test]
fn p4_1_encode_then_decode_is_the_identity_on_canonical_values() {
    for seed in 0..trials() {
        let ir = gen_ir(seed, true);
        let tags = lang_table();
        let bytes = encode(&ir, &tags);
        let mut dec_tags = lang_table();
        let back = decode(&bytes, &mut dec_tags)
            .unwrap_or_else(|| panic!("seed {seed}: decode failed on own encoding"));
        assert_eq!(back, ir, "seed {seed}");
    }
}

/// P4.2 the encoding is canonical: encode(decode(b)) == b for every b the encoder produced. Two
/// equal IRs therefore have one byte form (no representation freedom for a diff gate to trip on).
#[test]
fn p4_2_the_encoding_is_a_fixed_point_and_equal_values_have_equal_bytes() {
    for seed in 0..trials() {
        let ir = gen_ir(seed, true);
        let tags = lang_table();
        let bytes = encode(&ir, &tags);
        let mut dec_tags = lang_table();
        let back = decode(&bytes, &mut dec_tags).unwrap();
        assert_eq!(encode(&back, &dec_tags), bytes, "seed {seed}");
        // A deep rebuild (fresh Arcs) encodes identically: no pointer identity in the wire.
        let rebuilt = gen_ir(seed, true);
        assert_eq!(encode(&rebuilt, &tags), bytes, "seed {seed}");
    }
}

/// P4.3 unknown kinds pass through unchanged: a wire value with a reason code, content code,
/// argument code and field kind from a newer host decodes without error and re-encodes to the
/// identical bytes, and the in-memory form keeps the codes.
#[test]
fn p4_3_unknown_reason_content_arg_and_field_kinds_pass_through_byte_for_byte() {
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    fields.push(Field::new(
        FieldKind(0x0123), // unregistered core code
        Content::opaque(40).unwrap(),
        Origin::default(),
    ));
    fields.push(
        Field::new(
            FieldKind(0x8777), // extension range
            Content::Text(Arc::from("x")),
            Origin::default(),
        )
        .with_flags(FieldFlags(0xFFFE)), // reserved bits set
    );
    fields.push(Field::new(
        FieldKind::NAME,
        Content::msg(
            MsgId::ALL[0],
            vec![Arg {
                name: Arc::from("a"),
                val: ArgVal::opaque(9).unwrap(),
            }],
        ),
        Origin::default(),
    ));
    let mut ir = PresIr::new(
        Reason::unknown(999),
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 2,
            },
            local: LocalId(3),
        },
        7,
        fields,
    );
    ir.flags = IrFlags(0xFFFE);
    let tags = lang_table();
    let bytes = encode(&ir, &tags);
    let mut t2 = lang_table();
    let back = decode(&bytes, &mut t2).expect("an IR of a newer host decodes");
    assert_eq!(back.reason, Reason::unknown(999));
    assert!(!back.reason.editable());
    assert_eq!(back.fields[0].content, Content::opaque(40).unwrap());
    assert_eq!(back.fields[0].kind, FieldKind(0x0123));
    assert_eq!(back.fields[1].flags, FieldFlags(0xFFFE));
    assert_eq!(back.flags, IrFlags(0xFFFE));
    assert_eq!(
        back.fields[2].content, ir.fields[2].content,
        "an opaque argument is kept"
    );
    assert_eq!(encode(&back, &t2), bytes);
    assert_eq!(back, ir);
}

/// P4.4 an unknown Reason code is carried for every code outside the registered range.
#[test]
fn p4_4_every_unregistered_reason_code_is_kept() {
    for code in 9..=u16::MAX {
        let r = Reason::from_code(code, true, 0xFFFF_FFFF);
        assert_eq!(r, Reason::unknown(code));
        assert_eq!(r.code(), code);
    }
    // And the registered range is closed under code()/from_code() with its payloads.
    for code in 0..9u16 {
        for entered in [false, true] {
            for changed in [0u32, 1, 0x8000_0001, u32::MAX] {
                let r = Reason::from_code(code, entered, changed);
                assert_eq!(r.code(), code);
                assert!(r.editable());
                assert_eq!(
                    Reason::from_code(r.code(), entered, changed),
                    r,
                    "code {code}"
                );
            }
        }
    }
}

/// P4.5 the language of a field travels as its tag: the number is meaningful only inside one
/// `LangTags` table (hidden input). Decoding into a table that interned in a different order
/// gives a different `LangId` for the same content language, and equal IRs compare unequal.
#[test]
fn p4_5_lang_id_is_a_table_order_artifact_the_tag_is_the_identity() {
    let mut ir = gen_ir(1, true);
    ir.fields.clear();
    ir.fields.push(
        Field::new(
            FieldKind::NAME,
            Content::Text(Arc::from("x")),
            Origin::default(),
        )
        .with_lang(LangId(3)), // "ru" in lang_table()
    );
    let tags = lang_table();
    assert_eq!(tags.tag(LangId(3)), Some("ru"));
    let bytes = encode(&ir, &tags);

    let mut other = LangTags::new();
    other.intern("zh-hant").unwrap();
    other.intern("ru").unwrap();
    let back = decode(&bytes, &mut other).unwrap();
    assert_eq!(other.tag(back.fields[0].lang.unwrap()), Some("ru"));
    assert_ne!(
        back.fields[0].lang, ir.fields[0].lang,
        "same language, different number: compare tags, never LangId, across tables"
    );
    // The same interning order restores the exact value.
    let mut same = lang_table();
    assert_eq!(decode(&bytes, &mut same).unwrap(), ir);
    // Tag normalisation is idempotent.
    let mut t = LangTags::new();
    let a = t.intern("FR_ca").unwrap();
    assert_eq!(t.tag(a), Some("fr-ca"));
    assert_eq!(t.intern("fr-CA"), Some(a));
}

// ---- known gaps (counterexamples; see the report) ------------------------------------------------

/// G-P4-a (fixed): `Reason::Unknown` cannot be built outside the crate, and `Reason::unknown(c)`
/// maps a registered code to its own variant, so every constructible value round-trips.
#[test]
fn unknown_reason_never_collides_with_a_registered_code() {
    for code in 0..9u16 {
        let r = Reason::unknown(code);
        assert!(!matches!(r, Reason::Unknown { .. }), "code {code}");
        assert_eq!(r.code(), code);
        assert_eq!(Reason::from_code(r.code(), false, 0), r);
    }
    for code in 9..=u16::MAX {
        let r = Reason::unknown(code);
        assert!(matches!(r, Reason::Unknown { .. }));
        assert_eq!(Reason::from_code(r.code(), false, 0), r);
    }
}

/// G-P4-b (fixed): opaque content and argument kinds reject the registered kind codes on
/// construction, so they always encode and decode.
#[test]
fn opaque_kinds_reject_registered_kind_codes() {
    for code in 0..3u16 {
        assert_eq!(Content::opaque(code), None, "content {code}");
    }
    for code in 0..2u16 {
        assert_eq!(ArgVal::opaque(code), None, "arg {code}");
    }
    assert_eq!(Content::opaque(3).map(|c| c.code()), Some(3));
    assert_eq!(ArgVal::opaque(2).map(|a| a.code()), Some(2));
    assert_eq!(Content::opaque(u16::MAX).map(|c| c.code()), Some(u16::MAX));
}

/// G-P4-c (fixed): an unknown message key keeps its key and arguments, and re-encoding gives the
/// bytes that were received (the wire form is a plain message).
#[test]
fn unknown_message_key_is_forwarded_whole() {
    let c = Content::from_wire_msg(
        "a-key-of-a-newer-host",
        vec![Arg::num("n", 1), Arg::text("t", "x")],
    );
    assert_eq!(c.msg_key(), Some("a-key-of-a-newer-host"));
    let tags = lang_table();
    let mut ir = gen_ir(7, true);
    ir.fields.clear();
    ir.fields
        .push(Field::new(FieldKind::NAME, c.clone(), Origin::default()));
    let bytes = encode(&ir, &tags);
    let mut t = lang_table();
    let back = decode(&bytes, &mut t).unwrap();
    assert_eq!(back.fields[0].content, c);
    assert_eq!(encode(&back, &t), bytes);
}
