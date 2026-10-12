"""2-D "semantic map" builders: batch SOM (with incremental, anchored, hysteresis
updates), GTM, Growing Grid, and embedding + grid-snap baselines (PCA, t-SNE, UMAP).

All builders map items to cells of a rows x cols grid. Shared metrics live in
`metrics()`. Distances are Euclidean on L2-normalised fused vectors (= monotone
in cosine distance within each block).

Reference implementation; porting notes are in the report (section "Rust port").
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")
os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")

import numpy as np  # noqa: E402


# ---------------------------------------------------------------------------
# grid helpers
def grid_coords(rows: int, cols: int) -> np.ndarray:
    r, c = np.divmod(np.arange(rows * cols), cols)
    return np.stack([r, c], 1).astype(np.float64)


def sqdist(X: np.ndarray, W: np.ndarray) -> np.ndarray:
    d = (X * X).sum(1)[:, None] - 2 * X @ W.T + (W * W).sum(1)[None, :]
    return np.maximum(d, 0)


def pca_basis(X: np.ndarray, k: int = 2):
    mu = X.mean(0)
    U, S, Vt = np.linalg.svd(X - mu, full_matrices=False)
    V = Vt[:k]
    # deterministic sign convention: largest-|loading| component positive
    for i in range(k):
        if V[i, np.argmax(np.abs(V[i]))] < 0:
            V[i] = -V[i]
    return mu, V, S[:k] / np.sqrt(len(X))


# ---------------------------------------------------------------------------
class BatchSOM:
    """Batch SOM with Gaussian neighbourhood on a rectangular grid.

    Codebook update (batch rule, Kohonen 1999), optionally anchored to a previous
    codebook W0 with weight lam (in item units):
        W_j = (sum_i h(j, b_i) x_i + lam * W0_j) / (sum_i h(j, b_i) + lam)
    With sufficient statistics S_j = sum_{b_i=j} x_i and n_j = |{i: b_i=j}|, this is
        W = (H S + lam W0) / (H n + lam),
    so an insert/delete/modify of m items costs O(m (K D)) + O(K^2 D) for the refit.
    """

    def __init__(self, rows: int, cols: int):
        self.rows, self.cols = rows, cols
        self.G = grid_coords(rows, cols)
        self.GD2 = sqdist(self.G, self.G)
        self.W = None

    def H(self, sigma: float) -> np.ndarray:
        return np.exp(-self.GD2 / (2 * sigma * sigma))

    def init_pca(self, X: np.ndarray) -> None:
        mu, V, s = pca_basis(X)
        g = self.G.copy()
        g[:, 0] = (g[:, 0] / max(self.rows - 1, 1) - 0.5) * 2 * 2 * s[0]   # rows along PC1
        g[:, 1] = (g[:, 1] / max(self.cols - 1, 1) - 0.5) * 2 * 2 * s[1]
        self.W = mu + g[:, :1] * V[0] + g[:, 1:] * V[1]

    def bmu(self, X: np.ndarray):
        D = sqdist(X, self.W)
        b = D.argmin(1)
        return b, D

    def stats(self, X: np.ndarray, b: np.ndarray):
        K = len(self.W)
        Oh = np.zeros((len(b), K)); Oh[np.arange(len(b)), b] = 1.0
        S = Oh.T @ X
        n = Oh.sum(0)
        return S, n

    def refit(self, S, n, sigma, W0=None, lam=0.0):
        H = self.H(sigma)
        num = H @ S
        den = H @ n
        if W0 is not None and lam > 0:
            num = num + lam * W0
            den = den + lam
        ok = den > 1e-12
        W = self.W.copy()
        W[ok] = num[ok] / den[ok, None]
        self.W = W

    def fit(self, X, iters=30, sigma0=None, sigma1=0.8, init=True):
        if init or self.W is None:
            self.init_pca(X)
        sigma0 = sigma0 or max(self.rows, self.cols) / 2.0
        for t in range(iters):
            s = sigma0 * (sigma1 / sigma0) ** (t / max(iters - 1, 1))
            b, _ = self.bmu(X)
            S, n = self.stats(X, b)
            self.refit(S, n, s)
        return self.bmu(X)[0]

    def update(self, X, prev_assign, sigma=0.8, lam=0.0, iters=2, eps=0.0):
        """Warm-start update after a mutation batch.

        prev_assign[i] is the cell of item i before the update or -1 for new items.
        eps > 0 enables hysteresis: an item keeps its old cell unless the new BMU is
        closer by more than eps in squared distance. Returns the new assignment.
        """
        W0 = self.W.copy()
        assign = None
        for _ in range(iters):
            b, D = self.bmu(X)
            if eps > 0:
                old = prev_assign >= 0
                keep = old.copy()
                idx = np.where(old)[0]
                keep[idx] = D[idx, prev_assign[idx]] <= D[idx, b[idx]] + eps
                b = np.where(keep, prev_assign, b)
            S, n = self.stats(X, b)
            self.refit(S, n, sigma, W0, lam)
            assign = b
        return assign


# ---------------------------------------------------------------------------
def margin_certificate(D: np.ndarray, delta: np.ndarray) -> np.ndarray:
    """Items whose BMU is certified unchanged after prototypes move by at most delta_j
    (Euclidean): sqrt(d_b) + delta_b < min_j (sqrt(d_j) - delta_j). Triangle inequality."""
    R = np.sqrt(D)
    b = R.argmin(1)
    upper = R[np.arange(len(R)), b] + delta[b]
    low = R - delta[None, :]
    low[np.arange(len(R)), b] = np.inf
    return upper < low.min(1)


# ---------------------------------------------------------------------------
class GTM:
    """Generative Topographic Mapping (Bishop, Svensen, Williams 1998), EM fit.
    Latent grid = the cell grid; items are assigned to the posterior mode."""

    def __init__(self, rows, cols, m=4, alpha=1e-3):
        self.rows, self.cols = rows, cols
        g = grid_coords(rows, cols)
        self.Z = np.stack([g[:, 0] / max(rows - 1, 1), g[:, 1] / max(cols - 1, 1)], 1) * 2 - 1
        mu = np.stack(np.meshgrid(np.linspace(-1, 1, m), np.linspace(-1, 1, m)), -1).reshape(-1, 2)
        s = 2.0 / (m - 1)
        Phi = np.exp(-sqdist(self.Z, mu) / (2 * s * s))
        self.Phi = np.hstack([Phi, np.ones((len(Phi), 1))])
        self.alpha = alpha
        self.Wt = None
        self.beta = None

    def init_pca(self, X):
        mu, V, s = pca_basis(X)
        target = mu + self.Z[:, :1] * s[0] * 2 * V[0] + self.Z[:, 1:] * s[1] * 2 * V[1]
        self.Wt = np.linalg.lstsq(self.Phi, target, rcond=None)[0]
        Y = self.Phi @ self.Wt
        d = sqdist(Y, Y); np.fill_diagonal(d, np.inf)
        self.beta = 1.0 / max(d.min(1).mean() / 2, 1e-6)

    def resp(self, X):
        Y = self.Phi @ self.Wt
        L = -0.5 * self.beta * sqdist(X, Y)
        L -= L.max(1, keepdims=True)
        R = np.exp(L)
        return R / R.sum(1, keepdims=True), Y

    def fit(self, X, iters=30, init=True):
        if init or self.Wt is None:
            self.init_pca(X)
        N, D = X.shape
        for _ in range(iters):
            R, _ = self.resp(X)
            G = R.sum(0)
            A = self.Phi.T @ (G[:, None] * self.Phi) + (self.alpha / self.beta) * np.eye(self.Phi.shape[1])
            self.Wt = np.linalg.solve(A, self.Phi.T @ (R.T @ X))
            Y = self.Phi @ self.Wt
            self.beta = N * D / max((R * sqdist(X, Y)).sum(), 1e-12)
        return self.resp(X)[0].argmax(1)


# ---------------------------------------------------------------------------
class GrowingGrid:
    """Fritzke's Growing Grid (1995), batch variant: start 2x2, train, insert a row or
    column next to the unit with the largest accumulated error, until rows*cols >= K."""

    def __init__(self, k_target):
        self.k = k_target

    def fit(self, X, iters_per_step=8):
        som = BatchSOM(2, 2)
        som.fit(X, iters=iters_per_step)
        while som.rows * som.cols < self.k:
            b, D = som.bmu(X)
            err = np.bincount(b, weights=D[np.arange(len(X)), b], minlength=len(som.W))
            q = int(err.argmax())
            qr, qc = divmod(q, som.cols)
            W = som.W.reshape(som.rows, som.cols, -1)
            # most distant direct neighbour decides row vs column insertion
            cand = []
            for dr, dc in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                r, c = qr + dr, qc + dc
                if 0 <= r < som.rows and 0 <= c < som.cols:
                    cand.append((np.sum((W[qr, qc] - W[r, c]) ** 2), dr, dc))
            _, dr, dc = max(cand)
            if dr != 0:
                at = max(qr, qr + dr)
                new = (W[at - 1] + W[at]) / 2
                W = np.concatenate([W[:at], new[None], W[at:]], 0)
                rows, cols = som.rows + 1, som.cols
            else:
                at = max(qc, qc + dc)
                new = (W[:, at - 1] + W[:, at]) / 2
                W = np.concatenate([W[:, :at], new[:, None], W[:, at:]], 1)
                rows, cols = som.rows, som.cols + 1
            nsom = BatchSOM(rows, cols)
            nsom.W = W.reshape(rows * cols, -1)
            nsom.fit(X, iters=iters_per_step, sigma0=1.5, sigma1=0.8, init=False)
            som = nsom
        self.som = som
        return som.bmu(X)[0]


# ---------------------------------------------------------------------------
def snap(Y2: np.ndarray, rows: int, cols: int, edges=None):
    """Quantile grid snap of a 2-D embedding. Returns (cells, edges); pass edges back to
    snap new points into the same frozen bins."""
    if edges is None:
        edges = (np.quantile(Y2[:, 0], np.linspace(0, 1, rows + 1)[1:-1]),
                 np.quantile(Y2[:, 1], np.linspace(0, 1, cols + 1)[1:-1]))
    r = np.searchsorted(edges[0], Y2[:, 0])
    c = np.searchsorted(edges[1], Y2[:, 1])
    return r * cols + c, edges


# ---------------------------------------------------------------------------
def metrics(X, cells, rows, cols, labels=None, k=10, rng=None):
    """Common metrics on a cell assignment (prototypes = cell means for every method).

    qe: mean distance to own cell mean; te: fraction of items whose 2 nearest non-empty
    cell means are not 8-adjacent; trust/cont: trustworthiness/continuity (k) of grid
    coordinates with uniform in-cell jitter; purity: share of items whose cell majority
    label equals theirs; empty: share of empty cells; maxload: largest cell / mean load.
    """
    from sklearn.manifold import trustworthiness
    rng = rng or np.random.default_rng(0)
    K = rows * cols
    n = np.bincount(cells, minlength=K)
    S = np.zeros((K, X.shape[1])); np.add.at(S, cells, X)
    ne = n > 0
    P = S[ne] / n[ne, None]
    pid = np.full(K, -1); pid[ne] = np.arange(ne.sum())
    D = sqdist(X, P)
    qe = float(np.sqrt(D[np.arange(len(X)), pid[cells]]).mean())
    o = np.argsort(D, 1)[:, :2]
    cell_of = np.where(ne)[0]
    a, b = cell_of[o[:, 0]], cell_of[o[:, 1]]
    ra, ca = np.divmod(a, cols); rb, cb = np.divmod(b, cols)
    te = float(np.mean(np.maximum(np.abs(ra - rb), np.abs(ca - cb)) > 1))
    r, c = np.divmod(cells, cols)
    Y = np.stack([r, c], 1) + rng.uniform(-0.45, 0.45, (len(X), 2))
    out = {"qe": qe, "te": te,
           "trust": float(trustworthiness(X, Y, n_neighbors=k)),
           "cont": float(trustworthiness(Y, X, n_neighbors=k)),
           "empty": float(1 - ne.mean()), "maxload": float(n.max() / n[ne].mean())}
    if labels is not None:
        labels = np.asarray(labels)
        pur = 0
        for j in np.where(ne)[0]:
            m = cells == j
            _, cnt = np.unique(labels[m], return_counts=True)
            pur += cnt.max()
        out["purity"] = pur / len(X)
    return out


DIHEDRAL = [lambda r, c, R, C: (r, c), lambda r, c, R, C: (R - 1 - r, c), lambda r, c, R, C: (r, C - 1 - c),
            lambda r, c, R, C: (R - 1 - r, C - 1 - c)]


def stability(old_cells, new_cells, rows, cols, align=False):
    """Share of surviving items whose cell changed and mean Chebyshev displacement.
    With align=True the new grid is first mapped by the best of the 4 axis flips
    (square-grid transposes are not needed for rows != cols; we only use flips)."""
    best = None
    for f in (DIHEDRAL if align else DIHEDRAL[:1]):
        r, c = np.divmod(new_cells, cols)
        r2, c2 = f(r, c, rows, cols)
        nc = r2 * cols + c2
        ro, co = np.divmod(old_cells, cols)
        disp = np.maximum(np.abs(ro - r2), np.abs(co - c2))
        cand = (float(np.mean(nc != old_cells)), float(disp.mean()))
        if best is None or cand < best:
            best = cand
    return {"moved": best[0], "disp": best[1]}
