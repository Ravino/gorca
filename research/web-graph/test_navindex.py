"""Randomised differential test: OMIndex and TreapIndex vs BruteIndex.

Run: python test_navindex.py
"""
import random

from gen import page, random_subtree
from navindex import NC
from navindex import BruteIndex, Mirror, OMIndex, TreapIndex


def check(m: Mirror, ixs, rng, n_queries=60):
    brute, others = ixs[0], ixs[1:]
    order = m.preorder()
    for _ in range(n_queries):
        c = rng.randrange(NC)
        v = rng.choice(order)
        want_n, want_p = brute.next(c, v), brute.prev(c, v)
        for ix in others:
            assert ix.next(c, v) == want_n, (type(ix).__name__, "next", c, v)
            assert ix.prev(c, v) == want_p, (type(ix).__name__, "prev", c, v)
        cnt = brute.count(c)
        for ix in others:
            assert ix.count(c) == cnt, (type(ix).__name__, "count", c)
        if cnt:
            k = rng.randrange(cnt)
            want = brute.kth(c, k)
            for ix in others:
                assert ix.kth(c, k) == want, (type(ix).__name__, "kth", c, k)
    # order consistency for OM labels
    om = [ix for ix in others if isinstance(ix, OMIndex)][0]
    labs = [om.label[v] for v in order]
    assert labs == sorted(labs) and len(set(labs)) == len(labs)
    assert om.order == labs


def random_mutation(m: Mirror, rng, step):
    order = m.preorder()
    r = rng.random()
    if r < 0.35:     # insert a component somewhere
        p = rng.choice(order)
        idx = rng.randint(0, len(m.children[p]))
        m.insert_subtree(p, idx, random_subtree(step, rng.randint(1, 60)))
    elif r < 0.6 and len(order) > 50:   # delete a subtree
        v = rng.choice(order[1:])
        if len(m.subtree_preorder(v)) < len(order) // 3:
            m.delete_subtree(v)
    elif r < 0.75:   # replace (SPA re-render)
        v = rng.choice(order[1:])
        p = m.parent[v]
        if len(m.subtree_preorder(v)) < len(order) // 3:
            idx = m.children[p].index(v)
            m.delete_subtree(v)
            m.insert_subtree(p, idx, random_subtree(step + 7, rng.randint(1, 80)))
    else:            # category change (visited, role change)
        v = rng.choice(order)
        m.set_mask(v, m.mask[v] ^ (1 << rng.randrange(NC)))


def test_random(seed):
    rng = random.Random(seed)
    t = page(2000, seed=seed)
    ixs = [BruteIndex(), OMIndex(), TreapIndex()]
    m = Mirror(t, ixs)
    check(m, ixs, rng)
    for step in range(300):
        random_mutation(m, rng, step)
        if step % 10 == 0:
            check(m, ixs, rng, 20)
    check(m, ixs, rng)


def test_adversarial_om():
    """Repeated insertion at the same point forces relabels; check bound."""
    rng = random.Random(9)
    t = page(2000, seed=3)
    ixs = [BruteIndex(), OMIndex(), TreapIndex()]
    m = Mirror(t, ixs)
    p = 5
    n_ins = 3000
    for i in range(n_ins):
        m.insert_subtree(p, 0, random_subtree(i, 1))
    check(m, ixs, rng, 100)
    om = ixs[1]
    print(f"adversarial same-point inserts: {n_ins}, total relabelled={om.relabels}, "
          f"per insert={om.relabels / n_ins:.1f}")


if __name__ == "__main__":
    for s in range(6):
        test_random(s)
        print("random seed", s, "ok")
    test_adversarial_om()
    print("all ok")
