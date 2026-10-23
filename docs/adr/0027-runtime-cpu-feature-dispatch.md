# ADR-0027: Runtime CPU feature dispatch

Status: **Proposed** (r1, 2026-10-24). Owner: architect. Implementer: asm-simd-engineer. Gate: perf-engineer.
Owner decision (fixed): `docs/decisions.md`, row 2026-10-24 "Owner: hardware-aware Rust". If the CPU has a
useful extension gorca uses it; otherwise a portable path runs and gorca logs **one informational line
per missing feature at start-up, as a fact, not an error**. The owner wants the hardware used fully as
features accumulate, not "plain Rust".
Relates to: CLAUDE.md (performance philosophy: speed beats frugality; hot paths stay in Rust; Debian 13 and
Arch/Debian users share binaries), ADR-0001 (never block, event storms), ADR-0002 (`gorca-vector` exact f32
scan), ADR-0007 (text diff, typing echo), ADR-0022/0024 (`gorca-fuse`, 128 B entries), ADR-0026 (wasmtime
plugin cache, §WASM below), ADR-0019 §2.5 (`.cwasm` cache). Paper only: no kernel is committed here.

## 1. Context

gorca's budgets are in microseconds (keypress to speech p99 < 15 ms gorca-side; 10k+ AT-SPI events/s).
Plain Rust compiled for the baseline x86-64 target uses SSE2 only. Many of our inner loops are byte or
word scans (UTF-8 validation, UTF-8 to UTF-16 offset conversion, common prefix/suffix of two strings for
typing echo, comparing 128 B fused entries, filtering event codes). On a modern CPU these run 2-8x
faster with AVX2 or NEON-class instructions, but a binary built with `-C target-cpu=native` crashes with
SIGILL on an older CPU, and our binaries are shared with Debian and Arch users. So the extension must be
chosen **at run time**, once, and the choice must be observable, testable and safe.

### 1.1 Facts checked for this ADR

- The pinned toolchain is rustc 1.99.1 (`.toolchain`). `std::simd` (`portable_simd`) **is still
  unstable**: a probe `use std::simd::u8x16;` fails with `E0658: use of unstable library feature
  'portable_simd'` on the stable channel, and `rust-toolchain.toml` pins `stable`. We do not use it.
- `std::arch` intrinsics and `#[target_feature(enable = ...)]` are stable. `is_x86_feature_detected!` and
  `std::arch::is_aarch64_feature_detected!` are stable; the x86 macro also checks that the OS enabled the
  register state (XSAVE/XGETBV), and the result is cached in a static by std after the first call.
- The dev host has `avx2` but **no `avx512f`** (`/proc/cpuinfo`), so AVX-512 code can only be tested under
  an emulator (§6).
- `Cargo.lock` already contains `memchr`, `simdutf8` (and `multiversion_no_op`) through dependencies; the
  workspace lint is `unsafe_code = "deny"` and only `gorca-input-xkb` allows it today.
- `refs/wasmtime` (checkout `51.0.0-dev`; the workspace locks 49.x, so re-verify on the pinned version
  before M18a-2): Cranelift compiles for the **host's** features by default, a `.cwasm` records the ISA
  flags it was compiled with, and deserialize **fails** on a host that lacks one of them (§7).

### 1.2 Prior art

- **NVDA and Orca** (Python, with C++ helpers in NVDA) have no runtime-dispatch story to copy; the useful
  prior art is the Rust ecosystem's own:
- `memchr` (crates.io, not in `refs/`): runtime dispatch to AVX2/SSE2/NEON with a
  function-pointer resolved on first use. `simdutf8`: AVX2/SSE4.2/NEON validation, with an explicit
  `basic`/`compat` split and a `std`-feature runtime detect. `regex` and `aho-corasick` use `memchr`
  internally. Lesson: for byte scans and UTF-8 validation **do not write our own kernel**.
- Rust's own `std` uses `is_x86_feature_detected!` caching in `std_detect`, and `memcpy`/`memchr` in
  `libc`/glibc use IFUNC. IFUNC is not available to Rust code without `unsafe` linker tricks, so a
  function-pointer table is the Rust equivalent.
- Failure modes seen in the wild: (1) `target-cpu=native` release binaries crashing on other machines;
  (2) a feature check done on every call inside the hot loop; (3) AVX/SSE transition penalties when a
  non-VEX function is called between VEX ones; (4) a scalar fallback nobody tests, which rots; (5)
  wide-vector kernels that win on throughput benchmarks and lose on short inputs (typing echo diffs are
  tens of bytes); (6) AVX-512 license-based down-clocking on older Intel cores.

## 2. Decision

### 2.1 Principles (normative)

- **CD-1 Resolve once.** CPU features are detected exactly once per process, in `gorca_cpu::init()`, which
  the daemon calls right after the logger is up. Every dispatched kernel is a function pointer in a
  `&'static Kernels` table (`OnceLock`). No `is_*_feature_detected!` call exists outside `gorca-cpu`.
  Calling a kernel before `init()` is allowed (the table is created lazily by the same code); `init()`
  only adds the log lines.
- **CD-2 Dispatch at kernel granularity, never inside a loop.** A dispatched kernel processes at least
  one cache line of work or at least ~100 ns. Below a per-kernel length threshold (`MIN_LEN`, set by the
  benchmark) the caller uses the portable function inline and takes no indirect call. An indirect call
  costs about 1-2 ns and blocks inlining; a kernel that cannot amortise it is not dispatched.
- **CD-3 A portable implementation always exists and is the specification.** Every kernel has a
  `*_portable` function in safe Rust. It is the reference in tests and the only path on a CPU without the
  extension, on architectures we do not target (riscv, ppc, s390x), and under `GORCA_CPU_DISABLE`.
- **CD-4 Exact equivalence by default.** Every implementation of a kernel returns bit-identical results to
  the portable one. A float kernel (`gorca-vector` dot product) must either fix the reduction order so
  all implementations agree, or declare a tolerance in its registry entry (ULP bound) with the reason;
  a kernel with a tolerance is never used for a decision that must be reproducible (SemEntry equality,
  fact admission).
- **CD-5 No latency regression on small inputs.** A dispatched kernel's p99 on its worst realistic input
  (shortest, unaligned) is not worse than the portable function's p99 by more than 5 %.
- **CD-6 Presence is not a requirement.** gorca must run correctly, and pass the full test suite, with
  every extension disabled. A missing extension is information, never a warning or error, never spoken,
  never in the notification area.

### 2.2 Implementation preference order

1. **An existing dispatching crate** (`memchr`, `simdutf8`, `memchr`-based `bstr`/`regex`/`aho-corasick`,
   and for UTF-8/UTF-16 transcoding `simdutf`'s Rust binding if the evaluation in §5 passes). Their
   internal dispatch is out of our table; their features are reported in the log line by a documented
   probe (`gorca_cpu::foreign()` lists what each crate reports, when it can say). Zero `unsafe` of ours.
2. **Multiversioned plain Rust**: the same safe scalar-looking function compiled several times with
   `#[target_feature(enable = "avx2,bmi2")]` (or `"neon"`-class on aarch64) so LLVM autovectorises it.
   No intrinsics, no hand-written loads. Cheapest to write and review; wins for simple map/reduce loops.
3. **Hand-written `core::arch` intrinsics**, only by the asm-simd-engineer, only for a kernel that the
   profile proves hot and where tier 2 measurably misses the target by the gate of §5.
4. **`std::simd`** is out until it is stable on the pinned toolchain (§1.1). When it stabilises, tier 2.5
   (portable explicit SIMD) is reconsidered by a new ADR revision; it would let one source cover both
   x86 and aarch64.

Mechanism choice for tiers 2-3, **hand-written table versus the `multiversion` crate**: the table is the
decision. Reasons: one registry gives one place to force-disable, to log the missing features, to
enumerate implementations in tests and to benchmark both paths; the `multiversion` crate's per-function
dispatch macro has the same machine code but spreads detection over the code base and cannot be
enumerated or forced off from an env var without crate-specific features. `multiversion` stays allowed
**inside** a single `gorca-cpu` module for tier-2 kernels if it measurably removes boilerplate
(decided in slice CD-1 below with a 1-day comparison; both are the same cost at run time).

### 2.3 Architecture of the `gorca-cpu` crate

A new tokio-free, dependency-light crate `gorca-cpu` (workspace member). It is the **only** crate besides
`gorca-input-xkb` allowed `#![allow(unsafe_code)]` for SIMD (CD-9). Sketch (interface only):

```rust
/// Closed set of features gorca can use. Names match the std macros.
#[non_exhaustive]
pub enum Feature { Sse42, Avx2, Bmi2, Pclmulqdq, Avx512Bw /* gated by §2.6 */, Neon, Dotprod, Sve }

/// Zero-sized proof that `F` was detected AND not disabled. Constructible only inside `gorca-cpu`.
pub struct Cap<F: FeatureMarker>(PhantomData<F>);

pub struct Impl<Fn> { name: &'static str, requires: &'static [Feature], f: Fn }

/// Registry entry for one kernel: the portable fn plus implementations in preference order.
pub struct KernelSpec<Fn> { id: &'static str, portable: Fn, impls: &'static [Impl<Fn>],
                            min_len: usize, speedup_hint: Option<SpeedupHint> }

pub struct Kernels {                       // resolved once, 'static
    pub text_common_prefix: fn(&[u8], &[u8]) -> usize,
    pub utf16_units_to_utf8_offset: fn(&[u16], usize) -> usize,
    // ...one field per kernel, added only through the admission gate of §5
}
pub fn init() -> &'static Kernels;         // idempotent; first call detects, applies the disable list, logs
pub fn active() -> &'static ActiveReport;  // for `gorca doctor` and `explain`: kernel -> chosen impl
```

Resolution rule per kernel: take the first `Impl` whose `requires` are all detected and not disabled;
else `portable`. Detection is by individual feature (not by "level"), so a CPU with AVX2 but without
BMI2 (some VMs mask features) simply selects the next implementation.

`Cap<F>` is the type-state proof: the unsafe `#[target_feature]` call is made only inside a safe wrapper
that takes `Cap<F>`, so the invariant "this function runs only when the feature is present" is carried by
a type that can be built only by the detection code. Callers outside `gorca-cpu` never see `unsafe`.

### 2.4 Rust mechanics that apply (and traps)

- `#[target_feature(enable = "avx2")] unsafe fn` (or safe `fn` since target_feature 1.1 stabilised, which
  can be called without `unsafe` only from a context with the same features enabled); the **call from a
  non-matching context is `unsafe`** and is the one thing a reviewer must check (§8). Verify the exact
  stabilised rules on the pinned 1.99 before writing the first kernel.
- Do not pass SIMD-typed arguments or return values across a boundary where the two sides disagree on
  target features (ABI mismatch); kernels take and return slices and integers.
- Mark kernels `#[inline]` only inside the same feature context; the dispatch edge is `#[inline(never)]`
  so LLVM does not inline an AVX2 body into non-AVX2 code (it refuses, but the intent is explicit).
- `cfg!(target_feature = "avx2")` (compile-time) is used only to **skip** dispatch when a build already
  guarantees the feature (§3); never as the detection mechanism.
- On aarch64, NEON/ASIMD is part of the baseline: tier-2 and tier-3 kernels may use it unconditionally,
  with no `Cap`, no detection, no log line. Optional aarch64 features (`dotprod`, `aes`/`pmull`, `sve`,
  `lse`) use `std::arch::is_aarch64_feature_detected!` and the same table.
- Other architectures: scalar only. No claim, no test beyond compiling.

### 2.5 Dispatch overhead budget

- Table lookup is a load from a `&'static Kernels` (a static pointer, read-mostly, one cache line per
  few kernels). Kernels are called through `fn` pointers copied into the owning actor's struct at
  construction (`let diff = kernels().text_common_prefix;`), so the hot path has no `OnceLock` access.
- Target: dispatch overhead (indirect call and length check) ≤ 3 ns p99 per kernel call; bench `CD-B0`
  measures it with an empty kernel.

### 2.6 What we do not use, and why

- **AVX-512**: no kernel uses it by default. On older Intel cores it can lower the clock of the whole
  core (down-clocking, failure mode 6), which hurts exactly the latency we are protecting; on Zen 4 it is
  fine. A kernel may add an AVX-512 `Impl` only if the bake-off of §5.3 shows a win on **p99 end-to-end**
  (not on the isolated kernel) on at least one real AVX-512 machine and no loss on AVX2-only hosts. The
  dev host has no AVX-512, so this is untestable locally except in emulation (§6.2).
- **`-C target-cpu=native`** in any shipped build (§3).
- **Multiversioning of the whole program** (ld.so `glibc-hwcaps`, separate binaries per level): considered
  in §3.2, not adopted for the daemon.
- **IFUNC** via linker tricks: needs `unsafe` and ELF specifics; the fn-pointer table is equivalent.

## 3. Build policy

### 3.1 Baseline and variants

| Target | Default build | Notes |
|---|---|---|
| x86_64 | **x86-64-v1 (rustc default, SSE2)** | every dispatched kernel has a portable path; nothing assumes SSE4/AVX |
| x86_64 optional | **x86-64-v3 package variant** (`-C target-cpu=x86-64-v3`) behind a separate package/profile, **not built in Phase 3**, reconsidered when CD-B1 (below) shows a measurable gain | in a v3 build `gorca_cpu` still runs detection (a v3 binary on a v2 CPU would fault before `main`; the package declares the requirement); dispatch for features the build already guarantees compiles to a direct call via `cfg!(target_feature)` |
| aarch64 | default target (NEON baseline) | optional features dispatched as above |

- **Why v1, not v2.** The gain of v2 over v1 for *non-dispatched* code is small in a screen reader
  (parsing and message passing, not numeric loops), and everything we vectorise by hand is dispatched
  anyway. v1 keeps Debian 13 and Arch stock binaries valid on every amd64 CPU, which is the distribution
  promise. SSE4.2/POPCNT/SSSE3 are therefore *dispatch candidates*, not baseline.
- **`target-cpu=native` is forbidden in release and CI artefacts.** A lint in CI greps `.cargo/config.toml`,
  `RUSTFLAGS` in workflows and the Debian rules for `target-cpu=native` / `target-feature=+avx` and fails.
  A developer may use it locally in an uncommitted environment.
- **CD-B1 (measurement, not a commitment).** Build the daemon once with `-C target-cpu=x86-64-v3` and once
  with the default, run the PX-7 / event-storm bench and the keypress-to-speech bench; report the delta.
  If v3 gains ≥ 5 % on p99 or ≥ 10 % on throughput of non-dispatched code, open a ticket for the v3
  variant; otherwise the question is closed.

### 3.2 Alternatives for the build

glibc-hwcaps subdirectories (`/usr/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v3/`) would let the dynamic
loader choose a v3 *library*; our daemon is one executable, so it would need a v3 `cdylib` core, which
adds an ABI boundary for no demonstrated gain. Rejected until CD-B1 says v3 matters.

## 4. The log line

Emitted by `gorca_cpu::init()` after the logger is initialised, **once per missing or disabled feature**
(not once per kernel, not per call, not on reload). Level `INFO`, target `gorca::cpu`. Fixed format:

```text
cpu: <FEATURE> not available: <kernel>[, <kernel>...] run portable; with <FEATURE> <it|they> would be ~<N>x faster
cpu: <FEATURE> disabled by GORCA_CPU_DISABLE: <kernel>[, ...] run portable; with <FEATURE> <it|they> would be ~<N>x faster
```

Examples (illustrative numbers; real ones come from the table below):

```text
cpu: AVX2 not available: text-diff, utf16-offsets run portable; with AVX2 they would be ~3x faster
cpu: AVX2 disabled by GORCA_CPU_DISABLE: text-diff run portable; with AVX2 it would be ~3x faster
```

Rules:

- **Fact, not failure.** Never `WARN`/`ERROR`, never spoken, never a notification, no backtrace, no
  non-zero exit. The line states what is portable and what the feature would give.
- **The speed-up is measured, not guessed.** Each `KernelSpec` carries `speedup_hint` generated from the
  latest `bench/cpu/` run on the reference machine (`bench/cpu/hints.toml`, written by the perf-engineer's
  bench, reviewed in the PR). If a kernel has no measurement, the `~Nx faster` clause is **omitted**
  ("... run portable"), never invented. A single line aggregates kernels that share a feature and a
  hint; kernels with different hints are listed with their own factor in the same line
  (`text-diff ~3x, utf16-offsets ~5x`).
- **Only features gorca could use.** No line for a feature no kernel wants. On aarch64 with baseline NEON
  there is no line at all.
- **Machine-readable twin.** `gorca doctor` / the Control API diagnostics expose the full table (every
  kernel, chosen implementation, detected features) so that a bug report does not depend on log scraping.
  At `DEBUG`, one line per kernel with the chosen implementation.
- **Freshness gate.** CI compares `hints.toml` to the newest bench run on the reference machine and fails
  if a hint is off by more than 30 % (a stale "~3x" is a false statement in a log).

## 5. Admission of a kernel (what goes in, and who decides)

A hot loop becomes a dispatched kernel only if all hold. Candidates below are **not** commitments.

1. **Profile first.** The loop shows up in a perf-engineer profile with ≥ 1 % of gorca-side CPU in a named
   scenario (event storm, typing echo, browse mode build) or on the critical path of a latency budget.
2. **Tier 1 first.** An existing dispatching crate does not already give the result (`memchr`, `simdutf8`).
3. **Tier 2 first.** The multiversioned plain-Rust version was measured; tier 3 is allowed only if tier 2
   misses the gate by ≥ 1.5x.
4. **Gate.** Gain ≥ 2x on the kernel's median at its realistic input size, CD-5 holds on the shortest
   input, and the end-to-end scenario improves or is neutral (kernel wins that do not move the scenario
   are not merged, except to honour the owner's directive when the kernel is cheap and fully covered by
   the §6 tests: this exception is listed explicitly in the PR).
5. **Fuzz target and test registered** (§6) before merge.

Honest note: typing echo diffs are tens of bytes and their cost is microseconds against a 15 ms budget; a
vector kernel there is within noise for latency. The case for it is CPU headroom under event storms and
the owner's directive, not the p99. Each candidate states which of the two it serves.

### 5.1 Candidate hot loops (to be profiled, none committed)

| Candidate | Where | Likely tier | Serves | Note |
|---|---|---|---|---|
| Common prefix/suffix of two byte strings (leaf diff for typing echo, text diff first pass) | `gorca-text`, ADR-0007 | 1 (`memchr`-style mismatch scan) or 2 | echo CPU, storm | the diff is usually "one inserted/deleted run": prefix/suffix scan dominates; Myers/other diff only on the middle |
| UTF-8 validation of D-Bus/AT-SPI strings | `gorca-dbus`, `gorca-connector-atspi` | **1** (`simdutf8`) | storm throughput | check whether zbus already validates; do not validate twice |
| UTF-8 to UTF-16 index/offset conversion (Chromium/Firefox offsets are UTF-16 units, AT-SPI offsets are chars, ADR-0007 raw offsets) | `gorca-text`, `gorca-doc-source-atspi` | 1 (`simdutf` binding) if licensing/FFI fits, else 2-3 | browse mode build, long text | mostly ASCII text: an ASCII fast path (all bytes < 0x80, 32 B at a time) is the first win |
| 128 B `SemEntry` equality / dirty-mask compare | `gorca-fuse` (ADR-0024: 128 B entries) | 2 (4 x 32 B compare) | publish dedupe, storm | `memcmp` may already be optimal; measure `==` on the packed struct first |
| Event-code filtering of an ingress batch (drop/keep by event kind, superseded detection) | `gorca-ingress`, ADR-0004 | 2 | storm throughput | likely memory-bound; may show no gain; candidate only |
| Exact f32 dot / cosine scan | `gorca-vector` (ADR-0002) | 2-3 | perception layer | float: CD-4 reduction-order rule applies |
| Registry/selector prefilter over node attribute bitsets (ADR-0026 watch prefilter, 6-class masks) | `gorca-fuse`, `gorca-ext-host` | 2 (popcnt/bit tests) | plugin prefilter | `u64` ops: POPCNT/BMI2 give small gains; portable `count_ones` is already fast |
| Hashing | any | none | — | `ahash`/`foldhash` already pick fast paths at compile time; no table entry |

### 5.2 Ownership

- **asm-simd-engineer** implements `gorca-cpu` and every kernel, owns the `unsafe` modules and the
  per-kernel documentation (invariants, tail handling, why this tier).
- **perf-engineer** gates: owns `bench/cpu/` (portable vs each implementation, per input-size class,
  alignment classes, cold and warm), `hints.toml`, CD-B0/B1, the dispatch-overhead bench and the
  end-to-end scenario benches. A kernel merges only with the perf-engineer's numbers in the PR.
- **reviewer** applies the §8 checklist; **test-engineer** owns the equivalence fuzz and the CI matrix (§6).
- **architect** (this role) owns the policy; changes to it go through an ADR revision.

### 5.3 Bake-off (only if contested)

No bake-off for the policy. A bake-off is recommended only for a specific kernel where tiers 1, 2 and 3
are plausibly separated by more than the review cost (first likely: the UTF-16 offset conversion, arms:
`simdutf` binding, multiversioned scalar, hand AVX2/NEON), judged by the §5 gate on the Chromium and
Firefox browse-mode corpora.

## 6. Testing

### 6.1 Both paths run in CI, and the disable switch

- **Environment.** `GORCA_CPU_DISABLE=<list>` (comma separated feature names, or `all` / `portable`),
  read once in `init()`. A daemon flag `--cpu-disable` maps to the same list for field diagnosis. It is
  a diagnostic, not a setting: it is not stored in profiles and does not appear in the settings UI. An
  unknown name is reported once at `INFO` and ignored (a typo must not silently run SIMD in a "portable"
  test: the CI job also asserts the resulting `active()` report).
- **CI matrix** (test-engineer), every job runs the full `cargo test`:
  1. default (best available on the runner);
  2. `GORCA_CPU_DISABLE=all` (portable only);
  3. one job per feature step on x86 (`avx2` off; `avx2,bmi2,sse4.2` off) so intermediate
     implementations are exercised;
  4. the `active()` report is asserted in each job, so a job that did not run the path it names fails.
- **Coverage of paths the runners lack.** The dev host has AVX2 but not AVX-512 (§1.1). Missing CPU
  models are run under emulation: `qemu-x86_64` user-mode with `-cpu Nehalem` (SSE4.2 only), `-cpu
  Haswell` (AVX2), `-cpu max` (AVX-512), or Intel SDE; aarch64 under `qemu-aarch64` with `-cpu
  cortex-a72` / `max` for the optional features. Emulated runs are for equivalence tests only; the
  benchmarks are never run under emulation.

### 6.2 Equivalence testing and fuzzing

- **Registry-driven differential tests.** For each `KernelSpec`, for each `Impl` whose features the
  current process may run, a property test (proptest) compares the implementation to `portable` on:
  lengths 0-130 (every boundary around 16/32/64 B), all 64 alignments of the start, tails of every
  length, random and adversarial content (invalid UTF-8, lone surrogates, all-ASCII, all-equal,
  one-differing-byte at every position), and huge inputs (≥ 1 MiB) with a sparse difference.
- **Fuzz targets.** `cargo-fuzz` target `cpu_equiv` per kernel: inputs are bytes plus a control header
  choosing offsets and the implementation index; the oracle is `portable`; sanitizers on (ASan) so a
  one-byte over-read fails even when the result happens to agree. Run as a nightly job with a corpus
  stored in the repo's `fuzz/` tree; a crash is a regression test forever.
- **Miri** runs the portable path always and the SIMD paths where Miri implements the intrinsics (partial);
  it is a complement, not the proof.
- **Unsupported-instruction guard.** A test runs each `Impl` through `valgrind --tool=none` or SDE with
  `-cpu` set *below* its requirement and asserts the process was never able to select it (guards `Cap`).
- **Log tests.** Snapshot tests of the exact log lines (§4) for: all present (no line), one missing, many
  missing, disabled by env, no hint measured, unknown name in the list; assertion that `init()` called
  twice logs nothing the second time.
- **Performance guard.** perf-engineer's CI job fails if a dispatched kernel's chosen implementation is
  slower than `portable` on its reference input on the runner (a dispatch bug or a mis-tuned `min_len`).

## 7. WASM: wasmtime/Cranelift and the `.cwasm` cache

Findings in `refs/wasmtime` (checkout 51.0.0-dev; re-verify on the locked 49.x at M18a-2):

- `Config` detects host features with `detect_host_feature` (`crates/wasmtime/src/config.rs`, default
  `std_detect`-based) and Cranelift enables the matching ISA flags (`has_avx2`, `has_sse41`, `has_bmi2`,
  ...). So **guest code is compiled to use the host's extensions by default**; nothing in our code needs
  to do it, and plugin speed already benefits from AVX2/BMI2 where Cranelift uses them.
- A precompiled artefact stores the compiler's shared flags and **ISA flags**
  (`crates/wasmtime/src/engine/serialization.rs`, `Metadata { shared_flags, isa_flags }`). On
  `Engine::deserialize*` each enabled ISA flag is checked against the running host
  (`Engine::check_compatible_with_isa_flag`, `engine.rs`): an artefact compiled with `has_avx2 = true`
  loaded on a CPU without AVX2 returns an error (`"compilation setting ... is enabled, but not available
  on the host"`); it does not execute. An artefact compiled on a *weaker* CPU than the host loads only
  if the engine's own flags match; with host detection the engine has the larger flag set, so such an
  artefact has a different compatibility hash (next bullet) and is recompiled.
- `Engine::precompile_compatibility_hash()` hashes triple, compiler flags, **ISA flags**, tunables,
  features and the module version string (`crates/wasmtime/src/compile/code_builder.rs`,
  `HashedEngineCompileEnv`). It is documented as: equal hashes mean artefacts deserialize.

Consequences and decisions:

- **CD-W1 Cache key.** The `.cwasm` cache key (ADR-0019 §2.5, ADR-0026 pre-load) is
  `package sha256 + engine.precompile_compatibility_hash()` and not "wasmtime version + engine config" by
  hand. This makes a CPU change (disk or home directory moved to another machine, a VM live-migrated to a
  different host CPU, `GORCA_CPU_DISABLE`, a microcode/hypervisor change that masks features) an ordinary
  **cache miss**, never a failed load. ADR-0026 §2.4 (Pool) is amended accordingly in r3.
- **CD-W2 A failed deserialize is a miss, not a plugin fault.** `Component::deserialize` returning the
  ISA-mismatch error (or any error on a cache file) deletes the cache entry, increments a counter and
  recompiles in the background pre-load task; the slot stays `Unloaded`/`Cold` and fails open until it
  is ready (ADR-0026 §2.4 pre-load rule: no compile on a writer turn). It does not count as a strike.
- **CD-W3 Packages ship `.wasm`, never `.cwasm`.** Precompiled artefacts are produced on the user's
  machine at install or first enable and cached per user. A distro package that shipped `.cwasm` would
  be compiled on the build machine's CPU (or for a fixed baseline) and either fail (§7 above) or leave
  speed on the table.
- **CD-W4 No hand-tuning of Cranelift flags by default.** We keep host inference. The one exception: with
  `GORCA_CPU_DISABLE=all` the plugin engine is configured with the Cranelift baseline flags
  (`cranelift_flag_set`, `unsafe` in wasmtime's API) in one audited function in `gorca-ext-host`, so the
  portable CI job also exercises portable guest code and its distinct cache key (§6.1). This is the only
  `unsafe` wasmtime configuration we add; it requires correct knowledge of flag names (tested by
  compiling a probe component with it).
- **CD-W5 Fuel calibration is CPU-dependent.** κ_int/κ_bg (ADR-0026 §2.5.1) are measured per core class
  and CPU model; a new CPU fingerprint (the same ISA flag set that is in the cache key plus the CPU model
  string) invalidates the calibration table and the daemon recalibrates in the background, using the
  conservative default until done. WQ-2 already measures P/E cores; this adds "per CPU fingerprint".
- Wasm SIMD in plugin guests is not part of the 0.10 design (unverified in `Config`; check at M18a-2). If it
  is enabled, Cranelift needs SSE4.1-class features for parts of wasm SIMD lowering on x86 and the baseline
  flags of CD-W4 must include them: reopen then.

## 8. The `unsafe` policy and the reviewer checklist

- **CD-9 Containment.** `unsafe` for SIMD exists only in `gorca-cpu/src/arch/` modules (one file per
  architecture and kernel). The workspace lint stays `unsafe_code = "deny"`; those modules carry a
  module-level `#![allow(unsafe_code)]` with a one-line reason. A CI check fails if any other file gains
  `allow(unsafe_code)` outside the allow-list (`gorca-input-xkb`, `gorca-cpu/src/arch/`). Every other
  crate uses only the safe `Kernels` fields.
- **CD-10 Every `unsafe` block has a `// SAFETY:` comment** naming the proof: the `Cap<F>` parameter (feature
  present), the bounds argument (index arithmetic in terms of `len`), and alignment (we use unaligned
  loads only, `loadu`/`ld1`, so no alignment precondition).
- **Reviewer checklist** (the reviewer agent applies it to every PR touching `gorca-cpu` and records the
  result in the review file):
  1. Is every `#[target_feature]` function private, and is each call to it inside a safe wrapper taking
     `Cap<F>` for exactly the features in `enable = "..."`? (feature list in the attribute equals the
     `requires` of the registry entry.)
  2. No direct use of `is_*_feature_detected!` outside `gorca-cpu`; no `cfg!(target_feature)` used as
     detection.
  3. Bounds: all loads/stores are covered by a proof from `len`; the tail (< vector width) is handled by
     the portable code or a masked/overlapping load that stays inside the slice; no read past the end
     "because the page exists".
  4. Equivalence: portable function present; registry entry present; differential test and fuzz target
     present and run in both CI jobs; for floats, the CD-4 declaration.
  5. No SIMD types in public signatures or across target-feature boundaries.
  6. `min_len` justified by a benchmark in the PR; CD-5 numbers present.
  7. No `unsafe` outside `arch/`; no `transmute` of non-POD; `MaybeUninit` use explained.
  8. The speed-up hint exists or the clause is omitted (never invented).
  9. Disable switch honoured: the kernel is reachable only through the table.
  10. A kernel that adds an AVX-512 impl cites the §2.6 evidence.
- The `unsafe` count per crate is tracked in `docs/reports` at each milestone exit (like the plugin
  runtime's "integration code size and `unsafe` count" criterion).

## 9. Alternatives considered

| Alternative | Why not |
|---|---|
| `-C target-cpu=native` in release | SIGILL on other CPUs; shared Debian/Arch binaries |
| Baseline x86-64-v2 or v3 for everything | v2 excludes only very old CPUs but gains little for non-dispatched code; v3 excludes pre-2013 Intel and many low-end/VM CPUs. Kept as an optional variant behind CD-B1 |
| `std::simd` | unstable on the pinned stable toolchain (verified, E0658); revisit when stable |
| `multiversion` crate for everything | same machine code; dispersed detection, harder to force off / enumerate / log from one place; allowed inside `gorca-cpu` where it removes boilerplate |
| Per-call `is_x86_feature_detected!` | the macro is cheap (cached) but it is a load plus branch in the loop and spreads detection; CD-1 |
| IFUNC / `glibc-hwcaps` | `unsafe` linker tricks or a second library for no demonstrated gain |
| Our own `memchr`/UTF-8 validators | they exist, are fuzzed by many users, and dispatch already |
| No SIMD at all | contradicts the owner's directive and leaves 2-8x on byte loops during storms |
| One log line per kernel / WARN level | noise and a false impression of failure; owner asked for one informational line per feature |

## 10. Consequences

- A new small crate `gorca-cpu` and a `bench/cpu/` suite; one `INFO` line at start-up on CPUs lacking a
  used extension; a `GORCA_CPU_DISABLE` diagnostic switch and a doubled CI test matrix.
- `unsafe` grows only inside `gorca-cpu/src/arch/`, reviewable with the checklist in §8.
- The binary stays portable (x86-64-v1 / aarch64) and gets faster on capable CPUs without a rebuild.
- ADR-0026 §2.4 pool bullet changed to key the `.cwasm` cache by the compatibility hash; packages ship
  `.wasm` only (CD-W3); ADR-0019 §2.5 gets a pointer.
- Risks: a stale speed-up hint (freshness gate), a mis-tuned `min_len` (performance guard), a feature a
  hypervisor reports but does not implement (detection comes from CPUID/XGETBV; a SIGILL cannot be
  caught: the `GORCA_CPU_DISABLE` switch and the crash reporter's `active()` dump are the field remedy).

## 11. Metrics to validate

| Id | Metric | Gate |
|---|---|---|
| CD-B0 | dispatch overhead (indirect call + length check) with an empty kernel | ≤ 3 ns p99 |
| CD-B1 | default build vs `-C target-cpu=x86-64-v3` on PX-7 storm and keypress-to-speech | report; open the v3 variant ticket only at ≥ 5 % p99 or ≥ 10 % throughput |
| CD-B2 | each admitted kernel, per input-size class and alignment | ≥ 2x median vs portable at realistic size; CD-5 (≤ +5 % p99 on shortest input) |
| CD-B3 | end-to-end scenario per admitted kernel (event storm, typing echo, browse build) | improved or neutral; kernel not merged otherwise (§5 item 4) |
| CD-T1 | equivalence: every impl vs portable | 0 mismatches in proptest (≥ 10^5 cases per kernel per CI run) and fuzz (≥ 10 min per kernel nightly), ASan clean |
| CD-T2 | whole test suite with `GORCA_CPU_DISABLE=all` | 100 % pass |
| CD-T3 | log lines | snapshot-tested; exactly one per missing feature; none when nothing is missing |
| CD-T4 | `hints.toml` freshness | every hint within 30 % of the latest reference-machine bench |
| CD-W | `.cwasm` produced with `has_avx2` loaded under `-cpu Nehalem` emulation | deserialize error handled as a miss, plugin recompiled, 0 strikes |

## 12. Slices

| Slice | Content | Owner |
|---|---|---|
| CD-0 | `gorca-cpu` crate: `Feature`, `Cap`, registry, `init()`, env switch, log line, `active()`, snapshot tests; no kernels | asm-simd-engineer |
| CD-1 | comparison of hand table vs `multiversion` for one tier-2 kernel; CD-B0; CI matrix (§6.1) and lints (`target-cpu=native`, `allow(unsafe_code)` allow-list) | asm-simd-engineer, test-engineer |
| CD-2 | first kernels admitted through §5 from the profile (text prefix/suffix, ASCII fast path of UTF-16 offsets), `bench/cpu/`, `hints.toml`, fuzz targets | asm-simd-engineer, perf-engineer |
| CD-W | cache key = compatibility hash, miss-not-fault rule, CPU-fingerprint calibration (lands with M18a-2) | wasm-engineer |
| CD-B1 | v3 vs default measurement | perf-engineer |

## 13. Open questions

- **QC-1 (owner)** Is x86-64-v1 the right shipped baseline, or does the owner want v2 given that no
  supported CPU (Debian 13 amd64 users on current hardware) lacks SSE4.2? Recommended: v1 plus dispatch,
  v3 variant only if CD-B1 justifies it.
- **QC-2 (owner)** Is `GORCA_CPU_DISABLE` as a diagnostic (env and daemon flag, no settings key) acceptable?
- **QC-3** Whether to depend on `simdutf` (C++ through FFI, large) for UTF-16 conversion, or write the ASCII
  fast path plus tier-2/3 ourselves: decided by the §5.3 bake-off when the profile shows the need.
- **QC-4** The exact stabilised rules of `target_feature` 1.1 and safe intrinsics on 1.99.1: verify in CD-0
  before the first kernel (not re-verified here beyond the `std::simd` probe).
- **QC-5** The locked wasmtime is 49.x while `refs/wasmtime` is 51.0.0-dev: re-read `engine.rs` and
  `code_builder.rs` on the locked version in CD-W.
