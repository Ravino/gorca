# web-graph research prototype

Reference code for `docs/reports/2026-10-12-math-web-graph-model.md`.
Run `. ./env.sh` from the repo root first.

| File | What |
|---|---|
| `gen.py` | Seeded synthetic DOM-like a11y tree generator (10k/100k nodes, landmarks, card grids from templates, tables, forms, comment threads, div-soup wrappers) with ground-truth component families |
| `navindex.py` | Mirror arena + navigation indexes: `OMIndex` (order-maintenance labels + per-category sorted sets), `TreapIndex` (implicit treap over preorder with size/mask/count aggregates), `BruteIndex` oracle |
| `test_navindex.py` | Randomised differential test vs the oracle, plus an adversarial relabel test |
| `bench_navindex.py` | Mutation workload, Python timings, and trace generation for the Rust bench |
| `rust-bench/` | Std-only Rust port of both index designs; replays traces and checks every expected answer |
| `diff.py`, `test_diff.py` | Keyed child diff (LIS), unkeyed tree matching, cursor restoration, optimal re-fetch cover DP; tests and re-render evaluation |
| `embed.py` | Structural embeddings (canonicalisation, WL features, dedup + kNN clustering) and "next similar" quality evaluation |
| `bench_ann.py` | numpy / faiss / hnswlib / usearch build, query, recall and update cost on the real vectors |
| `layout_fp.py` | MinHash page-layout fingerprint experiment (negative result) |

```sh
cd research/web-graph
python test_navindex.py && python test_diff.py
python bench_navindex.py gen 100000 traces/nav100k.trace 5000 50000
(cd rust-bench && cargo run --release -- ../traces/testvec-nav2k.trace ../traces/nav100k.trace)
(cd rust-bench && cargo run --release -- --adversarial 100000)
python embed.py 100000 && python bench_ann.py && python layout_fp.py
```

`traces/testvec-nav2k.trace` is the committed test vector: initial tree, mutations, and
queries with expected answers. Larger traces are regenerated and are not committed.
