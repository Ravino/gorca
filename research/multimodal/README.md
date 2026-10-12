# multimodal research prototype

Reference code for `docs/reports/2026-10-12-multimodal-vectors-som.md`.
Run `. ./env.sh` from the repo root first, then `python fetch_models.py` once (≈1 GB into the gitignored `.models/`).
Everything runs on the CPU and locally, with no network at run time.

| File | What it does |
|---|---|
| `encoders.py` | ONNX Runtime wrappers: CLIP B/32 (fp32/int8), SigLIP B/16 int8, MobileCLIP-S0, multilingual-e5-small int8, MiniLM-L6 int8. Defines the full preprocessing contract. |
| `icons.py`, `icons_selective.py` | Names unlabeled buttons (zero-shot, closed UI vocabulary) and recognises the same control on real Adwaita icons; latency; accuracy vs coverage with abstention |
| `ocr.py` | Tesseract on rendered single-line UI labels (fonts, sizes, themes, upscaling): CER and latency |
| `corpus.py` | Builds real-text a11y-like trees from the local GNOME help (Mallard) in English and Russian |
| `text_bench.py` | Text-encoder latency, cross-lingual alignment, kNN topical coherence; writes `out/text_emb_C.npy` |
| `features.py` | Per-node modality blocks (struct, text, geometry) and weighted product-metric fusion |
| `maps.py` | Batch SOM (anchored incremental update with hysteresis), GTM, Growing Grid, quantile grid snap, metrics (QE, TE, trustworthiness, continuity, purity, occupancy), stability |
| `som_bench.py` | Static and dynamic (10 mutation steps) comparison of SOM, GTM, Growing Grid, and PCA/t-SNE/UMAP + grid snap |
| `som_scale.py` | SOM cost at 10k/100k × 384; Hamerly-bound certificate for incremental reassignment (sound, but ineffective in high dimensions) |
| `som_realtree.py` | SOM over the real browser dumps in `../web-graph/real-trees` (struct + geometry) |
| `som_demo.py` | Prints the 8×8 help-corpus map with spoken cell labels (`out/som_map_C.txt`) |
| `ann_bench.py` | Flat vs IVF-PQ (+ re-rank) vs usearch HNSW f16/i8 at 100k and 1M × 384 |
| `test_maps.py`, `testvec-som.json` | Plain-assert tests; Rust port test vector (4×4 SOM on 300×8 points, plus a hysteresis update) |

```sh
cd research/multimodal
python test_maps.py
python icons.py --threads 4 && python icons_selective.py && python ocr.py
python text_bench.py && python som_bench.py && python som_scale.py && python som_realtree.py && python som_demo.py
python ann_bench.py 100000 1000000
```

Results land in `out/*.json`. The embedding `.npy` files are not committed.
