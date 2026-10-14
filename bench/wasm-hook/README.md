# wasm-hook: component-model hook overhead (ADR-0008)

Measures what a per-engine web quirk hook costs when it runs as a wasmtime component, against the
same Rust logic called natively. The logic lives once in `logic/` and is linked into both arms.

- `wit/quirk.wit`: the world (batch `normalize`/`classify`, per-key `on-key`/`present`, a `walk`
  export that calls the host `get-node` import k times, `trap`/`spin` fault injection).
- `guest/`: the component (`wasm32-wasip2`, wit-bindgen 0.46).
- `host/`: wasmtime 49.0.3 embedding plus the native baseline; prints TSV.

It is a standalone cargo workspace so wasmtime never enters the gorca lockfile (ADR-0001 §11 rule 1).

```sh
. ./env.sh                         # from the repo root
cd bench/wasm-hook
cargo build --release -p quirk-guest --target wasm32-wasip2
cargo build --release -p quirk-host
taskset -c 3,4 ./target/release/quirk-host target/wasm32-wasip2/release/quirk_guest.wasm 200000 \
  > ../results/wasm-hook/<date>-pcore.tsv
```

Columns: `case n p50_ns p99_ns p999_ns max_ns mean_ns`. `n` is the batch size (deltas), the walk
length (host calls) or 1/0 for single calls. Engine modes: `epoch` (epoch interruption on),
`fuel` (fuel metering), `pool+epoch` (pooling allocator, 32 MiB memories). Two CPUs are pinned
because the epoch ticker thread needs its own core. Analysis: `docs/reports/wasm-hook-overhead.md`.
