# Orchestrator resume note (2026-10-24)

The session was stopped for the owner's move. These notes are what is needed to pick up the work.

## In main
- M22b1 cut-over is done. Focus and change speak through `gorca-present` / PresIr v1. The tag `legacy/focus-last` marks the last legacy commit, and the legacy focus/change paths are deleted.
- Owner build: `gorca 210e3d29`.
- M22b2 work is merged: browse and quick-nav go through gorca-present. The legacy browse path still exists. The `browse` and `quicknav` gates PASS on main (en+ru, 0 unclassified).
- Hardened cut-over gate (`xtask cutover <surface>`), determinism property tests, and the no-legacy rule (`xtask legacy`, 58 rows).
- Web fixes:
  - re-anchor after a removed pane;
  - scroll to the cursor range;
  - menu announcement;
  - dedupe of the doubled IME key echo.
- ADRs (all Proposed):
  - ADR-0026 r4: WASM plugins as actors.
  - ADR-0027 r2: runtime CPU feature dispatch, baseline x86-64-v1.
  - ADR-0022 r4: cut-over and no-legacy rule.

## In flight (branches)
- **M22b2 cut-over**
  - `cutover-browse` holds the single switch commit.
  - `m22b2-fixes` holds the fixes found by M8: the authored role for tab lists, and error text reaching the fused entry, plus the re-blessed goldens and the fixtures.
  - The resume note is `docs/reports/m22b2-resume.md`, on that branch.
  - Remaining steps, in order:
    1. Run Firefox M8 on `legacy/focus-last`, as the 3-way check of the 7 failures shared by main and the cut-over.
    2. Run the workspace tests.
    3. Merge `m22b2-fixes`.
    4. Rerun the gates.
    5. Get the judge's approval.
    6. Tag `legacy/browse-last`.
    7. Apply `cutover-browse`.
- **Bake-off M18a-0**
  - The harness is on main: `docs/bakeoff/m18a-0-plugin-call-model/`.
  - Arm A and arm B' are each in their own agent worktree. Results may be partial; see each arm's RESULTS.md.
  - Next: finish the runs, have the judge (opus) re-run the harness, show the owner the table, then write the ADR and merge the winner. B' is the design target.
- **Stopped by the owner:** the speech watchdog (voicego/audio). Its worktree is kept and not merged.

## Next after M22b2
1. M22b3: review and object nav.
2. M22b4: window, notification, info/explain, extensions.
3. M18a runtime.
4. telegram-web plugins (A and K).
5. Public push.

## Operating rules learned this session
- Build in one shared target, `/var/tmp/gorca-shared-target`, with `CARGO_INCREMENTAL=0`. Wipe it when it grows past 30 GB or free space drops below 12 GB, and only while no build is using it. The disk filled up to 100% once.
- Agents must run `xtask legacy` as part of their gates. This is now in CLAUDE.md.
- Owner preview flags live in `~/.config/gorca/preview.env`, loaded through a gorca.service drop-in.
