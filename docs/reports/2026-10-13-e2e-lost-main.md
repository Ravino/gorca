# e2e `lost` gate failures on main (a8686d0): noise plus an accounting bug in the harness

Date 2026-10-13. Author: perf-engineer. Host: Core Ultra 9 285H, 16 CPUs, powersave, RT kernel, load 10-15.

## Verdict

1. **Not a daemon regression.** The failures follow host load, and the daemon at the `daemon-r5` commit (c79b883) loses probes at the same rate as main (a8686d0) once the host is loaded.
2. **No bisect was needed.** The two endpoints are statistically indistinguishable, so there is no commit to find. Evidence below.
3. **`lost` was the wrong accounting for stress scenarios.** A probe that is overtaken by the next focus event is dropped by design; the harness counted it as lost.

## Evidence

Reviewer runs (`bench/results/e2e/pw-review/main-r1`, `main-r2`, in the main checkout, not committed here):

* main-r2 L3-50k lost per repeat: min 0, median 9, max 11. In the same run the harness-side floor `l0_ms` (emit to bus arrival, which involves no reader) reached p99 4.2 ms, max 8.8 ms, and `storm_max_lag_ms` reached 63.7 ms. The host was descheduling the harness processes themselves.
* main-r1 L3-50k lost max 1 (one repeat), L4 lost 0. main-r2 L4 lost max 2 with `l0_ms` max 9.2 ms.

Controlled A/B, same harness binary, only the reader differs (`--only L3-50k,L4 --repeats 6`, interleaved):

| condition | reader | L3-50k lost (min / median / max per repeat) | L4 lost max |
|---|---|---|---|
| natural load (ab-*, ~11-13) | r5 c79b883, 2 runs x 6 | 0 / 0 / 0 and 0 / 0 / 0 | 0 |
| natural load | main a8686d0, 3 runs x 6 | 1 repeat of 18 lost 1 probe, rest 0 | 0 |
| 12 busy loops started by this session, load ~25 (load-*) | r5 | 15 / 28.5 / 122 and 10 / 11.5 / 53 | 6 |
| same | main | 8 / 30.5 / 78 and 21 / 94 / 289 | 40 |

Induced load makes both readers lose probes, with a 10x spread between identical runs (`storm_max_lag_ms` up to 7 s, i.e. the load generator itself starved). At natural load the 1-in-18 vs 0-in-12 difference is not significant. The earlier `daemon-r5` and `p0-r1` passes were repeats that happened to land on quiet moments: a 150-sample `--quick` run only has a few chances to see a stall, and the L3-50k repeat is 1200 probes at 8-16 ms gaps, so any stall longer than the gap costs a probe.

## Why a stalled reader drops probes (by design)

ADR-0001 R-1: the per-app ingress slot `Focus` keeps only the newest node. If the app actor does not drain between two focus events (8-16 ms apart), the older one is replaced and never spoken. CLAUDE.md: "Stale speech is cancelled immediately when a newer event arrives". The ADR-0001 sentence "`FocusChanged` is never dropped" refers to overflow (it is never shed as a background event), not to supersession. The harness gate `lost == 0` (README: "focus events are lossless-class") conflated the two.

## Fix (harness only, in this branch)

`crates/gorca-bench/src/e2e/scenarios.rs`: `score_series` takes an `Accounting` mode.

* `Strict` (L1, L1s): every unspoken probe is `lost`, unchanged.
* `Coalescing` (L3-10k, L3-50k, L4): an unspoken probe with a spoken successor is `superseded`; only an unspoken probe with no spoken successor is `lost` (the newest focus never reached speech, or the reader spoke nothing). New scalars `superseded` (info) and `superseded_max_run`, gated at <= 10 in every repeat (about 120 ms of staleness at the probe gap), in `bin/e2e.rs`. Unit test `accounting_separates_superseded_from_lost`.
* README gate table updated.

Validation: with 8 busy loops (load ~21), main reader, `fixed-head-3`: L3-50k lost 0 in all 6 repeats while up to 71 probes per repeat were superseded (max run 5); L4 lost 0, max run 1. Sensitivity: a reader that speaks nothing, or never speaks the final probe, still fails `lost` (unit test).

## Residual risks

* Supersession now hides a daemon that randomly drops mid-stream focus events under stress; `superseded_max_run` only bounds consecutive ones. A stronger check would need a daemon-side counter of coalesced focus events, which lives in `gorca-core` (not edited here). Suggested: expose it in the daemon's stats so the harness can assert `superseded == daemon_coalesced_focus`.
* Tail latency under load is a separate matter: with induced load L3-50k p99 reached 15-26 ms for both readers (host starvation, not gorca CPU: `l0_ms` p99 was 5-10 ms with no reader involved).
* Results of the reviewer's runs are not copied into this branch.

Raw data: `bench/results/e2e/{ab-r5-*,ab-head-*,load-r5-*,load-head-*,fixed-head-3}`.
