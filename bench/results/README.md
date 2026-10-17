# Benchmark and test results

Raw output of the benchmarks, end-to-end latency runs and acceptance gates behind the numbers in
`docs/` and `docs/decisions.md`. Nothing here is deleted when a run fails or a later run supersedes it;
failing and degraded runs stay next to the passing ones.

## Reading the results

- **Conditions matter.** Every run was taken on one development workstation (Debian 13, an RT
  kernel, GNOME Shell 48), usually while other jobs were building, so host load was often 5 to 50.
  Latency gates are unreliable at that load. A run's header or `*-conditions.txt` file states load,
  governor and repeat count where they were recorded. A run that refused or was forced on a loaded host
  is labelled `DEGRADED` in its `summary.md`.
- **Gorca-side time.** The latency numbers measure gorca's own processing from the event or key to the
  start of the speech request, not audio output.
- **Authoritative runs.** The Phase 2 acceptance runs (`acceptance/phase2-*/summary.md`) are the
  closest to authoritative; the earlier `dryrun` and `--force` directories are kept as evidence of
  attempts that failed or ran on a loaded host.
- **Scrubbing.** Host paths were replaced with `<repo>` and the login name with `user`.

## Index

| Directory | What it holds |
|---|---|
| `acceptance/` | Phase 2 gate runner output (`bench/acceptance/phase2.sh`): one directory per attempt with `summary.md`, logs and per-step results. |
| `e2e/` | End-to-end latency scenario results (focus, keys, typing, browse, terminal), many per-milestone runs and flake evidence. |
| `b1/`, `b1-adopt*/`, `b1-mirror-fill/` | Bake-off B-1 (mirror fill policy): judge output, adoption gates and review probes. |
| `b2-ingress/` | Bake-off B-2 (event drain and ingress ring): candidate runs. |
| `b5/` | Bake-off B-5 (runtime topology: single, lanes, lanes-rt). |
| `doc-mb/`, `mirror-mb1/`, `web-prep/` | Micro-benchmarks of the document model and mirror store. |
| `ext-host/`, `wasm-hook/` | WASM extension host: hook overhead and the hostile-extension backstop. |
| `m9/` | Chromium accessibility activation probes. |
| `qe4/` | A text-editor quirk probe. |
| `resource-usage/` | Whole-daemon CPU and memory under event storms (`docs/reports/resource-usage.md`). |
