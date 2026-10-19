#!/usr/bin/env python3
"""Navigation over virtualized / recycled message lists (research prototype, see
docs/reports/virtual-list-navigation-math.md).

Four studies, one command each:

  browse    Browse-mode mirror over a virtualized list: per-arrow outcome classes (OK, stale,
            jump, dead) for the current cursor rule (A0) and two alternatives (A1 identity
            cursor + list-scoped resync, A2 = A1 + edge prefetch + bounded wait at the edge).
  focus     Focus-following mode: key -> speech latency from app focus latency and AT-SPI fetch
            cost; failure modes (focus lost on re-render, focus parked on the container,
            relabel of the focused node without a focus event).
  detect    Virtual-list detector: false-positive / false-negative rates against a population
            of list containers (virtual and not), as a function of observed scroll episodes.
  identity  Message identity across recycling: twin rates for several identity keys on a
            synthetic chat, and ambiguity of the window alignment (sequence) key.
  selftest  Deterministic checks of the model (test vectors).

All inputs are synthetic; parameters marked ASSUMED in the report are not measured on Signal,
Telegram Web or Slack.

Usage: python tools/research/virtual_list_nav.py {browse,focus,detect,identity,selftest,all}
       [--runs N] [--seed S]
"""
from __future__ import annotations

import argparse
import heapq
import math
import random
from collections import Counter, defaultdict

# --------------------------------------------------------------------------------------------
# 1. Browse-mode mirror over a virtualized list
# --------------------------------------------------------------------------------------------

KEYED, INVERTED, POOL = "keyed", "inverted", "pool"
DEBOUNCE = 0.150          # gorca SCROLL_DEBOUNCE (crates/gorca-core/src/browse/commands.rs:77)
EV_LAG_MED = 0.020        # event -> mirror visible (event + fetch), median, ASSUMED
RESYNC_COST = 0.006       # list-scoped GetTree of W rows x ~10 nodes, ASSUMED
RENDER_MED = 0.030        # app scroll/arrival -> DOM + AX tree updated, median, ASSUMED
WAIT_EDGE = 0.250         # A2: max wait for the window to extend before saying "loading"
PREFETCH_E = 2            # A2: prefetch when the cursor is within E rows of the mirror edge


def lognorm(rng, med, sigma):
    return med * math.exp(rng.gauss(0.0, sigma))


class Mirror:
    """Per-key DOM history plus visibility in the reader's mirror.

    A DOM change (t_dom, value) becomes visible in the mirror at t_vis (event lag, or never for a
    silent change until a resync). A resync captured at r_cap and visible at r_vis exposes the DOM
    state of every key as of r_cap. The mirror value at t is the DOM value with the latest t_dom
    among the visible ones."""

    def __init__(self, initial: dict[int, int]):
        self.hist: dict[int, list] = defaultdict(list)  # key -> [(t_dom, t_vis, value)]
        for k, v in initial.items():
            self.hist[k].append((-1.0, -1.0, v))
        self.resyncs: list[tuple[float, float]] = []    # (r_cap, r_vis), r_vis increasing

    def change(self, key, t_dom, t_vis, value):
        self.hist[key].append((t_dom, t_vis, value))

    def dom(self, key, t):
        v = None
        for td, _, val in self.hist.get(key, ()):
            if td <= t:
                v = val
        return v

    def view(self, key, t):
        cap = -2.0
        for rc, rv in self.resyncs:
            if rv <= t and rc > cap:
                cap = rc
        best_t, best_v = -3.0, None
        for td, tv, val in self.hist.get(key, ()):
            if (tv <= t or td <= cap) and td > best_t:
                best_t, best_v = td, val
        return best_v

    def keys(self):
        return list(self.hist.keys())


class ListWorld:
    """Logical list 0..N-1 (0 = oldest), viewport of V rows, overscan o on each side."""

    def __init__(self, typ, W, V, N0, lines, rng, p_ev, silent_heal, policy):
        self.typ, self.W, self.V = typ, W, V
        self.o = (W - V) // 2
        self.N = N0
        self.vlo = N0 - V
        self.lines = lines          # list of line counts per message (grows with arrivals)
        self.rng = rng
        self.p_ev = p_ev
        self.silent_heal = silent_heal   # A0: mean time until an unrelated repair heals silence
        self.policy = policy
        self.mirror = Mirror(self.dom_map())

    def window(self):
        lo = max(0, self.vlo - self.o)
        hi = min(self.N, self.vlo + self.V + self.o)
        return range(lo, hi)

    def key_of(self, i):
        if self.typ == KEYED:
            return i
        if self.typ == INVERTED:
            return self.N - 1 - i
        return i % self.W

    def dom_map(self):
        return {self.key_of(i): i for i in self.window()}

    def order_key(self, key):
        # DOM order of row nodes: keyed and inverted lists keep visual order; a slot pool keeps
        # slot order (rows are positioned by CSS transforms), the worst case.
        if self.typ == INVERTED:
            return -key
        return key

    def apply(self, t, mutate):
        """Run a DOM mutation at time t and log the per-key changes into the mirror."""
        before = self.dom_map()
        mutate()
        after = self.dom_map()
        evented_any = False
        for k in set(before) | set(after):
            a, b = before.get(k), after.get(k)
            if a == b:
                continue
            structural = a is None or b is None
            if structural or self.rng.random() < self.p_ev:
                tv = t + lognorm(self.rng, EV_LAG_MED, 0.5)
                evented_any = True
            elif self.policy.startswith("A0"):
                tv = t + self.rng.expovariate(1.0 / self.silent_heal)
            else:
                tv = math.inf   # healed only by a list resync
            self.mirror.change(k, t, tv, b)
        if evented_any and not self.policy.startswith("A0"):
            # A1/A2: any evented structural change in the container -> list-scoped resync.
            cap = t + lognorm(self.rng, EV_LAG_MED, 0.5)
            self.mirror.resyncs.append((cap, cap + RESYNC_COST))
        return evented_any

    def own_scroll_resync(self, t):
        # A1/A2: after our own ScrollIntoView settles we expect the list to change: resync.
        cap = t + RENDER_MED
        self.mirror.resyncs.append((cap, cap + RESYNC_COST))

    def scroll_to(self, msg):
        """ScrollIntoView of message msg: minimal viewport shift so that msg is visible."""
        if msg is None or not (0 <= msg < self.N):
            return
        if msg < self.vlo:
            self.vlo = msg
        elif msg >= self.vlo + self.V:
            self.vlo = msg - self.V + 1

    def arrive(self):
        at_bottom = self.vlo + self.V >= self.N
        self.N += 1
        self.lines.append(line_count(self.rng))
        if at_bottom:
            self.vlo += 1

    def rows(self, t):
        """Mirror rows in DOM order: [(key, believed msg)]."""
        out = []
        for k in self.mirror.keys():
            v = self.mirror.view(k, t)
            if v is not None:
                out.append((self.order_key(k), k, v))
        out.sort()
        return [(k, v) for _, k, v in out]


def line_count(rng):
    r = rng.random()
    return 1 if r < 0.5 else (2 if r < 0.8 else 3)


def simulate_browse(typ, W, tau_med, lam, p_ev, policy, rng, arrows=100, V=10, N0=400,
                    silent_heal=5.0, direction=-1):
    """One reading session of `arrows` line moves (direction -1 = upwards through history).

    Returns Counter of outcomes and the summed extra latency of A2 waits."""
    lines = [line_count(rng) for _ in range(N0)]
    w = ListWorld(typ, W, V, N0, lines, rng, p_ev, silent_heal, policy)
    if direction > 0:
        # Reading downwards from 60 messages above the bottom towards new arrivals.
        w.vlo = N0 - 62
        w.mirror = Mirror(w.dom_map())
    out = Counter()
    extra_wait = 0.0
    # The cursor starts on the last line of the newest message.
    start = N0 - 1 if direction < 0 else N0 - 60
    cur_key, cur_msg = w.key_of(start), start
    cur_line = lines[start] - 1 if direction < 0 else 0
    last_ordinal = None
    expected_prev = (start, cur_line)   # what the user last heard
    spoken_sig = (cur_key, cur_msg)
    ev: list = []
    seq = 0

    def push(t, kind, data=None):
        nonlocal seq
        seq += 1
        heapq.heappush(ev, (t, seq, kind, data))

    t = 0.0
    for _ in range(arrows):
        t += lognorm(rng, tau_med, 0.5)
        push(t, "key")
    if lam > 0:
        ta = 0.0
        while True:
            ta += rng.expovariate(lam)
            if ta > t + 1.0:
                break
            push(ta, "arrive")
    pending_scroll = None   # sequence id of the debounced scroll that is still current
    last_fire = -1.0        # A0T throttle state
    trailing_pending = False
    keys_done = 0
    while ev and keys_done < arrows:
        now, sid, kind, data = heapq.heappop(ev)
        if kind == "arrive":
            w.apply(now + lognorm(rng, RENDER_MED, 0.6), w.arrive)
            continue
        if kind == "scroll_fire":
            if data[0] != pending_scroll and data[1] == "debounced":
                continue
            target = data[2]
            if data[1] == "throttled":
                last_fire = now
                trailing_pending = False
                target = cur_key   # whatever the cursor is on when the throttle fires
            if policy.startswith("A0"):
                target = w.mirror.dom(target, now)   # the node, whatever it shows now
            tr = now + lognorm(rng, RENDER_MED, 0.6)
            w.apply(tr, lambda m=target: w.scroll_to(m))
            if not policy.startswith("A0"):
                w.own_scroll_resync(tr)
            continue
        # kind == "key"
        keys_done += 1
        rows = w.rows(now)
        if policy.startswith("A0"):
            res = move_a0(rows, w, cur_key, cur_line, last_ordinal, direction)
        else:
            res = move_identity(rows, w, cur_msg, cur_line, direction)
        if res is None and policy == "A2" and edge_has_more(w, now, direction):
            # Ask for the window to extend and wait (bounded) for the mirror to show the row.
            edge = min(m for _, m in rows) if direction < 0 else max(m for _, m in rows)
            tr = now + lognorm(rng, RENDER_MED, 0.6)
            w.apply(tr, lambda m=edge: w.scroll_to(m))
            w.own_scroll_resync(tr)
            ready = tr + RENDER_MED + RESYNC_COST + lognorm(rng, EV_LAG_MED, 0.5)
            if ready - now <= WAIT_EDGE:
                extra_wait += ready - now
                rows2 = w.rows(ready)
                res = move_identity(rows2, w, cur_msg, cur_line, direction)
                now_eval = ready
            else:
                extra_wait += WAIT_EDGE
                now_eval = now + WAIT_EDGE
        else:
            now_eval = now
        # Displacement since the last speech (what the current rule flags): node detached or
        # its content changed.
        if policy.startswith("A0"):
            cur_now = w.mirror.view(cur_key, now)
            if cur_now is None or cur_now != spoken_sig[1]:
                out["displaced_flag"] += 1
        if res is None:
            out["dead"] += 1
            out["dead_true"] += int(edge_has_more(w, now_eval, direction))
            continue
        key, believed, line, ordinal = res
        true = w.mirror.dom(key, now_eval)
        em, el = expected_prev
        exp = (em, el - 1) if el > 0 else ((em - 1, lines_of(w, em - 1) - 1) if direction < 0
                                            else None)
        if direction > 0:
            exp = (em, el + 1) if el + 1 < lines_of(w, em) else (em + 1, 0)
        stale = believed != true
        ok = (believed, line) == exp
        if ok and not stale:
            out["ok"] += 1
        elif ok and stale:
            out["stale_ok"] += 1
        elif stale:
            out["stale_jump"] += 1
        else:
            out["jump"] += 1
        if not ok:
            out["skipped_msgs"] += abs(believed - exp[0])
        cur_key, cur_msg, cur_line, last_ordinal = key, believed, line, ordinal
        expected_prev = (believed, line)
        spoken_sig = (key, believed)
        if policy == "A0T":
            # Throttle instead of a trailing debounce: fire at once if none fired in the last
            # DEBOUNCE, else one trailing fire at last_fire + DEBOUNCE (on the cursor then).
            if now_eval - last_fire >= DEBOUNCE:
                last_fire = now_eval
                push(now_eval, "scroll_fire", (sid, "prefetch", key))
            elif not trailing_pending:
                trailing_pending = True
                push(last_fire + DEBOUNCE, "scroll_fire", (sid, "throttled", None))
        else:
            # Scroll the cursor into view (debounced), as gorca does.
            push(now_eval + DEBOUNCE, "scroll_fire", (sid, "debounced",
                                                      key if policy == "A0" else believed))
            pending_scroll = sid
        if policy == "A2":
            idx = ordinal
            near = idx < PREFETCH_E if direction < 0 else idx >= len(rows) - PREFETCH_E
            if near and rows:
                edge = min(m for _, m in rows) if direction < 0 else max(m for _, m in rows)
                push(now_eval, "scroll_fire", (sid, "prefetch", edge))
    return out, extra_wait


def lines_of(w, m):
    if 0 <= m < len(w.lines):
        return w.lines[m]
    return 1


def edge_has_more(w, t, direction):
    lo = min(w.window())
    hi = max(w.window())
    return lo > 0 if direction < 0 else hi < w.N - 1


def move_a0(rows, w, cur_key, cur_line, last_ordinal, direction):
    """Current rule: the cursor is a node (key) plus a line; a detached node relocates to the
    same offset (approximated by the same row ordinal); then one line in `direction`."""
    if not rows:
        return None
    pos = next((i for i, (k, _) in enumerate(rows) if k == cur_key), None)
    if pos is None:
        pos = min(max(last_ordinal or 0, 0), len(rows) - 1)
    k, m = rows[pos]
    line = min(cur_line, lines_of(w, m) - 1)
    if direction < 0:
        if line > 0:
            return k, m, line - 1, pos
        if pos == 0:
            return None
        k2, m2 = rows[pos - 1]
        return k2, m2, lines_of(w, m2) - 1, pos - 1
    if line + 1 < lines_of(w, m):
        return k, m, line + 1, pos
    if pos + 1 >= len(rows):
        return None
    k2, m2 = rows[pos + 1]
    return k2, m2, 0, pos + 1


def move_identity(rows, w, cur_msg, cur_line, direction):
    """A1/A2: the cursor is a message identity plus a line; order by the identity's order key
    (timestamp / posinset), not by DOM order."""
    if not rows:
        return None
    by_msg = sorted(rows, key=lambda r: r[1])
    msgs = [m for _, m in by_msg]
    if cur_msg in msgs:
        i = msgs.index(cur_msg)
        k, m = by_msg[i]
        line = min(cur_line, lines_of(w, m) - 1)
        if direction < 0 and line > 0:
            return k, m, line - 1, i
        if direction > 0 and line + 1 < lines_of(w, m):
            return k, m, line + 1, i
        j = i + direction
    else:
        # The message scrolled out of the mirror: nearest one in the direction by order key.
        if direction < 0:
            cand = [i for i, m in enumerate(msgs) if m < cur_msg]
            j = cand[-1] if cand else -1
        else:
            cand = [i for i, m in enumerate(msgs) if m > cur_msg]
            j = cand[0] if cand else len(msgs)
    if not (0 <= j < len(msgs)):
        return None
    k, m = by_msg[j]
    return k, m, (lines_of(w, m) - 1 if direction < 0 else 0), j


def run_browse(runs, seed):
    print("## browse: outcomes per 100 arrows (reading upwards through history)")
    print("typ      W  tau   lam   p_ev dir pol |   ok stale_ok jump stale_jump dead | disp skip  wait_ms")
    grid = []
    for typ in (KEYED, INVERTED, POOL):
        for W in (20, 40):
            for tau in (0.04, 0.25, 0.6):
                for lam in (0.0, 0.05, 0.5):
                    grid.append((typ, W, tau, lam, 0.9, -1))
    for typ in (KEYED, INVERTED, POOL):
        for p_ev in (0.0, 0.5):
            grid.append((typ, 40, 0.25, 0.5, p_ev, -1))
        for lam in (0.05, 0.5):
            grid.append((typ, 40, 0.6, lam, 0.9, +1))
    results = {}
    for typ, W, tau, lam, p_ev, dirn in grid:
        for pol in (("A0", "A0T", "A1", "A2") if tau < 0.1 else ("A0", "A1", "A2")):
            tot = Counter()
            wait = 0.0
            for r in range(runs):
                rng = random.Random(f"{seed}/{typ}/{W}/{tau}/{lam}/{p_ev}/{dirn}/{r}")
                c, wt = simulate_browse(typ, W, tau, lam, p_ev, pol, rng, direction=dirn,
                                        N0=400 if dirn < 0 else 460)
                tot.update(c)
                wait += wt
            n = runs  # 100 arrows per run -> per-100 = total / runs
            row = {k: tot[k] / n for k in ("ok", "stale_ok", "jump", "stale_jump", "dead",
                                           "displaced_flag", "skipped_msgs")}
            row["wait_ms"] = 1000 * wait / (n * 100)
            results[(typ, W, tau, lam, p_ev, dirn, pol)] = row
            print(f"{typ:8s} {W:2d} {tau:4.2f} {lam:4.2f} {p_ev:4.1f} {'up' if dirn < 0 else 'dn'} {pol}  | "
                  f"{row['ok']:5.1f} {row['stale_ok']:6.1f} {row['jump']:6.1f} "
                  f"{row['stale_jump']:7.1f} {row['dead']:5.1f} | {row['displaced_flag']:4.1f} "
                  f"{row['skipped_msgs']:5.1f} {row['wait_ms']:6.1f}")
    return results


# Analytic companion: probability that the cursor's node is replaced/relabelled between two
# keystrokes. Rates: scroll shift s rows/s (window moves), arrivals lam/s.
def p_displaced(typ, W, s, lam, tau, at_bottom):
    """Closed form, see report section 2.1. tau = inter-key time."""
    if typ == KEYED:
        # Only a shift that pushes the cursor's row out of the window; the cursor sits in the
        # viewport, distance to the far window edge >= o = (W-V)/2 rows; with a shift rate s the
        # probability within tau is P(s*tau >= o) for a deterministic drift, 0 otherwise.
        V = 10
        o = (W - V) / 2
        shift = s * tau + (lam * tau if at_bottom else 0.0)
        return 1.0 if shift >= o + V / 2 else 0.0
    if typ == INVERTED:
        return 1.0 - math.exp(-(lam + s) * tau)   # every arrival and every shift relabels
    # POOL: any shift of the window by d rows relabels the row unless d is a multiple of W.
    return 1.0 - math.exp(-(s + (lam if at_bottom else 0.0)) * tau)


# --------------------------------------------------------------------------------------------
# 2. Focus-following mode: latency and failure modes
# --------------------------------------------------------------------------------------------

APPS = {
    # name: (app key->focus median s, sigma, tail prob, tail median s)   ASSUMED
    "signal": (0.022, 0.5, 0.05, 0.090),
    "slack": (0.030, 0.6, 0.08, 0.120),
    "telegram-web": (0.015, 0.5, 0.04, 0.060),
}
ATSPI_CALL = (0.00030, 0.6)   # median s, sigma per call on an idle Chrome UI thread (ASSUMED)
ATSPI_BUSY = (0.05, 0.004)    # prob a call queues behind a busy UI thread, median extra s
GORCA_PROC = (0.0008, 0.4)    # gorca-side processing per focus event (from B-1 class numbers)
BUS_HOP = (0.0003, 0.4)       # ATK event -> bridge -> a11y bus -> gorca


def sample_latency(rng, app, fetch):
    med, sig, tp, tmed = APPS[app]
    lat = lognorm(rng, med, sig)
    if rng.random() < tp:
        lat += lognorm(rng, tmed, 0.5)
    lat += lognorm(rng, *BUS_HOP)
    calls = {"cached": 0, "obj3": 3, "row_tree": 3 + 12, "row_gettree": 4}[fetch]
    for _ in range(calls):
        c = lognorm(rng, *ATSPI_CALL)
        if rng.random() < ATSPI_BUSY[0]:
            c += lognorm(rng, ATSPI_BUSY[1], 0.7)
        lat += c
    gorca = lognorm(rng, *GORCA_PROC)
    return lat + gorca, gorca


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p / 100.0 * len(xs)))]


def run_focus(runs, seed):
    rng = random.Random(seed)
    print("## focus: key -> speech start (ms), app focus latency + AT-SPI fetch + gorca")
    print("app           fetch        p50    p90    p99   gorca-only p99   browse p99 (mirror hit)")
    res = {}
    n = max(2000, runs * 200)
    for app in APPS:
        for fetch in ("cached", "obj3", "row_gettree", "row_tree"):
            xs, gs = [], []
            for _ in range(n):
                a, g = sample_latency(rng, app, fetch)
                xs.append(a * 1000)
                gs.append(g * 1000)
            br = [lognorm(rng, *GORCA_PROC) * 1000 for _ in range(n)]
            res[(app, fetch)] = (pct(xs, 50), pct(xs, 90), pct(xs, 99))
            print(f"{app:13s} {fetch:11s} {pct(xs,50):6.1f} {pct(xs,90):6.1f} {pct(xs,99):6.1f}"
                  f"   {pct(gs,99):6.2f}          {pct(br,99):5.2f}")
    # Failure modes per 100 arrows, by virtualizer, from a per-arrow model:
    #   lost:   the focused row is unmounted by a re-render (keyed: only when a window shift
    #           passes it; pool: never unmounted; inverted: the key of the focused DOM node
    #           changes meaning on every arrival, React keeps the element) -> focus to <body>.
    #   relabel: the focused node keeps focus but shows another message (pool/inverted) with no
    #           focus event -> silence unless name-change on the focus object is presented.
    #   container: the app has no item focus model (arrows scroll the container) -> silence.
    print("## focus: failure modes per 100 arrows (model, ASSUMED rates)")
    print("virtualizer  roving  lam  lost  relabel_silent(no name-change rule)  relabel_silent(with rule)  container")
    for typ in (KEYED, INVERTED, POOL):
        for roving in (True, False):
            for lam in (0.0, 0.5):
                tau = 0.6
                if not roving:
                    lost = relabel = relabel_r = 0.0
                    container = 100.0
                else:
                    container = 0.0
                    p_rr = 0.003     # ASSUMED: re-render remounts the focused keyed row
                    if typ == KEYED:
                        lost = 100 * p_rr
                        relabel = relabel_r = 0.0
                    elif typ == INVERTED:
                        p = 1 - math.exp(-lam * tau)
                        lost = 100 * p_rr
                        relabel = 100 * p
                        relabel_r = 100 * p * 0.1   # name-change seen but >1 relabels coalesce
                    else:
                        # the app scrolls by one row per arrow at the viewport edge: ~1/V of
                        # arrows shift the pool; the focused slot is relabelled when it is the
                        # slot that wraps (1/W of shifts) -- the app usually re-focuses.
                        lost = 100 * (0.1 * 0.3)
                        relabel = 100 * (0.1 / 4)
                        relabel_r = relabel * 0.1
                print(f"{typ:11s} {str(roving):6s} {lam:4.1f} {lost:5.2f} {relabel:10.2f}"
                      f"{'':27s}{relabel_r:6.2f}{'':21s}{container:6.1f}")
    return res


# --------------------------------------------------------------------------------------------
# 3. Virtual-list detector
# --------------------------------------------------------------------------------------------

CONTAINER_TYPES = {
    # name: (virtual?, prior weight, generator params)  ASSUMED population on chat/web apps
    "virt_keyed": (True, 0.10),
    "virt_pool": (True, 0.03),
    "virt_inverted": (True, 0.03),
    "static_list": (False, 0.45),
    "growing_log": (False, 0.12),
    "lazy_append": (False, 0.10),
    "paginated": (False, 0.07),
    "carousel": (False, 0.03),
    "filtered": (False, 0.07),
}


def gen_container(rng, typ):
    """Return (setsize_gap, posinset_gt1, role_feed, episodes) where episodes is a generator of
    per-scroll-episode observations (removed_far_edge, added_near_edge, relabelled, n_before,
    n_after, scrolled) as the mirror would see them after one ScrollIntoView or app scroll."""
    virtual = CONTAINER_TYPES[typ][0]
    n = rng.randint(20, 60) if virtual else rng.choice([rng.randint(3, 15), rng.randint(16, 200)])
    has_setsize = virtual and rng.random() < 0.35      # ASSUMED share of virtual lists with aria-setsize
    setsize_gap = has_setsize
    posinset_gt1 = has_setsize and rng.random() < 0.9
    if typ == "paginated" and rng.random() < 0.3:
        setsize_gap, posinset_gt1 = True, True        # "page 2 of 10" tables with aria-rowcount
    role_feed = virtual and rng.random() < 0.05

    def episode():
        d = rng.randint(1, 6)
        if typ == "virt_keyed":
            return (d, d, 0, n, n + rng.randint(-1, 1), True)
        if typ == "virt_pool":
            return (0, 0, n if rng.random() < 0.5 else d, n, n, True)
        if typ == "virt_inverted":
            return (d, d, rng.randint(n // 2, n), n, n, True)
        if typ == "static_list":
            return (0, 0, 0, n, n, rng.random() < 0.5)
        if typ == "growing_log":
            return (0, rng.randint(0, 2), 0, n, n + rng.randint(0, 2), True)
        if typ == "lazy_append":
            k = rng.randint(0, 20)
            return (0, k, 0, n, n + k, True)
        if typ == "paginated":
            # page switch replaces all rows once in a while (not on plain scroll)
            if rng.random() < 0.15:
                return (n, n, 0, n, n, False)
            return (0, 0, 0, n, n, True)
        if typ == "carousel":
            if rng.random() < 0.6:
                return (1, 1, 0, n, n, False)        # timer rotation, no scroll by us
            return (0, 0, 0, n, n, True)
        if typ == "filtered":
            m = rng.randint(0, n)
            return (rng.randint(0, n), rng.randint(0, n), 0, n, m, False)
        raise ValueError(typ)

    return setsize_gap, posinset_gt1, role_feed, episode


def detector_score(setsize_gap, posinset_gt1, role_feed, eps, require_scroll=True):
    """Rule set D (report section 4): virtual if a declared-size signal holds, or if at least
    two scroll episodes show turnover at a steady child count. Only episodes that follow a
    scroll (ours or a scroll event in the container) count."""
    if setsize_gap or posinset_gt1 or role_feed:
        return True, "declared"
    hits = 0
    for rem, add, rel, nb, na, scrolled in eps:
        if require_scroll and not scrolled:
            continue
        steady = abs(na - nb) <= max(1, 0.1 * nb)
        turnover = (rem + rel) / max(nb, 1)
        if steady and turnover >= 0.03 and (rem > 0 or rel > 0):
            hits += 1
    return hits >= 2, "churn"


def run_detect(runs, seed):
    rng = random.Random(seed)
    print("## detect: rule D, by number of observed scroll episodes")
    print("episodes  require_scroll  FPR(all non-virtual)  FNR(all virtual)  per-type positives")
    names = list(CONTAINER_TYPES)
    weights = [CONTAINER_TYPES[t][1] for t in names]
    n = max(4000, runs * 400)
    out = {}
    for k in (0, 1, 2, 3, 5, 10):
        for req in (True, False):
            pos = Counter()
            tot = Counter()
            fp = fn = nv = vv = 0
            for _ in range(n):
                typ = rng.choices(names, weights)[0]
                ss, pi, feed, ep = gen_container(rng, typ)
                eps = [ep() for _ in range(k)]
                v, _ = detector_score(ss, pi, feed, eps, req)
                tot[typ] += 1
                pos[typ] += v
                if CONTAINER_TYPES[typ][0]:
                    vv += 1
                    fn += not v
                else:
                    nv += 1
                    fp += v
            out[(k, req)] = (fp / nv, fn / vv)
            per = " ".join(f"{t}={pos[t]/max(tot[t],1):.2f}" for t in names)
            print(f"{k:8d}  {str(req):14s}  {fp/nv:20.4f}  {fn/vv:16.4f}  {per}")
    return out


# --------------------------------------------------------------------------------------------
# 4. Message identity across recycling
# --------------------------------------------------------------------------------------------

SHORT = ["ok", "thanks", "lol", "yes", "no", "sure", "👍", "haha", "good night", "on my way",
         "call me", "nice", "?", "!", "wow", "same", "ok!", "brb", "done", "❤️", "yep", "k",
         "😂", "omg", "ty", "np", "cool", "great", "see you", "hmm"]


def gen_chat(rng, M, authors, p_short, rate_per_min):
    """Messages (author, text, minute) in time order. Short texts Zipf over SHORT."""
    zipf = [1.0 / (i + 1) for i in range(len(SHORT))]
    aw = [1.0 / (i + 1) ** 0.7 for i in range(authors)]
    t = 0.0
    msgs = []
    a = 0
    for i in range(M):
        # bursts: 70 % of gaps are short (same conversation), 30 % long
        t += rng.expovariate(rate_per_min) if rng.random() < 0.7 else rng.expovariate(rate_per_min / 20)
        if rng.random() < 0.35:          # turn change
            a = rng.choices(range(authors), aw)[0]
        text = rng.choices(SHORT, zipf)[0] if rng.random() < p_short else f"long-{i}"
        msgs.append((a, text, int(t)))
    return msgs


def run_identity(runs, seed):
    rng = random.Random(seed)
    print("## identity: share of messages with a twin under each key (window W=40 / history M=1000)")
    print("chat                      K1 text      K2 +author   K3 +minute   K4 +prev/next K3   K5 align(K3)  K5t align(K1)")
    chats = {
        "group, distinct": (6, 0.10, 0.5),
        "two people, chatty": (2, 0.35, 2.0),
        "two people, ok/thanks": (2, 0.60, 4.0),
    }
    res = {}
    for name, (A, ps, rate) in chats.items():
        acc = defaultdict(list)
        for _ in range(max(40, runs)):
            msgs = gen_chat(rng, 1000, A, ps, rate)
            keys = {
                "K1": [m[1] for m in msgs],
                "K2": [(m[0], m[1]) for m in msgs],
                "K3": [m for m in msgs],
            }
            k3 = keys["K3"]
            keys["K4"] = [(k3[i - 1] if i else None, k3[i], k3[i + 1] if i + 1 < len(k3) else None)
                          for i in range(len(k3))]
            for kn, ks in keys.items():
                for scope, Wn in (("W", 40), ("M", 1000)):
                    lo = 1000 - Wn
                    c = Counter(ks[lo:])
                    acc[(kn, scope)].append(sum(1 for k in ks[lo:] if c[k] > 1) / Wn)
            # K5: window alignment. Old window [lo, lo+40) and new window shifted by d in
            # [-10, 10]; the shift is ambiguous if two shifts explain the overlap equally well.
            # Evaluated with the strong per-item key (K3) and with text only (K1).
            for kname, seqk in (("K5", k3), ("K5t", keys["K1"])):
                amb = 0
                trials = 200
                for _ in range(trials):
                    lo = rng.randint(100, 900)
                    d = rng.randint(-10, 10)
                    old = seqk[lo:lo + 40]
                    new = seqk[lo + d:lo + d + 40]
                    scores = []
                    for dd in range(-15, 16):
                        sc = 0
                        for j in range(40):
                            oj = j + dd
                            if 0 <= oj < 40 and old[oj] == new[j]:
                                sc += 1
                        scores.append(sc)
                    best = max(scores)
                    amb += sum(1 for sc in scores if sc == best) > 1
                acc[(kname, "W")].append(amb / trials)
                acc[(kname, "M")].append(float("nan"))
        row = {k: sum(v) / len(v) for k, v in acc.items()}
        res[name] = row
        print(f"{name:24s} " + "  ".join(
            f"{row[(k,'W')]:.3f}/{row[(k,'M')]:.3f}" if not k.startswith("K5") else f"{row[(k,'W')]:.4f}"
            for k in ("K1", "K2", "K3", "K4", "K5", "K5t")))
    # Closed form q = sum p^2 check for K1 on the ok/thanks chat.
    return res


# --------------------------------------------------------------------------------------------
# selftest: deterministic test vectors
# --------------------------------------------------------------------------------------------

def selftest():
    rng = random.Random(1)
    # TV-1: keyed list, no arrivals, slow typing, fresh mirror: every arrow is OK under all
    # policies (the window follows the cursor through ScrollIntoView).
    for pol in ("A0", "A1", "A2"):
        c, _ = simulate_browse(KEYED, 40, 1.0, 0.0, 1.0, pol, random.Random(7))
        assert c["ok"] >= 97, (pol, c)
    # TV-2: inverted list, arrivals 2/s, A0 jumps on most arrows, A1 does not.
    c0, _ = simulate_browse(INVERTED, 40, 0.6, 2.0, 1.0, "A0", random.Random(3))
    c1, _ = simulate_browse(INVERTED, 40, 0.6, 2.0, 1.0, "A1", random.Random(3))
    assert c0["jump"] + c0["stale_jump"] > 30, c0
    assert c1["ok"] + c1["stale_ok"] >= 95, c1
    # TV-3: pool list in slot DOM order: A0 hits a false top at slot 0 (or wraps) even when fresh.
    c0, _ = simulate_browse(POOL, 20, 1.0, 0.0, 1.0, "A0", random.Random(5))
    assert c0["jump"] + c0["dead_true"] > 0, c0
    # TV-4: mirror visibility semantics.
    m = Mirror({1: 10})
    m.change(1, 1.0, math.inf, 11)    # silent
    assert m.view(1, 5.0) == 10 and m.dom(1, 5.0) == 11
    m.resyncs.append((2.0, 2.01))
    assert m.view(1, 2.0) == 10 and m.view(1, 2.02) == 11
    # TV-5: detector: a static list with any number of episodes is never positive; a keyed
    # virtual list with two scroll episodes always is.
    eps = [(0, 0, 0, 30, 30, True)] * 5
    assert detector_score(False, False, False, eps)[0] is False
    eps = [(3, 3, 0, 30, 30, True)] * 2
    assert detector_score(False, False, False, eps)[0] is True
    # TV-6: closed forms
    assert abs(p_displaced(INVERTED, 40, 0.0, 1.0, 0.5, False) - (1 - math.exp(-0.5))) < 1e-12
    assert p_displaced(KEYED, 40, 0.0, 0.0, 0.5, False) == 0.0
    print("selftest ok")
    _ = rng


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["browse", "focus", "detect", "identity", "selftest", "all"])
    ap.add_argument("--runs", type=int, default=20)
    ap.add_argument("--seed", type=int, default=20261019)
    a = ap.parse_args()
    if a.cmd in ("selftest", "all"):
        selftest()
    if a.cmd in ("browse", "all"):
        run_browse(a.runs, a.seed)
    if a.cmd in ("focus", "all"):
        run_focus(a.runs, a.seed)
    if a.cmd in ("detect", "all"):
        run_detect(a.runs, a.seed)
    if a.cmd in ("identity", "all"):
        run_identity(a.runs, a.seed)


if __name__ == "__main__":
    main()
