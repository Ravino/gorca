# Multimodal node vectors, vector search and Kohonen maps for gorca

Author: math-researcher · Date: 2026-10-12 · Status: research report with measured numbers
Code: `research/multimodal/` (see its README). Raw results: `research/multimodal/out/*.json`.
Builds on `docs/reports/2026-10-12-math-web-graph-model.md`.

## 0. Summary and what to build now

**Recommendation.**

1. **Core (build now, small).** Add the following:
   - A **content-keyed feature store** tied to the mirror lifecycle, with a **privacy eligibility predicate** that lives in core.
   - A **`VectorIndex` trait** with an exact f32 SIMD scan as the only implementation for now.
   - A **`MapSurface` state machine** (8×8 grid navigation, cell labels, zoom), backed by a pure-Rust **batch SOM with anchored incremental refit and hysteresis**. It ships behind a feature flag until blind users have tested it.
2. **Perception sidecar** (`gorca-perceive`), not WASM. It is a separate native process with no network that hosts ONNX Runtime models. Start with the multilingual text encoder (e5-small int8, 1.8 ms per string). Add Tesseract OCR and the SigLIP icon namer once pixel capture is verified.
3. **First user-facing features, in order.** Semantic find (text only, works today); naming unlabeled icon buttons, as a cascade of label memory, then OCR, then SigLIP with abstention; OCR fallback. Image captioning comes last and is on demand.

**Challenge to the orchestrator's hypothesis.** The split is right: generic infrastructure goes in core, encoders are pluggable. Three amendments:
- **(a)** Encoders must be **out-of-process native sidecars, not WASM components**. Model files are 50–400 MB, inference needs SIMD and threads, and ORT and candle do not run well in wasmtime. A process boundary gives the crash isolation and a no-network sandbox.
- **(b)** The store must be keyed by **content hash, not node identity**. Re-renders and duplicates dominate; see the previous report, where 100k nodes collapse to about 1.7k unique skeletons.
- **(c)** The SOM is not "the" map. Core should own a **generic grid surface** with stable cells. SOM is the default layout algorithm behind it because it measured best here; it is not fixed as an API concept.

## 1. Assumptions, and proven versus heuristic

| Claim | Status |
|---|---|
| Fusing by concatenating unit blocks equals a weighted product of chordal metrics (section 3) | proven (algebra) |
| Anchored batch-SOM drift bound, and the hysteresis dwell-time bound (section 6.4) | proven (short proofs given) |
| Hamerly-style exact reassignment certificate is sound | proven, and checked by an assert in `som_scale.py` |
| SOM beats GTM, Growing Grid and t-SNE/UMAP+snap for this use | measured on one real corpus (GNOME help, 1.9k items) plus real browser test trees (struct and geometry only) |
| A semantic map helps blind users navigate | **unverified hypothesis**; it needs a user study |
| Icon naming accuracy | measured on Adwaita symbolic icons only; web icon fonts, logos and photos are untested |
| ANN crossovers | measured on synthetic vectors shaped like e5 embeddings (cluster centres from real embeddings, real PCA spectrum) |
| How common unlabeled controls are on the web | **not measured here**. From memory, WebAIM Million 2024 reports empty buttons on about 28% of home pages and missing alt text on about 55%; this needs citation checking. The real-trees dumps are well-labelled test pages (1 unnamed image among 3.5k controls). |

Hardware: Core Ultra 9 285H, CPU only. An RTX 5060 mobile is present and the nvidia module is loaded, but there is no `nvidia-smi`, no `/dev/nvidia*` and no onnxruntime-gpu, so **GPU numbers were not measured**. Timings are ONNX Runtime 1.31 CPU EP; numpy timings are single-threaded with BLAS pinned to one thread.

## 2. Modalities: what we can get cheaply and locally

| Modality | Source | Cost | Notes |
|---|---|---|---|
| Structure | mirror: role, states, depth, parent role, child count, WL subtree hash | free, already in core | Strong at grouping "kinds of things" (role purity 0.99 on real trees), weak at semantics (section 6) |
| Text | name, description, text content (AT-SPI Text) | e5-small int8: **1.8 ms p50 / 3.4 ms p99** per short string (4 threads); MiniLM 0.9 ms | Russian is a requirement, so the encoder must be multilingual: e5 en→ru top-1 is 0.62 vs MiniLM 0.02 |
| Geometry | `Component.GetExtents` | one IPC call per node, which the mirror already batches | Gives visual neighbours, reading order and "toolbar row" grouping |
| Visual | screen pixels of a node's bounds | needs capture (section 2.1) plus an encoder at 39 ms per crop (SigLIP q8, 4 threads), 113 ms on 1 thread | Only for nodes that need it (unnamed controls, images, inaccessible canvases). Never bulk. |
| Interaction | gorca's own focus, activation and dwell logs | free | The most privacy-sensitive signal; per-app, opt-in, local |

### 2.1 Pixels under GNOME 48 Wayland (checked in source; ask linux-platform-engineer to measure)

- `org.gnome.Shell.Screenshot`: **not usable**. `DBusSenderChecker` allows only `org.gnome.SettingsDaemon.MediaKeys` and `org.freedesktop.impl.portal.desktop.gnome`, unless the shell runs in unsafe mode (`refs/gnome-shell/js/ui/screenshot.js`).
- **`org.gnome.Mutter.ScreenCast`: usable.** `check_permission` only compares the caller with the peer that created the session (`refs/mutter/src/backends/meta-screen-cast-session.c`), so there is no allowlist. `RecordArea` and `RecordWindow` deliver PipeWire frames. GNOME Shell shows a recording indicator while it runs (`js/ui/status/remoteAccess.js`). This is a feature for trust, but the indicator would be permanently visible if we kept a stream open.
- Portal `Screenshot` with `interactive=false` shows a one-time dialog, and the grant is stored for *all* unsandboxed apps (`refs/xdg-desktop-portal/desktop-portal/screenshot.c`). It produces a full-screen PNG on disk, which is slow and puts pixels on disk. Fallback only.
- Portal `ScreenCast` with `persist_mode=2` shows one dialog and returns a restore token. It is the portable path for non-GNOME desktops.
- Web content: the browser source layer (CDP or an extension) can capture element screenshots without the compositor. See `2026-10-12-web-source-layer.md`.
- **Open for linux-platform-engineer:**
  - ScreenCast session setup latency and first-frame latency;
  - whether a short-lived on-demand `RecordArea` (open, grab one frame, close) stays under about 150 ms;
  - indicator behaviour for `is-platform` sessions;
  - whether the cursor and other windows can be excluded.

## 3. Fusion: a formal choice

Let each modality m give a unit vector u_m (missing → absent). With weights w_m, the fused vector
v = [w_1 u_1, …, w_M u_M] / sqrt(Σ w_m²) gives

  ||v − v'||² = Σ_m w_m² ||u_m − u'_m||² / Σ w_m² = Σ_m w_m² · 2(1 − cos_m) / Σ w_m².

This is a **weighted product of per-modality chordal metrics**, so it is a true metric on the product of spheres. Missing modalities are handled by renormalising over the blocks that both items have. That is not a metric across different presence patterns (the triangle inequality can fail), which is acceptable for k-NN ranking and SOM, but not for HNSW correctness guarantees.

Options:

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Early fusion (concatenate, fixed weights) | one index, one metric; SOM needs a single space | changing one encoder re-embeds everything; weights are global | **use for maps** (small N, rebuilt cheaply) |
| Learned joint projection (CCA or contrastive head) | best joint geometry | needs labelled pairs we don't have, and churns with encoders | not now |
| **Late fusion** (one index per modality, rank fusion such as RRF or a weighted score sum) | encoders swap independently; missing modalities are natural; per-modality privacy | k queries per lookup | **use for search** |

Measured ablation (8×8 SOM on the help corpus; page purity is measured in text space):

| Blocks | page purity | app purity |
|---|---|---|
| text only | 0.252 | 0.695 |
| structure only | 0.151 | 0.538 |
| structure + geometry | 0.152 | 0.536 |
| fused (1, 0.5, 0.25) | 0.232 | 0.673 |
| fused text-heavy (1, 0.25, 0.1) | 0.247 | 0.717 |

Text carries the semantics. Structure splits the map by kind: in the fused map the headings occupy one half and the paragraphs the other (`out/som_map_C.txt`). The weights therefore *are* the user-visible "lens": expose two or three presets (By topic, By kind, Mixed) rather than a slider.

## 4. Local models (measured, CPU)

| Encoder | Use | Latency | Quality |
|---|---|---|---|
| multilingual-e5-small int8 (118 MB) | text, ru+en | 1.8 ms p50 / 3.4 ms p99 per string (4 threads) | kNN-10 app agreement 0.80; en→ru top-1 0.62 |
| all-MiniLM-L6 int8 (22 MB) | text, English only | 0.9 ms | app agreement 0.83; cross-lingual 0.02 |
| SigLIP B/16 int8 (vision 93 MB) | icon naming | 39 ms per crop (4 threads), 113 ms (1 thread) | icon top-1 0.54–0.61, top-3 0.70–0.75 |
| MobileCLIP-S0 fp32 | icon naming | 18 ms (4 threads), 50 ms (1 thread) | top-1 0.37–0.43 |
| CLIP B/32 fp32 / int8 | icon naming | 29 / 13 ms | top-1 0.09–0.21, which is **unusable** for icons |
| Tesseract 5.5 LSTM (eng) | OCR of one line | 6–18 ms per line (1 thread) | CER 0.3–1.8% |

ORT int8 is the realistic CPU path. candle (pure Rust) supports SigLIP and e5 but has no int8 kernels as mature as ORT's; prefer the `ort` crate and keep candle as a fallback. A GPU would make captioning VLMs interactive. That is untested here and must stay optional.

## 5. Use cases ranked by value × feasibility

| # | Use case | Value | Feasibility (measured) | Plan |
|---|---|---|---|---|
| 1 | **Naming unlabeled buttons and icons** | highest | **medium**. Zero-shot SigLIP gets 0.61 top-1 / 0.75 top-3 over 75 UI labels; with margin abstention it gets **0.95 accuracy at 25% coverage** and 0.77 at 50%. Recognising the same icon across theme and scale: **ink-mask cosine 0.90 top-1**, beating all neural encoders (0.54–0.83). | A cascade, cheapest first: (i) non-visual hints such as tooltip, description, web source-layer attributes and file names; (ii) **label memory**: an exact or near visual hash plus app and context matched against controls seen labelled elsewhere or named by the user; (iii) OCR if the crop has text; (iv) SigLIP closed-vocabulary naming spoken as "maybe X" only above the margin threshold. Speak "button, unlabeled" immediately and amend asynchronously; never block speech. |
| 2 | **Semantic find** ("go to the part about fonts", "find the settings button") over the current page or app | high | **high, available now**. Text only, 1.8 ms per string, exact search over at most 10⁵ items in under 8 ms | First feature to ship. It needs only the text encoder and `VectorIndex`. |
| 3 | **OCR fallback** for inaccessible or canvas apps | high | **medium-high**. CER under 1% at 13 px and up, 6–18 ms per line; upscaling 11 px text 3× halves the error. Whole-window layout analysis is not measured. Russian traineddata is not installed. | Needs ScreenCast. Make the output a synthetic subtree in the mirror (role `text`, synthetic bounds) so ordinary navigation works on it. |
| 4 | **Describing images without alt** | high | **low on CPU**. It needs a captioning VLM (Florence-2-base, SmolVLM-256M, moondream class), costing seconds on CPU (not measured) | Explicit on-demand command only; optional GPU. |
| 5 | Cross-app "same control" recognition | medium | **high** (ink-mask 0.90) | It is the substrate for #1(ii), not a user feature by itself. |
| 6 | "Find the thing that looks like a cart" | medium-low | medium. It is #1 in reverse; ranking about 50 visible controls is easier than a 75-way label choice | Reuses the #1 embeddings. |
| 7 | Page and layout fingerprinting | low | known weak: MinHash cannot separate re-renders from same-template pages (previous report) | Leave to the structural diff. |

## 6. Kohonen SOMs as a navigation and overview primitive

### 6.1 Formulation

Items are x_i ∈ R^D (fused unit blocks). The grid has K = R×C cells g_j ∈ Z². The batch SOM fixed point is

  W_j = Σ_i h_σ(g_j, g_{b(i)}) x_i / Σ_i h_σ(g_j, g_{b(i)}),  where b(i) = argmin_j ||x_i − W_j|| and h_σ is a Gaussian on the grid.

We use PCA initialisation with a fixed sign convention, which makes the map deterministic (tested), and anneal σ from R/2 to σ₁ over 30 iterations. Each iteration costs O(N K D + K² D). The online Kohonen rule has no energy function in general (Erwin et al. 1992); the batch rule converges to a local fixed point. Topology preservation is therefore an **empirical** property and is measured below.

### 6.2 Static comparison (help corpus, N=1864, D=411, 8×8; `out/som.json`)

| Method | QE | TE (cell means) | trust@10 | cont@10 | empty cells | max load / mean | build |
|---|---|---|---|---|---|---|---|
| **SOM, σ₁=0.5** | **0.316** | 0.152 (codebook TE 0.05) | **0.956** | 0.952 | 17% | 3.0 | 0.26 s |
| SOM, σ₁=0.8 | 0.321 | 0.219 (codebook TE 0.013) | 0.952 | 0.948 | 25% | 3.4 | 0.26 s |
| GTM (5×5 RBF) | 0.324 | 0.177 | 0.950 | 0.950 | **48%** | 2.3 | 1.7 s |
| Growing Grid (→ 6×11) | 0.322 | 0.161 | 0.953 | 0.953 | 29% | 2.4 | 0.56 s |
| PCA + snap | 0.350 | 0.051 | 0.895 | 0.919 | 30% | 3.1 | 2 ms |
| t-SNE + snap | 0.331 | 0.087 | 0.929 | 0.957 | 17% | 2.5 | 4.5 s |
| UMAP + snap | 0.334 | 0.099 | 0.932 | 0.954 | 48% | 3.0 | 2.7 s |

Real browser trees (struct+geometry only, `out/som_realtree.json`): trust 0.97–0.98, role purity 0.99. A 21k-node page builds in 0.7 s in Python; Rust f32 SIMD should be about 10× faster.

Reading: SOM has the best trustworthiness (map neighbours really are data neighbours), which is the property arrow-key navigation needs. Snap methods score lower TE because quantile bins force contiguity, but they place false neighbours side by side (lower trust). GTM is principled but leaves half the grid empty. Growing Grid picks a non-square shape the user cannot predict.

### 6.3 Dynamics: 10 mutation steps, each deleting 3%, inserting 3% and modifying 3% of items

| Method | moved / step (untouched items) | cumulative drift after 10 steps | final trust | update p50 |
|---|---|---|---|---|
| SOM frozen (assign new items only) | 0 | 0 | 0.954 | 2 ms |
| SOM anchored incremental | 1.2% | 12% | 0.957 | 8 ms |
| **SOM anchored + hysteresis** | **0.008%** | **0%** | **0.954** | **8.5 ms** |
| SOM warm retrain | 8.5% | 24% | 0.956 | 42 ms |
| SOM cold retrain (best flip aligned) | 28% | 44% | 0.957 | 182 ms |
| GTM warm EM | 0.7% | 5.9% | 0.948 (TE 0.28, 53% empty) | 234 ms |
| UMAP frozen + transform | 0 | 0 | 0.937 (39% empty) | 80 ms (2.2 s max) |
| UMAP retrain (flip aligned) | 87% | 99% | 0.931 | 1.6 s |
| t-SNE retrain (flip aligned, 3 steps) | 58% | 59% | 0.938 | 3.2 s |

**Verdict.** SOM with an anchored incremental refit and hysteresis is the only method that is high-quality, stable and fast at once. Re-running t-SNE or UMAP destroys the user's mental map, even after the best axis flip.

### 6.4 Stability theory (proven) and a negative result

**Proposition 1 (anchored drift).** Hold the assignments b fixed. The anchored update is

  W'_j = (Σ_i h_{j b(i)} x_i + λ W_j) / (Σ_i h_{j b(i)} + λ).

If m items on the unit sphere change, then ||W'_j − W_j^{old}|| ≤ 2m / (N_j^h + λ), where N_j^h = Σ_i h_{j b(i)}. *Proof:* the numerator changes by at most Σ h·||x − x'|| ≤ 2m, and the denominator is unchanged. ∎ Choose λ ≈ 0.1 N/K. Implementation: keep the sufficient statistics S = Σ_{b(i)=j} x_i and n_j. A mutation then costs O(m K D) for the BMUs and O(K² D) for the refit; with K=64 and D=411 that is about 1.7 M FLOP.

**Proposition 2 (hysteresis dwell time).** Hysteresis rule: an item keeps its cell c unless d²(x, W_c) > d²(x, W_best) + ε. For unit x and ||W|| ≤ 1, a prototype move δ changes d² by at most 4δ, so the gap d²_c − d²_best grows by at most 8 δ_max per update. An untouched item that was at its BMU therefore cannot move for at least ε / (8 δ_max) updates. ∎ We use ε = 0.25 × median(QE²). This gives a tunable, provable bound on how fast the map can reshuffle under the user.

**Negative result.** Hamerly-style bounds give an exact certificate: maintain an upper bound u_i on the distance to the BMU and a lower bound l_i on the distance to the second-nearest prototype, and re-check only when u_i ≥ l_i. The certificate is sound (asserted on samples) but **useless here**: on 10k–100k e5-like vectors, 54–74% of items need a re-check per 100-item update, because of distance concentration (near-tied BMUs). It also confirms that without hysteresis about 1.7–2% of items legitimately change BMU per update. Stability must be a *policy* (hysteresis), not exactness.

Scale (`out/som_scale.json`, numpy, 1 thread): one BMU pass over 100k×384 against K=64 takes 0.51 s, and 1.05 s for K=256. Global maps over 10⁵ items are therefore background jobs. That is acceptable, because a 64-cell map over 10⁵ items is useless for navigation anyway: about 1500 items per cell.

### 6.5 Navigation design (proposal)

- **Scope.** One map per scope: the current page or document, the current app window, or (opt-in) "everything I've visited". Target 20–50 items per cell; zooming into a cell (Enter) opens a sub-SOM, or the cell's members in document order once there are 30 or fewer. Escape zooms out.
- **Movement.** Arrows move between cells and **skip empty cells** (17–25% of SOM units are interpolating units). A boundary plays an earcon.
- **Cell label.** The spoken label is the top 2–4 TF-IDF terms of the members, the member count and the dominant role, for example: "[0,7] 83 items, mostly headings: commands, preferences, plugin, navigation".
- **Spatial audio.** Column maps to stereo azimuth and row maps to pitch.
- **"Where am I".** This command speaks the cell of the focused node; the reverse jump lands on a cell's first member in document order.
- **Updates.** Updates run on a worker. The surface is a state machine (`Empty → Building → Ready ⇄ Updating`, `Failed`), and navigation always reads the last `Ready` generation. It is never blocking and never part of the speech hot path.
- **Unproven UX.** Run a small study with blind users before making it default.

### 6.6 Kohonen alternatives considered

- **GTM:** worse occupancy and 7–30× slower updates.
- **Growing SOM / Growing Grid:** an unpredictable shape; useful only to choose the grid size offline.
- **t-SNE or UMAP + snap:** unstable on retrain. A frozen UMAP plus `transform` is stable but slow, leaves 39% of cells empty and needs numba, so it is not portable to Rust.
- **Hierarchical SOM:** the zoom design above.

## 7. Vector search for the multimodal store

Per-page or per-app sets are 10²–10⁴ items. An exact scan is microseconds to a few milliseconds, so **ANN is not needed** there (consistent with the previous report).

Cross-app, cross-session sets (history, label memory) can reach 10⁵–10⁶. Single query, 1 thread, D=384, synthetic e5-like data (`out/ann_*.json`):

| N | Method | p50 / p99 ms | recall@10 | bytes/vector |
|---|---|---|---|---|
| 100k | exact f32 (faiss flat) | 8.3 / 11.2 | 1.0 | 1536 |
| 100k | IVF-PQ m=96, nprobe 32, re-ranked from f32 | 0.23 / 0.67 | 0.99 | 96 (+f32 for re-rank) |
| 100k | usearch HNSW16 f16 | 0.16 / 0.34 | 0.977 | 1520 |
| 100k | usearch HNSW16 i8 | 0.13 / 0.25 | **0.70** | 849 |
| 1M | exact f32 | 72.6 / 95.6 | 1.0 | 1536 |
| 1M | IVF-PQ m=48, nprobe 32, re-ranked | 0.45 / 0.57 | 0.72 | 48 |
| 1M | IVF-PQ m=96, nprobe 32, re-ranked | 0.69 / 0.96 | 0.94 | 96 (+f32 for re-rank) |
| 1M | usearch HNSW16 f16, ef 64 | 0.34 / 0.86 | 0.85 (raise ef/M) | 1334 |
| 1M | usearch HNSW16 i8 | 0.23 / 0.52 | **0.45** | 797 |

HNSW remove+re-add: 250 µs per vector at 100k and 590 µs at 1M (one thread). IVF was trained on 100k points for 4000 lists (faiss warns that this is undertrained), so its 1M recall is pessimistic.

**Crossover.**
- Exact search stays under the 15 ms budget up to about 1.5×10⁵ × 384 on one core (f32, faiss; a Rust SIMD f32 scan should match it).
- Above that, use **HNSW f16** (usearch); at 10⁶ it needs a larger ef or M to reach 0.95 or more (not tuned here).
- **PQ** pays off only when RAM matters: 1M × 384 f32 is 1.5 GB, against 96 MB for PQ-96, whose recall holds only with re-ranking.
- **i8 quantisation in usearch is not acceptable** (recall 0.70; the same failure as in the previous report).

**Persistent per-user index.** It is useful for two things:
- **Label memory.** A visual hash or embedding of a control, its app, its context, and the label a user gave it or that was seen elsewhere. Thousands of entries, high value, low risk: store only control chrome, never content.
- **History search** ("the page where I read about X"). High value but essentially a keylogger of reading. Off by default, per-app opt-in, TTL, and never containing password or editable fields.

Text embeddings must be treated as **plaintext-equivalent**: inversion attacks recover most of a short input (Morris et al., 2023, "Text Embeddings Reveal (Almost) As Much As Text"; numbers from memory, to be verified). Persist them with 0600 permissions under `$XDG_STATE_HOME/gorca`, optionally encrypted through the Secret Service.

## 8. Core or module: interface sketch

```rust
// ---- core: features (gorca-core::features) -------------------------------------
pub enum Modality { Structure, Text, Geometry, Visual, Interaction }
pub struct EncoderId { pub name: SmolStr, pub version: u32, pub dim: u16 }
/// blake3(modality, encoder, normalised input), truncated to 128 bits; identical
/// controls across nodes, re-renders, apps and sessions share one entry.
pub struct ContentKey(pub [u8; 16]);

pub enum Exclusion { PasswordRole, EditableContent, SensitiveApp(AppId), UserOptOut, Offscreen }
pub enum Eligibility { Allowed, Excluded(Exclusion) }
/// In core, not a plugin: plugins can never see what this denies.
pub trait PrivacyPolicy: Send + Sync {
    fn eligibility(&self, node: &NodeView<'_>, m: Modality) -> Eligibility;
}

pub enum FeatureState { Absent, Pending { deadline: Instant }, Ready(Arc<[f32]>), Failed(FeatureError) }
pub trait FeatureStore {
    fn key(&self, node: NodeId, m: Modality) -> Result<ContentKey, Exclusion>;
    fn get(&self, key: &ContentKey, enc: &EncoderId) -> FeatureState;
    /// Non-blocking: enqueues to the sidecar with a deadline; result arrives as an event.
    fn request(&self, node: NodeId, m: Modality, prio: Priority, deadline: Instant);
    /// Called from the mirror's node-removed path; refcounted, LRU-evicted.
    fn release(&self, node: NodeId);
}

// ---- core: vector index ---------------------------------------------------------
pub trait VectorIndex: Send {
    fn upsert(&mut self, id: ItemId, v: &[f32]);
    fn remove(&mut self, id: ItemId);
    fn knn(&self, q: &[f32], k: usize, filter: &dyn Fn(ItemId) -> bool) -> SmallVec<[(ItemId, f32); 16]>;
}
// impls: ExactF32 (default, SIMD scan); HnswF16 (usearch, feature "ann") only for > 1e5 items.

// ---- core: map surface ------------------------------------------------------------
pub struct CellId { pub row: u8, pub col: u8 }
pub enum Dir { Up, Down, Left, Right }
pub enum MapState { Empty, Building { gen: u64 }, Ready { gen: u64 }, Updating { gen: u64 }, Failed(MapError) }
pub trait MapLayout: Send {               // SomLayout is the default implementation
    fn build(&mut self, items: &[(ItemId, &[f32])]) -> Assignment;
    fn update(&mut self, delta: &ItemDelta, prev: &Assignment) -> Assignment; // anchored + hysteresis
}
pub trait MapSurface {
    fn state(&self) -> &MapState;
    fn cell_of(&self, item: ItemId) -> Option<CellId>;
    fn members(&self, cell: CellId) -> &[ItemId];               // document order
    fn step(&self, from: CellId, dir: Dir) -> Option<CellId>;   // skips empty cells
    fn label(&self, cell: CellId) -> &CellLabel;                // terms, count, dominant role
    fn zoom(&self, cell: CellId) -> Option<MapScope>;
}
```

Sidecar protocol (`gorca-perceive`, a Unix socket with length-prefixed postcard messages, every request carrying a deadline):
`Encode{req, encoder, inputs: [Text | Image{w,h,stride,rgba}]}` → `Vectors{req, f32 rows}`;
`Ocr{req, image, langs}` → `Lines[{text, bbox, conf}]`;
`NameIcon{req, image, vocab_id}` → `Candidates[{label_id, score}], abstain`.
It runs as a systemd user unit with `PrivateNetwork=yes`, `MemoryMax=`, `Nice=10` and `CPUWeight=` low; the core restarts it on crash, and its absence only disables features. The models are downloaded once and checksummed. Labels are a localised closed vocabulary (ru/en), so the vision model never has to output Russian.

## 9. Privacy specification (hard constraints)

1. **Never encode:**
   - `password-text` nodes, their descendants and their bounds in any crop, which are blacked out before encoding;
   - the contents of editable text nodes (`STATE_EDITABLE`). Their accessible *name* or label is allowed.
2. App-level denylist with defaults: password managers and terminals running `sudo` or `ssh` prompts. It is user-editable.
3. Everything is local. The sidecar has no network; models are fetched only by an explicit user command.
4. Embeddings are as sensitive as text. In-memory by default; persistence only for the label memory (control chrome); history is opt-in with a TTL.
5. Pixel capture happens on demand only, for the bounds of specific nodes. No continuous stream, frames stay in RAM, and the GNOME indicator is visible whenever it runs.

## 10. Rust port notes

- **SOM.** f32, row-major codebook K×D. BMU uses the ||x||² − 2x·w + ||w||² form with a SIMD dot product. For 2k × 64 × 411 that is about 54 M FMA, under 2 ms on one core. Keep S and n as sufficient statistics in f64 to avoid drift under many add/subtract cycles. PCA init uses a power iteration with the sign convention "largest-|loading| component positive"; deterministic tie-break is the lowest index.
- **Test vector.** `research/multimodal/testvec-som.json`: 300×8 points, 4×4 grid, 20 iterations, σ 2 → 0.5. The expected codebook must match to 1e-6 and the assignments exactly, followed by one anchored and hysteresis update with given λ and ε.
- **Metrics** (for CI regression): QE and codebook TE are trivial; trustworthiness needs exact kNN and is test-only.
- **Encoders.** `ort` crate plus `tokenizers` crate load the same .onnx and tokenizer.json files. Preprocessing is specified in `encoders.py`: CLIP and SigLIP use bicubic resize to 224 with mean/std normalisation; e5 uses "passage: " or "query: " prefixes with mean pooling.
- **OCR.** Either `leptess` (FFI to libtesseract, which avoids the process spawn included in the measured 6–18 ms) or pure-Rust `ocrs` (an alternative, not evaluated).
- **Same-control hash.** The ink mask is the absolute luminance difference from the border median, resized to 32×32 and zero-mean normalised, then compared by cosine. It is trivially portable; a 64-bit pHash of it serves for exact-match lookups.

## 11. Risks and open questions

- The usefulness of a semantic map for blind users is a hypothesis; a user study is needed before it becomes default.
- Icon naming is measured on Adwaita only. Web icon fonts (Font Awesome, Material) and brand logos are untested. Collect a labelled crop set once pixel capture works (privacy-safe: chrome only).
- Pixel capture latency and indicator UX via Mutter ScreenCast are not measured (linux-platform-engineer).
- The ANN results use synthetic vectors; real multi-app item sets don't exist yet.
- GPU is not measured (driver nodes absent). Captioning VLM latency on CPU is not measured.
- Synthetic geometry was used for the help corpus; real bounds may change the fused map.
- Prevalence statistics (WebAIM) and the embedding-inversion numbers are quoted from memory and need verification.
