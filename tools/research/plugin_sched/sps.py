"""Reference implementation of the statistical plugin scheduler (SPS) for ADR-0026 B'.

Paper/simulation only. Integer arithmetic throughout (times in whole microseconds, probabilities in
Q16 fixed point) so that a Rust port is bit-exact and a replay of the same inputs gives the same
decisions. No randomness inside the scheduler.

Three decisions per keypress (see docs/reports/plugin-statistical-scheduler.md):
  (a) on key-down:   K = number of pool threads to pre-wake (spin), from per-context activation stats
                     and a CPU token bucket;
  (b) on dispatch:   thread assignment (hot queue or parked thread, weighted-SPT order with aging),
                     the wait set W and the writer deadline t* (value-minus-delay optimum over a grid);
  (c) fuel grants:   weighted progressive filling per hot thread (MQ-3 generalised).
Observations (replies) update decayed log-histograms and the offence/exclusion state.

Run `python sps.py` for the self-test (test vectors).
"""
from __future__ import annotations

from dataclasses import dataclass, field

Q = 1 << 16  # Q16 one

# Log-spaced bucket lower edges in microseconds; bucket b = [EDGES[b], EDGES[b+1]), last = [1024, inf).
EDGES = [0, 4, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 1024]
NB = len(EDGES)
INF_US = 4096  # upper edge used for the open last bucket


def upper(b: int) -> int:
    return EDGES[b + 1] if b + 1 < NB else INF_US


def bucket(x_us: int) -> int:
    b = 0
    while b + 1 < NB and x_us >= EDGES[b + 1]:
        b += 1
    return b


class Hist:
    """Decayed histogram: weight 16 per sample, all counts halved every HALF samples (deterministic)."""

    HALF = 64
    W = 16

    def __init__(self, prior: list[int] | None = None):
        self.c = list(prior) if prior else [0] * NB
        self.n = 0

    def add(self, x_us: int) -> None:
        self.c[bucket(max(0, x_us))] += self.W
        self.n += 1
        if self.n % self.HALF == 0:
            self.c = [v >> 1 for v in self.c]

    def total(self) -> int:
        return sum(self.c)

    def quantile(self, p_q16: int) -> int:
        """Smallest bucket upper edge u with F(u) >= p (conservative: never below the true quantile)."""
        tot = self.total()
        if tot == 0:
            return INF_US
        acc = 0
        for b in range(NB):
            acc += self.c[b]
            if acc * Q >= p_q16 * tot:
                return upper(b)
        return INF_US

    def cdf_at(self, t_us: int) -> int:
        """Q16 lower bound of P(X < t): mass of buckets whose upper edge <= t (pessimistic)."""
        tot = self.total()
        if tot == 0:
            return 0
        acc = 0
        for b in range(NB):
            if upper(b) <= t_us:
                acc += self.c[b]
            else:
                break
        return acc * Q // tot

    def pmf(self) -> list[tuple[int, int]]:
        """(upper edge, Q16 mass) pairs; used for the start-latency convolution."""
        tot = self.total()
        if tot == 0:
            return [(INF_US, Q)]
        return [(upper(b), v * Q // tot) for b, v in enumerate(self.c) if v]


def ewma_q16(old: int, x_q16: int, shift: int) -> int:
    """old + (x - old) / 2^shift, integer; floor division so it is identical in Rust (>> on i64)."""
    return old + ((x_q16 - old) >> shift)


# Labels: manual hints. Values are priors (Q16) and constraints.
LABELS = {
    #             value prior   activation prior  wait-eligible  spin-eligible  weight
    "interactive": (Q,            Q // 2,           True,          True,          2),
    "default":     (Q // 2,       Q // 4,           True,          True,          1),
    "refinement":  (Q // 4,       Q // 8,           False,         False,         1),
}
TIER_VALUE_CAP = {"first": Q, "trusted": (3 * Q) // 4, "third": Q // 2}
TIER_SPIN_CAP = {"first": 16, "trusted": 16, "third": 1}  # hot-queue places per keypress
TIER_WAIT_CAP_US = {"first": 10_000, "trusted": 10_000, "third": 100}  # max d_i a tier may hold the writer


@dataclass
class Params:
    D: int = 300            # us, PF-8 bound from the start of the writer turn
    fuse_us: int = 8        # reserved for apply+fuse after the wait (inside D)
    snap_us: int = 6        # snapshot + send base cost (inside D)
    lam_q16: int = 3          # price of writer delay in Q16 value units per us (~1 fact per 20 ms):
    #                           inside the pre-approved D the gate favours facts; lam only stops
    #                           waiting for plugins whose expected value is ~0
    theta_spin_q16: int = Q // 100  # min expected gain (facts) for a plugin to count toward K
    k_mult_q16: int = Q     # K = ceil(1.0 * sum alpha) (sweep in the report)
    k_max: int = 6          # hot threads per keypress at most
    spin_bucket_us: int = 200_000   # token bucket size (us of spin CPU)
    spin_rate_us_per_s: int = 150_000  # refill: 15 % of one core
    spin_est_us: int = 3_000  # charged per pre-woken thread before the real cost is known
    act_shift: int = 5      # activation EWMA, ~22-keypress half-life per context
    val_shift: int = 5
    n0: int = 4             # prior pseudo-count weight (in EWMA terms: initial value = prior)
    offence_r: int = 32     # on-time replies that forgive one offence
    excl_max: int = 256     # exclusion cap in activations (starvation bound)
    age_max: int = 8        # hot-thread denials before promotion to the front
    margin_us: int = 8      # added to the union-bound deadline
    warmup: int = 8         # replies before a plugin's own histogram is trusted
    cap_fuel_us: int = 200  # B-2 per-call cap (wall-time equivalent)
    kappa: int = 21_739     # fuel units per us (interactive calibration rate)
    # ---- warm hook (plugin-warm-hook-model.md); none of these change the decisions above ----
    warm_bucket_us: int = 60_000      # hook CPU bucket (us)
    warm_rate_us_per_s: int = 30_000  # refill: 3 % of one core
    warm_est_us: int = 150            # charged per hook before its measured cost is known
    warm_delta_us: int = 60           # prior shift cold -> warm exec until both histograms are trusted
    theta_warm_q16: int = Q // 100    # min expected gain (facts) per hook, like theta_spin
    warm_gap_ms: int = 20             # frequency cap: at most one hook per plugin per 20 ms
    warm_third_max: int = 1           # third-party hooks per key-down


@dataclass
class PluginStats:
    pid: int
    label: str
    tier: str
    exec_h: Hist = field(default_factory=Hist)
    useful_q16: int = 0
    act: dict = field(default_factory=dict)  # ctx -> Q16 activation estimate
    offences: int = 0
    offence_total: int = 0
    excluded: int = 0       # activations remaining excluded from W
    ontime_run: int = 0
    denied: int = 0         # consecutive hot-thread denials (aging)
    exec_hw: Hist = field(default_factory=Hist)  # exec when the call path was warm (hooked)
    last_warm_ms: int = -(1 << 40)

    def __post_init__(self):
        self.useful_q16 = LABELS[self.label][0]

    def value(self) -> int:
        return min(self.useful_q16, TIER_VALUE_CAP[self.tier])

    def alpha(self, ctx) -> int:
        return self.act.get(ctx, LABELS[self.label][1])


@dataclass
class Assignment:
    pid: int
    hot: int | None          # hot thread index or None (parked thread)
    offset_us: int           # predicted queue offset on the hot thread
    deadline_us: int         # d_i (from t0)
    grant_fuel: int
    exec_allow_us: int = INF_US  # e99 + margin: an offence needs exec above this (plugin's own fault)
    clipped: bool = False    # d_i was cut to the writer window: no offence possible
    hold_us: int = 0         # how long the writer waits for this plugin: min(d_i, t*); 0 = not waited


@dataclass
class Plan:
    assign: list[Assignment]
    wait: list[int]          # plugin ids in the countdown
    t_star: int              # max d_i over the wait set (<= D - snap - fuse). The writer ends at
    #                          max_{i in wait} min(reply_i, d_i): each waited plugin holds it at most d_i


class Scheduler:
    def __init__(self, plugins: list[tuple[int, str, str]], params: Params | None = None):
        self.p = params or Params()
        self.s = {pid: PluginStats(pid, lab, tier) for pid, lab, tier in plugins}
        # Pooled start-latency histograms by thread state (host property, shared by all plugins).
        self.start_h = {"hot": Hist(), "cold": Hist()}
        self.bucket_us = self.p.spin_bucket_us
        self.last_ms = 0
        self.turn = 0
        # warm hook state (pooled host properties)
        self.warm_cost_h = Hist()
        self.warm_bucket = self.p.warm_bucket_us
        self.warm_last_ms = 0
        self.warm_delta = self.p.warm_delta_us

    # ---------- (a) key-down: how many threads to pre-wake ----------
    def _refill(self, now_ms: int) -> None:
        dt = max(0, now_ms - self.last_ms)
        self.last_ms = now_ms
        self.bucket_us = min(self.p.spin_bucket_us, self.bucket_us + dt * self.p.spin_rate_us_per_s // 1000)

    def _f_state(self, st: PluginStats, state: str, t_us: int, offset: int = 0, warm: bool = False) -> int:
        """Q16 lower bound of P(start + exec < t), start from the pooled histogram of `state`.

        warm=True: the exec histogram of hooked calls once it has `warmup` samples, before that the
        cold histogram shifted left by the pooled cold-warm difference (`warm_delta`)."""
        acc = 0
        if warm and st.exec_hw.n >= self.p.warmup:
            h, shift = st.exec_hw, 0
        else:
            h, shift = st.exec_h, (self.warm_delta if warm else 0)
        for up, m in self.start_h[state].pmf():
            acc += m * h.cdf_at(t_us - offset - up + shift) // Q
        return acc

    def _exec_q(self, st: PluginStats, p_q16: int, warm: bool, default: int) -> int:
        """Exec quantile, warm-aware (same fallback as _f_state)."""
        if warm and st.exec_hw.n >= self.p.warmup:
            return st.exec_hw.quantile(p_q16)
        if not st.exec_h.total():
            return default
        q = st.exec_h.quantile(p_q16)
        return max(1, q - self.warm_delta) if warm and q < INF_US else q

    def keydown(self, ctx, now_ms: int) -> int:
        self._refill(now_ms)
        dw = self.p.D - self.p.fuse_us - self.p.snap_us
        scores = []
        load = 0
        for st in self.s.values():
            if not LABELS[st.label][3]:
                continue
            a = st.alpha(ctx)
            gain = max(0, self._f_state(st, "hot", dw) - self._f_state(st, "cold", dw))
            if not self.start_h["hot"].total() or not self.start_h["cold"].total():
                gain = Q // 2  # no host data yet: assume warm helps
            sc = a * st.value() // Q * gain // Q
            if sc >= self.p.theta_spin_q16:
                scores.append(sc)
                load += a  # expected number of valuable plugins triggered (Q16)
        if not scores:
            return 0
        # K = ceil(expected valuable triggered plugins), at least 1; dispatch packs the rest on hot
        # queues (list scheduling) or sends them to parked threads.
        k = min(len(scores), self.p.k_max, max(1, -(-load * self.p.k_mult_q16 // (Q * Q))))
        k = min(k, self.bucket_us // self.p.spin_est_us)
        self.bucket_us -= k * self.p.spin_est_us
        return max(0, k)

    def keydown_plan(self, ctx, now_ms: int, warm: bool = True, k_force: int | None = None):
        """Key-down with explicit worker choice and the warm action.

        Returns (k, armed, hooks): `armed` = the k spin-eligible plugins with the highest score
        (their workers spin until the focus change; one worker per plugin, as in the runtime);
        `hooks` = plugins whose warm hook fires now, chosen greedily by expected gain per us of hook
        CPU under the warm bucket (fractional-knapsack order). K and the spin bucket follow
        `keydown`; `k_force` overrides K (0 = no spinning, hooks only)."""
        self._refill(now_ms)
        p = self.p
        dw = p.D - p.fuse_us - p.snap_us
        no_host = not self.start_h["hot"].total() or not self.start_h["cold"].total()
        scored = []
        load = 0
        for st in self.s.values():
            if not LABELS[st.label][3]:
                continue
            a = st.alpha(ctx)
            gain = Q // 2 if no_host else max(0, self._f_state(st, "hot", dw) - self._f_state(st, "cold", dw))
            sc = a * st.value() // Q * gain // Q
            if sc >= p.theta_spin_q16:
                scored.append((sc, st.pid))
                load += a
        k = 0
        if scored:
            k = min(len(scored), p.k_max, max(1, -(-load * p.k_mult_q16 // (Q * Q))))
            k = min(k, self.bucket_us // p.spin_est_us)
        if k_force is not None:
            k = min(k_force, len(scored))
        k = max(0, k)
        self.bucket_us -= k * p.spin_est_us
        scored.sort(key=lambda x: (-x[0], x[1]))
        armed = [pid for _, pid in scored[:k]]
        hooks: list[int] = []
        if not warm:
            return k, armed, hooks
        dt = max(0, now_ms - self.warm_last_ms)
        self.warm_last_ms = now_ms
        self.warm_bucket = min(p.warm_bucket_us, self.warm_bucket + dt * p.warm_rate_us_per_s // 1000)
        cost = max(1, self.warm_cost_h.quantile(Q // 2) if self.warm_cost_h.total() else p.warm_est_us)
        cand = []
        for st in self.s.values():
            # never refinement (never waited: a hook cannot add a first-utterance fact), never an
            # excluded plugin, at most one hook per plugin per warm_gap_ms (frequency cap)
            if not LABELS[st.label][2] or not LABELS[st.label][3] or st.excluded:
                continue
            if now_ms - st.last_warm_ms < p.warm_gap_ms:
                continue
            state = "hot" if st.pid in armed else "cold"
            g = max(0, self._f_state(st, state, dw, 0, True) - self._f_state(st, state, dw))
            sc = st.alpha(ctx) * st.value() // Q * g // Q
            if sc >= p.theta_warm_q16:
                cand.append((sc * 1024 // cost, sc, st.pid))
        cand.sort(key=lambda x: (-x[0], x[2]))
        third = 0
        for _, _, pid in cand:
            if self.warm_bucket < cost:
                break
            st = self.s[pid]
            if st.tier == "third":
                if third >= p.warm_third_max:
                    continue
                third += 1
            self.warm_bucket -= cost
            st.last_warm_ms = now_ms
            hooks.append(pid)
        return k, armed, hooks

    def observe_warm(self, pid: int, cpu_us: int, est_us: int) -> None:
        """A hook finished: its CPU feeds the pooled cost histogram and settles the warm bucket."""
        self.warm_cost_h.add(cpu_us)
        self.warm_bucket = min(self.p.warm_bucket_us,
                               max(-self.p.warm_bucket_us, self.warm_bucket + est_us - cpu_us))

    def spin_refund(self, est_us: int, actual_us: int) -> None:
        """Called when a pre-woken thread parks: correct the estimate charged at key-down."""
        self.bucket_us = min(self.p.spin_bucket_us, max(-self.p.spin_bucket_us, self.bucket_us + est_us - actual_us))

    # ---------- (b)+(c) dispatch ----------
    def dispatch(self, triggered: list[int], k_hot: int, armed: list[int] | None = None,
                 warm: frozenset = frozenset()) -> Plan:
        """armed=None: K generic pre-armed queues (ADR text). armed=list: one worker per plugin (the
        runtime); a plugin is hot iff its own worker was armed, offset 0. `warm`: hooked plugins,
        whose deadlines and F use the warm exec histogram."""
        p = self.p
        per_thread = armed is not None
        if per_thread:
            k_hot = len(armed)
        self.turn += 1
        n = len(triggered)
        dw = p.D - p.fuse_us - p.snap_us
        hot_q99 = self.start_h["hot"].quantile(Q * 99 // 100) if self.start_h["hot"].total() else 8
        cold_q99 = self.start_h["cold"].quantile(Q * 99 // 100) if self.start_h["cold"].total() else 256
        cold_q90 = self.start_h["cold"].quantile(Q * 9 // 10) if self.start_h["cold"].total() else 128
        hot_q90 = self.start_h["hot"].quantile(Q * 9 // 10) if self.start_h["hot"].total() else 4
        hot_q50 = self.start_h["hot"].quantile(Q // 2) if self.start_h["hot"].total() else 4

        def index(pid):
            st = self.s[pid]
            e50 = max(1, st.exec_h.quantile(Q // 2) if st.exec_h.total() else 32)
            promoted = st.denied >= p.age_max
            w = LABELS[st.label][4]
            eligible = LABELS[st.label][2] and st.excluded == 0
            # Weighted SPT (Smith / c-mu): value*weight per us of exec; promoted first; rotating tie.
            key = st.value() * w // e50 if eligible else 0
            # Third-party jobs are always queued last on a hot thread, so a stalling third-party job
            # never delays another plugin's job (queue-position rule; aging only within the class).
            return (1 if st.tier == "third" else 0, 0 if promoted else 1, -st.denied if promoted else 0, -key,
                    (pid + self.turn) % max(1, n), pid)

        order = sorted(triggered, key=index)
        free = [0] * k_hot      # predicted queue end with e90 (placement)
        free99 = [0] * k_hot    # conservative queue end with e99 (deadlines)
        hot_used = {t: 0 for t in TIER_SPIN_CAP}
        assign: list[Assignment] = []
        for pid in order:
            st = self.s[pid]
            w_i = pid in warm
            e90 = self._exec_q(st, Q * 9 // 10, w_i, 64)
            e99 = self._exec_q(st, Q * 99 // 100, w_i, INF_US)
            hot = None
            off = 0
            if per_thread:
                if pid in armed:
                    hot, off, off99 = armed.index(pid), 0, 0
            elif k_hot and LABELS[st.label][3] and hot_used[st.tier] < TIER_SPIN_CAP[st.tier]:
                k = min(range(k_hot), key=lambda j: (free[j], j))
                if free[k] + hot_q90 <= cold_q90:
                    hot, off = k, free[k]
                    off99 = free99[k]
                    free[k] += e90
                    free99[k] += min(e99, p.cap_fuel_us)
                    hot_used[st.tier] += 1
            if hot is None:
                d = cold_q99 + e99 + p.margin_us
            else:
                d = off99 + hot_q99 + e99 + p.margin_us
            if LABELS[st.label][3] and LABELS[st.label][2] and st.excluded == 0:
                st.denied = 0 if hot is not None or not k_hot else st.denied + 1
            cap_t = min(dw, TIER_WAIT_CAP_US[st.tier])
            assign.append(Assignment(pid, hot, off, min(d, cap_t), 0, e99 + p.margin_us, d > cap_t))

        # Writer deadline t*: maximise sum_j v_j F_j(t) - lam * t over the grid (bucket edges <= dw, dw).
        elig = [a for a in assign if LABELS[self.s[a.pid].label][2] and self.s[a.pid].excluded == 0
                and not (a.clipped and self.s[a.pid].tier == "third")]
        grid = sorted({e for e in EDGES if 0 < e <= dw} | {dw} | {a.deadline_us for a in elig})
        best_t, best_obj = 0, 0
        for t in grid:
            obj = -p.lam_q16 * t
            for a in elig:
                st = self.s[a.pid]
                state = "cold" if a.hot is None else "hot"
                # a waited plugin holds the writer only until its own d_i (see Plan doc)
                if st.exec_h.n < p.warmup:  # no data yet: the label prior says "it fits by d_i"
                    f = Q if t >= a.deadline_us else 0
                else:
                    f = self._f_state(st, state, min(t, a.deadline_us), a.offset_us, a.pid in warm)
                obj += st.value() * f // Q
            if obj > best_obj:
                best_t, best_obj = t, obj
        wait = []
        for a in elig:
            st = self.s[a.pid]
            h = min(best_t, a.deadline_us)
            if st.exec_h.n < p.warmup:
                f = Q
            else:
                f = self._f_state(st, "cold" if a.hot is None else "hot", h, a.offset_us, a.pid in warm)
            if h > 0 and st.value() * f // Q >= p.lam_q16 * h:  # worth its own hold
                a.hold_us = h
                wait.append(a.pid)
        best_t = max([a.hold_us for a in assign] + [0])

        # (c) weighted progressive filling per hot thread; parked threads get the per-call cap.
        cap = p.cap_fuel_us * p.kappa
        for k in range(k_hot):
            on_k = sorted([a for a in assign if a.hot == k], key=lambda a: (a.offset_us, a.pid))
            pool = max(0, dw - hot_q50) * p.kappa
            wsum = sum(LABELS[self.s[a.pid].label][4] for a in on_k)
            for a in on_k:
                w = LABELS[self.s[a.pid].label][4]
                g = min(cap, pool * w // max(1, wsum))
                a.grant_fuel = g
                wsum -= w
                pool -= g  # unused fuel returns to `pool` in the runtime (here: prediction only)
        for a in assign:
            if a.hot is None:
                a.grant_fuel = cap
        return Plan(assign, wait, best_t)

    # ---------- observations ----------
    def observe_context(self, ctx, triggered: set[int]) -> None:
        for st in self.s.values():
            x = Q if st.pid in triggered else 0
            st.act[ctx] = ewma_q16(st.alpha(ctx), x, self.p.act_shift)

    def observe_reply(self, pid: int, state: str, start_us: int, exec_us: int, resp_us: int,
                      a: Assignment, waited: bool, useful: bool, warm: bool = False) -> None:
        """start_us = thread start latency (queue offset excluded), resp_us = from send to reply.
        warm: the call followed this plugin's warm hook (exec goes to the warm histogram)."""
        st = self.s[pid]
        deadline_us = a.deadline_us
        self.start_h[state].add(start_us)
        (st.exec_hw if warm else st.exec_h).add(exec_us)
        if warm and st.exec_hw.n >= self.p.warmup and st.exec_h.n >= self.p.warmup:
            d = max(0, st.exec_h.quantile(Q // 2) - st.exec_hw.quantile(Q // 2))
            self.warm_delta += (d - self.warm_delta) >> 3  # pooled over plugins, integer EWMA
        st.useful_q16 = ewma_q16(st.useful_q16, Q if useful else 0, self.p.val_shift)
        late = resp_us > (a.hold_us if waited else deadline_us)
        own_fault = exec_us > a.exec_allow_us and not a.clipped
        if st.excluded:
            st.excluded -= 1
        if late and waited and own_fault:
            st.offences += 1
            st.offence_total += 1
            st.excluded = min(self.p.excl_max, 1 << min(st.offences, 30))
            st.ontime_run = 0
        elif not late:
            st.ontime_run += 1
            if st.ontime_run >= self.p.offence_r and st.offences:
                st.offences -= 1
                st.ontime_run = 0


# ---------------------------------------------------------------- self-test / test vectors
def _selftest() -> None:
    h = Hist()
    for x in [3, 5, 9, 9, 20, 40, 70, 130, 300, 2000]:
        h.add(x)
    assert h.total() == 160
    assert h.quantile(Q // 2) == 24, h.quantile(Q // 2)       # 5th sample (20) -> bucket [16,24)
    assert h.quantile(Q * 99 // 100) == INF_US
    assert h.cdf_at(12) == 4 * Q // 10                        # 3,5,9,9 below 12
    assert ewma_q16(Q, 0, 5) == Q - (Q >> 5)
    # decay: 64 samples halve
    h2 = Hist()
    for _ in range(64):
        h2.add(10)
    assert h2.total() == 64 * 16 // 2

    s = Scheduler([(0, "interactive", "first"), (1, "default", "third"), (2, "refinement", "first")])
    A = Assignment
    for i in range(200):
        s.observe_reply(0, "hot", 2, 40, 42, A(0, 0, 0, 120, 0, 64), True, True)
        s.observe_reply(0, "cold", 200, 40, 240, A(0, None, 0, 290, 0, 64), False, True)
        s.observe_reply(1, "hot", 2, 10, 12, A(1, 0, 0, 60, 0, 24), True, False)
        s.observe_reply(2, "cold", 200, 150, 350, A(2, None, 0, 290, 0, 200), False, True)
    for _ in range(50):
        s.observe_context("tab", {0, 1, 2})
    k = s.keydown("tab", 1000)
    plan = s.dispatch([0, 1, 2], k)
    assert k >= 1
    assert 2 not in plan.wait, "refinement never waited"
    assert 0 in plan.wait
    assert plan.t_star <= 300 - 8 - 6
    vec = (k, [(a.pid, a.hot, a.offset_us, a.deadline_us, a.grant_fuel, a.hold_us) for a in plan.assign],
           plan.wait, plan.t_star)
    # Golden vector (port must reproduce bit-exactly).
    GOLD = (1, [(0, 0, 0, 60, 4086932, 60), (2, None, 0, 286, 4347800, 0), (1, 0, 48, 72, 2043466, 0)], [0], 60)
    assert vec == GOLD, vec

    # Offence escalation: a waited late reply excludes 2^m activations, capped.
    st = s.s[1]
    late = A(1, 0, 0, 60, 0, 24)
    s.observe_reply(1, "hot", 2, 290, 292, late, True, False)
    assert st.offences == 1 and st.excluded == 2
    s.observe_reply(1, "hot", 2, 290, 292, late, True, False)
    assert st.offences == 2 and st.excluded == 4
    # late because of the host (exec within allowance) or clipped deadline: no offence
    s.observe_reply(1, "cold", 280, 10, 292, late, True, False)
    s.observe_reply(1, "hot", 2, 290, 292, A(1, 0, 0, 60, 0, 24, True), True, False)
    assert st.offences == 2
    for _ in range(4):
        s.observe_reply(1, "hot", 2, 10, 12, late, False, False)
    assert st.excluded == 0
    print("sps.py self-test OK", vec)
    _selftest_warm()


def _selftest_warm() -> None:
    """Warm action (plugin-warm-hook-model.md): selection, caps, warm-aware dispatch; golden vector."""
    A = Assignment
    s = Scheduler([(0, "interactive", "first"), (1, "default", "first"), (2, "refinement", "first"),
                   (3, "interactive", "third"), (4, "interactive", "third")])
    for i in range(100):
        s.observe_reply(0, "cold", 120, 140, 260, A(0, None, 0, 290, 0, 400), False, True)
        s.observe_reply(0, "cold", 15, 40, 55, A(0, None, 0, 290, 0, 400), False, True, warm=True)
        s.observe_reply(1, "cold", 120, 20, 140, A(1, None, 0, 290, 0, 400), False, True)
        s.observe_reply(2, "cold", 120, 150, 270, A(2, None, 0, 290, 0, 400), False, True)
        for pid in (3, 4):
            s.observe_reply(pid, "cold", 120, 150, 270, A(pid, None, 0, 290, 0, 400), False, True)
        s.observe_reply(0, "hot", 2, 40, 42, A(0, 0, 0, 120, 0, 64), True, True)
    for _ in range(50):
        s.observe_context("nav", {0, 1, 2, 3, 4})
    k, armed, hooks = s.keydown_plan("nav", 10_000, k_force=0)
    assert k == 0 and armed == []
    assert 2 not in hooks, "refinement is never hooked"
    assert 1 not in hooks, "a plugin that fits cold gains nothing from a hook"
    assert 0 in hooks
    assert sum(1 for h in hooks if s.s[h].tier == "third") <= 1, "third-party hook cap"
    # frequency cap: the same plugins cannot be hooked again within warm_gap_ms
    _, _, again = s.keydown_plan("nav", 10_005, k_force=0)
    assert not set(again) & set(hooks)
    plan = s.dispatch([0, 1, 2], 0, armed=[], warm=frozenset(hooks))
    vec = (k, armed, hooks, [(a.pid, a.hot, a.deadline_us, a.hold_us) for a in plan.assign], plan.wait,
           plan.t_star, s.warm_delta, s.warm_bucket)
    GOLD = (0, [], [0, 3], [(0, None, 184, 184), (1, None, 160, 160), (2, None, 286, 0)], [0, 1], 184, 137, 59700)
    assert vec == GOLD, vec
    print("sps.py warm self-test OK", vec)


if __name__ == "__main__":
    _selftest()
