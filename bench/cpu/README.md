# bench/cpu: kernel benchmarks (ADR-0027 CD-B0, CD-B2, CD-B3)

Source: `crates/gorca-cpu/benches/kernels.rs` (criterion). Each kernel group runs the portable
function, the old code where one existed (`naive`, `before`), and every implementation the CPU
can run, per input size.

```text
. ./env.sh
cargo bench -p gorca-cpu --bench kernels -- --warm-up-time 1 --measurement-time 2
python3 bench/cpu/summarize.py run1.txt run2.txt run3.txt   # min over runs, speed-up per case
```

Pin the bench binary to one core (`taskset -c 2 <bench binary> --bench ...`) and run it three
times; `summarize.py` takes the minimum per case. The speed-up hints in
`crates/gorca-cpu/hints.toml` are taken from these tables (rounded down to a stable value).

`results/` holds the raw criterion output of every run that backs a number in
`docs/reports/adr-0027-impl.md`. Files are never overwritten; a rerun gets a new date prefix.
Conditions of each set are in `results/CONDITIONS.txt`.
