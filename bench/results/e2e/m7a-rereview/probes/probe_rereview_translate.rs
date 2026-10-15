// M7a re-review probe: appended as `mod probe_m7a_rereview_translate` inside translate.rs `mod tests`.
use super::*;

/// MAX_DOC_NODES is checked against `by_src.len()` before a SyncChildren, and `build_spec`
/// counts only its own subtree: one child list of k new subtrees on an EXISTING parent
/// (a progressive build's later block, a children-changed refetch) is not bounded by the cap.
#[test]
fn probe_one_list_of_new_subtrees_passes_the_node_cap() {
    let (mut w, r) = channel(1);
    let mut t = Translator::new();
    t.apply(&mut w, chain(2)).unwrap();
    w.publish();
    let kids_n = 60_000u64; // under MAX_CHILDREN
    let per = 9u64; // grandchildren per child
    let mut nodes = Vec::new();
    let mut b = Vec::new();
    let mut kids = Vec::new();
    for i in 0..kids_n {
        let c = 10_000_000 + i * 100;
        kids.push(c);
        nodes.push(n(c, Role::Panel));
        let g: Vec<u64> = (1..=per).map(|j| c + j).collect();
        for &x in &g {
            nodes.push(n(x, Role::Panel));
        }
        b.push(list(c, &g));
    }
    let mut batch = vec![DocDelta::Upsert(nodes), list(2, &kids)];
    batch.extend(b);
    batch.push(DocDelta::Settled { batch: 2 });
    t.apply(&mut w, batch).unwrap();
    w.publish();
    let len = r.with(WebDoc::len);
    eprintln!("doc nodes {len}, MAX_DOC_NODES {MAX_DOC_NODES}, truncated {}", t.stats().truncated);
    assert!(len > MAX_DOC_NODES, "cap held: {len}");
}

/// Deferred lists are capped against `deferred + orphans` at step 2, but orphans are drained
/// at the start of the batch and re-parked at step 4 against their own entry count: the
/// two pools together hold up to 2 x MAX_PENDING_IDS.
#[test]
fn probe_deferred_plus_orphans_exceed_the_pending_cap() {
    let (mut w, _r) = channel(1);
    let mut t = Translator::new();
    t.apply(&mut w, chain(2)).unwrap();
    let orphans: Vec<SrcNode> = (0..MAX_PENDING as u64).map(|i| n(50_000_000 + i, Role::Panel)).collect();
    let l1: Vec<u64> = (0..MAX_CHILDREN as u64).map(|i| 1_000_000 + i).collect();
    let l2: Vec<u64> = (0..MAX_CHILDREN as u64).map(|i| 2_000_000 + i).collect();
    for b in 0..3u64 {
        let mut batch = vec![list(9_000_000, &l1), list(9_000_001, &l2)];
        if b == 0 {
            batch.insert(0, DocDelta::Upsert(orphans.clone()));
        }
        batch.push(DocDelta::Settled { batch: 2 + b });
        t.apply(&mut w, batch).unwrap();
    }
    let ids: usize = t.deferred.values().map(Vec::len).sum();
    let total = ids + t.orphans.len();
    eprintln!("deferred ids {ids} + orphans {} = {total}, cap {MAX_PENDING}", t.orphans.len());
    assert!(total > MAX_PENDING, "cap held: {total}");
}
