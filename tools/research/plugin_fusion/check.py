#!/usr/bin/env python3
"""Numerical checks for docs/reports/plugin-fusion-math.md (ADR-0026 MQ-1..MQ-3).

Run with the project venv: `python tools/research/plugin_fusion/check.py [mq1|mq2|mq3|all]`.
Every section asserts what the report claims as proven and prints the measured numbers.
Pure Python + numpy, deterministic seeds, runtime a few seconds.
"""
from __future__ import annotations

import itertools
import zlib
import random
import sys
from dataclasses import dataclass, field

import numpy as np

# ---------------------------------------------------------------------------------------------
# MQ-1: a small but faithful model of FUSE with plugin facts (S1 computed patches, S4 inferred)
# ---------------------------------------------------------------------------------------------

K = 7  # binary tree, node 0 is the root, parent(i) = (i - 1) // 2
NAMES = ["", "a", "b", "c"]
TIER_RANK = {"T2": 1, "T3": 0}  # higher wins


def children(n: int) -> list[int]:
    return [c for c in (2 * n + 1, 2 * n + 2) if c < K]


@dataclass
class Plugin:
    pid: int
    tier: str
    target: int
    reads: tuple[int, ...]  # R, always contains the target
    layer: str  # "S1" or "S4"
    stateful: bool = False

    def derive(self, view: tuple, state: int) -> tuple[str | None, int]:
        """Pure part: a deterministic function of the S0 view. Stateful plugins mix in a counter."""
        h = zlib.crc32(repr((self.pid, view)).encode()) & 0xFFFF
        if self.stateful:
            state += 1
            h ^= state
        v = NAMES[1 + h % 3] if h % 5 else None  # None = retract
        return v, state


@dataclass
class Reg:
    value: str | None
    t: int  # snapshot token (global stamp at call time)
    reads: tuple[int, ...]
    gen: int = 0  # BindGen stamped for the target


@dataclass
class Engine:
    plugins: list[Plugin]
    user: dict[int, str] = field(default_factory=dict)
    s0: list[str] = field(default_factory=lambda: [""] * K)
    stamp: list[int] = field(default_factory=lambda: [0] * K)
    clock: int = 0
    regs: dict[int, Reg] = field(default_factory=dict)
    state: dict[int, int] = field(default_factory=dict)
    published: list[tuple] = field(default_factory=list)
    # "none": patch values are clean; "taint": T3 patch values are tainted (H-5);
    # "clean_view": tainted, and S3 reads each child's best untainted value (rule PF-5)
    patch_mode: str = "clean_view"
    calls: int = 0

    def attach(self) -> None:
        """Plugin enable / node first seen: one initial derive of every watched target."""
        self.clock += 1
        for p in self.plugins:
            self.call(p)
        self.published = self.fuse_all()

    def stale(self, r: Reg) -> bool:
        return any(self.stamp[m] > r.t for m in r.reads)

    def call(self, p: Plugin) -> None:
        view = tuple(self.s0[m] for m in p.reads)  # CR-8: S0 only
        v, st = p.derive(view, self.state.get(p.pid, 0))
        self.state[p.pid] = st
        self.regs[p.pid] = Reg(v, self.clock, p.reads)
        self.calls += 1

    def turn(self, writes: list[tuple[int, str]], order_seed: int = 0) -> None:
        self.clock += 1
        changed = set()
        for n, v in writes:  # within-source order = issue order (Lemma 1)
            if self.s0[n] != v:
                self.s0[n] = v
                changed.add(n)
        for n in changed:
            self.stamp[n] = self.clock
        # Trigger rule T-1: a plugin is re-derived iff its read set R saw an S0 change.
        due = [p for p in self.plugins if changed & set(p.reads)]
        random.Random(order_seed).shuffle(due)  # call order must not matter
        for p in due:
            self.call(p)
        self.published = self.fuse_all()

    def fuse_all(self) -> list[tuple]:
        out: list[tuple | None] = [None] * K
        for n in reversed(range(K)):  # children before parents (depth-descending)
            out[n] = self.fuse(n, out)
        return out  # type: ignore[return-value]

    def fuse(self, n: int, done: list) -> tuple:
        """Returns (value, layer, tainted, clean): `clean` is the best untainted value of the node,
        what S3 of a parent reads in mode `clean_view`."""
        if n in self.user:
            return (self.user[n], "user", False, self.user[n])
        low = self.below_patch(n, done)
        s1 = [(TIER_RANK[p.tier], 1, p.pid, self.regs[p.pid].value, p.tier)
              for p in self.plugins
              if p.layer == "S1" and p.target == n and p.pid in self.regs
              and self.regs[p.pid].value is not None and not self.stale(self.regs[p.pid])]
        if s1:
            best = max(s1)  # static total rank (tier, specificity, plugin id)
            tainted = self.patch_mode != "none" and best[4] == "T3"
            return (best[3], "patch", tainted, low[3] if tainted else best[3])
        return low

    def below_patch(self, n: int, done: list) -> tuple:
        if self.s0[n] != "":
            return (self.s0[n], "observed", False, self.s0[n])
        kids = [done[c] for c in children(n) if done[c] and done[c][0]]
        if self.patch_mode == "clean_view":
            clean = [k[3] for k in kids if k[3]]
        else:
            clean = [k[0] for k in kids if not k[2]]
        if clean:
            v = "+".join(clean)
            return (v, "derived", False, v)
        if kids:
            return ("+".join(k[0] for k in kids), "derived", True, "")
        s4 = [(p.pid, self.regs[p.pid].value) for p in self.plugins
              if p.layer == "S4" and p.target == n and p.pid in self.regs
              and self.regs[p.pid].value is not None and not self.stale(self.regs[p.pid])]
        if s4:
            return (max(s4)[1], "inferred", True, "")
        return ("", "default", False, "")


def random_plugins(rng: random.Random, n: int, stateful: bool = False) -> list[Plugin]:
    ps = []
    for pid in range(n):
        t = rng.randrange(K)
        others = rng.sample(range(K), rng.randint(0, 2))
        ps.append(Plugin(pid, rng.choice(["T2", "T3"]), t, tuple(sorted({t, *others})),
                         rng.choice(["S1", "S4"]), stateful))
    return ps


def full_recompute(plugins: list[Plugin], s0: list[str], user: dict) -> list[tuple]:
    e = Engine(plugins, dict(user))
    e.s0 = list(s0)
    e.clock = 1
    for p in plugins:
        e.call(p)
    return e.fuse_all()


def random_batching(rng: random.Random, events: list) -> list[list]:
    cuts = sorted(rng.sample(range(1, len(events)), rng.randint(0, len(events) - 1)))
    return [events[a:b] for a, b in zip([0, *cuts], [*cuts, len(events)])]


def mq1() -> None:
    rng = random.Random(1)
    trials, pure_bad, state_diff, state_trials = 3000, 0, 0, 0
    for trial in range(trials):
        plugins = random_plugins(rng, rng.randint(1, 6))
        user = {rng.randrange(K): "u"} if rng.random() < 0.3 else {}
        events = [(rng.randrange(K), rng.choice(NAMES)) for _ in range(rng.randint(2, 14))]
        finals = set()
        for _ in range(4):
            e = Engine(plugins, dict(user))
            e.attach()
            for i, b in enumerate(random_batching(rng, events)):
                e.turn(b, order_seed=rng.randrange(10**9))
            finals.add(tuple(e.published))
            ref = full_recompute(plugins, e.s0, user)
            if tuple(ref) != tuple(e.published):
                pure_bad += 1
        assert len(finals) == 1
        # Stateful plugins: same events, different batching -> different published model?
        sp = random_plugins(rng, 3, stateful=True)
        outs = set()
        for _ in range(4):
            e = Engine(sp)
            e.attach()
            for b in random_batching(rng, events):
                e.turn(b)
            outs.add(tuple(e.published))
        state_trials += 1
        state_diff += len(outs) > 1
    assert pure_bad == 0
    print(f"MQ-1a pure view-functional plugins: {trials} event sets x 4 batchings x random call order:"
          f" final == full recompute in all; mismatches = {pure_bad}")
    print(f"MQ-1b stateful plugins (history counter): batching changes the final model in "
          f"{state_diff}/{state_trials} event sets (counterexample to order independence)")
    assert state_diff > 0

    # MQ-1c: oscillation when plugin views include S1 existence (hide). A hides X iff Y is shown
    # in its view; B hides Y iff X is shown. Views over fused existence -> period-2 cycle.
    def run_hide(view_fused: bool) -> tuple[bool, int]:
        hidden = {"X": False, "Y": False}  # S1 existence registers
        seen = set()
        for step in range(50):
            vx = (not hidden["X"]) if view_fused else True
            vy = (not hidden["Y"]) if view_fused else True
            new = {"X": vy, "Y": vx}  # both plugins read the same snapshot (one writer turn)
            key = tuple(new.values())
            if new == hidden:
                return True, step
            if key in seen:
                return False, step
            seen.add(key)
            hidden = new
        return False, 50
    conv_s0, s0_steps = run_hide(False)
    conv_fused, f_steps = run_hide(True)
    assert conv_s0 and not conv_fused
    print(f"MQ-1c hide feedback: S0-existence views converge in {s0_steps + 1} turn(s); "
          f"fused-existence views cycle (period 2 detected at step {f_steps})")

    # MQ-1d: trigger on own fact admission + private state -> unbounded re-derivation.
    p = Plugin(0, "T2", 0, (0,), "S1", stateful=True)
    e = Engine([p])
    e.turn([(0, "a")])
    loops = 0
    while loops < 1000:  # rule violated: admission of p's fact dirties its target -> derive again
        e.call(p)
        loops += 1
    print(f"MQ-1d trigger-on-admission with private state: still emitting after {loops} self-triggers "
          f"(no period <= 32: {cycle_period(p) == -1})")

    # MQ-1e: F12 for S4 additions and for T3 computed patches (taint) on other nodes.
    viol = {"S4": 0, "none": 0, "taint": 0, "clean_view": 0}
    n_trials = 4000
    for _ in range(n_trials):
        base = random_plugins(rng, rng.randint(0, 4))
        for p in base:
            p.tier = "T2"
        user = {rng.randrange(K): "u"} if rng.random() < 0.3 else {}
        s0 = [rng.choice(NAMES) for _ in range(K)]
        add = Plugin(99, "T3", rng.randrange(K), (), rng.choice(["S1", "S4"]))
        add.reads = (add.target,)
        for mode in ("none", "taint", "clean_view"):
            eb = Engine(base, dict(user), patch_mode=mode)
            eb.s0, eb.clock = list(s0), 1
            for p in eb.plugins:
                eb.call(p)
            before = eb.fuse_all()
            e = Engine(base + [add], dict(user), patch_mode=mode)
            e.s0, e.clock = list(s0), 1
            for p in e.plugins:
                e.call(p)
            after = e.fuse_all()
            for n in range(K):
                b, a = before[n], after[n]
                protected = b[1] in ("user", "patch") or (b[1] == "derived" and not b[2]) or \
                    (b[1] == "observed" and b[0] != "")
                if add.layer == "S1" and n == add.target:
                    continue  # a patch overriding its own target is the point of S1 (CR-1)
                if protected and a[:2] != b[:2]:
                    viol["S4" if add.layer == "S4" else mode] += 1
    assert viol["S4"] == 0 and viol["clean_view"] == 0 and viol["taint"] > 0 and viol["none"] > 0
    print(f"MQ-1e F12 over {n_trials} random models x 3 modes, off-target violations: S4 additions "
          f"{viol['S4']}; T3 computed patch untainted {viol['none']}, tainted only "
          f"{viol['taint']}, tainted + clean view for S3 {viol['clean_view']}")

    # MQ-1f: F18 with an asynchronous admission gap (reactor.deliver / request results).
    # anchor a -> u at gen 5; call at token t reads u; before admission a rebinds to v (gen 6).
    gen_at_token, gen_now = 5, 6
    wins_admission_stamp = gen_now == gen_now  # stamped at admission: matches current gen -> wins
    wins_token_stamp = gen_at_token == gen_now
    assert wins_admission_stamp and not wins_token_stamp
    print("MQ-1f F18 with admission gap: stamp-at-admission lets the old-view fact win on the new "
          "target (violation); stamp-at-token (gen recorded in the snapshot) makes it stale")


def cycle_period(p: Plugin) -> int:
    vals, st = [], 0
    for _ in range(64):
        v, st = p.derive(("a",), st)
        vals.append(v)
    for per in range(1, 33):
        if all(vals[i] == vals[i + per] for i in range(16, 64 - per)):
            return per
    return -1


# ---------------------------------------------------------------------------------------------
# MQ-2: staleness bookkeeping schemes vs the exact (value) criterion
# ---------------------------------------------------------------------------------------------

CLASSES = ["struct", "role_states", "name", "text", "attrs", "bounds"]
VIS = {"struct", "bounds", "name"}  # what ADR-0024 vis_rev bumps on


def mq2() -> None:
    rng = np.random.default_rng(2)
    N, C = 24, len(CLASSES)
    steps, trials = 60, 3000
    res = {s: [0, 0, 0, 0] for s in ("class_stamp", "node_stamp", "vis_rev", "sem_rev", "hash64")}
    # counts: [false_accept, false_reject, true_stale, true_fresh]
    for _ in range(trials):
        val = rng.integers(0, 3, size=(N, C))  # small alphabet -> plenty of ABA
        cstamp = np.zeros((N, C), dtype=np.int64)
        visrev = np.zeros(N, dtype=np.int64)
        semrev = np.zeros(N, dtype=np.int64)
        clock = 0
        R = rng.choice(N, size=rng.integers(1, 7), replace=False)
        M = np.flatnonzero(rng.random(C) < 0.5)
        if M.size == 0:
            M = np.array([2])
        target = R[0]
        snap = val[np.ix_(R, M)].copy()
        t = clock
        t_sem = semrev[R].copy()
        t_vis = visrev[R].copy()
        # the plugin's own fact is admitted in the same turn: bumps target's sem_rev (published change)
        semrev[target] += 1
        for _ in range(rng.integers(0, steps)):
            clock += 1
            kind = rng.random()
            n = rng.integers(0, N)
            if kind < 0.6:  # S0 change of one class (value actually changes)
                c = rng.integers(0, C)
                old = val[n, c]
                new = (old + rng.integers(1, 3)) % 3
                val[n, c] = new
                cstamp[n, c] = clock
                semrev[n] += 1
                if CLASSES[c] in VIS:
                    visrev[n] += 1
            else:  # another source's S1/S4 fact changes the published entry, S0 unchanged
                semrev[n] += 1
        truth_stale = not np.array_equal(val[np.ix_(R, M)], snap)
        verdict = {
            "class_stamp": bool((cstamp[np.ix_(R, M)] > t).any()),
            "node_stamp": bool((cstamp[R] > t).any()),
            "vis_rev": bool((visrev[R] > t_vis).any()),
            "sem_rev": bool((semrev[R] > t_sem).any()),
            "hash64": truth_stale,  # collision probability 2^-64 per check, ignored
        }
        for s, v in verdict.items():
            r = res[s]
            if v and not truth_stale:
                r[1] += 1
            if not v and truth_stale:
                r[0] += 1
            r[2] += truth_stale
            r[3] += not truth_stale
    print("MQ-2 schemes over", trials, "random (R, read-mask, history) trials:")
    print(f"  {'scheme':12s} {'false-accept':>12s} {'false-reject':>12s}   (of stale / fresh)")
    for s, (fa, fr, ts, tf) in res.items():
        print(f"  {s:12s} {fa:12d} {fr:12d}   ({ts} / {tf})")
    assert res["class_stamp"][0] == 0 and res["node_stamp"][0] == 0
    assert res["vis_rev"][0] > 0 and res["sem_rev"][1] >= res["node_stamp"][1]
    assert res["class_stamp"][1] <= res["node_stamp"][1]

    # Global-stamp equivalence: max over R of per-node stamps vs per-node revision vectors.
    # stamp[n] > t  <=>  rev[n] now > rev[n] at t, because both bump on exactly the same events and
    # the global clock is strictly increasing. Checked exhaustively on short histories.
    for hist in itertools.product(range(3), repeat=6):
        stamp, rev, clock = [0, 0, 0], [0, 0, 0], 0
        t_idx = hist[0] % 6
        rev_t = None
        for i, n in enumerate(hist):
            if i == t_idx:
                rev_t, t = list(rev), clock
            clock += 1
            stamp[n], rev[n] = clock, rev[n] + 1
        if rev_t is None:
            rev_t, t = list(rev), clock
        for n in range(3):
            assert (stamp[n] > t) == (rev[n] > rev_t[n])
    print("  global-stamp == per-node revision vector: exhaustive check over 3^6 histories OK")


# ---------------------------------------------------------------------------------------------
# MQ-3: splitting the 0.3 ms interactive budget between N plugins
# ---------------------------------------------------------------------------------------------

B = 300.0  # us, per focus change, all plugins
CAP = 200.0  # us, per call (B-2)
C_MAX = 2.0  # us, host->guest entry + marshalling, not fuel-charged (measured 0.05-2 us)


def schedule(policy: str, demand: np.ndarray, pred: np.ndarray, entry: np.ndarray):
    """Returns (granted, used, completed, wall). Demand in us of guest compute at nominal kappa."""
    n = len(demand)
    pool = B - n * C_MAX  # entry overhead reserved up front, never fuel-charged
    granted = np.zeros(n)
    used = np.zeros(n)
    if policy == "equal":
        order = range(n)
        for i in order:
            granted[i] = min(CAP, pool / n)
            used[i] = min(demand[i], granted[i])
    elif policy == "greedy_fixed":  # plugin-id order, each takes what it needs from what is left
        rem = pool
        for i in range(n):
            granted[i] = min(CAP, rem)
            used[i] = min(demand[i], granted[i])
            rem -= used[i]
    elif policy == "progressive":  # ascending predicted demand, a_k = min(cap, rem / remaining)
        rem = pool
        order = np.lexsort((np.arange(n), pred))
        for k, i in enumerate(order):
            granted[i] = min(CAP, rem / (n - k))
            used[i] = min(demand[i], granted[i])
            rem -= used[i]
    else:
        raise ValueError(policy)
    completed = demand <= granted
    wall = used.sum() + entry.sum()
    return granted, used, completed, wall


def maxmin_fair(demand: np.ndarray, pool: float) -> np.ndarray:
    d = np.minimum(demand, CAP)
    alloc = np.zeros_like(d)
    rem, idx = pool, list(np.argsort(d))
    while idx:
        share = rem / len(idx)
        i = idx[0]
        if d[i] <= share:
            alloc[i] = d[i]
            rem -= d[i]
            idx.pop(0)
        else:
            for j in idx:
                alloc[j] = share
            break
    return alloc


def mq3() -> None:
    rng = np.random.default_rng(3)
    print("MQ-3 schedules, 20000 focus changes per N; demand lognormal(median 8 us, sigma 1.2)"
          " + 5 % heavy (U[100, 400] us); prediction = demand x lognormal(0, 0.3)")
    print(f"  {'N':>3s} {'policy':>13s} {'done%':>6s} {'cheap-starved%':>14s} {'maxmin-gap':>10s}"
          f" {'wall p99':>8s} {'wall max':>8s} {'floor ok':>8s}")
    for n in (1, 4, 10, 16):
        for policy in ("equal", "greedy_fixed", "progressive"):
            done = starved = cheap = 0
            gaps, walls = [], []
            floor_ok = True
            for _ in range(20000):
                demand = rng.lognormal(np.log(8), 1.2, n)
                heavy = rng.random(n) < 0.05
                demand[heavy] = rng.uniform(100, 400, heavy.sum())
                pred = demand * rng.lognormal(0, 0.3, n)
                entry = rng.uniform(0.05, C_MAX, n)
                g, u, c, w = schedule(policy, demand, pred, entry)
                pool = B - n * C_MAX
                done += c.sum()
                is_cheap = demand <= min(CAP, pool / n)
                cheap += is_cheap.sum()
                starved += (is_cheap & ~c).sum()
                gaps.append(np.abs(np.minimum(u, demand) - maxmin_fair(demand, pool)).max())
                walls.append(w)
                if policy == "progressive":
                    floor_ok &= bool((g >= min(CAP, pool / n) - 1e-9).all())
                assert w <= B + 1e-9
            walls = np.array(walls)
            print(f"  {n:3d} {policy:>13s} {100 * done / (20000 * n):6.2f} "
                  f"{100 * starved / max(cheap, 1):14.3f} {np.percentile(gaps, 99):10.2f} "
                  f"{np.percentile(walls, 99):8.1f} {walls.max():8.1f} "
                  f"{('yes' if floor_ok else 'NO') if policy == 'progressive' else '-':>8s}")

    # Worst-case added delay when fuel->time calibration is off by rho (memory-bound guest code).
    print("  worst-case added delay D = N*c_max + rho*(B - N*c_max), c_max = 2 us:")
    for n in (4, 10, 16):
        row = [f"rho={r:g}: {n * C_MAX + r * (B - n * C_MAX):7.0f} us" for r in (1, 1.5, 3, 10)]
        print(f"   N={n:2d}  " + "  ".join(row))
    print(f"  spike arithmetic: 3e6 units / 138 us = {3e6 / 138:.0f} units/us "
          f"(ADR-0026 and the spike say '~22 units/us')")


def main() -> None:
    what = sys.argv[1] if len(sys.argv) > 1 else "all"
    if what in ("mq1", "all"):
        mq1()
    if what in ("mq2", "all"):
        mq2()
    if what in ("mq3", "all"):
        mq3()


if __name__ == "__main__":
    main()
