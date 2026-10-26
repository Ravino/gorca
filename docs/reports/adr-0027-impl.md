# ADR-0027 r2 implementation report (2026-10-26)

Author: asm-simd-engineer. Branch `worktree-agent-a207fb97330f74ff3`. Raw numbers: `bench/cpu/results/` (conditions in
`CONDITIONS.txt`: host loaded, load average 26-31, cpu2 pinned, governor powersave; min over 3 runs).

## What exists

- `crates/gorca-cpu`: `Feature` (35 x86/aarch64 features incl. AVX-512 family, AVX-VNNI, BMI2, POPCNT, SSE4.2, AVX10.1 via
  CPUID 7.1/0x24 + XCR0, NEON/SVE/SVE2 detection stubs), `FeatureSet`, `detect()`, `cpu_info()`, `raw_isa()`;
  `kernel_impl!` (one list -> `#[target_feature]` attributes, `REQUIRES`, private trampoline, `call_checked`,
  compile-time check that literal == `Feature`), `kernel_spec!`, `Impl`/`KernelSpec::resolve`, `Kernels` table via
  `declare_kernels!`, `Cap<F>` + `marker::*`, `DisableList` (`all|portable|<list>`, unknown names reported),
  `init(disable)` / `kernels()` / `active()` / `log_report()`, report builder with `hints.toml`.
- Tests: 2^k feature-mask resolver test (CD-T5), differential proptest (20k-50k cases, every length/offset 0-140, all
  alignments, 2 Mi element sparse difference, 255-step counter boundary) of every impl against an independent oracle,
  log-line snapshots (none/one/many/disabled/no-hint/unknown/`all`), hybrid-core symmetry test (16 CPUs agree),
  daemon test `tests/cpu_dispatch.rs` (flag, env, flag-over-env, `init` ordering shown by `table resolved by: init`).
- Daemon: `gorca_cpu::init` is the first statement of `main` (flag from a minimal argv scan, else
  `GORCA_CPU_DISABLE`); `log_report()` after the logger; `gorca --version --verbose` prints the report;
  `--cpu-disable=LIST`; the plugin engine's `portable` now comes from the same parsed list (`all`/`portable`) and
  still uses `Config::target(host triple)` (CD-W4).
- Real sample: `cpu: Intel(R) Core(TM) Ultra 9 285H ...`, features in use sse3..avxvnni aes; with `--cpu-disable avx2`:
  `cpu: AVX2 disabled by GORCA_CPU_DISABLE: utf8-scalar-count, common-prefix-chars, utf8-extend-chars run portable; with AVX2 they would be faster (utf8-scalar-count ~4.5x, common-prefix-chars ~1.7x, utf8-extend-chars ~4x)`.

## Kernels (all AVX2 on this host; no profile of PX-7 exists, CD-P still open, so sites chosen by code reading + microbench)

| Kernel | Call sites wired | Impl | Speed-up over portable (1-4 KiB) | Over the old code |
|---|---|---|---|---|
| `utf8-scalar-count` | connector text fetch (`text/mod.rs`, screen length and clip), `gorca-text` UTF-8 raw->scalar | tier 3 intrinsics (tier 2 autovec was 6x *slower* than std) | ASCII 2.6x@256 B, 4.8x@1 KiB, 5.8x@4 KiB; Cyrillic 2.1x@256, 4.4x@1 KiB, 5.6x@4 KiB | portable = std `chars().count()`, so identical |
| `common-prefix-chars` | typing-echo `leaf_diff` in `gorca-core` text/caret.rs | tier 3 intrinsics | 1.6x@64, 1.7x@256, 1.9x@1024, 1.5x@4096 chars | 5-6x over `zip().take_while()`; portable alone is 3-4x |
| `utf8-extend-chars` | `leaf_diff` decode (`utf8_to_chars`) | tier 2 (safe Rust compiled with AVX2) | ASCII 3.9x@256, 4.3x@1 KiB, 4.1x@4 KiB, 2.9x@64 KiB; non-ASCII 1.0-1.7x (no regression) | portable = std |

Model of `leaf_diff` (decode + prefix, CD-B3): 200 chars 2.4-3.8x, 1000 chars 1.8-3.8x, 5000 chars 4.0-4.5x faster; 40 chars
within noise (0.86-1.43x, both paths below `min_len`). Dispatch overhead (CD-B0): inline portable 1.75 ns, through the table
into the AVX2 trampoline 1.58-1.99 ns for 8 B (gate 3 ns). Absolute savings are tens to hundreds of ns per call, so these
serve CPU headroom under storms and the owner's directive, not keypress p99 (ADR section 5 honest note).

Gate notes: `common-prefix-chars` is under 2x over its own new portable function (L1-bandwidth bound, ymm vs xmm loads);
admitted against the replaced code. `min_len`: 128 B / 64 chars / 64 B.

## Gates run

- `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`: clean (before the last edits, which were
  re-checked with `-p gorca-cpu` clippy and daemon tests).
- `cargo xtask deps`: dependency, environment, entry, legacy, host-feature and secret rules ok. `xtask strings` fails on
  `crates/gorca-daemon/src/lib.rs:378` (the private-namespace warning from the base commit 1aee82fa, not this change).
- Workspace `cargo test` (first pass): 3465 passed. Failures were environmental: the shared target was wiped by the
  disk watchdog mid-run (missing rlibs, doctests of bench crates), and `cpu_dispatch` ran against a `gorca` binary
  from another agent's build (`CARGO_BIN_EXE_gorca` is `target/debug/gorca`, shared); `term_parity` is a load flake.
  Reruns alone: `-p gorca-cpu`, `cpu_dispatch`, `term_parity` pass.
- Touched crates with and without `GORCA_CPU_DISABLE=all`: see the end of this file.

## Open / not done

- CD-P profile, CI matrix jobs, Intel SDE jobs, `bench/cpu/hints.toml` freshness gate (hints live in
  `crates/gorca-cpu/hints.toml`), CD-B1 (v3 build), `.cwasm` cache key (CD-W) and the `unsafe` allow-list lint.
- aarch64: code paths are cfg-gated and untested (no aarch64 std installed in `.toolchain`; not added to avoid
  changing the shared toolchain).
- Final full-workspace run (normal and `GORCA_CPU_DISABLE=all`) to be done once by the orchestrator.
