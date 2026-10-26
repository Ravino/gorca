"""When may the reader state a list total ("list, 30 items") and a position ("3 of 30")?

Monte Carlo over list visits. Every number marked ASSUMED is a modelling parameter, not a
measurement; the report states which conclusions survive changing them.

List classes:
  STATIC  fully rendered, never virtual
  LOG     fully rendered, grows by arrivals (chat history without windowing): the count at the
          moment of speech is true, but membership churns
  VDECL   virtualised, author setsize/posinset on rendered items
  VUND    virtualised, no setsize: the DOM holds a window of W = V + 2o rows of N
          (INV: bottom-anchored, e.g. a message list opened at the newest message)

A visit: the reader enters the list (count statement), then reads j items one by one from the
window's start (or end, if bottom-anchored). The viewport moves when the cursor leaves the V
visible rows; the app then re-renders the window (a scroll-attributed churn episode) with
probability p_ev (the reader's scroll request may be a no-op, F3 in telegram-web-a-study.md).
The churn reaches the mirror after the statement of the key that caused it.

Rules (all use author setsize/posinset first when valid, and a plugin fact when present):
  R0  NVDA/Orca style: DOM count and DOM rank always
  R1  suspect after one scroll-attributed churn episode in this visit -> no total, no DOM rank
  R1m R1 + the suspicion is remembered per list identity for the session (later visits)
  R2  "stable count" gate: state the total only if the member count was unchanged for T >= 2 s
  R3  R1m + a geometry certificate at entry (container extent vs members' extents), detection
      g and false-alarm f ASSUMED
  R4  R1m + hedge (no exact total) whenever m >= m0 and no completeness evidence

    python tools/research/list_position/virtual_rule.py
"""

from __future__ import annotations

import math
import random
import sys
from dataclasses import dataclass

REGIMES = {
    # ASSUMED class mix of the lists a user enters, per regime
    "general-web": {"STATIC": 0.93, "LOG": 0.02, "VDECL": 0.01, "VUND": 0.04},
    "chat-app": {"STATIC": 0.45, "LOG": 0.10, "VDECL": 0.10, "VUND": 0.35},
}


@dataclass
class Lst:
    kind: str
    n: int
    w: int
    v: int
    inverted: bool
    visits: int


def draw_list(rng: random.Random, mix: dict[str, float]) -> Lst:
    kind = rng.choices(list(mix), list(mix.values()))[0]
    if kind == "STATIC":
        n = max(1, int(round(math.exp(rng.gauss(math.log(8), 1.0)))))
    elif kind == "LOG":
        n = max(1, int(round(math.exp(rng.gauss(math.log(50), 1.0)))))
    else:
        n = max(1, int(round(math.exp(rng.gauss(math.log(200), 1.0)))))
    v = rng.randint(6, 15)  # ASSUMED visible rows
    o = rng.randint(2, 10)  # ASSUMED overscan per side
    inv = kind == "VUND" and rng.random() < 0.4  # ASSUMED share of bottom-anchored lists
    # ASSUMED revisits per session: app lists are re-entered often
    visits = 1 + (rng.randint(2, 15) if kind in ("VUND", "VDECL", "LOG") else rng.randint(0, 2))
    return Lst(kind, n, v + 2 * o, v, inv, visits)


def simulate(regime: str, sessions: int = 4000, p_ev: float = 0.9, g: float = 0.8, f: float = 0.01,
             m0: int = 12, seed: int = 1) -> dict[str, dict[str, float]]:
    rng = random.Random(seed)
    mix = REGIMES[regime]
    rules = ["R0", "R1", "R1m", "R2", "R3", "R4"]
    st = {r: {"cnt": 0, "cnt_false": 0, "cnt_hedged_true": 0, "pos": 0, "pos_false": 0,
              "pos_hedged_true": 0} for r in rules}
    for _ in range(sessions):
        lists = [draw_list(rng, mix) for _ in range(rng.randint(3, 8))]
        for L in lists:
            truncated = L.kind in ("VUND", "VDECL") and L.n > L.w
            m = min(L.n, L.w) if L.kind in ("VUND", "VDECL") else L.n
            geo_hit = (rng.random() < g) if (L.kind == "VUND" and truncated) else (rng.random() < f)
            remembered = False  # R1m/R3/R4 memory across visits of this list
            complete_seen = False  # R4: reader reached both ends of a non-virtual list w/o churn
            for _visit in range(L.visits):
                j = max(1, int(rng.expovariate(1 / 15)))  # ASSUMED items read per visit
                # LOG lists churn by arrivals; ASSUMED P(arrival seen within the 2 s gate) = 0.3
                log_churn = L.kind == "LOG" and rng.random() < 0.3
                suspect_visit = False
                # timeline of statements: index 0 = entry count, then positions 1..j
                start = (L.n - m) if (truncated and L.inverted) else 0
                shifted = 0  # how far the rendered window moved
                for step in range(j + 1):
                    if step == 0:
                        true_ok = not truncated  # DOM count == N
                        kind = "cnt"
                    else:
                        idx = step - 1 if not L.inverted else (L.n - step)
                        if idx < 0 or idx >= L.n:
                            break
                        # a truncated list's DOM total is wrong, so "k of N" is false even when
                        # k happens to be right (window at the top, not yet shifted)
                        true_ok = not truncated
                        kind = "pos"
                    for r in rules:
                        author = L.kind == "VDECL"
                        if author:
                            say_exact, correct = True, True
                        else:
                            sus = suspect_visit
                            if r in ("R1m", "R3", "R4"):
                                sus = sus or remembered
                            if r == "R3":
                                sus = sus or geo_hit
                            if r == "R2":
                                sus = log_churn  # only a count change inside the gate window
                            if r == "R4" and not sus:
                                sus = (m >= m0) and not complete_seen
                            if r == "R0":
                                sus = False
                            say_exact = not sus
                            correct = true_ok
                        s = st[r]
                        s[kind] += 1
                        if say_exact and not correct:
                            s[kind + "_false"] += 1
                        if not say_exact and true_ok:
                            s[kind + "_hedged_true"] += 1
                    # after the statement: the cursor may leave the viewport -> churn (truncated only)
                    if step >= L.v and truncated and not suspect_visit and rng.random() < p_ev:
                        suspect_visit = True
                        shifted += 1
                    if L.kind == "LOG" and log_churn and step == 1:
                        suspect_visit = True  # arrivals are churn at the end, also scroll-agnostic
                if suspect_visit and L.kind != "LOG":
                    remembered = True
                if not truncated and j >= L.n:
                    complete_seen = True
    out = {}
    for r, s in st.items():
        out[r] = {
            "P(false | count)": s["cnt_false"] / max(1, s["cnt"]),
            "P(false | pos)": s["pos_false"] / max(1, s["pos"]),
            "hedged-correct count": s["cnt_hedged_true"] / max(1, s["cnt"]),
            "hedged-correct pos": s["pos_hedged_true"] / max(1, s["pos"]),
        }
    return out


def table(regime: str, **kw) -> str:
    res = simulate(regime, **kw)
    cols = list(next(iter(res.values())))
    lines = [f"regime {regime} {kw}", "rule  " + "  ".join(f"{c:>22s}" for c in cols)]
    for r, v in res.items():
        lines.append(f"{r:5s} " + "  ".join(f"{v[c]:22.4f}" for c in cols))
    return "\n".join(lines)


if __name__ == "__main__":
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 4000
    for reg in REGIMES:
        print(table(reg, sessions=n))
        print()
    print(table("chat-app", sessions=n, p_ev=0.5))
    print()
    print(table("chat-app", sessions=n, g=0.5, f=0.05))
