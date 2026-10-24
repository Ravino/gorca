# M22b2 resume note (2026-10-24)

## Branches and commits
- `m22b2-work` (19c79c71): gate-ready base, merged to main (128533a0); both gates PASS on main.
- `cutover-browse` (edfedca1, rebased on main e713e375): the single cut-over commit (legacy presenter, `aria.rs`, replay deleted; 15 `M22b2` rows removed from `xtask/legacy.allow`).
- `m22b2-fixes` (on top of `cutover-browse`): WIP commit e037daaf plus the final commit with the M8 results and this note. Contains the two live-path fixes, the re-blessed goldens and the new fixtures.

## Found by M8 on Firefox (cut-over vs main)
- `WG.tabs.line`: tab list name and role not spoken. The `ROLE_AUTHORED` bit lived in the flags group, which the document source never wrote for non-stateful roles (states are read for stateful roles only). Fix: `gorca-sem-atspi/src/record.rs` marks the flags group known when the role is authored.
- `FB.qn.invalid`: error text missing. The fetch attached `error-text` after the normaliser ran. Fix: `gorca-doc-source-atspi/src/fetch.rs` attaches it before.
- Why the goldens missed them: the replay gave every node its states and had no error-message relation. `agree_tests.rs` now reads states for stateful roles only and builds error text from the recorded `error-message` relation. Reverting the first fix makes the goldens fail.
- Verified: `M8-BWG,M8-BFB` on the fixed daemon pass (`bench/results/e2e/b2-dbg-wg`).

## M8 results (bench/results/e2e/)
- `b2-m8-ff-main`, `b2-m8-ff-cut` (cut-over BEFORE the fixes): Firefox, 7 gate failures each, same pre-existing ones (FLR, FLT, FLS, RINV, LV latency 105.6 ms vs 100 ms gate, plus BFB/MB on main and BWG/BFB on the cut-over). Step diffs: FB.qn.invalid and WG.tabs.line fail only on the cut-over; LT.search.go flaky on the cut-over; MB.a.open flaky on main.
- `b2-m8-chr-main`: Chromium, main, 5 gate failures.
- `b2-m8-chr-cut`: Chromium, cut-over WITH the fixes, complete: 4 gate failures vs 5 on main; no step passing on main fails; FB.invalid.focus (FAIL->PASS) and FM.invalid (GAP->PASS) improved.
- Host load 7-9 during the runs; some runs overlapped with capture/test builds (timing gates are not clean). Governor performance.
- Compare with `python3 -I <scratch>/ab23-cmp.py LABEL...` (per-step table diff of `*-steps.md`); recreate it if the scratchpad is gone: parse the `| step | P | status |` rows.

## Left to do
1. TODO: M8 on Firefox with `legacy/focus-last` (build in its own target dir; worktree prepared at `/var/tmp/gorca-m8-tag`, wrapper pattern as `ab23-main.sh`) to classify pre-existing failures.
2. TODO: M8 on Firefox with the FIXED cut-over daemon (the Firefox cut-over run was before the fixes), and compare step by step with main: no step that passes on main may fail.
3. Full workspace tests after the fixes: `cargo test --workspace --no-fail-fast --lib --bins --tests`, plus clippy, `cargo xtask deps/strings`. New fixture pages may affect other corpus tests (expect.toml, focus/change goldens, agree tests): not run yet.
4. `cargo xtask legacy` needs the committed browse/quicknav PASS reports (already on main).
5. Judge review, then tag `legacy/browse-last`, then land `cutover-browse` + `m22b2-fixes` (squash to one commit).
6. Re-run `bench/m6/browse-latency.sh m22b2-work 5` after the fixes (last result: all deltas under 1.6 us, `bench/results/m6/browse-latency-2026-10-24.txt`).
7. Clean up: `git worktree remove /var/tmp/gorca-m8-main /var/tmp/gorca-m8-tag`; delete the `bench/results/e2e/b2-dbg-wg` debug run if not wanted.

## Known gaps
- Error-text for pages where the relation target is in another frame is not covered.
- `FB.invalid.focus` passes now on the cut-over though it failed on main.
