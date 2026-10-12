"""Mirror reconciliation: keyed forest diff, unkeyed subtree matching, cursor
restoration, and the optimal re-fetch cover of a dirty set.

1. keyed_child_diff(old, new)        O(m log m)  minimal moves via LIS (proof in report)
2. keyed_forest_diff(...)            recursive, only into dirty surviving children
3. match_trees(old, new)             identity recovery when keys are lost
                                     (Chromium tree rebuild, SPA re-render that
                                     recreates accessibles). GumTree-style:
                                     top-down exact hash, bottom-up dice, then
                                     per-parent LCS on (role, name) for leftovers.
                                     O(n log n + n*d) for depth d.
4. restore_cursor(...)               map (node, offset) across a re-render.
5. refetch_cover(...)                exact min-cost cover of dirty nodes by
                                     subtree re-fetches (tree DP, linear in the
                                     size of the virtual tree of dirty nodes).
"""
from __future__ import annotations

import bisect
import hashlib
from collections import defaultdict

from gen import Tree


# ---------------------------------------------------------------------------
# 1. Keyed child-list diff
def lis_indices(seq: list) -> list:
    """Indices of one longest strictly increasing subsequence. O(m log m)."""
    tails, tails_idx, prev = [], [], [-1] * len(seq)
    for i, x in enumerate(seq):
        j = bisect.bisect_left(tails, x)
        if j == len(tails):
            tails.append(x)
            tails_idx.append(i)
        else:
            tails[j] = x
            tails_idx[j] = i
        prev[i] = tails_idx[j - 1] if j > 0 else -1
    out, i = [], tails_idx[-1] if tails_idx else -1
    while i >= 0:
        out.append(i)
        i = prev[i]
    return out[::-1]


def keyed_child_diff(old: list, new: list) -> dict:
    """Edit script turning child-key list `old` into `new`.

    Returns {'delete': keys, 'insert': [(key, new_index)], 'move': [(key, new_index)],
             'keep': keys}. |move| = |common| - LIS(common positions) is minimal
    among scripts that only delete, insert and move single elements.
    """
    new_pos = {k: i for i, k in enumerate(new)}
    old_set = set(old)
    common_old_order = [k for k in old if k in new_pos]
    seq = [new_pos[k] for k in common_old_order]
    stay = {common_old_order[i] for i in lis_indices(seq)}
    return {
        "delete": [k for k in old if k not in new_pos],
        "insert": [(k, i) for i, k in enumerate(new) if k not in old_set],
        "move": [(k, new_pos[k]) for k in common_old_order if k not in stay],
        "keep": sorted(stay, key=new_pos.get),
    }


def apply_child_diff(old: list, d: dict) -> list:
    """Reference applier (for tests): deletes, then moves/inserts by target index."""
    gone = set(d["delete"]) | {k for k, _ in d["move"]}
    cur = [k for k in old if k not in gone]
    for k, i in sorted(d["insert"] + d["move"], key=lambda x: x[1]):
        cur.insert(i, k)
    return cur


def keyed_forest_diff(old_children, new_children, root, dirty) -> dict:
    """Recursive keyed diff, descending only into surviving children in `dirty`.

    old_children/new_children: callables key -> list of child keys.
    Returns per-parent child diffs. Cost: sum over visited parents of m log m.
    """
    out, stack = {}, [root]
    while stack:
        v = stack.pop()
        d = keyed_child_diff(old_children(v), new_children(v))
        if d["delete"] or d["insert"] or d["move"]:
            out[v] = d
        for k in d["keep"] + [k for k, _ in d["move"]]:
            if k in dirty:
                stack.append(k)
    return out


# ---------------------------------------------------------------------------
# 3. Unkeyed matching
def _h(*parts) -> int:
    return int.from_bytes(hashlib.blake2b(repr(parts).encode(), digest_size=8).digest(), "little")


def subtree_hashes(t: Tree):
    """Bottom-up: skeleton hash (roles only), full hash (roles + names), size, height."""
    n = len(t)
    order = t.preorder()
    skel, full, size, height = [0] * n, [0] * n, [1] * n, [0] * n
    for v in reversed(order):
        ch = t.children[v]
        skel[v] = _h(t.role[v], tuple(skel[c] for c in ch))
        full[v] = _h(t.role[v], t.name[v], t.level[v], tuple(full[c] for c in ch))
        for c in ch:
            size[v] += size[c]
            height[v] = max(height[v], height[c] + 1)
    return skel, full, size, height


def match_trees(a: Tree, b: Tree, min_dice: float = 0.5) -> dict:
    """Return a partial bijection old-node -> new-node.

    Phase 1 (top-down, GumTree): pair subtrees whose full hash is unique on both
             sides, highest first (ambiguous ones are left to phase 3).
    Phase 2 (bottom-up): an unmatched old internal node is paired with the new
             node of the same role that shares the most matched descendants
             (dice >= min_dice). Candidates come from ancestors of matched
             descendants, so cost is O(n * depth).
    Phase 3 (recovery): inside each matched parent pair, align still-unmatched
             children by LCS on (role, name), then by role alone.
    """
    _, fa, sa, ha = subtree_hashes(a)
    _, fb, sb, hb = subtree_hashes(b)
    oa, ob = a.preorder(), b.preorder()
    ra = {v: i for i, v in enumerate(oa)}
    M: dict[int, int] = {}
    Mi: dict[int, int] = {}

    def add_subtree(x, y):
        st = [(x, y)]
        while st:
            u, w = st.pop()
            if u in M or w in Mi:
                continue
            M[u] = w
            Mi[w] = u
            st.extend(zip(a.children[u], b.children[w]))

    # Phase 1: unique-unique exact subtree matches, highest first
    cnt_a, cnt_b = defaultdict(int), defaultdict(int)
    by_hash_b = {}
    for u in oa:
        cnt_a[fa[u]] += 1
    for w in ob:
        cnt_b[fb[w]] += 1
        by_hash_b[fb[w]] = w
    for u in sorted(oa, key=lambda u: (-ha[u], ra[u])):
        if u in M or (ha[u] == 0 and not a.name[u]):
            continue
        h = fa[u]
        if cnt_a[h] == 1 and cnt_b.get(h) == 1:
            w = by_hash_b[h]
            if w not in Mi:
                add_subtree(u, w)

    # Phase 2
    for u in reversed(oa):           # bottom-up (children before parents)
        if u in M or not a.children[u]:
            continue
        votes = defaultdict(int)
        st = list(a.children[u])
        desc = 0
        while st:
            x = st.pop()
            desc += 1
            if x in M:
                y = b.parent[M[x]]
                while y >= 0:
                    votes[y] += 1
                    y = b.parent[y]
            st.extend(a.children[x])
        best, best_d = None, 0.0
        for w, cnt in votes.items():
            if w in Mi or b.role[w] != a.role[u]:
                continue
            d = 2.0 * cnt / (desc + sb[w] - 1)
            if d > best_d:
                best, best_d = w, d
        if best is not None and best_d >= min_dice:
            M[u] = best
            Mi[best] = u
    if 0 not in M and 0 not in Mi:
        M[0] = 0
        Mi[0] = 0

    # Phase 3: top-down over matched pairs, align leftover children by LCS on
    # full hash (whole subtree), then (role, name), then role alone.
    keys = (
        (lambda x: fa[x], lambda y: fb[y], True),
        (lambda x: (a.role[x], a.name[x]), lambda y: (b.role[y], b.name[y]), False),
        (lambda x: a.role[x], lambda y: b.role[y], False),
    )
    for u in oa:                     # preorder: parents before children
        w = M.get(u)
        if w is None:
            continue
        for ka, kb, whole in keys:
            ca = [x for x in a.children[u] if x not in M]
            cb = [y for y in b.children[w] if y not in Mi]
            if not ca or not cb:
                break
            for x, y in _lcs_pairs([ka(x) for x in ca], [kb(y) for y in cb]):
                if whole:
                    add_subtree(ca[x], cb[y])
                else:
                    M[ca[x]] = cb[y]
                    Mi[cb[y]] = ca[x]
    return M


def _lcs_pairs(s: list, t: list) -> list:
    """LCS alignment (O(|s||t|), children lists are short; cap for safety)."""
    if len(s) * len(t) > 250_000:
        # fall back to greedy in-order matching for huge child lists
        out, j = [], 0
        pos = defaultdict(list)
        for i, x in enumerate(t):
            pos[x].append(i)
        for i, x in enumerate(s):
            lst = pos.get(x)
            if lst:
                k = bisect.bisect_left(lst, j)
                if k < len(lst):
                    out.append((i, lst[k]))
                    j = lst[k] + 1
        return out
    n, m = len(s), len(t)
    dp = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            dp[i][j] = dp[i + 1][j + 1] + 1 if s[i] == t[j] else max(dp[i + 1][j], dp[i][j + 1])
    out, i, j = [], 0, 0
    while i < n and j < m:
        if s[i] == t[j]:
            out.append((i, j))
            i += 1
            j += 1
        elif dp[i + 1][j] >= dp[i][j + 1]:
            i += 1
        else:
            j += 1
    return out


# ---------------------------------------------------------------------------
# 4. Cursor restoration
def restore_cursor(a: Tree, b: Tree, M: dict, node: int, offset: int):
    """Map a reading position (node, offset) in `a` to `b`.

    1. node matched -> same node, offset clamped to the new name length.
    2. else nearest matched ancestor u; descend in b along the same child-index
       path (by role where possible).
    3. else first matched node preceding `node` in document order.
    Returns (node_in_b, offset, how).
    """
    if node in M:
        w = M[node]
        return w, min(offset, len(b.name[w])), "matched"
    path, u = [], node
    while u >= 0 and u not in M:
        p = a.parent[u]
        if p < 0:
            break
        path.append((a.children[p].index(u), a.role[u]))
        u = p
    if u in M:
        w = M[u]
        for idx, role in reversed(path):
            ch = b.children[w]
            if not ch:
                break
            same = [c for c in ch if b.role[c] == role]
            w = same[min(idx, len(same) - 1)] if same else ch[min(idx, len(ch) - 1)]
        return w, 0, "ancestor-path"
    order = a.preorder()
    i = order.index(node)
    for x in reversed(order[:i]):
        if x in M:
            return M[x], 0, "preceding"
    return 0, 0, "root"


# ---------------------------------------------------------------------------
# 5. Optimal re-fetch cover
def refetch_cover(parent: list, children: list, size: list, dirty: dict,
                  alpha: float, beta: float):
    """Exact min-cost cover of dirty nodes.

    dirty: node -> local patch cost a_v (e.g. alpha + beta*(1 + deg(v))).
    Option at node v: re-fetch the whole subtree, cost alpha + beta*size[v],
    which resolves every dirty node below; or recurse.
      best(v) = min(alpha + beta*size[v], [v dirty]*a_v + sum_{c in kids*(v)} best(c))
    where kids*(v) are children whose subtree contains a dirty node.
    Only the union of root paths of dirty nodes is visited: O(|D| * depth).
    Returns (cost, set_of_refetch_roots, set_of_individually_patched).
    """
    mark = defaultdict(list)   # node -> children on a dirty path
    seen = set()
    for d in dirty:
        v = d
        while v not in seen:
            seen.add(v)
            p = parent[v]
            if p < 0:
                break
            mark[p].append(v)
            v = p
    root = next(v for v in seen if parent[v] < 0)
    best, choice = {}, {}
    # post-order over the virtual tree
    order, st = [], [root]
    while st:
        v = st.pop()
        order.append(v)
        st.extend(mark.get(v, ()))
    for v in reversed(order):
        rec = dirty.get(v, 0.0) + sum(best[c] for c in mark.get(v, ()))
        full = alpha + beta * size[v]
        if full < rec:
            best[v], choice[v] = full, "refetch"
        else:
            best[v], choice[v] = rec, "recurse"
    roots, patched = set(), set()
    st = [root]
    while st:
        v = st.pop()
        if choice[v] == "refetch":
            roots.add(v)
            continue
        if v in dirty:
            patched.add(v)
        st.extend(mark.get(v, ()))
    return best[root], roots, patched
