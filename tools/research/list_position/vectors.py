"""Test vectors for the Rust port: op sequences with the expected per-list members after each op.

Node ids are allocated sequentially from 0 (0 = document root, a sentinel container); an inserted
subtree takes consecutive ids in preorder. Spec encoding: [role, hidden, [children...]] with role
in {"item", "list", "other"}. Expected state after each op: {list_id: [member ids in order]}
(count = len, rank(member) = index + 1, kth(list, k) = members[k]).

    python tools/research/list_position/vectors.py > tools/research/list_position/vectors.json
"""

from __future__ import annotations

import json
import random

from prop_test import rand_spec, step
from setrank import Doc, Spec, check, oracle


def enc(s: Spec) -> list:
    return [s.role, s.hidden, [enc(c) for c in s.children]]


def state(doc: Doc) -> dict[str, list[int]]:
    m = oracle(doc)
    return {str(k): v for k, v in sorted(m.items()) if k != doc.root}


class Recorder(Doc):
    """Doc that logs its public mutations."""

    def __init__(self, seed: int):
        super().__init__(seed)
        self.log: list[dict] = []
        self._depth = 0

    def _rec(self, entry: dict) -> None:
        if self._depth == 1:
            self.log.append(entry)

    def insert_subtree(self, parent, idx, spec):
        self._depth += 1
        try:
            x = super().insert_subtree(parent, idx, spec)
            self._rec({"op": "insert", "parent": parent, "index": idx, "spec": enc(spec), "id": x})
            return x
        finally:
            self._depth -= 1

    def remove_subtree(self, x):
        self._depth += 1
        try:
            super().remove_subtree(x)
            self._rec({"op": "remove", "node": x})
        finally:
            self._depth -= 1

    def move_subtree(self, x, p, i):
        self._depth += 1
        try:
            super().move_subtree(x, p, i)
            self._rec({"op": "move", "node": x, "parent": p, "index": i})
        finally:
            self._depth -= 1

    def set_props(self, x, role=None, hidden=None):
        self._depth += 1
        try:
            super().set_props(x, role, hidden)
            e = {"op": "props", "node": x}
            if role is not None:
                e["role"] = role
            if hidden is not None:
                e["hidden"] = hidden
            self._rec(e)
        finally:
            self._depth -= 1

    def wrap(self, parent, i, j, role="list"):
        self._depth += 1
        try:
            n = super().wrap(parent, i, j, role)
            self._rec({"op": "wrap", "parent": parent, "from": i, "to": j, "role": role, "id": n})
            return n
        finally:
            self._depth -= 1

    def dissolve(self, n):
        self._depth += 1
        try:
            super().dissolve(n)
            self._rec({"op": "dissolve", "node": n})
        finally:
            self._depth -= 1


def hand_vector() -> dict:
    """Readable scenario: nested list, late role patches, a synthetic list, hidden item."""
    d = Recorder(1)
    item = lambda *k: Spec("item", False, list(k))  # noqa: E731
    other = lambda *k: Spec("other", False, list(k))  # noqa: E731
    page = other(
        Spec("list", False, [item(other()), item(Spec("list", False, [item(), item()])), item()]),
        other(other(), other(), other(), other()),
    )
    steps = []
    d.insert_subtree(d.root, 0, page)
    steps.append(state(d))
    seq = [
        lambda: d.set_props(11, role="item"),  # a plugin re-roles children of the plain block
        lambda: d.set_props(12, role="item"),
        lambda: d.wrap(10, 0, 3, "list"),  # synthetic list over the first three children
        lambda: d.set_props(12, hidden=True),
        lambda: d.set_props(3, role="other"),  # the first item of the outer list stops being one
        lambda: d.set_props(6, role="other"),  # the nested list dissolves: its items move up
        lambda: d.dissolve(15),
    ]
    for f in seq:
        f()
        check(d)
        steps.append(state(d))
    return {"name": "hand", "ops": d.log, "expect": steps}


def random_vector(seed: int, n: int) -> dict:
    rng = random.Random(seed)
    d = Recorder(seed)
    synth: set[int] = set()
    expect = []
    for _ in range(n):
        d._depth = 0
        step(d, rng, synth)
        check(d)
        if len(expect) < len(d.log):
            expect.append(state(d))
    return {"name": f"random-{seed}", "ops": d.log, "expect": expect}


if __name__ == "__main__":
    vecs = [hand_vector()] + [random_vector(s, 40) for s in range(4)]
    for v in vecs:
        assert len(v["ops"]) == len(v["expect"]), (v["name"], len(v["ops"]), len(v["expect"]))
    print(json.dumps(vecs, indent=None, separators=(",", ":")))
