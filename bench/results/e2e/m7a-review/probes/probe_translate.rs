// M7a review probe: appended as `mod probe_m7a_translate` inside translate.rs `mod tests`.
use super::*;

fn depth(d: &WebDoc) -> usize {
    d.preorder()
        .map(|id| {
            let mut k = 0;
            let mut c = d.parent(id);
            while let Some(p) = c {
                k += 1;
                c = d.parent(p);
            }
            k
        })
        .max()
        .unwrap_or(0)
}

/// Two chains under the root, then the head of the second is moved under the tail of the
/// first (an existing node in a new list): the depth cap is only checked for new nodes.
#[test]
fn probe_moving_existing_subtrees_bypasses_the_depth_cap() {
    let (mut w, r) = channel(1);
    let mut t = Translator::new();
    let len = u64::from(MAX_DEPTH) - 1;
    let mut nodes = vec![n(1, Role::DocumentWeb)];
    let mut lists = Vec::new();
    // chain A: 10..10+len, chain B: 100_000..100_000+len
    for base in [10u64, 100_000] {
        for i in 0..len {
            nodes.push(n(base + i, Role::Panel));
            if i > 0 {
                lists.push(list(base + i - 1, &[base + i]));
            }
        }
    }
    let mut b = vec![
        DocDelta::Reset { root: SrcNodeId(1), nav_id: 1 },
        DocDelta::Upsert(nodes),
        list(1, &[10, 100_000]),
    ];
    b.extend(lists);
    b.push(DocDelta::Settled { batch: 1 });
    t.apply(&mut w, b).unwrap();
    w.publish();
    let before = r.with(depth);
    // Move B under the tail of A.
    t.apply(
        &mut w,
        vec![
            list(1, &[10]),
            list(10 + len - 1, &[100_000]),
            DocDelta::Settled { batch: 2 },
        ],
    )
    .unwrap();
    w.publish();
    let after = r.with(depth);
    eprintln!("depth before {before}, after move {after}, MAX_DEPTH {MAX_DEPTH}");
    assert!(after > MAX_DEPTH as usize, "cap held: {after}");
}

/// Wide fan-out: one list of 300k children is accepted whole (no width or node cap).
#[test]
fn probe_wide_fan_out_has_no_cap() {
    let (mut w, r) = channel(1);
    let mut t = Translator::new();
    let k = 300_000u64;
    let mut nodes = vec![n(1, Role::DocumentWeb)];
    nodes.extend((2..2 + k).map(|i| n(i, Role::Panel)));
    let kids: Vec<u64> = (2..2 + k).collect();
    t.apply(
        &mut w,
        vec![
            DocDelta::Reset { root: SrcNodeId(1), nav_id: 1 },
            DocDelta::Upsert(nodes),
            list(1, &kids),
            DocDelta::Settled { batch: 1 },
        ],
    )
    .unwrap();
    w.publish();
    assert_eq!(r.with(|d| d.len()), k as usize + 1);
    assert_eq!(t.known(), k as usize + 1);
}

/// Deferred lists: the cap counts entries, not ids; one list for an unknown parent can be huge
/// and survives across batches.
#[test]
fn probe_deferred_list_size_is_not_capped() {
    let (mut w, _r) = channel(1);
    let mut t = Translator::new();
    t.apply(&mut w, chain(2)).unwrap();
    let huge: Vec<u64> = (1_000_000..1_000_000 + 1_000_000).collect();
    for b in 0..5 {
        t.apply(&mut w, vec![list(9_000_000 + b, &huge), DocDelta::Settled { batch: 2 + b }])
            .unwrap();
    }
    assert_eq!(t.deferred.len(), 5);
    let ids: usize = t.deferred.values().map(Vec::len).sum();
    eprintln!("deferred ids held: {ids}");
    assert_eq!(ids, 5_000_000);
}

/// Repeated moves grow the mirror without bound; time a per-key walk at the bottom.
#[test]
fn probe_repeated_moves_grow_depth_without_bound() {
    let (mut w, r) = channel(1);
    let mut t = Translator::new();
    t.apply(&mut w, chain(2)).unwrap();
    let len = u64::from(MAX_DEPTH) - 2;
    let mut tail = 2u64;
    let rounds = 100u64;
    for k in 0..rounds {
        let base = 1_000 + k * 1_000;
        let mut nodes = Vec::new();
        let mut b = Vec::new();
        for i in 0..len {
            let mut x = n(base + i, Role::Panel);
            if i + 1 == len {
                x = n(base + i, Role::StaticText);
                x.text = Some(Arc::from("deep"));
            }
            nodes.push(x);
            if i > 0 {
                b.push(list(base + i - 1, &[base + i]));
            }
        }
        // New chain under the root (depth ok), then moved under the old tail.
        let mut first = vec![DocDelta::Upsert(nodes), list(1, &[2, base])];
        first.extend(b);
        first.push(DocDelta::Settled { batch: 10 + 2 * k });
        t.apply(&mut w, first).unwrap();
        t.apply(
            &mut w,
            vec![list(1, &[2]), list(tail, &[base]), DocDelta::Settled { batch: 11 + 2 * k }],
        )
        .unwrap();
        tail = base + len - 1;
    }
    w.publish();
    let d = r.with(depth);
    let t0 = std::time::Instant::now();
    let items = r.with(|doc| {
        let end = doc.text_len();
        crate::browse::present::describe(doc, end.saturating_sub(4), end)
    });
    eprintln!("depth {d}; describe at the bottom took {:?}; items {}", t0.elapsed(), items.len());
    assert!(d > 50_000);
}
