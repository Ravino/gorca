# M22b4 plan and status (2026-10-25)

Scope: the `xtask/legacy.allow` rows tagged M22b4 (41 rows / 98 uses at the start). Each sub-cut-over
has its own gate, goldens and commit, and ends with its rows deleted. After the last one a single
`cutover-rest` commit empties `xtask/legacy.allow` and makes the rule ban every legacy API with no
allowlist (LAST_EXPIRY handling). That commit is never applied to main by the implementer.

## Done: sub-cut-over 1, window lead-in and notice

- `Reason::Window` (9) and `Reason::Notice` (10) appended to PresIr v1 (append-only codes).
- `gorca_present::desktop` (`compose_window`, `compose_notice`, `notice_has_words`), Reader methods
  `window_lead` and `notice_words` in `present/node.rs`.
- Deleted: `lead_segments`, `notice_segments`, the two `role-spoken` rows (window.rs, notification.rs).
- Gate: `cargo xtask cutover window|notification`, reports `bench/results/cutover/{window,notification}-2026-10-24.md`
  (PASS, en+ru, 0 unclassified, 0 unexplained drops, deterministic under a shuffled arrival order).
  Window: 106 nodes of 7 families (Telegram fixtures hold no window node). Notice: 33 nodes x 2 cases of
  4 families (gtk4, firefox, chromium, gnome-shell hold the only alerts and dialogs). One intended row
  per surface: `role-word-improved` (an ARIA alertdialog is a dialog on every engine).
- Goldens: `tests/fixtures/sem/golden/{window,notification}.{en,ru}.tsv` (`desktop_golden_tests.rs`).
- E2E (private Shell world): `tests/shell/p1_wired.py shell` steps "window: release Alt activates a
  window" and "notification: banner is read" pass; output `bench/results/e2e/m22b4a-p1*`.
- Latency: `bench/results/m6/desktop-latency-2026-10-25.txt` (p99 318 ns / 481 ns).
- Tag `legacy/window-last` marks the last commit with the legacy builders.

## Open items from the inventory

- C3b inferred label: already an S4 fact with taint, layout revision and the `browse.hedge_inferred`
  switch (decisions row 2026-10-24, M22b1 slice 4). No Reader overlay remains. Only the oracle copy
  `subtree-label` in `app_actor/agree.rs` is left (sub-cut-over 4).
- Entries for lazily cached ancestors (where-am-I): sub-cut-over 2.
- WIT 0.9 accessors for policy facts: sub-cut-over 3.

## Remaining sub-cut-overs

2. **Info and explain**: `commands/info.rs` (where-am-I, node info, own state table, window title:
   7 `role-spoken` + 3 `state-words` uses), `commands/diag.rs` / `present/log.rs` explain-last on `PresIr`
   origin and `ProvTag`. Needs `Reason::WhereAmI` rows and an entry per ancestor.
3. **Extensions and plugins**: `PolicyFacts` from `SemEntry` accessors (builtin_ext.rs, node.rs,
   notification.rs, window.rs, reader.rs, shell_policy.rs), `gorca-ext-api` / `gorca-ext-host` /
   `extensions/*` leave `NodeSnapshot` and the legacy `Role` enum; `Verdict::Say` becomes an IR edit.
4. **Carriers**: `NodeMeta`, `AttrMap`, `.attrs`, `web_attrs`, live-region and label-inference reads in
   `doc_actor`, `statemodel/world.rs`, `agree` modules and `SemRole::legacy_family`.
5. **cutover-rest**: delete `xtask/legacy.allow`, ban everything unconditionally.
