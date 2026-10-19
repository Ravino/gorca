# Fusion engine: memory vs latency, reader memory budget, restated M-9/M-11

Date: 2026-10-19. Author: math-researcher. Follow-up to M22a-B6
(`docs/reports/m22a-b6-fuse-bench.md`). The model, measurements and proofs are in
`docs/reports/semantic-fusion-math.md` §11; reproduce with
`OMP_NUM_THREADS=1 python research/fuse-mem/model.py all` and the probe in `research/fuse-mem/`.
For the engineer on the fix branch (worktree `agent-a5fa450750973b3b7`): section 2 is the ordered
work list, section 4 the gates to bench against.

Owner guidance applied: 25 MB/100k is not strict, but nothing is wasted; future ML inference
(OCR, image/UI description, small local models) needs headroom; compute-on-access or async is
acceptable where the metrics show it costs no latency.

## 1. What must be resident, what can be lazy

| State | Size today (100k) | Residency | Why |
|---|---|---|---|
| Registers (`NodeRegs` + heap) | 14-104 MB (4-18 groups) | **resident** | source of truth; recomputing needs IPC |
| Published `SemEntry` (all live nodes) | 16 MB | **resident** | every keypress reads 4.5-5.5 entries; lazy saves ≤ 16 MB but costs k × 3 µs on bulk reads (math §11.6) |
| Tree topology, depth, dirty/age state | ≈ 5-10 MB | **resident**, compact | focus-chain and priority walks |
| S0 of every node | 100 MB (B6) / ≈ 8 MB projected | **not stored**: rebuilt from entry + sparse Δ (exact, math §11.3) | only neighbours' selectors and change detection read it |
| S0 metadata (`info/src/seq/hit`), explain trails | — | **on demand** | explain/debug only; recomputed by `s0_with` with a tracer |
| Fused state of non-priority dirty nodes | — | **async** (background class, aging) | never on the speech path; a read of such a node promotes it |

Access model results (math §11.6, real-shape trees, 10k-500k nodes): a keypress reads the target
and its ancestor chain; with entries evictable, hit rate is 0.75-0.87 at any cache size (jumps
dominate), p99 4 misses → +12 µs per keypress. So entries *could* be lazy without hurting
keypress latency, but the saving (160 B/node) is small next to the register store and bulk queries
(elements list, find) would pay 3 µs per item. **Keep entries resident**; keep "lazy" for CPU
(backlog) and for debug data, not for entries.

## 2. Recommendations for the fix branch (ordered by value)

**R1. Replace stored S0 with `S0 = rebuild(pub) ⊕ Δ` (high confidence).** Per node store only
`Δ = {(g, slot) : g ∈ R, rebuild(entry, g) ≠ s0[g]}` (sparse map `NodeIdx → SmallVec<[(Group,
Slot); 1]>`, or an `Option<Box<..>>` in `Node`), computed in `process` after fusion. Neighbours get
an `S0View { entry, delta }` with `slot(g)`; `PatchSource::hits` takes the view instead of
`&S0Node` (the node's own S0 stays a full stack value). `changed_groups` = compare
`rebuild(old_pub) ⊕ Δ_old` with the new S0 over `R` only. Exact by construction (math §11.3), no
recomputation, ≈ 2 B/node expected; removes the projected `Vec<(Group, S0Slot)>` (24 B inline +
≈ 64-80 B heap per node on the branch) and its allocation per fusion. Handle a patch reload that
grows `R`: per-writer `delta_groups` mask; reads of groups outside it recompute that neighbour's
S0 until the epoch sweep passes the node. Do **not** build an S0 LRU: it tops out at 0.85 hit rate
and a miss on a wide node costs up to 128 µs (§11.4).

**R2. Register store: exact capacity, then `Absent` runs (high / medium).** Today 14 one-by-one
inserts give capacity 16 (528 B chunk), 17-18 give 32 (1040 B). (a) Ingest the initial snapshot of
a node with exact capacity, or `shrink_to_fit` once after it (later inserts are rare). (b) Store
`Absent` registers as `(seq, GroupMask)` runs (a full state read writes ≈ 9 `Absent` groups with
one seq): the join stays per group (apply `Absent(g, s)` = remove `g` from older runs/values when
`s` wins, add to the run of `s`); keep the exact tie order of `tie_cmp` between `Absent` and values
and cover it with the differential corpus. (c) Optional: thin `Arc` for names (`Val` 24 → 16 B,
`Reg` 32 → 24 B). Realistic node: 528 → 144 B.

**R3. Fix the bench world (high).** B6 writes 3-4 groups per node; real AT-SPI nodes write ≈ 14
(role, name, description, all state groups as value or `Absent`). Add a `rich` world (14 regs,
9 `Absent`; 5 % with 18) or the RSS gate under-reports by ≈ 2×.

**R4. Node diet (medium).** Move rare fields out of `Node` into sparse maps (`attr`, `contrib`,
Δ), shrink `pos`/`vis` to `u32`, replace the `dirty_of`/`age` hash maps with inline `u32` fields
(dirty class + enqueue tick). Target ≤ 176 B inline (≤ 128 B with topology in SoA arrays
`parent/first_child/next_sibling/depth: u32`). Also cuts per-event hash lookups (the bookkeeping
that dominates per-event cost, §11.2).

**R5. Scheduler: dependency-ordered priority sub-classes (high).** Class 0: the focused node and
its content-feeding descendants (bounded by `max_children`, depth-desc so children fuse before the
name that reads them); class 1: its ancestors (nearest first) and the active descendant; class 2:
visible window by reading-order distance from focus; class 3: backlog FIFO with aging (aging must
not take the only slot of a 1-call run). Replace the 100k `BTreeSet<DirtyKey>` by small heaps for
classes 0-2 (≤ `max_priority` = 1024) and a `VecDeque` + in-queue bit for class 3. Expected
fusions until the chain is published ≈ |closure| + depth ≈ 10-40 (B6: p50 144, p99 816).

**R6. Incremental publish (high).** Hand class 0/1 publishes to the reader as soon as each node
is fused (channel push or a ≤ 32-fusion / ≤ 100 µs slice), never at the end of a 2 ms run.
Simulation: priority + incremental keeps priority latency at 6-16 µs p99 under a 20k-event burst;
priority + end-of-run publish gives 0.9-1.6 ms; FIFO gives 23-65 ms (math §11.7). Priority events
must also bypass the 1 ms coalescer window (not modelled here; check C1b).

**R7. TTL index (high, already in progress).** `BTreeMap<(expiry_ms, RegKey)>` or a min-heap
with lazy deletion: `Tick` O(expired · log F) instead of O(F) (115 µs at 50k facts).

**R8. Read-promotes-dirty (medium).** When a reader reads a node that is still dirty in class 3,
promote it to class 0 rather than fusing on the reader thread; readers keep reading the last
published snapshot lock-free.

## 3. Reader-wide memory budget policy

### 3.1 Classes

| Class | Contents | Budget (30 GB host) | Policy |
|---|---|---|---|
| **K0 core, non-evictable** | fusion (registers, entries, Δ, side tables, indices), mirrors, doc treaps, input/speech state | ≈ 0.5 KB/node (design D: 49-57 MB/100k) + 32 MB fixed | never evicted (dropping it means wrong speech); bounded by per-app node limits; monitored, alarm at 1.5 × model |
| **K1 evictable caches** | text/flat-buffer caches of background documents, explain data, OCR/description results, prefetch of off-screen entries for background tabs | `C = clamp(MemAvailable / 64, 16 MB, 256 MB)` (≈ 234 MB with 15 GB available) | LRU/ARC per cache, sized by the shared budget; AIMD below |
| **K2 ML inference** | OCR (tesseract), encoders (SigLIP/MobileCLIP/e5, on-disk 23-485 MB per model dir), later a small VLM/LLM (0.5-4 GB) | `ML = min(4 GB, 0.10 × MemTotal)` and only while `MemAvailable ≥ ML_needed + 2 GB` | **separate process(es)** (crash isolation, own cgroup, can be stopped without touching the core); load on demand, unload after 5 min idle; prefer mmap-able weights so clean pages are reclaimable |

Process placement: the core runs in its own systemd user scope with `MemoryLow = K0 estimate +
64 MB` (the kernel reclaims others first, so the reader does not page-fault on the speech path);
the ML service gets no protection and a `MemoryHigh` = its budget. Set `M_ARENA_MAX` (glibc,
e.g. 4) or use a decaying allocator; call `malloc_trim(0)` after a large document closes. Both are
**[H]** until the telemetry below shows fragmentation numbers.

### 3.2 Adaptive rule (AIMD on PSI and MemAvailable)

Use a PSI trigger fd (`/proc/pressure/memory`, "some 150000 1000000") for wake-ups instead of
polling; re-evaluate at most every 2 s.

| State | Condition | Action |
|---|---|---|
| Green | `some avg10 < 2 %` and `MemAvailable > 15 % MemTotal` | K1 target grows by +10 % of max per minute; ML may load |
| Yellow | `some avg10 ≥ 5 %` for 2 windows, or `MemAvailable < 10 %` | K1 target × 0.5 (at most once per 10 s); unload idle ML models; pause background prefetch |
| Red | `full avg10 ≥ 5 %` or `MemAvailable < 5 %` | K1 to its 16 MB floor; stop the ML process unless a user-initiated request is running; `malloc_trim` |

Thresholds are set above what this host shows when busy: under 11-15 load from parallel builds,
`some avg10` was 0.05-1.5 % and `full avg10` up to 1.46 % (B6 run files; `/proc/pressure/memory`
on 2026-10-19), so 2 % / 5 % do not shrink caches on ordinary build load. K0 is never shrunk by
this rule.

### 3.3 Telemetry (local only)

Export from a `gorca-metrics` registry (gauges + HDR-style log-linear histograms, lock-free
per-thread, merged on read) through the private debug D-Bus interface or a socket under
`$XDG_RUNTIME_DIR`, readable by a `gorca-ctl stats` command; optional JSON-lines snapshots under
`$XDG_STATE_HOME/gorca/metrics/` (rotated, opt-in). Never sent off the machine.

- **Memory gauges:** per app and per writer: live nodes; bytes in registers, entries, Δ, side
  tables, indices, `Node` arrays; bytes/node; mirror and doc bytes; per-cache budget/used; ML
  process RSS per model; process RSS/PSS/anon (`/proc/self/smaps_rollup`); malloc in-use vs
  arena (`mallinfo2`, fragmentation ratio); major faults of core threads (`getrusage`); PSI
  some/full avg10, `MemAvailable`, budget state and transition count.
- **Latency histograms:** fusion per input event, per fused node, per 1 ms batch; priority event →
  published; focus change → chain published; `Tick`; read-promotion latency; on-demand compute
  (explain, rebuild for `R' \ R`); keypress → speech start (existing end-to-end).
- **Cache efficiency:** hit/miss/eviction per cache; a miss-ratio curve per cache estimated with
  spatially hashed sampling (SHARDS, 1 % sample, constant memory) so the budget can be set at the
  knee instead of guessed.
- **Scheduler:** backlog size, oldest dirty age, fusions until chain published, early-cutoff rate,
  publishes per event.

## 4. Restated gates (replace M-9 and M-11 of ADR-0022)

Justification: the gorca-side budget is 15 ms p99 keypress → speech. Allocate ≤ 1 % (150 µs) to
fusion on the speech path, hence 100 µs for the priority publish. Per-event cost matters only
through utilisation and queueing; at ρ ≤ 0.2 the queue adds < 2× (math §11.7), so a per-event p99
of 10 µs costs ≤ 10 µs of speech latency. A 2 µs per-event p99 is not reachable with cold traffic
on a 100k mirror (≥ 3-5 µs for a multi-fusion cold event, math §11.8) and buys nothing measurable.

| Gate | Measure | Target | Today (perfA branch / B6) |
|---|---|---|---|
| **M-9'** memory | engine RSS increase, 100k nodes, **realistic** world (14 regs, 9 `Absent`; 5 % with 18) | ≤ 64 MB (≤ 640 B/node); entries alone ≤ 16 MB | bench world only: 68.7 / 154 MB; model for realistic: 111 (B) → 49 (D) |
| **M-11a** per input event | storm 25k-50k ev/s + 5k facts/s + 100 patches, 100k nodes | p50 ≤ 3 µs, p99 ≤ 10 µs, p99.9 ≤ 50 µs (host conditions recorded) | p50 1.4-3.0, p99 5.1-9.0 (pass) / p99 8.3-13 (fail) |
| **M-11b** capacity | mean cost at 50k ev/s + 5k facts/s; 20k-event burst drain | writer busy ≤ 25 %; burst of 20k drained in ≤ 70 ms; `pending` returns to 0 | 20-21 % busy (B6) |
| **M-11c** priority end-to-end | writer receives a focus-chain/visible event → `Published` delivered, during the storm **and** during a 20k burst | p99 ≤ 100 µs | ≈ 2 ms (end-of-run publish, B6 finding 5) |
| **M-11d** focus change | `set_focus_chain` → every nameable chain node published, ~100k dirty | p99 ≤ 0.5 ms | 1.53-1.61 ms (fail; R5 fixes) |
| **M-11e** `Tick` | cost at 0 / 50k stored facts | p99 ≤ 5 µs, flat in facts | 116 µs at 50k (R7 fixes) |
| informative | per event on a 2k hot set (L2-resident) | p50 ≤ 1 µs, p99 ≤ 2 µs | not measured separately |

## 5. Risks and open questions

- Register counts per real node, the Δ fraction under real patch sets, and the `rebuild` cost are
  modelled, not measured (math §11.9); R3 makes the bench measure the first.
- `Absent` runs (R2b) touch the join's tie order; the differential corpus must cover equal-seq
  `Absent`/value pairs before it lands.
- The coalescer's handling of priority events (R6) is outside `gorca-fuse` and not checked here.
- Mirror, doc treap and text-cache sizes per node are not measured; K0's 0.5 KB/node covers the
  fusion engine only until the telemetry reports the rest.
- ML budget numbers are on-disk sizes and rules of thumb; ONNX Runtime arena overhead is unmeasured.
