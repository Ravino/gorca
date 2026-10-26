"""Property test: random mutations of a document with nested lists, hidden nodes, role patches and
synthetic containers; after every step the signed-weight structure must agree with the oracle.

    python tools/research/list_position/prop_test.py [runs] [steps]
"""

from __future__ import annotations

import random
import sys

from setrank import ITEM, LIST, OTHER, Doc, Spec, check


def rand_spec(rng: random.Random, depth: int) -> Spec:
    role = rng.choices([ITEM, LIST, OTHER], [4, 1, 3])[0]
    s = Spec(role=role, hidden=rng.random() < 0.08)
    if depth > 0:
        for _ in range(rng.choice([0, 0, 1, 2, 3, 4])):
            s.children.append(rand_spec(rng, depth - 1))
    return s


def live_nodes(doc: Doc) -> list[int]:
    out, st = [], [doc.root]
    while st:
        y = st.pop()
        out.append(y)
        st.extend(doc.kids[y])
    return out


def is_ancestor(doc: Doc, a: int, b: int) -> bool:
    while b != -1:
        if a == b:
            return True
        b = doc.parent[b]
    return False


def step(doc: Doc, rng: random.Random, synth: set[int]) -> str:
    nodes = live_nodes(doc)
    op = rng.choices(
        ["insert", "remove", "move", "role", "hide", "wrap", "dissolve"],
        [5, 2, 2, 4, 1, 1, 1],
    )[0]
    if op == "insert" or len(nodes) < 4:
        p = rng.choice(nodes)
        doc.insert_subtree(p, rng.randint(0, len(doc.kids[p])), rand_spec(rng, 3))
        return "insert"
    x = rng.choice(nodes[1:])
    if op == "remove":
        if x in synth:
            return "skip"
        synth.difference_update({s for s in synth if is_ancestor(doc, x, s)})
        doc.remove_subtree(x)
    elif op == "move":
        p = rng.choice(nodes)
        if is_ancestor(doc, x, p) or x in synth:
            return "skip"
        doc.move_subtree(x, p, rng.randint(0, len(doc.kids[p]) - (1 if doc.parent[x] == p else 0)))
    elif op == "role":
        doc.set_props(x, role=rng.choice([ITEM, LIST, OTHER]))
    elif op == "hide":
        doc.set_props(x, hidden=not doc.hidden[x])
    elif op == "wrap":
        p = rng.choice(nodes)
        k = len(doc.kids[p])
        if k < 2:
            return "skip"
        i = rng.randint(0, k - 2)
        j = rng.randint(i + 2, k)
        synth.add(doc.wrap(p, i, j))
    elif op == "dissolve":
        if not synth:
            return "skip"
        n = rng.choice(sorted(synth))
        synth.discard(n)
        doc.dissolve(n)
    return op


def main() -> None:
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 200
    steps = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    tally: dict[str, int] = {}
    for seed in range(runs):
        rng = random.Random(seed)
        doc = Doc(seed)
        synth: set[int] = set()
        for _ in range(steps):
            op = step(doc, rng, synth)
            tally[op] = tally.get(op, 0) + 1
            try:
                check(doc)
            except AssertionError as e:
                print(f"FAIL seed={seed} after {op}: {e}")
                raise
    print(f"OK {runs} runs x {steps} steps; ops {dict(sorted(tally.items()))}")


if __name__ == "__main__":
    main()
