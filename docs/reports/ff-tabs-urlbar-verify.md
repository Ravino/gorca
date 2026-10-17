# Review: Firefox tab-switch title and address bar (760e460e), 2026-10-17

Verdict: **MERGE AFTER FIXES**. The fixes are small. Gates are green and nothing blocks merging.

## Gates (run by the reviewer, CARGO_BUILD_JOBS=4, RUST_TEST_THREADS=4)
| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | rc=0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | rc=0, no warnings |
| `cargo test --workspace` (full output counted) | rc=0: 2178 passed, 0 failed, 17 ignored |
| `cargo xtask strings` | ok (318 literals, 102 allowed) |
| `git merge-tree main HEAD` | one textual conflict, `docs/decisions.md` (append-only, keep both sides) |
| Merged tree (main + branch), `cargo check -p gorca-bench --all-targets` | ok. Main's only code delta since the merge base is in gorca-bench (ru.rs, m1.rs, scenarios.rs) |

The debug target is now 15 GB (gates plus probes) and incremental has been cleared. Disk has 27 GB free.

## Findings (ranked)

### F1 (Medium, should fix or explicitly accept): double echo of a typed character in the address bar. This branch exposes it; the defect itself is older.
- Where: `text/mod.rs:247-253` (ComboBox now trackable) exposes a defect in `text/caret.rs` and `gorca-text/src/echo.rs::consume_insert`.
- Evidence: in the UB1 runs, the typed "2" is heard once in `ff-ub1-d` (combo not tracked) and twice (`"2","2"`) in `ff-ub1-e`, `ff-ub1-f` and `ff-ub1-g` (combo tracked). Counters in `ff-ub1-g`: `inserted_spoken: 1`.
- Mechanism: Gecko replaces a selection with delete events and a caret move only, with no insert for the first key. That key's expectation stays in the ring under the Gecko quirk TTL (3 s). The next key's insert cannot skip the stale expectation once it is older than `SKIP_MAX_AGE_MS` (1 s). The insert is then Unmatched, held, and flushed as unexplained text, so the user hears the key echo plus the inserted text.
- Reproduced by unit probes on a **plain `TextEntry`** with the Gecko slack: a 2200 ms gap gives `["2"] ... ["2"]` (double), a 300 ms gap gives a single "2", and without the quirk it is single. The probes are in `bench/results/e2e/ff-urlbar-review/probe_ffu.patch`.
- Conclusion: the defect predates this branch and hits every Firefox field when the user types over a selection and pauses 1–3 s before the second key. This branch makes it deterministic on the most common address-bar path (Ctrl+L, type).
- Fix: when a typed key's caret event arrives with no insert, but a delete of the selection happened in the same epoch (replace-selection), consume that key's expectation. Alternatively, drop expectations older than the newest key's predecessor once a newer key's insert matches. Add the probe as a regression test. The text/caret area is owned by another agent, so this can be a tracked follow-up if the orchestrator accepts the regression.

### F2 (Low): a cold tab's node is spoken twice, with different wording
- Where: `browse/commands.rs:1645-1663` (`web_first_useful` titled path).
- Evidence: `ff-tb1-fix3` note3 gives `"Link Gamma, link"`, then about 30 ms later `"browse mode, Tab Gamma, link, Link Gamma"`. `describe_node` puts the role first, while the focus presenter puts the name first. The Interrupt priority limits the overlap.
- Fix: build the re-spoken utterance with the focus presenter's ordering. Optionally suppress the first, generic focus utterance while the mode is `Loading` with a join miss in a web app.

### F3 (Low): `web_retry_loading` has no title lead
- Where: `browse/commands.rs:~1826` (`web_retry_loading`) does not use `switched` or `web_document_lead`.
- Scenario: a tab whose document already exists but whose focused node has not been joined yet (dynamic content, re-render) resolves through the retry path, and no title is spoken. Not exercised by TB1.
- Fix: compute `switched` before `web_activate` there too and call `web_document_lead`, or the titled re-speak when `inline == false`.

### F4 (Low, unverified): browser UI that has a Document in its chain
- `outside_every_document` returns false for any chain containing `Role::Document`. If Firefox exposes XUL sidebar or devtools documents as `Document`/`DocumentWeb`, those nodes keep the old Loading path ("focus mode" word on editable nodes). This is the behaviour from before the branch, so it is not a regression. The opposite misclassification (web content treated as browser UI) needs a fully cached chain that skips the document node; I found no path that does that. A cold chain falls back to Loading (safe).

### F5 (Info): test coverage
- TB1 ran once (5 switches) and UB1 once on the final binary. There were no x5 repeats, and Chromium, FR1 (frames) and the dialog e2e were not rerun.
- Code inspection shows iframes are nested nodes of the same top-level mirror (registry "locates the top-level document"; `translate.rs` nested_doc), so in-page frame focus never sets `switched`.
- `active` is cleared only on document removal, so excursions to the browser UI and same-document focus do not repeat the title. A unit test covers this.

## Checked, no issue
- Never-block: `outside_every_document` is a synchronous directory-cache walk bounded at 32 hops, and `web_document_lead` reads only the mirror. There are no new IPC calls.
- No `unwrap`/`expect` in non-test code. The e2e uses `unwrap_or_else(PoisonError::into_inner)` and `unwrap_or_default`.
- No new user-facing literals. The title comes from the page, the mode words already existed, and xtask strings is ok.
- Same-document focus moves, the click/suppress path and the Chromium frame-document focus are excluded by `suppress_focus` and same-`DocId` checks. Pending lead items expire through `TAIL_TTL`.
- Escape in the URL bar does not speak the restored URL (`ff-ub1-g` note28).
- The two updated tests change behaviour on purpose: the first focus into any document since start now speaks its title.

## Must-fix list
1. F1: fix it, or have the orchestrator accept it explicitly with a tracked follow-up and the probe turned into a regression test.
2. Resolve the `docs/decisions.md` conflict at merge.
