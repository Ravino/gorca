// M7a review probe: appended as `mod probe_m7a_identity` inside identity.rs `mod tests`.
use super::*;

fn chain_forest(root_path: u64, n: u64, cycle: bool) -> FetchedDoc {
    let nodes: Vec<SrcNode> = (0..n).map(|i| node(root_path + i, "x", "div")).collect();
    let mut children: Vec<(SrcNodeId, SmallVec<[SrcNodeId; 8]>)> = (0..n - 1)
        .map(|i| {
            (
                SrcNodeId(root_path + i),
                [SrcNodeId(root_path + i + 1)].into_iter().collect(),
            )
        })
        .collect();
    if cycle {
        // The last node lists the first one again (a duplicate path from a broken server).
        children.push((
            SrcNodeId(root_path + n - 1),
            [SrcNodeId(root_path)].into_iter().collect(),
        ));
    }
    FetchedDoc {
        block: TreeBlock {
            root: SrcNodeId(root_path),
            completeness: Completeness::States,
            nodes,
            children,
        },
        keys: (0..n).map(|i| PathKey(root_path + i)).collect(),
        raw_roles: vec![32; n as usize],
        child_counts: vec![1; n as usize],
        roots: vec![0],
        timings: FetchTimings::default(),
        incomplete: false,
    }
}

#[test]
#[ignore = "probe: overflows the stack (aborts the test binary)"]
fn probe_deep_fresh_subtree_recurses() {
    let mut m = IdentityMap::default();
    m.load(&base());
    let added = chain_forest(1_000, 100_000, false);
    let _ = m.apply_children(
        SrcNodeId(2),
        &[PathKey(3), PathKey(4), PathKey(1_000)],
        &added,
        &HashSet::new(),
    );
}

#[test]
#[ignore = "probe: infinite recursion (aborts the test binary)"]
fn probe_cyclic_fresh_subtree_recurses_forever() {
    let mut m = IdentityMap::default();
    m.load(&base());
    let added = chain_forest(1_000, 3, true);
    let _ = m.apply_children(
        SrcNodeId(2),
        &[PathKey(3), PathKey(4), PathKey(1_000)],
        &added,
        &HashSet::new(),
    );
}

#[test]
fn probe_depth_600_fresh_subtree_is_kept_whole() {
    // No depth cap in the source: 600 levels pass through to the translator.
    let mut m = IdentityMap::default();
    m.load(&base());
    let added = chain_forest(1_000, 600, false);
    let o = m.apply_children(
        SrcNodeId(2),
        &[PathKey(3), PathKey(4), PathKey(1_000)],
        &added,
        &HashSet::new(),
    );
    assert_eq!(o.stats.new_ids, 600);
}
