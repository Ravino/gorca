// SPDX-License-Identifier: GPL-3.0-or-later
//! M22b determinism properties P2 (presenter purity) and P3 (shadow diff stability and narrow
//! deviation predicates). Statements and proofs: `docs/reports/m22b-determinism-properties.md`.
//!
//! `proptest` is not vendored in the project-local cargo home, so the generator is the seeded
//! xorshift the fusion property tests use; every failure message carries the seed. Scale with
//! `GORCA_PROP_TRIALS`.
//!
//! Tests named `known_gap_*` pin a counterexample that exists today (they pass while the gap is
//! open and fail when it is fixed, which is the signal to turn them into the property they
//! document). Counterexamples to desired properties are recorded in the report.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::sync::Arc;

use gorca_i18n::{Lang, MsgId as M};
use gorca_model::sem::{
    Avail, Check, Current, Expand, GridPos, Invalid, Live, NameFrom, Orientation, Popup, PosSrc,
    Press, Select, SemEntry, SemFlags, SemRole, SemText, SetPos, Sort,
};
use gorca_model::{AppId, ConnectorId, LocalId, NodeKey, SegmentKind};
use gorca_present::ir::{Arg, Content, Field, FieldKind, Origin, PresIr, Reason};
use gorca_present::{
    ComposeCtx, Deviations, DiffCtx, DiffOutcome, LegacyToken, RenderOpts, ReportFlags, Token,
    TokenText, compose, compose_change, diff, render,
};

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
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

fn trials() -> u64 {
    std::env::var("GORCA_PROP_TRIALS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500)
}

fn key() -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(0),
            serial: 1,
        },
        local: LocalId(7),
    }
}

// ---- generators --------------------------------------------------------------------------------

const STRS: &[&str] = &[
    "",
    "OK",
    "Save",
    "Close window",
    "50 percent",
    "ünï \u{1F600}",
    "a\u{1e}ns:key\u{1f}x=y",
    "Сохранить",
    "  padded  ",
];

fn opt_str(r: &mut Rng) -> Option<Arc<str>> {
    r.chance(50).then(|| Arc::from(*r.pick(STRS)))
}

fn pick_code<T: Copy>(r: &mut Rng, all: &'static [T]) -> T {
    *r.pick(all)
}

fn gen_entry(r: &mut Rng) -> SemEntry {
    let mut e = SemEntry::default();
    let s = &mut e.sem;
    s.role = pick_code(r, SemRole::ALL);
    s.check = pick_code(r, Check::ALL);
    s.press = pick_code(r, Press::ALL);
    s.expand = pick_code(r, Expand::ALL);
    s.select = pick_code(r, Select::ALL);
    s.current = pick_code(r, Current::ALL);
    s.invalid = pick_code(r, Invalid::ALL);
    s.popup = pick_code(r, Popup::ALL);
    s.avail = pick_code(r, Avail::ALL);
    s.name_from = pick_code(r, NameFrom::ALL);
    s.orientation = pick_code(r, Orientation::ALL);
    s.sort = pick_code(r, Sort::ALL);
    s.flags = SemFlags::from_bits_truncate(u32::try_from(r.next() & 0xFFFF_FFFF).unwrap());
    s.level = u8::try_from(r.below(8)).unwrap();
    if r.chance(40) {
        s.pos = Some(SetPos {
            pos: u32::try_from(r.below(20)).unwrap(),
            size: std::num::NonZeroU32::new(u32::try_from(r.below(20)).unwrap()),
            src: pick_code(r, PosSrc::ALL),
        });
    }
    if r.chance(20) {
        s.grid = Some(GridPos {
            row: 1 + u32::try_from(r.below(9)).unwrap(),
            col: 1 + u32::try_from(r.below(9)).unwrap(),
            row_span: 1,
            col_span: 1,
        });
    }
    if r.chance(15) {
        s.live = Some(Live {
            politeness: gorca_model::event::Politeness::Polite,
            atomic: r.chance(50),
            relevant: gorca_model::sem::Relevant::all(),
            root: r.chance(50),
        });
    }
    if r.chance(70) {
        s.text = Some(Arc::new(SemText {
            description: opt_str(r),
            role_description: opt_str(r),
            error_text: opt_str(r),
            placeholder: opt_str(r),
            key_shortcuts: opt_str(r),
            value_text: opt_str(r),
            col_index_text: opt_str(r),
            row_index_text: opt_str(r),
            details_target: None,
            dom_id: None,
        }));
    }
    e.name = opt_str(r);
    e.sem_rev = u32::try_from(r.next() & 0xFFFF).unwrap();
    e
}

/// The same value with every `Arc` freshly allocated: nothing may depend on pointer identity.
fn deep_rebuild(e: &SemEntry) -> SemEntry {
    let re = |s: &Option<Arc<str>>| s.as_ref().map(|x| Arc::from(&**x));
    let mut out = e.clone();
    out.name = re(&e.name);
    out.sem.text = e.sem.text.as_ref().map(|t| {
        Arc::new(SemText {
            description: re(&t.description),
            role_description: re(&t.role_description),
            error_text: re(&t.error_text),
            placeholder: re(&t.placeholder),
            key_shortcuts: re(&t.key_shortcuts),
            value_text: re(&t.value_text),
            col_index_text: re(&t.col_index_text),
            row_index_text: re(&t.row_index_text),
            details_target: t.details_target,
            dom_id: re(&t.dom_id),
        })
    });
    out
}

fn gen_ctx(r: &mut Rng) -> ComposeCtx {
    ComposeCtx {
        flags: ReportFlags(u32::try_from(r.next() & 0xFF).unwrap()),
        may_refine: r.chance(50),
        web: r.chance(50),
        skip_selected: r.chance(30),
        hedge_inferred: false,
        lang: gorca_i18n::Lang::En,
    }
}

/// The comparable form of a rendered utterance.
type Spoken = Vec<(u8, String)>;

fn spoken(tokens: &[Token]) -> Spoken {
    tokens
        .iter()
        .map(|t| {
            (
                gorca_present::diff::kind_code(t.kind),
                t.text.as_str().to_owned(),
            )
        })
        .collect()
}

fn present(
    e: &SemEntry,
    reason: Reason,
    ctx: ComposeCtx,
    opts: RenderOpts<'_>,
) -> Option<(PresIr, Spoken)> {
    let ir = compose(e, key(), reason, ctx).ok()?;
    let s = spoken(&render(&ir, opts));
    Some((ir, s))
}

fn present_change(
    old: &SemEntry,
    new: &SemEntry,
    ctx: ComposeCtx,
    opts: RenderOpts<'_>,
) -> (PresIr, Spoken) {
    let ir = compose_change(old, new, key(), ctx);
    let s = spoken(&render(&ir, opts));
    (ir, s)
}

// ---- P2: presenter purity -------------------------------------------------------------------------

/// P2.1 `render(compose(e, reason, ctx), opts)` is a function: the same inputs, the same
/// outputs, byte for byte and field for field, run any number of times, on rebuilt copies of the
/// entry (fresh allocations), and across 8 threads at once.
#[test]
fn p2_1_compose_and_render_are_a_pure_function_of_entry_reason_and_settings() {
    let _pin = gorca_i18n::scoped(Lang::En);
    for seed in 0..trials() {
        let mut r = Rng::new(seed);
        let e = gen_entry(&mut r);
        let ctx = gen_ctx(&mut r);
        let opts = RenderOpts {
            description_pause: r.chance(50),
            ..RenderOpts::default()
        };
        let a = present(&e, Reason::Focus, ctx, opts).expect("focus has a rule row");
        let b = present(&e, Reason::Focus, ctx, opts).unwrap();
        let c = present(&deep_rebuild(&e), Reason::Focus, ctx, opts).unwrap();
        assert_eq!(a, b, "seed {seed}: second run differs");
        assert_eq!(a, c, "seed {seed}: pointer identity leaks into the output");

        let e2 = gen_entry(&mut r);
        let x = present_change(&e, &e2, ctx, opts);
        let y = present_change(&deep_rebuild(&e), &deep_rebuild(&e2), ctx, opts);
        assert_eq!(x, y, "seed {seed}: change presenter");
    }
}

#[test]
fn p2_2_thread_interleaving_cannot_change_the_output() {
    // Every thread pins the same language: the language is a hidden input (P2.5), not a race.
    let work: Vec<(SemEntry, ComposeCtx, RenderOpts<'_>)> = (0..400u64)
        .map(|seed| {
            let mut r = Rng::new(seed ^ 0x77);
            (
                gen_entry(&mut r),
                gen_ctx(&mut r),
                RenderOpts {
                    description_pause: r.chance(50),
                    ..RenderOpts::default()
                },
            )
        })
        .collect();
    let work = Arc::new(work);
    let serial: Vec<_> = {
        let _p = gorca_i18n::scoped(Lang::En);
        work.iter()
            .map(|(e, c, o)| present(e, Reason::Focus, *c, *o))
            .collect()
    };
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let work = Arc::clone(&work);
            std::thread::spawn(move || {
                let _p = gorca_i18n::scoped(Lang::En);
                // Each thread walks the list from a different offset.
                let n = work.len();
                let mut out = vec![None; n];
                for k in 0..n {
                    let i = (k * 7 + t * 53) % n;
                    let (e, c, o) = &work[i];
                    out[i] = Some(present(e, Reason::Focus, *c, *o));
                }
                out.into_iter().map(Option::unwrap).collect::<Vec<_>>()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), serial);
    }
}

/// P2.3 the spoken words read no provenance, confidence, taint, `known`, `stale` or `fallback`
/// bits: scrambling them changes nothing. (M22b1 slice 4: the provenance of the `name` group picks
/// the field kind, `INFERRED_LABEL` for a layout-inferred name, and the hedge only when
/// `ComposeCtx::hedge_inferred` is on; this test runs with it off, so the words stay equal while
/// the IR kind may differ. `p4_*` below pins the kind and the hedge.) (Consequence worth knowing: an `Unknown` group is
/// presented as its default; ADR-0022 section 2.3 says compose omits it. Not a determinism issue,
/// recorded in the report.)
#[test]
fn p2_3_provenance_taint_and_known_masks_are_not_inputs() {
    let _pin = gorca_i18n::scoped(Lang::En);
    for seed in 0..trials() {
        let mut r = Rng::new(seed ^ 0xA3);
        let e = gen_entry(&mut r);
        let mut f = e.clone();
        f.prov = gorca_model::sem::ProvTag([r.next(), r.next()]);
        f.hedge = gorca_model::sem::SlotBits2(r.next());
        f.taint = gorca_model::sem::SlotBits2(r.next());
        f.known = gorca_model::sem::GroupMask(u32::try_from(r.next() & 0xFFFF_FFFF).unwrap());
        f.stale = gorca_model::sem::GroupMask(u32::try_from(r.next() & 0xFFFF_FFFF).unwrap());
        f.fallback = gorca_model::sem::GroupMask(u32::try_from(r.next() & 0xFFFF_FFFF).unwrap());
        let ctx = gen_ctx(&mut r);
        let o = RenderOpts::default();
        let words = |x: Option<(gorca_present::PresIr, Spoken)>| x.map(|(_, w)| w);
        assert_eq!(
            words(present(&e, Reason::Focus, ctx, o)),
            words(present(&f, Reason::Focus, ctx, o)),
            "seed {seed}"
        );
        let g = gen_entry(&mut r);
        assert_eq!(
            present_change(&e, &g, ctx, o),
            present_change(&f, &g, ctx, o),
            "seed {seed}"
        );
    }
}

/// P2.4 `sem_rev` and `may_refine` reach the IR (cache key and refine guard) and `flags`
/// reaches nothing else: the spoken tokens do not depend on them.
#[test]
fn p2_4_sem_rev_and_report_flags_do_not_change_the_words() {
    let _pin = gorca_i18n::scoped(Lang::En);
    for seed in 0..trials() {
        let mut r = Rng::new(seed ^ 0x51);
        let e = gen_entry(&mut r);
        let mut f = e.clone();
        f.sem_rev = f.sem_rev.wrapping_add(1);
        let mut ctx = gen_ctx(&mut r);
        let o = RenderOpts::default();
        let a = present(&e, Reason::Focus, ctx, o).unwrap();
        let b = present(&f, Reason::Focus, ctx, o).unwrap();
        assert_eq!(a.1, b.1, "seed {seed}");
        assert_eq!(a.0.sem_rev + 1, b.0.sem_rev, "wrapping aside");
        ctx.flags = ReportFlags(ctx.flags.0 ^ 0xFF);
        assert_eq!(
            a.1,
            present(&e, Reason::Focus, ctx, o).unwrap().1,
            "seed {seed}"
        );
    }
}

/// P2.5 the UI language is an explicit input (fixed): `ComposeCtx::lang` and `RenderOpts::lang`
/// carry it, so the output is a function of (entry, reason, ctx, opts) and the process-wide
/// language and the thread pin are not read at all.
#[test]
fn p2_5_the_ui_language_is_an_explicit_input_of_compose_and_render() {
    let mut e = SemEntry::default();
    e.sem.role = SemRole::Checkbox;
    e.sem.check = Check::On;
    e.name = Some(Arc::from("Mute"));
    let ctx_in = |lang| ComposeCtx {
        flags: ReportFlags(0),
        may_refine: false,
        web: false,
        skip_selected: false,
        hedge_inferred: false,
        lang,
    };
    let opts_in = |lang| RenderOpts {
        lang,
        ..RenderOpts::default()
    };
    let with = |lang: Lang| present(&e, Reason::Focus, ctx_in(lang), opts_in(lang)).unwrap();
    let (en, ru) = (with(Lang::En), with(Lang::Ru));
    assert_eq!(en.0, ru.0, "the IR is language-free (words travel as ids)");
    assert_ne!(
        en.1, ru.1,
        "the rendered tokens follow the explicit language"
    );
    // The ambient language (process atomic and thread pin) changes nothing.
    for ambient in Lang::ALL {
        let _p = gorca_i18n::scoped(ambient);
        assert_eq!(with(Lang::En), en, "ambient {ambient:?}");
        assert_eq!(with(Lang::Ru), ru, "ambient {ambient:?}");
    }
}

/// P2.6 the extension catalogs are an explicit input (fixed): `RenderOpts::ext` is an immutable
/// snapshot, so registering or unregistering in the process-wide registry after the snapshot was
/// taken changes nothing, and the default options (no catalogs) render an add-on field as absent.
#[test]
fn p2_6_the_extension_catalogs_are_an_explicit_snapshot_of_render() {
    let ns = "m22b-determinism-ns";
    let ir = PresIr::new(
        Reason::Focus,
        key(),
        0,
        [Field::new(
            FieldKind(0x8000),
            Content::ext(ns, "hello", vec![Arg::num("n", 2)]),
            Origin::default(),
        )]
        .into_iter()
        .collect(),
    );
    let before = gorca_i18n::ext::snapshot();
    let errors = gorca_i18n::ext::register(ns, Lang::En, "hello = hi {n}\n");
    assert!(errors.is_empty(), "{errors:?}");
    let after = gorca_i18n::ext::snapshot();
    let with = |ext: &gorca_i18n::ext::ExtCatalogs| {
        spoken(&render(
            &ir,
            RenderOpts {
                ext,
                ..RenderOpts::default()
            },
        ))
    };
    // The registry now has the namespace; only the snapshot decides what renders.
    assert!(
        with(&before).is_empty(),
        "a snapshot taken earlier is unchanged"
    );
    assert!(spoken(&render(&ir, RenderOpts::default())).is_empty());
    assert_eq!(with(&after), vec![(0, "hi 2".to_owned())]);
    assert_eq!(with(&after), vec![(0, "hi 2".to_owned())], "immutable");
}

/// P2.7 only `Focus` has a rule row in `compose`; every other reason is the same typed error,
/// whatever the entry (so a replay gate sees one deterministic "unsupported", never a panic).
#[test]
fn p2_7_unsupported_reasons_are_a_stable_typed_error() {
    for seed in 0..200 {
        let mut r = Rng::new(seed);
        let e = gen_entry(&mut r);
        let ctx = gen_ctx(&mut r);
        for code in 1..=12u16 {
            let reason = Reason::from_code(code, true, 3);
            let a = compose(&e, key(), reason, ctx).map(|_| ());
            assert_eq!(a, compose(&e, key(), reason, ctx).map(|_| ()));
            assert!(a.is_err(), "reason code {code} has no row yet");
        }
    }
}

/// P2.8 `Opaque` content and unregistered field kinds render as if absent, and never panic.
#[test]
fn p2_8_unknown_content_renders_nothing_and_is_otherwise_inert() {
    let _p = gorca_i18n::scoped(Lang::En);
    for seed in 0..trials() {
        let mut r = Rng::new(seed ^ 0x99);
        let ctx = gen_ctx(&mut r);
        let e = gen_entry(&mut r);
        let base = compose(&e, key(), Reason::Focus, ctx).unwrap();
        let mut noisy = base.clone();
        for i in 0..=noisy.fields.len() {
            if r.chance(60) {
                let junk = Field::new(
                    FieldKind(*r.pick(&[0x0123u16, 0x7FFF, 0x8123])),
                    Content::opaque(u16::try_from(3 + r.below(1000)).unwrap()).unwrap(),
                    Origin::default(),
                );
                noisy.fields.insert(i.min(noisy.fields.len()), junk);
            }
        }
        let o = RenderOpts::default();
        assert_eq!(
            spoken(&render(&base, o)),
            spoken(&render(&noisy, o)),
            "seed {seed}"
        );
    }
}

// ---- P3: shadow diff ----------------------------------------------------------------------------

const KINDS: [SegmentKind; 7] = [
    SegmentKind::Text,
    SegmentKind::Role,
    SegmentKind::State,
    SegmentKind::Value,
    SegmentKind::Description,
    SegmentKind::Said,
    SegmentKind::Capital,
];

/// Words that classifiers look at, plus noise.
fn vocabulary() -> Vec<String> {
    let mut v: Vec<String> = [
        M::RolePanel,
        M::AriaGroup,
        M::StatePressed,
        M::StateReadOnly,
        M::RoleLabel,
        M::RoleStaticText,
        M::StateChecked,
    ]
    .iter()
    .map(|m| gorca_i18n::t(*m).to_owned())
    .collect();
    v.extend(["Save", "\u{2022} Save", "Delete all", "all", "50", "x"].map(str::to_owned));
    v
}

fn legacy(k: SegmentKind, s: &str) -> LegacyToken {
    LegacyToken {
        kind: k,
        text: Arc::from(s),
    }
}

fn newtok(k: SegmentKind, s: &str) -> Token {
    Token {
        kind: k,
        text: TokenText::Shared(Arc::from(s)),
    }
}

/// The unit of comparison, the same reduction `diff` makes.
fn key_l(t: &LegacyToken) -> (u8, String) {
    (gorca_present::diff::kind_code(t.kind), t.text.to_string())
}
fn key_n(t: &Token) -> (u8, String) {
    (
        gorca_present::diff::kind_code(t.kind),
        t.text.as_str().to_owned(),
    )
}

const SURFACES: &[&str] = &["focus", "focus-web", "change", "change-web", "nowhere"];

fn all_rows(surfaces: &[&str]) -> Deviations {
    let mut s = String::new();
    for id in gorca_present::diff::Classifier::ALL {
        for surf in surfaces {
            let _ = write!(
                s,
                "[[deviation]]\nid = \"{}\"\nsurface = \"{surf}\"\nreason = \"test\"\nuntil = \"2099-01-01\"\n",
                id.id()
            );
        }
    }
    Deviations::parse(&s).unwrap()
}

fn gen_tokens(r: &mut Rng, vocab: &[String]) -> (Vec<LegacyToken>, Vec<Token>) {
    let mut l = Vec::new();
    let mut n = Vec::new();
    let common = r.below(4);
    for _ in 0..common {
        let k = *r.pick(&KINDS);
        let s = r.pick(vocab);
        l.push(legacy(k, s));
        n.push(newtok(k, s));
    }
    for _ in 0..r.below(3) {
        l.push(legacy(*r.pick(&KINDS), r.pick(vocab)));
    }
    for _ in 0..r.below(3) {
        n.push(newtok(*r.pick(&KINDS), r.pick(vocab)));
    }
    (l, n)
}

fn gen_diff_ctx<'a>(r: &mut Rng, surface: &'a str) -> DiffCtx<'a> {
    DiffCtx {
        surface,
        role: *r.pick(SemRole::ALL),
        name_derived: r.chance(50),
        untoggled: r.chance(50),
        not_toggle: r.chance(50),
        not_selectable: r.chance(50),
        avail_na: r.chance(50),
        toggle_button: r.chance(50),
        has_value_text: r.chance(50),
        unavailable_item: false,
        canvas_graphic: false,
        alert_dialog: false,
        lang: gorca_i18n::Lang::En,
    }
}

/// Equal/OrderOnly are one class for the purpose of order permutations.
fn class(o: &DiffOutcome) -> DiffOutcome {
    match o {
        DiffOutcome::OrderOnly => DiffOutcome::Equal,
        other => other.clone(),
    }
}

fn shuffle<T>(r: &mut Rng, xs: &mut [T]) {
    for i in (1..xs.len()).rev() {
        xs.swap(i, r.below(i + 1));
    }
}

/// P3.1 the classification is a function of the two token *multisets* and the context: running
/// it twice, or after permuting either side, gives the same class (Equal and OrderOnly are one
/// class: they differ only in the order, which is what permuting changes).
#[test]
fn p3_1_diff_depends_on_multisets_context_and_rows_only() {
    let vocab = vocabulary();
    let rows = all_rows(SURFACES);
    for seed in 0..trials() * 4 {
        let mut r = Rng::new(seed);
        let (l, n) = gen_tokens(&mut r, &vocab);
        let surface = *r.pick(SURFACES);
        let ctx = gen_diff_ctx(&mut r, surface);
        for dev in [&rows, &Deviations::builtin(), &Deviations::empty()] {
            let a = diff(&l, &n, dev, ctx);
            assert_eq!(a, diff(&l, &n, dev, ctx), "seed {seed}: not repeatable");
            let (mut l2, mut n2) = (l.clone(), n.clone());
            shuffle(&mut r, &mut l2);
            shuffle(&mut r, &mut n2);
            let b = diff(&l2, &n2, dev, ctx);
            assert_eq!(
                class(&a),
                class(&b),
                "seed {seed}: order leaks into the class"
            );
        }
    }
}

/// P3.2 oracle for the first two classes, computed independently: identical sequences are
/// `Equal`; the same multiset in another order is `OrderOnly`; anything else is `Allowed` or
/// `Different`.
#[test]
fn p3_2_equal_and_order_only_match_an_independent_oracle() {
    let vocab = vocabulary();
    let rows = all_rows(SURFACES);
    for seed in 0..trials() * 4 {
        let mut r = Rng::new(seed ^ 0x32);
        let (l, n) = gen_tokens(&mut r, &vocab);
        let ctx = gen_diff_ctx(&mut r, "focus");
        let out = diff(&l, &n, &rows, ctx);
        let (kl, kn): (Vec<_>, Vec<_>) =
            (l.iter().map(key_l).collect(), n.iter().map(key_n).collect());
        let (mut sl, mut sn) = (kl.clone(), kn.clone());
        sl.sort();
        sn.sort();
        let want = if kl == kn {
            Some(DiffOutcome::Equal)
        } else if sl == sn {
            Some(DiffOutcome::OrderOnly)
        } else {
            None
        };
        match want {
            Some(w) => assert_eq!(out, w, "seed {seed}"),
            None => assert!(
                matches!(out, DiffOutcome::Allowed(_) | DiffOutcome::Different { .. }),
                "seed {seed}: {out:?}"
            ),
        }
    }
}

/// P3.3 `Allowed(id)` is sound: the row exists for exactly that surface. With no rows, or on a
/// surface that has none, nothing is ever `Allowed`.
#[test]
fn p3_3_allowed_needs_a_row_for_that_id_on_that_surface() {
    let vocab = vocabulary();
    let rows = all_rows(&["focus", "focus-web", "change", "change-web"]);
    let builtin = Deviations::builtin();
    let mut allowed_seen = 0u32;
    for seed in 0..trials() * 6 {
        let mut r = Rng::new(seed ^ 0x33);
        let (l, n) = gen_tokens(&mut r, &vocab);
        let surface = *r.pick(SURFACES);
        let ctx = gen_diff_ctx(&mut r, surface);
        assert!(!matches!(
            diff(&l, &n, &Deviations::empty(), ctx),
            DiffOutcome::Allowed(_)
        ));
        if surface == "nowhere" {
            assert!(!matches!(diff(&l, &n, &rows, ctx), DiffOutcome::Allowed(_)));
        }
        for dev in [&rows, &builtin] {
            if let DiffOutcome::Allowed(id) = diff(&l, &n, dev, ctx) {
                allowed_seen += 1;
                assert!(dev.allows(id, surface), "seed {seed}: {id} on {surface}");
            }
        }
    }
    assert!(
        allowed_seen > 20,
        "the generator reaches the classifiers: {allowed_seen}"
    );
}

/// One positive case per classifier: tokens, context, and the surface its row lives on.
struct Case {
    id: &'static str,
    surface: &'static str,
    role: SemRole,
    name_derived: bool,
    untoggled: bool,
    legacy: Vec<(SegmentKind, String)>,
    new: Vec<(SegmentKind, String)>,
}

fn w(m: M) -> String {
    gorca_i18n::t(m).to_owned()
}

fn cases() -> Vec<Case> {
    use SegmentKind::{Role, State, Text, Value};
    let base = |id, surface, role| Case {
        id,
        surface,
        role,
        name_derived: false,
        untoggled: false,
        legacy: vec![],
        new: vec![],
    };
    vec![
        Case {
            legacy: vec![(Text, "Name".into()), (Value, "typed text".into())],
            new: vec![(Text, "Name".into())],
            ..base("field-contents-not-fused", "focus", SemRole::TextBox)
        },
        Case {
            name_derived: true,
            legacy: vec![(Role, "list item".into())],
            new: vec![(Text, "Inbox".into()), (Role, "list item".into())],
            ..base("content-derived-name", "focus", SemRole::ListItem)
        },
        Case {
            untoggled: true,
            legacy: vec![(Text, "Tab".into()), (State, w(M::StatePressed))],
            new: vec![(Text, "Tab".into())],
            ..base("pressed-without-toggle", "focus", SemRole::Tab)
        },
        Case {
            legacy: vec![(Text, "Links".into())],
            new: vec![(Text, "Links".into()), (Role, "navigation".into())],
            ..base("role-from-model", "focus", SemRole::Navigation)
        },
        Case {
            legacy: vec![(Text, "\u{2022} Save".into())],
            new: vec![(Text, "Save".into())],
            ..base("marker-stripped-name", "focus", SemRole::ListItem)
        },
        Case {
            legacy: vec![(Role, w(M::RoleLabel))],
            new: vec![(Role, w(M::RoleStaticText))],
            ..base("static-text-word", "focus-web", SemRole::StaticText)
        },
        Case {
            legacy: vec![(Role, w(M::RolePanel))],
            new: vec![(Role, w(M::AriaGroup))],
            ..base("grouping-word", "focus-web", SemRole::Group)
        },
        Case {
            legacy: vec![(Text, "Log".into()), (Role, "log".into())],
            new: vec![(Text, "Log".into())],
            ..base("role-not-in-table", "focus-web", SemRole::Log)
        },
        Case {
            legacy: vec![(Text, "Volume".into())],
            new: vec![(Text, "Volume".into()), (Value, "50 percent".into())],
            ..base("value-from-attrs", "focus-web", SemRole::Slider)
        },
        Case {
            legacy: vec![(Text, "Search".into())],
            new: vec![(Text, "Search".into()), (State, w(M::StateReadOnly))],
            ..base("read-only-from-model", "focus", SemRole::SearchBox)
        },
    ]
}

fn run_case(c: &Case, dev: &Deviations, mutate: impl FnOnce(&mut Case)) -> DiffOutcome {
    let mut c = Case {
        id: c.id,
        surface: c.surface,
        role: c.role,
        name_derived: c.name_derived,
        untoggled: c.untoggled,
        legacy: c.legacy.clone(),
        new: c.new.clone(),
    };
    mutate(&mut c);
    let l: Vec<_> = c.legacy.iter().map(|(k, s)| legacy(*k, s)).collect();
    let n: Vec<_> = c.new.iter().map(|(k, s)| newtok(*k, s)).collect();
    diff(
        &l,
        &n,
        dev,
        DiffCtx {
            surface: c.surface,
            role: c.role,
            name_derived: c.name_derived,
            untoggled: c.untoggled,
            not_toggle: false,
            not_selectable: false,
            avail_na: false,
            toggle_button: false,
            has_value_text: false,
            unavailable_item: false,
            canvas_graphic: false,
            alert_dialog: false,
            lang: gorca_i18n::Lang::En,
        },
    )
}

fn unclassified(o: &DiffOutcome) -> bool {
    matches!(o, DiffOutcome::Different { .. })
}

/// P3.4 every classifier fires on its own case and only there. The positive cases are
/// `Allowed(id)` with the builtin rows (each case lives on a surface the builtin list names) or
/// with a permissive list; the near misses below must come out `Different`.
#[test]
fn p3_4_each_classifier_fires_on_its_positive_case() {
    let permissive = all_rows(&["focus", "focus-web", "change", "change-web"]);
    for c in cases() {
        assert_eq!(
            run_case(&c, &permissive, |_| {}),
            DiffOutcome::Allowed(c.id),
            "positive case of {}",
            c.id
        );
    }
}

/// P3.5 near misses: for each case, change exactly one thing its narrow predicate names and the
/// outcome must be UNCLASSIFIED (`Different`). The mutations are the gates written in
/// `diff.rs::explain`: the surface, the role, the context flags, the multiplicity of the
/// tokens, the token kind and the exact word.
#[test]
fn p3_5_a_near_miss_of_every_predicate_is_unclassified() {
    use SegmentKind::{Role, Said, State, Text, Value};
    let permissive = all_rows(&["focus", "focus-web", "change", "change-web"]);
    let mut checked = 0u32;
    for c in cases() {
        // Wrong surface: no row for it.
        let m = run_case(&c, &permissive, |c| c.surface = "nowhere");
        assert!(unclassified(&m), "{}: surface: {m:?}", c.id);
        // The builtin list keeps each row on its own surface: the other of focus/focus-web has
        // no row for ids that exist on one only, so the diff stays Different there.
        // Wrong role, for the classifiers whose predicate names roles. `content-derived-name`
        // is gated by the `name_derived` provenance flag and `marker-stripped-name` by nothing
        // (known gap G-P3-a), so neither has a role gate to break.
        if !matches!(c.id, "content-derived-name" | "marker-stripped-name") {
            let m = run_case(&c, &permissive, |c| c.role = SemRole::Button);
            assert!(unclassified(&m), "{}: role: {m:?}", c.id);
        }
        // An extra token no classifier can explain, on either side.
        for kind in [Said, SegmentKind::Capital] {
            let m = run_case(&c, &permissive, |c| c.legacy.push((kind, "zzz".into())));
            assert!(unclassified(&m), "{}: extra legacy {kind:?}: {m:?}", c.id);
            let m = run_case(&c, &permissive, |c| c.new.push((kind, "zzz".into())));
            assert!(unclassified(&m), "{}: extra new {kind:?}: {m:?}", c.id);
        }
        // Context flag the gate reads.
        if c.name_derived {
            let m = run_case(&c, &permissive, |c| c.name_derived = false);
            assert!(unclassified(&m), "{}: name_derived: {m:?}", c.id);
        }
        if c.untoggled {
            let m = run_case(&c, &permissive, |c| c.untoggled = false);
            assert!(unclassified(&m), "{}: untoggled: {m:?}", c.id);
        }
        checked += 1;
    }
    // Multiplicity and kind gates, per classifier.
    let by = |id: &str| cases().into_iter().find(|c| c.id == id).unwrap();

    let c = by("field-contents-not-fused");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .legacy
            .push((Value, "again".into())))),
        "two value tokens"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .new
            .push((Value, "now".into())))),
        "the model has a value"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.role = SemRole::SpinButton)),
        "spin button"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.legacy[1].0 = State)),
        "not a value token"
    );

    let c = by("content-derived-name");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .new
            .push((Text, "Extra".into())))),
        "two new names"
    );
    // (A legacy name that differs from the derived one is the second, intended branch.)
    assert!(
        unclassified(&run_case(&c, &permissive, |c| {
            c.legacy.push((Text, "Old".into()));
            c.legacy.push((Text, "Older".into()));
        })),
        "two legacy names against one derived name"
    );

    let c = by("pressed-without-toggle");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.legacy[1].1 = w(M::StateChecked))),
        "another state word"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.role = SemRole::Switch)),
        "not a tab"
    );

    let c = by("role-from-model");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .legacy
            .push((Role, "region".into())))),
        "legacy had a role word"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .new
            .push((Role, "region".into())))),
        "two role words"
    );

    let c = by("marker-stripped-name");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.legacy[0].1 = "Save \u{2022}".into())),
        "marker after the name"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.new[0].1 = "Saved".into())),
        "not a suffix"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.legacy[0].0 = Role)),
        "not text"
    );

    let c = by("static-text-word");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.new[0].1 = "caption".into())),
        "another word"
    );

    let c = by("grouping-word");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.legacy[0].1 = "frame".into())),
        "legacy word not panel"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.new[0].1 = "box".into())),
        "new word not grouping"
    );

    let c = by("role-not-in-table");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .new
            .push((Role, "journal".into())))),
        "model has a role word"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .legacy
            .push((Role, "x".into())))),
        "two legacy role words"
    );

    let c = by("value-from-attrs");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .legacy
            .push((Value, "49".into())))),
        "legacy spoke a value"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c
            .new
            .push((Value, "51".into())))),
        "two new values"
    );

    let c = by("read-only-from-model");
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.new[1].1 = "locked".into())),
        "another word"
    );
    assert!(
        unclassified(&run_case(&c, &permissive, |c| c.role = SemRole::Slider)),
        "slider is not a text role"
    );
    assert_eq!(checked, 10);
}

/// P3.6 surfaces: the builtin list names each id on specific surfaces; on any other surface the
/// same positive case is `Different`.
#[test]
fn p3_6_builtin_rows_are_scoped_to_their_surfaces() {
    let builtin = Deviations::builtin();
    let mut fired = 0u32;
    for c in cases() {
        for surf in ["focus", "focus-web", "change", "change-web"] {
            let out = run_case(&c, &builtin, |x| x.surface = surf);
            if builtin.allows(c.id, surf) {
                if surf == c.surface {
                    assert_eq!(out, DiffOutcome::Allowed(c.id), "{} on {surf}", c.id);
                    fired += 1;
                }
            } else {
                assert!(unclassified(&out), "{} on {surf} has no row: {out:?}", c.id);
            }
        }
    }
    assert!(fired >= 8, "{fired}");
}

/// P3.7 near-miss fuzz: junk tokens of kinds no classifier can consume (`Said`, `Capital`), added
/// to a positive case, always leave a residue: the outcome is `Different` and the counts are the
/// junk counts plus whatever the positive case already left (zero).
#[test]
fn p3_7_unexplainable_residue_is_always_reported() {
    let permissive = all_rows(&["focus", "focus-web", "change", "change-web"]);
    for seed in 0..trials() {
        let mut r = Rng::new(seed ^ 0x37);
        let all = cases();
        let c = r.pick(&all);
        let (jl, jn) = (r.below(3), r.below(3));
        if jl + jn == 0 {
            continue;
        }
        let out = run_case(c, &permissive, |c| {
            for i in 0..jl {
                c.legacy.push((SegmentKind::Said, format!("junk-l{i}")));
            }
            for i in 0..jn {
                c.new.push((SegmentKind::Capital, format!("junk-n{i}")));
            }
        });
        assert_eq!(
            out,
            DiffOutcome::Different {
                legacy_only: u32::try_from(jl).unwrap(),
                new_only: u32::try_from(jn).unwrap()
            },
            "seed {seed} {}",
            c.id
        );
    }
}

/// G-P3-a (narrowness, fixed): `marker-stripped-name` accepts only a removed prefix made of
/// list-marker characters and whitespace. "Delete all" -> "all" is a dropped word: `Different`.
#[test]
fn marker_stripped_name_rejects_a_dropped_word() {
    let permissive = all_rows(&["focus", "focus-web"]);
    let mut c = cases()
        .into_iter()
        .find(|c| c.id == "marker-stripped-name")
        .unwrap();
    c.legacy = vec![(SegmentKind::Text, "Delete all".into())];
    c.new = vec![(SegmentKind::Text, "all".into())];
    c.role = SemRole::Button;
    assert!(
        matches!(
            run_case(&c, &permissive, |_| {}),
            DiffOutcome::Different { .. }
        ),
        "a dropped word must not be classified as a stripped marker"
    );
    for (old, new) in [("\u{2022} Alpha", "Alpha"), ("- x", "x")] {
        c.legacy = vec![(SegmentKind::Text, old.into())];
        c.new = vec![(SegmentKind::Text, new.into())];
        assert_eq!(
            run_case(&c, &permissive, |_| {}),
            DiffOutcome::Allowed("marker-stripped-name"),
            "{old:?} -> {new:?}"
        );
    }
    for (old, new) in [
        ("Alpha", "Alpha"),
        ("2 apples", "apples"),
        // An ordinal is information the name keeps.
        ("3. Beta", "Beta"),
        ("(3) Beta", "Beta"),
    ] {
        c.legacy = vec![(SegmentKind::Text, old.into())];
        c.new = vec![(SegmentKind::Text, new.into())];
        assert!(
            !matches!(run_case(&c, &permissive, |_| {}), DiffOutcome::Allowed(_)),
            "{old:?} -> {new:?}"
        );
    }
}

/// G-P3-b (attribution, fixed): `diff_report` lists every classifier that fired, in `ALL` order;
/// the `Allowed` id is the first of them.
#[test]
fn every_used_classifier_is_reported() {
    let permissive = all_rows(&["focus"]);
    // A text box: the contents are not fused AND the model says read only; both rows apply.
    let l = [
        legacy(SegmentKind::Text, "Search"),
        legacy(SegmentKind::Value, "typed"),
    ];
    let n = [
        newtok(SegmentKind::Text, "Search"),
        newtok(SegmentKind::State, &w(M::StateReadOnly)),
    ];
    let ctx = DiffCtx {
        surface: "focus",
        role: SemRole::SearchBox,
        name_derived: false,
        untoggled: false,
        not_toggle: false,
        not_selectable: false,
        avail_na: false,
        toggle_button: false,
        has_value_text: false,
        unavailable_item: false,
        canvas_graphic: false,
        alert_dialog: false,
        lang: gorca_i18n::Lang::En,
    };
    let (outcome, used) = gorca_present::diff::diff_report(&l, &n, &permissive, ctx);
    assert_eq!(outcome, DiffOutcome::Allowed("field-contents-not-fused"));
    assert_eq!(used, ["field-contents-not-fused", "read-only-from-model"]);
    assert_eq!(diff(&l, &n, &permissive, ctx), outcome);
}

/// P3.9 the allow-list is monotone: enabling more deviation rows never turns an `Allowed`
/// outcome into `Different`. The classifiers share one mutable residue and run in a fixed order,
/// so monotonicity is not obvious (an earlier classifier could consume a token a later one
/// needs); the search over merged positive cases (two classifiers competing for one residue) found
/// no counterexample in 200 000 trials (`GORCA_PROP_TRIALS=5000`), and the pairwise gates in
/// `diff.rs` are mutually exclusive on tokens by inspection (see the report).
#[test]
fn p3_9_adding_a_row_never_turns_allowed_into_different() {
    let vocab = vocabulary();
    let full = all_rows(&["focus", "focus-web"]);
    let ids: Vec<_> = gorca_present::diff::Classifier::ALL
        .iter()
        .map(|c| c.id())
        .collect();
    for seed in 0..trials() * 8 {
        let mut r = Rng::new(seed ^ 0x6C);
        // Positive cases of one or two classifiers merged, plus noise tokens: the shape in which
        // two classifiers compete for the same residue.
        let (mut l, mut n) = gen_tokens(&mut r, &vocab);
        let all = cases();
        let (c1, c2) = (r.pick(&all), r.pick(&all));
        for c in [c1, c2] {
            l.extend(c.legacy.iter().map(|(k, s)| legacy(*k, s)));
            n.extend(c.new.iter().map(|(k, s)| newtok(*k, s)));
        }
        let ctx = DiffCtx {
            surface: "focus",
            role: if r.chance(50) { c1.role } else { c2.role },
            name_derived: c1.name_derived || c2.name_derived,
            not_toggle: false,
            not_selectable: false,
            avail_na: false,
            toggle_button: false,
            has_value_text: false,
            unavailable_item: false,
            canvas_graphic: false,
            alert_dialog: false,
            untoggled: c1.untoggled || c2.untoggled,
            lang: gorca_i18n::Lang::En,
        };
        // A subset of rows.
        let mut s = String::new();
        for id in &ids {
            if r.chance(50) {
                let _ = write!(
                    s,
                    "[[deviation]]\nid = \"{id}\"\nsurface = \"focus\"\nreason = \"t\"\nuntil = \"2099-01-01\"\n"
                );
            }
        }
        let subset = Deviations::parse(&s).unwrap();
        if let DiffOutcome::Allowed(_) = diff(&l, &n, &subset, ctx) {
            let big = diff(&l, &n, &full, ctx);
            assert!(
                matches!(big, DiffOutcome::Allowed(_)),
                "seed {seed}: subset {s:?} allows, all rows give {big:?}; legacy {:?} new {:?} ctx {ctx:?}",
                l.iter().map(key_l).collect::<Vec<_>>(),
                n.iter().map(key_n).collect::<Vec<_>>()
            );
        }
    }
}
