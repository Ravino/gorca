# Chromium flat review: pre-merge verification (2026-10-18)

Branch `worktree-agent-a5693fce30a10ab7a`, reviewed at 635f0f8b; reviewer commit 5ad1bd2d (fixture).
Verdict: **MERGE AFTER FIXES** (M-1 and M-2 below; both small).

## Gates (run at 635f0f8b, shared toolchain symlinked)
| gate | result |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` | ok |
| `cargo test --workspace --no-fail-fast` | 2243 passed, 0 failed, 17 ignored; `gorca-daemon/tests/shell_parity.rs` 2/2 pass |
| `cargo xtask strings` (4 jobs) | ok (320 literals, 102 allowed) |
| merge-tree vs main 2a017ed3 | main is the merge base: fast-forward, no conflicts; `.toolchain` not tracked |
| e2e M8-FL/FLS after 5ad1bd2d | Chromium `bench/results/e2e/cflrv1` 3/3, Firefox `fflrv1` 3/3, fail=0 |

Note: the author's green runs (`cfl15`, `ffl15`) carry `git_rev=43c7fea2 git_dirty=true`, not 635f0f8b.

## Findings

### M-1 (High) Busy-retry also retries `Timeout`: 4x latency and a slow app declared Hung
`crates/gorca-core/src/review/build.rs` `lines_once`: `transient = matches!(e, Busy | Timeout)`, and
`lines_of` retries every transient answer up to `LINES_TRIES = 4`.
- Each try waits the full class deadline (Interactive 120 ms, Prefetch 300 ms). Probe P-1
  (`revalidate` on one hung text node, temporary test, reverted): 5 requests (1 Fetch + 4 lines),
  **1.56 s**, versus about 0.6 s before.
- Every timeout goes to `HealthMsg::Timeout`; `health.rs` `HUNG_AFTER = 3` consecutive timeouts.
  One slow text node (for example a large VTE buffer or a heavy Blink text node) now produces 4 in a
  row from a single model build, so a slow-but-alive app becomes **Hung** (same class of defect as
  B-1 RV-1). `revalidate` calls `lines_of` sequentially, so its cost multiplies by the probe items.
- The lines phase has no overall deadline, so an interactive first build can run 4 x 120 ms + 30 ms
  past `BUILD_FIRST` per in-flight wave.
Fix: retry only `Busy`; keep `Timeout` as "transient, mark truncated" without retrying; skip the
retry once `started + spec.budget` has passed. Add a unit test (the fake's `hang` set): one hung text
node gives exactly one lines request and a `Truncated(Deadline)` model. The busy path still has no
unit test (it needs a `Busy` answer in the fake `Script`).

### M-2 (Medium) Paging resume does not notice child-list changes between passes
`build.rs` `expand`: the resumed prefix in `partial_pages` is reused without validation, and the next
page starts at `prefix.len()` of the *new* list. Probe P-2 (form(600), stop after page 1, insert a
child at index 0, resume): the parent's list becomes **`Known` with 601 entries, 600 unique, the
new child missing**, and the model can report Complete. A removal drops a child the same way. Front
insertion between passes is common on live pages (feeds, chat).
Fix: store the parent's `Count(n)` (and the last prefix key) with the pages; on resume drop the entry
when the fresh count differs, or read the next page from `start - 1` and require that its first key
equals the prefix's last key; otherwise restart from page 0. Also forget the entry when a
children-changed for that node reaches the task (or key it by mirror revision of the parent).
Probe P-3 (stale prefix re-applied with a fresh `issued` seq overwriting a newer event write) did
**not** reproduce: the renamed child kept its new name. Not reported as a defect.

### L-1 (Medium, unverified) `blink-text-extents-frame` assumes window origin (0,0)
`caller.rs` asks `Text` in SCREEN for every `Toolkit::Chromium` app, including Electron and Chromium on
XWayland. There SCREEN text extents include the window position while `Component.GetExtents` is still
asked in WINDOW, so text zones would shift by the window origin. `GetOffsetAtPoint` takes window
points as screen points the same way. Verified only on Chromium with `--ozone-platform=wayland`.
Suggest: ask both Component and Text in SCREEN for this row, or detect the offset once per document.

### L-2 (Low) `name_strip` is generic and changes GTK/Shell output
`cluster.rs` `finalize`: every named content container (GTK frames, dialogs, panels) now says its name
on the row of its first child in tree order instead of at the centre of its rectangle. This is the
better rule, but "first child" is tree order, not the top-most zone (CSS `order`, column layouts).
Unit tests for GTK/Shell/terminal clustering all pass; no GTK/Shell flat-review e2e was run in this
review. Suggest `min_by_key(y)` over the kids' rects.

### Quirk placement (point 4): OK
Both rows live in the engine quirk table (`Toolkit::text_extents_use_screen_coords`,
`EditQuirks::text_leaves_repeat_parent` filled from `Toolkit::text_leaf_children_repeat_parent_text`);
the core sees only an engine-neutral bool in `ReviewIo`. No presenter special case. Unit test
`only_blink_has_the_review_text_quirks` covers GTK3/4, Qt, VTE, Firefox, Other.

### Paging budget (point 3, R2-1): mostly OK
Bounded: `until` stops paging; a resumed call reads exactly one owed page; the build round loop checks
the budget; `partial_pages` lives for one build task (created per `spawn_review`), entries are removed
when a list finishes or a page read fails. `std::sync::Mutex` guards never cross an `.await`.
The only gap is M-2.

## Changed e2e expectations (point 1)
Old->new in `docs/reports/chromium-flat-review.md` reviewed: FL.cur/next1..5/back and FLS.bottom/up/up2
are stronger than before (FLS.bottom previously passed on the second-to-last row because Firefox lost
12 rows). FL.end/FL.prev were weakened to "not chrome". Fixed in reviewer commit **5ad1bd2d**:
new fixture `tests/web/m8/pages/flat.html` (the top of news.html, cut to fit about 600 px on both rigs)
for M8-FL; FL.end now asserts "bottom row of the page" (plus the chrome exclusion), FL.prev
"penultimate row of the page". All other M8-FL steps unchanged and green on both engines.

## Must fix before merge
1. M-1: retry only `Busy`; no retry on `Timeout`; bound by the build budget; unit test.
2. M-2: validate or drop the resumed prefix when the child list changed; unit test with a front insert.
