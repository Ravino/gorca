# ADR-0027 review (runtime CPU feature dispatch), r1

Reviewer, 2026-10-24, main 1853506a. Paper review only (no build: disk space). Sources checked:
`docs/adr/0027-runtime-cpu-feature-dispatch.md`, pinned wasmtime 49.0.3 / cranelift 0.136.3 in
`.toolchain/cargo/registry/src/*/`, `crates/gorca-ext-host/src/wasm.rs`, host `/proc/self/timerslack_ns`,
`valgrind --help`, `docs/reports/` (profiling evidence).

**Verdict: ACCEPT WITH CHANGES.** The policy (resolve once, portable path is the spec, table in one
crate, v1 baseline, `.cwasm` keyed by the compatibility hash, packages ship `.wasm`) is right. The
type-state safety story does not survive the table edge, the tier-2 mechanism can silently produce
scalar code, the test plan names tools that cannot do what it claims, and §7 contains two factual
errors about wasmtime 49.

## Findings (ranked)

### H-1 `Cap<F>` does not reach the call: the soundness proof is the resolver, not the type (§2.3, §8 item 1)
Since target_feature 1.1 (stable 1.86) a safe `#[target_feature]` fn **cannot be coerced to a safe
`fn` pointer** (only to `unsafe fn`), and a ZST `Cap<F>` cannot be captured into a `fn` pointer.
`Kernels` holds `fn(&[u8], &[u8]) -> usize`, so every SIMD impl needs a trampoline
`fn t(a, b) -> usize { unsafe { avx2_body(a, b) } }` whose only safety argument is "the resolver
installs `t` only after detecting `requires`". `Cap<F>` never appears on that path. The one thing that
makes it sound is `Impl.requires` == the `enable = "..."` list, and §8 item 1 checks that by eye.
Failure: `enable = "avx2,bmi2"`, `requires: &[Avx2]`; on a VM that masks BMI2 (the ADR's own example
in §2.3) the resolver installs the impl and `shlx/pdep` raises SIGILL, which the crash-isolation goal
cannot catch.
Fix: generate both from one token list with a declarative macro
(`kernel_impl!(text_common_prefix, [avx2, bmi2] => body)` emits the `#[target_feature]` fn, the
trampoline and the `Impl { requires }`); drop the claim that `Cap` carries the invariant (or keep
`Cap` only as the argument of the macro-generated constructor); add a resolver test over all 2^k
feature masks asserting `installed.requires ⊆ mask \ disabled`. Record the rustc rules (no safe
fn-pointer coercion, no `#[inline(always)]` on `#[target_feature]` fns, `unsafe` call from a
non-matching context) in §2.4 instead of deferring them to QC-4. Note: rust-src is not installed in
`.toolchain`, so these rules come from the 1.86/1.87 release notes, not a local probe; CD-0 must
still run the probe.

### H-2 Tier 2 can compile to scalar code without anyone noticing (§2.2 item 2, §2.4 inline bullet)
Autovectorisation with AVX2 only happens if the shared scalar body is inlined **into** the
`#[target_feature]` function. `#[inline]` is a hint; a body that LLVM keeps out of line (size,
iterator adaptors from another crate, recursion) is compiled once for the SSE2 baseline and the
"AVX2" impl is the same code plus a trampoline. The §6 performance guard only fails when the impl is
*slower* than portable, so this passes CI and the log line then advertises "~3x" from a stale hint.
Fix: shared bodies are `#[inline(always)]` (allowed on non-target-feature fns), and CD-B2 must show
a speed-up ≥ the admission gate on the runner for every impl, or a codegen test (`cargo asm` /
`objdump` on the release artefact) asserts at least one `ymm` instruction in each AVX2 impl.

### M-1 Disable list vs lazy init: `--cpu-disable` can be silently ignored (CD-1, §6.1)
CD-1 lets a kernel run before `init()` and builds the table lazily; §6.1 adds a daemon flag mapped
to the same list. If any kernel runs during argument/config parsing (UTF-8 validation of the
config file is a listed candidate), the table is resolved from the env alone and the flag is
dropped. Fix: the flag is applied by setting the disable list before the first kernel call (parse
argv with no kernels on that path), and `init(disable)` returns an error/INFO line if the table was
already resolved with a different list; CI asserts `active()` after the flag, not only after the env.

### M-2 The test plan names tools that cannot do the job (§6.1, §6.2)
- `valgrind --tool=none ... -cpu below its requirement`: valgrind has no `-cpu` option
  (`valgrind --help` on this host lists none) and does not emulate a weaker CPU; it also does not
  support AVX-512. Remove.
- `qemu-x86_64 -cpu max (AVX-512)`: qemu is not installed here, and I could not confirm that the
  TCG `max` model exposes AVX-512 on the Debian 13 qemu; the ADR must cite it or drop it. Intel SDE
  (`-skx`, `-icx`, `-spr`, and `-nhm`/`-p4p` for old CPUs) is the dependable path for AVX-512 and for
  "below requirement" runs.
- The "default (best available on the runner)" CI job is non-deterministic across runner hosts; pin
  the expected `active()` per job or the "job ran the path it names" assertion is vacuous.
Fix: CI matrix = native default + `GORCA_CPU_DISABLE=all` + per-step disables on the reference
runner, plus SDE jobs (Nehalem-class, Haswell, SKX/ICX) running the equivalence tests and the
whole-suite `active()` check; the "unsupported instruction guard" becomes "full test suite under SDE
`-nhm` finishes with no SIGILL and `active()` == portable for every AVX kernel".

### M-3 §7 errors about the pinned wasmtime 49.0.3
- Wasm SIMD is **on by default** (`Config::wasm_simd`, config.rs ~L1100: "This is `true` by default";
  x86_64 baseline for Cranelift SIMD is SSE2). `gorca-ext-host/src/wasm.rs` does not disable it. The
  "not part of the 0.10 design (unverified)" bullet is wrong: either disable it explicitly in the
  0.10 engine or state that it is on.
- CD-W4 needs no `unsafe`: host inference happens only when no triple is given
  (`wasmtime-internal-cranelift-49.0.3/src/isa_builder.rs:30-35`, `infer_native_flags` iff
  `!triple_specified`). `Config::target(<host triple>)` (safe, config.rs:404) yields the baseline
  flag set. Using `cranelift_flag_set` (unsafe, config.rs:1668) to turn off every inferred flag one
  by one is the error-prone path the ADR wants to avoid.
- CD-W1 lists `GORCA_CPU_DISABLE` among CPU changes that make a cache miss; only `=all` reaches the
  plugin engine (CD-W4). Say so, or map each disabled feature to its Cranelift flag.
Verified correct on 49.0.3: `check_compatible_with_isa_flag` (engine.rs:547) and
`precompile_compatibility_hash` (engine.rs:886, hashes `compiler.isa_flags()`, code_builder.rs:854).
So a CPU change is a cache miss and a mismatched artefact is rejected, not executed. QC-5 can be
closed for these two facts.

### M-4 No profile exists for any candidate, yet CD-2 commits two kernels (§5.1, §12)
`docs/reports/` contains no gorca-side CPU profile (no perf/flamegraph run) naming UTF-8, prefix
scan or UTF-16 offsets. §5 item 1 is the right gate, but slice CD-2 names "text prefix/suffix, ASCII
fast path of UTF-16 offsets" as the first kernels, and §5 item 4's "owner directive" exception lets a
kernel merge with no end-to-end gain. The ADR itself says the typing-echo diff is within noise, and
CD-2's own MIN_LEN rule (~100 ns of work) will route those tens-of-bytes diffs to the portable path.
Fix: CD-2 starts with a perf-engineer profile of PX-7 storm, typing echo and browse build; kernels
are named from it. Limit the owner-directive exception to tiers 1-2 (no hand intrinsics, no new
`unsafe` beyond the macro trampoline). For UTF-8 validation, note that zbus already validates
strings during deserialisation; a `simdutf8` call of ours would be a second pass, not a replacement.

### L-1 Checklist gaps (§8)
Concrete overall. Add: (a) `requires`/`enable` generated from one list (H-1); (b) evidence that each
impl actually vectorised (H-2); (c) no `#[target_feature]` fn is `pub` or reachable via a safe fn
pointer except through the macro trampoline; (d) the impl is listed in the 2^k resolver test.
AVX/SSE transition (failure mode 3) needs no rule: LLVM inserts `vzeroupper` on exit from VEX code.

### L-2 Dispatch overhead
Design is right: fn pointer copied into the actor, length check inline, no `OnceLock` on the hot
path. CD-B0 must measure through the trampoline (indirect call + direct call into the
non-inlinable `#[target_feature]` fn), not an empty `fn`.

## QC-1: baseline
Recommend **x86-64-v1** (agree with the ADR). Debian 13 amd64 is a v1 distribution; v2 (SSSE3,
SSE4.1/4.2, POPCNT, CX16) excludes Core 2, AMD K8/K10 (Phenom II has no SSSE3/SSE4.1), first-generation
Atom, and VMs on the default `qemu64` model, all plausible on donated or old machines that blind
users rely on. The only code that would gain from v2 is non-dispatched parsing and message passing,
where the ADR's own CD-B1 rule decides. A SIGILL before `main` on a screen reader means no speech
and no way to read the error. Keep v1; ship a v3 variant only if CD-B1 passes.

## QC-2: `GORCA_CPU_DISABLE`
Accept as a diagnostic (env + daemon flag, no settings key), with M-1 fixed and unknown names
reported. Add one rule: the disable list is printed in the `active()` dump and the crash report.

## Required before acceptance
H-1, H-2, M-1, M-2, M-3 text changes; M-4 slice CD-2 re-scoped to "profile first".
