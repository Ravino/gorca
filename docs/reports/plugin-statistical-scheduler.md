# A statistical scheduler for WASM plugins under B' (ADR-0026)

Date 2026-10-25, math researcher. Status: research (paper and simulation only, no production code).
Scripts: `tools/research/plugin_sched/` (`sps.py` reference scheduler with self-test and golden
vector, `sim.py` workload and host model, `ram.py` warm-instance retention, `run_all.py` all
experiments). Inputs read: ADR-0026 r4/r5 (§2.4.2, §2.4.6, §2.5), `plugin-fusion-math.md` (PF-1..PF-8,
MQ-2, MQ-3), `m18a-2-review.md` §2 (C-state probe), `docs/decisions.md` (B' confirmation,
`busy_skips = 72 594`).

Tags: **[P]** proven here, **[M]** measured in the simulation (synthetic, not on gorca), **[H]**
heuristic or assumption.

## 0. Answer

**Recommended: SPS, a deterministic, full-information scheduler.** It has three parts:

1. **Key-down pre-wake.** A per-context activation EWMA drives how many pool threads spin.
   Typing and editor arrows learn α ≈ 0, so they wake nothing. A CPU token bucket caps the
   spinning.
2. **Dispatch.** Jobs are packed onto the spinning threads by weighted-SPT list scheduling, with
   aging. Third-party jobs always go last on a queue.
3. **Writer wait.** Each plugin gets its own hold `h_i = min(d_i, t*)`. `d_i` is a union-bound
   99 % deadline: thread start q99 plus the plugin's exec q99. `t*` maximises expected facts minus
   λ·delay.

Labels set priors and constraints, and stats compute everything else.

Why bandits are not needed: the writer measures every dispatched job, whether it waited or not.
So the wait decision gets full-information feedback, and there is nothing to explore. The only
counterfactual is "hot vs parked start", and that is a host property, pooled over all plugins.

| Guarantee | Statement | Status |
|---|---|---|
| PF-8 | writer stall ≤ D = 300 µs from the turn start, whatever the plugins do | [P] |
| per-plugin harm | a plugin holds the writer at most `h_i ≤ d_i`; third party ≤ 100 µs | [P] |
| adversary | offences (exec above own q99, while waited) ≤ `N/(R+1) + N/E + log2 E + 1` per N activations | [P] |
| starvation | no job is ever dropped; exclusion ≤ E = 256 activations per offence; a denied hot place is promoted after 8 denials | [P] |
| determinism | integer state machine; replay of 3 000 keypresses: 0 mismatches | [P]+[M] |
| cost | O(m log m + m·G) per keypress, m ≤ 16, G ≤ 32: constant | [P] |

Simulation [M], 20 000 keypresses, 10 plugins, default C-states:

| | WA-park | WA-wake | LABELS | **SPS** |
|---|---|---|---|---|
| facts at first utterance (benign, waitable) | 0.956 | 1.000 | 1.000 | **0.997–0.999** |
| worst benign plugin | 0.906 | 1.000 | 1.000 | **0.992–0.996** |
| writer stall p50, honest / oracle adversary (µs) | 238 / 278 | 135 / 206 | 77 / 205 | **86 / 85** |
| writer stall p99 (µs) | 300 | 250–254 | 217–219 | 243–273 (233 with PM QoS) |
| scheduling CPU (% of one core at 5 keys/s) | 0.0 | 49.8 | 44.7 | **5.5–6.9** |
| adversary stalls the writer waited for | 0–7 495 | 0–7 495 | 0–7 495 | **0–117, each ≤ 100 µs** |

SPS keeps facts at first utterance within 0.3 % of pre-wake-all, at about 1/8 of the CPU. Against
the worst adversary, its median stall is 2.4× lower than LABELS. It pays for this with a higher p99
stall than LABELS (+25 to +55 µs): plugins that land on parked threads wait out the C-state tail.
More spinning does not fix this (k_mult 2 gives p99 279 µs, §5.3). Per-CPU PM QoS on the pool
CPUs does: p99 233 µs at 0.1 % of a core (§5.5).

## 1. Formulation

**Epochs.** Each keypress k has a context `x = (scope, key class)`. Key classes are nav (arrows
outside an editor), tab, enter, edit-arrow and type; the input layer already knows these. On key-down
(epoch K) the scheduler picks K_k, the number of pool threads to pre-wake. A focus change can then
follow, after `f` ≈ 1–20 ms. At that point the writer knows the triggered set `M ⊆ plugins`
(trigger rule §2.4.1, `|M| ≤ 16`). At dispatch (epoch D) the scheduler picks:
- a thread for each job: a hot queue with a predicted offset, or a parked thread;
- the wait set W and each plugin's hold `h_i`;
- fuel grants.

Replies are observations.

**Response model [H, from the probe].** Response = `snap + start(state) + queue + exec_i`.
- `start(hot)` ≈ 1–3 µs.
- `start(parked)` is the host's wake latency. With the default idle states (C2 127 µs, C3 1048 µs
  exit) the review measured 55/141/180 µs (p50/p99/max) for one wake, and 182 µs for the slowest of
  ten.
- `exec_i` depends on the plugin, and is assumed independent of the thread state.

This split matters. The start latency is shared by all plugins and the scheduler cannot control
it. Exec is the plugin's own doing, and so is the only thing a plugin can be blamed for (§4.3).

**Objective.** Maximise facts at first utterance (FFU), weighted by usefulness. FFU is the share of
triggered jobs whose reply arrives before the writer publishes. The constraints:
1. stall ≤ D (PF-8);
2. CPU spent spinning within a budget;
3. no starvation;
4. robust to a plugin that lies with its timing;
5. deterministic;
6. O(1) per keypress.

Within D the owner has already approved the delay, so λ (the price of 1 µs of stall) is small. It
only stops waiting for plugins whose expected value is near zero.

## 2. Survey: what transfers and what does not

| Family | Idea | Verdict for gorca |
|---|---|---|
| CFS / EEVDF (Linux ≥ 6.6) | weighted virtual runtime; EEVDF serves the *eligible* task (lag ≥ 0) with the earliest virtual deadline `ve + slice/w` | **Adapted, partly.** Weights come from the labels (interactive 2, other 1). EEVDF's per-request deadline becomes the per-plugin hold `d_i`. Its lag clamp, which keeps a sleeper from being starved or from hoarding credit, becomes bounded aging (§4.4). Virtual time itself is **rejected**: the resource is a per-event latency budget, not a throughput share (same argument as MQ-3 §4.4 against DRR and carry-over). |
| sched_ext | a BPF scheduler with a kernel watchdog that falls back to the default on stall | **Pattern adopted.** If the policy fails (stats corrupt, invariants broken), fall back to the LABELS policy and keep the hard D. As a host lever it is out of scope. |
| Lottery | randomised proportional share | **Rejected:** not deterministic, high variance over short windows. |
| Stride | deterministic proportional share; pass += L/tickets; O(1) error | **Not needed.** Exploration is unnecessary (full information), and per-turn shares are handled by progressive filling without carry-over. |
| LRU | evict the least recent | Baseline for warm instances; loses to frequency under scans (§5.6). |
| ARC | adaptive recency/frequency split with ghost lists | **Rejected:** with n ≤ 50 exact counters are cheap, and ARC's ghost lists add complexity with no measured gain. |
| TinyLFU / W-TinyLFU | frequency-sketch *admission*: a candidate replaces the victim only if it is more frequent | **Adopted for instance retention** with exact decayed counters, no sketch. Best hit rate in §5.6. |
| LFU with aging | exponentially decayed counts | **Adopted** for activation α (per context) and retention (per scope). |
| EWMA | `x ← x + (y − x)/2^s` | **Adopted** for α and usefulness u. **Rejected** for latency: a mean misleads on heavy tails. Deadlines need quantiles, hence the decayed log-histograms. |
| Multi-armed bandits, UCB, Thompson | explore–exploit under bandit feedback | **Not needed** (key finding). Every dispatched job is measured whether or not the writer waits, so the wait decision has full-information feedback. The only counterfactual, hot vs cold start, is pooled across plugins. UCB-style optimism is also exploitable: it trusts before evidence. Thompson needs a PRNG in the state and is harder to explain. SPS uses *pessimistic* CDFs instead. |
| cμ rule / Smith (WSPT) | order jobs by `c_i μ_i` = weight / expected size; optimal for weighted completion on one server | **Adopted** for queue order on a hot thread: key `v_i·w_i / e50_i`. |
| SRPT | shortest remaining first; optimal mean response, needs pre-emption and sizes | Not applicable: guest calls are not pre-empted (fuel only). WSPT is the non-preemptive analogue. |
| Gittins index | Bayes-optimal priority for unknown sizes on one server | **Rejected:** m ≤ 16 jobs on parallel threads (Gittins is optimal only on a single server), and the index is costly. The gain over WSPT is small at these sizes [H]. |
| Optimal stopping | when to stop waiting for stochastic arrivals | **Adopted:** `t* = argmax_t Σ v_j F_j(min(t, d_j)) − λt` over a grid, then per-plugin holds. |

## 3. The model (exact, as in `sps.py`)

### 3.1 State per (plugin, scope)

All quantities are integers (µs, Q16 fixed point with `Q = 65536`).

- `exec_h`: decayed log-histogram of exec times. 16 buckets with lower edges
  `0,4,8,12,16,24,32,48,64,96,128,192,256,384,512,1024`. Each sample adds 16; all counts halve
  every 64 samples (half-life 64 calls).
- `act[x]`: activation probability per context, `α ← α + ((1[i ∈ M] − α) >> 5)`. The initial value
  is the label prior.
- `useful`: `u ← u + ((1[facts changed the published entry] − u) >> 5)`. The value is
  `v_i = min(u_i, tier cap)`, with caps first 1, trusted 0.75, third 0.5.
- `offences m_i`, `excluded e_i`, `ontime_run`, `denied g_i`.

Global: `start_h[hot]` and `start_h[cold]`, pooled histograms of start latency, plus the spin token
bucket.

Primitives. `Q_h(p)` is the upper edge of the first bucket whose cumulative mass reaches p, so it
never under-estimates. `F_h(t)` is the mass of the buckets whose upper edge is ≤ t, so it never
over-estimates. The CDF of `start + exec` is a 16-term convolution:
`F_i^state(t, off) = Σ_b P_start^state(b) · F_exec_i(t − off − upper(b))`.
In the port, cache it per plugin and state after each reply.

### 3.2 Labels: priors and constraints

| Label | value prior | α prior | may be waited | may spin / take a hot place | weight |
|---|---|---|---|---|---|
| `interactive` | 1.0 | 0.5 | yes | yes | 2 |
| (none) | 0.5 | 0.25 | yes | yes | 1 |
| `refinement` | 0.25 | 0.125 | **never** | **never** | 1 |

Tier constraints override what a manifest claims about itself:
- third-party: `v ≤ 0.5`, at most one hot-queue place per keypress, `d_i ≤ 100 µs`, and never waited
  when its deadline is clipped;
- trusted: `v ≤ 0.75`;
- an owner override (`always-wait` or `never-wait`, first-party only) is a constraint that comes
  before the stats.

For warm-up: until a plugin has 8 replies, its label prior counts as "fits within its hold"
(`F = 1` at `d_i`).

### 3.3 (a) Key-down: how many threads to spin

`dw = D − snap − fuse` (286 µs with snap 6 and fuse 8). For every spin-eligible plugin:

```
gain_i  = max(0, F_i^hot(dw) − F_i^cold(dw))
score_i = α_i[x] · v_i · gain_i
S       = { i : score_i ≥ θ },  θ = 0.01
K       = min(|S|, K_max = 6, max(1, ⌈k_mult · Σ_{i∈S} α_i[x]⌉), ⌊bucket / spin_est⌋)
```

`k_mult` = 1. K = 0 if S is empty. `spin_est` = 3 ms is charged at key-down, and the actual cost
(spin until the focus change, or the 50 ms window, plus a 40 µs tail) is settled when the thread
parks. The bucket holds 200 ms and refills at 150 ms/s.

Typing and edit-arrow contexts learn α = 0 within about 30 keypresses and spin nothing. That alone
is most of the CPU gap to pre-wake-all.

### 3.4 (b) Dispatch: placement, deadlines, wait set

1. **Order.** Sort the triggered jobs by:
   - tier class (third-party last);
   - promoted first, where promoted means `g_i ≥ age_max = 8`, and promoted jobs by largest `g_i`;
   - WSPT key `v_i·w_i / e50_i`, descending;
   - a rotating tie-break `(id + turn) mod m`.
2. **Place**, using list scheduling over the K hot queues. Take the least-loaded queue `k`. Use it
   if the job is spin-eligible, the tier still has a place, and
   `free_k + Q_start^hot(0.9) ≤ Q_start^cold(0.9)`. Otherwise use a parked thread. Then
   `free_k += e90_i` and `free99_k += min(e99_i, 200)`.
3. **Deadline (union bound).**
   `d_i = off99_i + Q_start^state(0.99) + Q_exec_i(0.99) + 8 µs`, clipped to `min(dw, tier cap)`.
   Since `P(X+Y > a+b) ≤ P(X>a) + P(Y>b)`, a job meets `d_i` with probability ≥ 98 % when the
   estimates are right [P].
4. **Writer deadline.** `t* = argmax_{t ∈ G} Σ_{j∈E} v_j F_j(min(t, d_j), off_j) − λ t`, with
   `λ = 3/Q` per µs (one fact per ~22 ms). The grid G is the bucket edges ≤ dw, plus dw, plus every
   `d_j`. E is the set of eligible jobs: wait-eligible label, `e_j = 0`, and not (third-party and
   clipped).
5. **Holds.** `h_j = min(t*, d_j)`. `j ∈ W` iff `v_j F_j(h_j) ≥ λ h_j`. The writer ends at
   `max_{j∈W} min(reply_j, h_j)`, or right after the snapshot if W is empty. Plugins outside W still
   count when they arrive in time. **This replaces the shared countdown to D**: in r5 any plugin in
   the countdown could hold the writer to D, which is also what review gap 7 found for busy
   plugins.

### 3.5 (c) Fuel shares: weighted progressive filling per hot queue (an answer to MQ-4)

For hot queue k:
- pool `P_k = κ_int · (dw − Q_start^hot(0.5))`;
- in queue order, grant `a = min(cap, R · w_i / W_rem)`, where `R` is the remaining pool and
  `W_rem` the remaining weight;
- unused fuel returns to `R`.

A job on a parked thread runs alone and gets `cap` (200 µs × κ). Its effect on the writer is
bounded by its hold, not by its fuel.

MQ-3 Proposition B, with weights, holds per queue [P]:
- `Σ grants ≤ P_k`, because each grant is at most `R`;
- floor `a_i ≥ min(cap, P_k·w_i/W_k)` (same induction as MQ-3);
- so a hot queue's serial work fits in `dw` when κ is calibrated (ρ = 1).

This is the parallel grant rule MQ-4 asked for: "progressive filling per thread, cap per lone job".
It replaces the serial-equivalent rule that gave each of ten plugins about 23 µs of fuel (r5 gap 6).

### 3.6 Observations and offences

`observe_reply(pid, state, start, exec, resp, assignment, waited, useful)`:
- add `start` to `start_h[state]` (queue offset excluded) and `exec` to `exec_h`;
- update `u`;
- if `e > 0`, decrement it.

**Offence** iff all three hold:
1. waited and `resp > h_i`;
2. `exec > Q_exec_i(0.99) + 8` (the q99 stored at dispatch), so the plugin's own code was slow,
   not the host's wake;
3. the deadline was not clipped.

On an offence: `m += 1`, `e = min(E = 256, 2^m)`, `ontime_run = 0`. On every reply that is not
late, `ontime_run += 1`, and at `R = 32` set `m -= 1` and `ontime_run = 0`.

The on-time count includes jobs the writer did not wait for. Job messages must not reveal whether
the writer waits for them (normative: same `Derive` content, same deadline field).

### 3.7 Warm instances (RAM)

Keep every W instance of an open scope `Ready` while the total is within the budget: PX-3 bounds 50
plugins × 2 lanes at ≤ 120 MB, so normally nothing is evicted. Above the budget:
- **victim** = the lowest decayed activation count (half-life 512 activations);
- **admission** (TinyLFU): a cold-started instance stays `Ready` only if its decayed count exceeds
  the victim's. Otherwise it runs once and is dropped. This resists scans, such as a page that
  matches many plugins once.

### 3.8 Fallback

If any invariant check fails, the writer uses the fixed LABELS policy (wait for non-refinement
plugins up to D, no pre-wake) and logs it once. The checks are: a histogram total of 0 after
warm-up, `t* > dw`, `Σ grants > P_k`, or a counter overflow. This follows the sched_ext watchdog
pattern. The PF-8 bound does not depend on the scheduler at all.

## 4. Guarantees

Notation: N is the number of activations of one plugin.

### 4.1 PF-8 [P]

The writer's end is `max_{j∈W} min(reply_j, h_j) ≤ max h_j ≤ t* ≤ dw` after the snapshot. With
snapshot and fuse it is ≤ D. This holds for any replies, any stats and any plugin behaviour. A
corrupt state can only shrink holds. The bound is the existing futex/timerfd deadline: the hold
only moves the absolute deadline earlier. One wait per keypress path (r4) is unchanged, because
the scheduler runs inside that one wait.

### 4.2 Per-plugin harm [P]

Fix the other plugins' replies. Adding plugin i to W raises the writer's end by at most
`max(0, min(reply_i, h_i) − end_{−i}) ≤ h_i ≤ d_i`. For a third-party plugin `d_i ≤ 100 µs`, and
clipped third-party plugins are never in E.

So **faking fast is self-defeating**: a plugin that looks fast has a small `d_i`, which caps the
stall it can cause. A plugin that looks slow gets a large `d_i`, but it is waited for only if
`v_i F_i(h_i) ≥ λ h_i`. Its value is capped by tier, and is measured from facts that actually
changed the published entry, not from facts emitted.

### 4.3 Offence bound [P]

Let O be the number of offences in N activations. m rises by 1 per offence and falls by 1 per R
on-time replies, so `O = (#decrements) + m_N ≤ N/R + m_N`.

An offence at level m ≥ log2 E consumes E activations of exclusion, so at most N/E offences happen
at those levels. Reaching level log2 E takes at most log2 E + 1 offences. Hence:

`O ≤ N/(R+1) + N/E + log2 E + 1`.

With R = 32 and E = 256, that is ≤ 3.4 % of activations + 9. Each offence costs ≤ `h_i` (§4.2).

An adversary that knows when it is waited on (oracle) stalled 117 times in 7 495 activations
(1.56 %) [M]. Its mean extra stall is ≤ 1.56 % × 100 µs ≈ 1.6 µs per activation.

A benign plugin offends with probability ≈ P(exec > own q99 + 8) ≤ 1 %, below the forgiveness rate
1/33. m is then a positive-recurrent walk near 0, and the longest run of exclusion of a benign
plugin in the simulation was small (§5.4).

This is a tolerance, not detection. A plugin that behaves exactly like an honest plugin with a 3 %
tail cannot be told apart from one, and the scheduler accepts it at the price it would accept from
the honest plugin [H, inherent].

### 4.4 Starvation-freedom [P]

1. **Jobs.** The scheduler only decides placement and waiting. Every triggered job is dispatched in
   its own turn (newest wins, displaced jobs requeued: r5 gap 7). So every plugin's facts reach the
   model, at worst through the LB-4 refine. The final model equals `Full(S0)` (Theorem P) whatever
   SPS decides.
2. **Wait exclusion** after an offence lasts ≤ E = 256 of the plugin's activations. After that the
   plugin is eligible again (cap on `e`).
3. **Hot place.** A spin- and wait-eligible plugin denied a hot place while K ≥ 1 increments `g`. At
   `g ≥ 8` it sorts before every non-promoted plugin of its tier class, so it heads a queue. Since
   `free = 0 ≤ Q_start^cold(0.9) − Q_start^hot(0.9)`, it gets the place. Among m ≤ 16 promoted
   plugins at most K·(places per queue) are served per turn, so the denials of any plugin are
   ≤ `8 + ⌈m/K⌉ ≤ 24`. The exception is third-party plugins, which are capped at one place by
   design.
4. **CPU starvation of the desktop** is bounded by the spin bucket: at most 200 ms of burst and
   150 ms/s on average.

### 4.5 Determinism and complexity [P]

Every update is integer arithmetic on the scheduler's state and the measured inputs (durations,
contexts, triggered sets, usefulness bits). Given the same input log, the decisions are bit-equal.
`run_all.py` records every call of an oracle-adversary run (3 000 keypresses), replays them into a
fresh scheduler and compares: **0 mismatches** (§5.7).

The first utterance still depends on real timing, as ADR §2.4.5 already accepts. The model does
not.

Cost per keypress, with the CDF grids cached per plugin and state after each reply (an O(16²)
update, off the critical path in the admission turn):
- key-down: O(n_scope);
- dispatch: O(m log m + m·|G|), with m ≤ 16 and |G| ≤ 32.

That is ≈ 600 multiply-adds, a few µs in Rust [H]. It is O(1) under the ADR caps.

### 4.6 Fit with PF-1..PF-8, MQ-2, MQ-3 [P]

SPS changes *when* a reply is admitted and on which thread a job runs. It never changes *what* a
job computes or how it is admitted.
- PF-1/PF-2 (view-functional, S0-only): the view is the frozen snapshot as before.
- PF-3/PF-5/PF-6: admission is unchanged.
- PF-4: no scheduler decision enqueues a job; usefulness and timing feed stats only, never
  triggers.
- PF-7 / MQ-2: the token check is unchanged. A late reply is admitted or rejected by `(t, R, M, g)`
  exactly as before.
- PF-8: §4.1.
- MQ-3: §3.5 generalises Proposition B per queue with weights.

The stats themselves are read in the writer turn only and written from `PluginReply` handling, so
they are writer-owned state (no sharing across actors).

## 5. Simulation

### 5.1 Setup

`sim.py`, 20 000 keypresses, seed 1, with an exponential inter-key gap of mean 200 ms (5 keys/s).

Context mix and focus-change probability:

| Context | share | P(focus change) |
|---|---|---|
| tg-nav | 30 % | 0.95 |
| tg-tab | 8 % | 0.98 |
| tg-enter | 5 % | 0.7 |
| tg-edit-arrow | 10 % | 0.02 |
| tg-type | 17 % | 0 |
| oth-nav | 15 % | 0.8 |
| oth-tab | 5 % | 0.98 |
| oth-type | 10 % | 0 |

The focus change arrives LN(3 ms, 0.7) after key-down. The pre-wake window is 50 ms.

Plugins (exec lognormal median/σ in µs, usefulness, base activation):

| # | name | label | tier | exec | u | α | notes |
|---|---|---|---|---|---|---|---|
| 0 | telegram | interactive | first | 45 / 0.5 | 0.8 | 0.95 | Telegram scopes only |
| 1 | cheap-a | — | first | 8 / 0.4 | 0.3 | 0.6 | |
| 2 | cheap-b | — | trusted | 12 / 0.5 | 0.5 | 0.4 | |
| 3 | cheap-c | interactive | first | 6 / 0.3 | 0.6 | 0.3 | |
| 4 | medium-a | — | trusted | 60 / 0.6 | 0.4 | 0.5 | busy 20 % (+5–40 µs cancel) |
| 5 | medium-b | interactive | first | 90 / 0.5 | 0.7 | 0.2 | |
| 6 | heavy | refinement | first | 180 / 0.4 | 0.3 | 0.5 | busy 20 % |
| 7 | slow-tail | — | trusted | 20 / 1.2 | 0.4 | 0.4 | heavy tail |
| 8 | rare | — | first | 30 / 0.5 | 0.9 | 0.03 | |
| 9 | adversary | self-labelled interactive | third | 10 / 0.3 when honest | 0.5 | 0.7 | broad selectors |

Adversary strategies. A stall is exec 190 µs, just under the 200 µs cap, so it draws no strike.
- `honest`: never stalls.
- `periodic`: every 20th activation.
- `random5`: 5 %.
- `burst`: 300 fast, then 30 stalls.
- `slow`: always slow but "useful".
- `oracle`: stalls exactly when it is in W. This is the strongest strategy and assumes a side
  channel.

Host, from the m18a-2 probe:
- hot start U[1,3] µs;
- parked start: 25 % LN(8, 0.5), 60 % LN(70, 0.4), 15 % LN(200, 0.3), capped at 340 µs;
- the writer spins (in-flight fix) but is pre-empted with p = 1 % by U[30,120] µs;
- one batched futex wake;
- per-call cap 200 µs (MQ-4 parallel grants, so no 23 µs crowding).

Policies:
- **WA-park**: r5 as reviewed, all triggered plugins in the countdown to D, no pre-wake.
- **WA-wake**: the same, plus pre-waking every plugin worker of the scope on every non-typing key.
- **LABELS**: pre-wake interactive and unlabelled workers on non-typing keys, wait for them to D,
  never wait for refinement.
- **SPS**: this report.

CPU is the scheduling overhead only: spinning plus wakes and tails. Exec is the same in every
policy and is excluded.

### 5.2 Main results [M] (`results.json` → `main`)

FFU counts benign, waitable plugins. Stall is measured over keypresses that triggered ≥ 1 plugin.

| adversary | policy | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited |
|---|---|---|---|---|---|---|---|
| honest | WA-park | 0.9561 | 0.9064 | 238 | 300 | 0.0 | 0 |
| honest | WA-wake | 1.0000 | 1.0000 | 135 | 250 | 49.8 | 0 |
| honest | LABELS | 1.0000 | 1.0000 | 77 | 217 | 44.7 | 0 |
| honest | SPS | 0.9975 | 0.9918 | 86 | 273 | 5.5 | 0 |
| periodic | WA-park | 0.9561 | 0.9064 | 241 | 300 | 0.0 | 374 |
| periodic | WA-wake | 1.0000 | 1.0000 | 140 | 251 | 49.8 | 374 |
| periodic | LABELS | 1.0000 | 1.0000 | 79 | 217 | 44.7 | 374 |
| periodic | SPS | 0.9984 | 0.9961 | 82 | 247 | 6.5 | 1 |
| random5 | WA-park | 0.9561 | 0.9064 | 240 | 300 | 0.0 | 353 |
| random5 | WA-wake | 1.0000 | 1.0000 | 139 | 250 | 49.8 | 353 |
| random5 | LABELS | 1.0000 | 1.0000 | 79 | 217 | 44.7 | 353 |
| random5 | SPS | 0.9983 | 0.9957 | 83 | 252 | 6.5 | 1 |
| burst | WA-park | 0.9561 | 0.9064 | 243 | 300 | 0.0 | 660 |
| burst | WA-wake | 1.0000 | 1.0000 | 144 | 251 | 49.8 | 660 |
| burst | LABELS | 1.0000 | 1.0000 | 81 | 217 | 44.7 | 660 |
| burst | SPS | 0.9982 | 0.9951 | 83 | 251 | 6.4 | 3 |
| slow | WA-park | 0.9561 | 0.9064 | 278 | 300 | 0.0 | 7495 |
| slow | WA-wake | 1.0000 | 1.0000 | 206 | 254 | 49.8 | 7495 |
| slow | LABELS | 1.0000 | 1.0000 | 205 | 219 | 44.7 | 7495 |
| slow | SPS | 0.9985 | 0.9963 | 82 | 243 | 6.9 | 0 |
| oracle | WA-park | 0.9561 | 0.9064 | 278 | 300 | 0.0 | 7495 |
| oracle | WA-wake | 1.0000 | 1.0000 | 206 | 254 | 49.8 | 7495 |
| oracle | LABELS | 1.0000 | 1.0000 | 205 | 219 | 44.7 | 7495 |
| oracle | SPS | 0.9980 | 0.9946 | 85 | 263 | 5.9 | 117 |

Readings:
- **WA-park** loses about 4 % of facts to the C3 wake tail (worst plugin 0.906). The writer always
  runs close to D, because one parked plugin holds the countdown.
- **WA-wake / LABELS** get every fact, but spin about 45–50 % of a core at 5 keys/s. Most of that is
  pre-waking on edit-arrow keys that move no focus, then waiting out the 50 ms window. Against the
  oracle and slow adversaries, their stall p50 jumps from 77 to 205 µs: a self-labelled interactive
  third party is waited on to D.
- **SPS** loses 0.2–0.3 % of facts and spins about 6 %. Its p50 does not move under any adversary.
  Its p99 sits 25–55 µs above LABELS because jobs on parked threads keep the cold tail (§5.3, §5.5).

Seed 2, oracle adversary (stability check):

| policy | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |
|---|---|---|---|---|---|---|
| WA-park | 0.9530 | 0.8876 | 278 | 300 | 0.0 | 7402 / 7402 |
| LABELS | 1.0000 | 1.0000 | 205 | 224 | 45.1 | 7402 / 7402 |
| SPS | 0.9975 | 0.9921 | 86 | 269 | 5.7 | 116 / 7402 |

### 5.3 Sweeps [M] (oracle adversary, 10 000 keypresses)

`k_mult` sets the spin count versus the cold tail:

| k_mult | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |
|---|---|---|---|---|---|---|
| 0.5 | 0.9950 | 0.9809 | 97 | 300 | 4.3 | 57 / 3768 |
| 1.0 | 0.9985 | 0.9961 | 85 | 263 | 6.3 | 59 / 3768 |
| 1.5 | 0.9976 | 0.9941 | 82 | 265 | 8.6 | 59 / 3768 |
| 2.0 | 0.9970 | 0.9931 | 81 | 279 | 9.8 | 59 / 3768 |

λ, the price of delay (Q16 units per µs):

| lam_q16 | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |
|---|---|---|---|---|---|---|
| 3 | 0.9985 | 0.9961 | 85 | 263 | 6.3 | 59 / 3768 |
| 13 | 0.9978 | 0.9948 | 85 | 263 | 6.3 | 59 / 3768 |
| 33 | 0.9939 | 0.9820 | 85 | 261 | 6.3 | 59 / 3768 |
| 65 | 0.9807 | 0.9404 | 84 | 254 | 6.3 | 59 / 3768 |

Spin budget (refill rate):

| spin_rate | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |
|---|---|---|---|---|---|---|
| 30000 | 0.9900 | 0.9715 | 99 | 300 | 4.6 | 58 / 3768 |
| 75000 | 0.9968 | 0.9892 | 87 | 290 | 5.9 | 59 / 3768 |
| 150000 | 0.9985 | 0.9961 | 85 | 263 | 6.3 | 59 / 3768 |

### 5.4 Starvation [M]

The longest run of consecutive activations in which a benign plugin was not in W was 2 activations (max over all SPS runs; adversary excluded).
They come from warm-up and from turns where a plugin's expected value did not cover its hold.
No benign plugin was ever excluded for long by an offence.

### 5.5 Host lever: per-CPU PM QoS [M]

Writing `pm_qos_resume_latency_us = 20` for the pool CPUs (sysfs, root) forbids C2/C3 on those CPUs
only, and nowhere else. The simulation models it as parked start 70 % LN(8, 0.5) and 30 % LN(18, 0.4),
capped at 60 µs:

| policy | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |
|---|---|---|---|---|---|---|
| WA-park | 1.0000 | 1.0000 | 214 | 270 | 0.0 | 3768 / 3768 |
| LABELS | 1.0000 | 1.0000 | 205 | 225 | 44.4 | 3768 / 3768 |
| SPS | 0.9996 | 0.9943 | 88 | 233 | 0.1 | 57 / 3768 |

This is a system-wide change, so it is a decision for the owner. Spinning becomes nearly
unnecessary, and SPS then mostly decides W and the holds.

### 5.6 Warm-instance retention under a RAM budget [M] (`ram.py`)

40 instances with Zipf(1.1) popularity. 30 % of the ranks rotate every 2 000 activations, and 5 %
of activations are one-off scans. The table gives the hit rate (a miss means a cold first call of
about 316 µs, so the first utterance is missed).

| M (instances kept) | LRU | LFU | decayed LFU | decayed LFU + TinyLFU admission |
|---|---|---|---|---|
| 4 | 0.323 | 0.291 | 0.452 | **0.482** |
| 8 | 0.498 | 0.444 | 0.609 | **0.622** |
| 12 | 0.605 | 0.552 | 0.698 | **0.705** |
| 20 | 0.738 | 0.720 | 0.806 | **0.810** |

Plain LFU never forgets a past phase, and LRU is flushed by scans. Under the PX-3 numbers this only
matters above about 50 plugins.

### 5.7 Replay [M]

17594 recorded scheduler calls (oracle adversary, seed 7, 3 000 keypresses) replayed into a fresh scheduler: **0 mismatches**, output hash `9f3a899cdbc785ac`.

## 6. Recommendation (text for ADR-0026, new §2.4.7 "Statistical scheduler")

> **§2.4.7 Statistical scheduler (normative once M18a-6 ships; math report
> plugin-statistical-scheduler.md).** The writer owns, per (plugin, scope):
> - a decayed log-histogram of exec times (16 buckets, edges 0,4,8,12,16,24,32,48,64,96,128,192,
>   256,384,512,1024 µs; weight 16, halve every 64 samples);
> - per key-class activation `α ← α + ((hit − α) >> 5)`;
> - usefulness `u ← u + ((changed − u) >> 5)` with value `v = min(u, cap_tier)` (1, 0.75, 0.5);
> - the offence state `(m, e, ontime_run)` and the denial count `g`.
>
> Globally it owns pooled start-latency histograms for hot and parked threads and a spin bucket
> (200 ms, 150 ms/s). Labels: `interactive` (prior v 1, α 0.5, weight 2), none (0.5, 0.25, 1),
> `refinement` (never waited, never spun). Third-party plugins: `v ≤ 0.5`, one hot place, hold
> ≤ 100 µs.
>
> 1. **Key-down.** Spin `K = min(|S|, 6, max(1, ⌈Σ_{S} α⌉), bucket/3 ms)` pool threads, where `S` is
>    the set of spin-eligible plugins with `α·v·(F^hot(dw) − F^cold(dw)) ≥ 0.01`. Spinning ends at
>    the focus change, or after 50 ms; the actual cost is charged to the bucket.
> 2. **Dispatch.** Order jobs by (third-party last, promoted `g ≥ 8` first, `v·w/e50` descending,
>    rotating tie-break). Place them by list scheduling on the spinning threads while
>    `free + q90_hot ≤ q90_cold`; the rest go to parked threads.
>    Set `d_i = off99 + q99_start + q99_exec + 8 µs`, clipped to `min(dw, tier cap)`.
>    Set `t* = argmax_t Σ v_j F_j(min(t, d_j)) − (3/65536)·t` on the bucket-edge grid.
>    Set `h_j = min(t*, d_j)`, with `j ∈ W` iff `v_j F_j(h_j) ≥ λ h_j`.
>    The writer ends at `max_{j∈W} min(reply_j, h_j)`. This replaces the shared countdown to D;
>    plugins outside W are dispatched untagged.
> 3. **Fuel.** Weighted progressive filling per spinning thread over `κ_int·(dw − q50_hot)`;
>    a job on a parked thread gets the per-call cap. This answers MQ-4.
> 4. **Offence.** Waited, `reply > h`, `exec > q99_exec + 8`, not clipped: `m += 1`,
>    `e = min(256, 2^m)`, and the plugin is not waited for during its next `e` activations.
>    Every 32 on-time replies forgive one offence. `Derive` messages must not reveal membership
>    of W.
> 5. **Retention.** All W instances of open scopes stay Ready within the B-7 budget. Above it,
>    evict by decayed activation (half-life 512) with TinyLFU admission.
> 6. **Fallback.** On any invariant failure use the fixed-label policy; D is enforced regardless.
>
> Integer-only, replayable; O(m log m + 32 m) per keypress. Guarantees and their proofs:
> report §4. The PX-6 gate is measured with SPS on default power management.

Parameters, frozen for M18a-6 and to be re-tuned on real telemetry:

| parameter | value |
|---|---|
| D | 300 |
| snap / fuse reserve | 6 / 8 |
| λ | 3/Q |
| θ | Q/100 |
| k_mult | 1 |
| K_max | 6 |
| spin bucket | 200 ms at 150 ms/s |
| spin estimate | 3 ms |
| pre-wake window | 50 ms |
| α and u shifts | 5 |
| histogram half-life | 64 |
| warm-up | 8 replies |
| margin | 8 µs |
| R | 32 |
| E | 256 |
| age_max | 8 |
| third-party hold cap | 100 µs |

## 7. Porting notes (Rust)

- `struct PluginStats { exec: [u32; 16], n: u32, act: SmallVec<[(KeyClass, u32); 6]>, useful: u32,
  m: u8, e: u16, ontime: u8, denied: u8 }`. Keep it in the writer's slot table next to `Slot`
  (§2.5.4). It is writer-owned, with no atomics.
- Cache `F_hot[32]` and `F_cold[32]` per plugin on the grid. Recompute them after every reply, in
  the admission handling of `PluginReply` (after publish), so the dispatch path only reads arrays.
- Golden vector: `python tools/research/plugin_sched/sps.py` asserts
  `(1, [(0,0,0,60,4086932,60), (2,None,0,286,4347800,0), (1,0,48,72,2043466,0)], [0], 60)`. A port
  must reproduce it bit-exactly. Note that Python's `>>` on negative ints is a floor shift; use
  `i64 >> s`, not division, in `ewma_q16`.
- Third-party "last on queue" is an ordering invariant. Test it with a property test: no
  third-party job precedes a non-third-party job on any hot queue.
- The spin thread is the existing adaptive spin with a "pre-armed until" deadline. K threads are
  chosen among idle pool threads; plugin actors are not pinned to them.
- Telemetry needed and not yet present:
  - a "facts changed the published entry" bit per `Done` (fuse diff on the focus-chain entries);
  - `start` vs `exec` split in `Done` (`queued` exists; the wake latency must be measured on the
    pool thread as `now − enqueue` at job start);
  - the key class at key-down from the input layer.

## 8. Risks and open questions

- **Synthetic only.** The workloads, usefulness rates and the C-state mixture are modelled on one
  probe. Re-fit them on PX-6 telemetry before freezing the parameters.
- **Exec is assumed independent of thread state.** A cold cache on a freshly woken CPU can inflate
  exec. If it does, keep two exec histograms per plugin (hot and cold) and accept bandit feedback
  for the rarely-hot ones, with exploration by stride on spare hot places.
- **Wait-membership side channel.** A plugin may infer from timing whether the writer waits for it.
  The oracle strategy already assumes it can, and the harm stays ≤ 100 µs per offence.
- **λ and usefulness** need the fuse-diff bit. Until it exists, use `u` = "emitted ≥ 1 admitted
  fact", which a plugin can inflate (the tier caps still bound it).
- **p99 stall** is 25–55 µs above LABELS on default C-states. Raising k_mult does not lower it
  (§5.3); only PM QoS (§5.5) or spinning a thread per plugin (LABELS, about 45 % of a core) does.
  This matters if the PX-6 gate is the p99 of the stall rather than FFU.
- **Gate.** The ADR gate "≥ 99 % overall, ≥ 97 % per plugin" is met in the simulation (0.997,
  worst 0.99). It is not verified on the real host.
