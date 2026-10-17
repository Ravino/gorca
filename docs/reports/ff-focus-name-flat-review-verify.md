# Review: Firefox focus naming and flat review (711fb314)

Date: 2026-10-17. Reviewer verdict: **MERGE AFTER FIXES** (one high item, the rest low).
Branch `worktree-agent-a006f03fcecc9742b`, commits 2e4ca740, 711fb314 over merge base b2723b99.

## Gates (measured)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` (jobs 4, threads 4, load ~10) | 2182 passed, 0 failed, exit 0 |
| `gorca-daemon/tests/shell_parity.rs` | 2/2 pass with the main checkout's toolchain |
| `cargo xtask strings` | ok (318 literals, 102 allowed) |
| `git merge-tree --write-tree main HEAD` | no conflicts (main moved by docs only) |

**shell_parity is environmental.** The worktree's `.toolchain/` is a private directory (not linked
to the main checkout) whose rustup has only `x86_64-unknown-linux-gnu`; the main toolchain has
`wasm32-wasip2`. Running the same tree with `. <repo>/env.sh` passes both tests. Same rustc
(1.99.1) in both.

## Findings

### H-1 Flat review does not reach on-screen content of a long, scrolled page (high)
`crates/gorca-core/src/review/objnav.rs:54-59`, `review/collect.rs:135`.
Inside a `DocumentWeb`, `VISIBLE` now suffices, so nothing prunes off-screen web subtrees. The
collector walks DFS in document order with `nodes_per_build = 4096` (`review/model.rs:39`), and
off-screen zones are only dropped later by `cluster::placed` against the *window* rect.
Probe (temporary test appended to `collect.rs`, reverted; source kept at
scratch `probe_collect.rs`): window, document, 5000 VISIBLE-only paragraphs above the viewport,
then an on-screen heading. Result: `PROBE nodes=4096 truncated=Some(NodeCap) heading=false`.
Failure scenario: open a Wikipedia article, scroll to the middle, press numpad 8. The review holds
chrome only (or a truncated model), and the build spends its IO budget fetching bounds, children
and text for thousands of off-screen nodes. M8-FL uses `news.html`, which fits one screen, so the
e2e does not cover it.
Fix: inside web, a node that lacks `SHOWING` is descended only when its bounds (fetch them for
containers, `Miss::Bounds`) intersect the `DocumentWeb`'s rect; otherwise prune the subtree. Add
a collect test like the probe plus an M8-FL step on a page several screens long, scrolled.

### L-1 Web content is clipped by the window, not by the document viewport (low)
`review/cluster.rs:151` (`placed`) and `build.rs:448` (`clip_of` = surface rect).
Content just scrolled above the viewport (within the toolbar height) or below it under the find
bar/devtools lies inside the window rect, so it is kept and clusters onto chrome rows. Part of
the "first line mixes chrome and content" observation in the dev report. Fix: clip nodes under a
`DocumentWeb` by that document's extents (same change as H-1).

### L-2 `web_overlay` fills a nameless ComboBox from its subtree text (low, unverified live)
`browse/commands.rs:1176` with `needs_name` including `Role::ComboBox` (`present/flow.rs:95`).
The subtree text of an editable combobox is its typed value, and of a `<select>` may be option
text; focus would then say the value as the name and again as the value. Fix: exclude
ComboBox (and any node with a value) from the fill.

### L-3 Name-follow window applies to every app and every prior name (low, by design)
`present/flow.rs:1055-1062`, `app_actor.rs:955`. Any name change of the focused node within 2 s
is spoken (Prio::Message), also in GTK/Shell and Chromium, and also when the focus already had a
non-blank name. Bounded: at most a few utterances per focus (a ticking label on a just-focused
button speaks up to ~2-4 times). Double speech with the 120 ms name retry does not occur: both
paths update `presented` and compare against it (`on_name_reply` returns when the presented name
is non-blank; `on_state_reply` returns on an empty `changed`). Restricting to a blank initial name
would miss the "label replaced after focus" case, so keeping it unconditional is acceptable; a
behaviour note is enough. Inconsistency: a Play/Pause toggle is announced only within 2 s of the
focus (pre-existing behaviour after that). Not verified live: whether VTE's accessible name
tracks the window title (that would speak the title once after a fast command).

### L-4 Glyph rules are inconsistent and `subtree_label` does not drop glyphs (low)
`browse/present.rs:567` checks only BMP PUA; `name_is_blank` also covers planes 15/16 and
U+FFFC. `subtree_label`'s comment says icon glyphs are dropped, but a mixed label such as
"\u{e900} Send" keeps the glyph. Terminal and GTK text paths do not call either rule (grep), so
Nerd-font terminal content is unaffected; only all-glyph names/browse items are blanked.

## Not findings
- Gecko does set `OFFSCREEN` from the per-document viewport cache
  (`refs/firefox/accessible/ipc/RemoteAccessible.cpp:1950-1968`): a node absent from
  `mOnScreenAccessibles` lacks SHOWING, which explains the containers; the dev claim holds.
- `visibility:hidden` maps to INVISIBLE (no VISIBLE) and stays pruned.
- The formatting-only hunks (commands.rs title lead, caret.rs, m1.rs) are rustfmt output.

## Round 2 (6b2d2c23; fixes in f73c7290, main merged at 77d45fc7) — 2026-10-18

Verdict: **MERGE**, with one follow-up (R2-1).

Gates were run once on 6b2d2c23 with 4 jobs and 4 test threads; incremental files were cleared afterwards.
- `cargo fmt --check`: clean.
- clippy with `-D warnings`: clean.
- `cargo test --workspace`: 2213 passed, 0 failed. shell_parity now passes 2/2 (`.toolchain` is a symlink and is not tracked).
- `xtask strings`: ok.
- `git merge-tree` against current main 06ada4e9: no conflicts (77d45fc7 already contains it).

Each finding, checked against the code:
- **H-1: fixed.** `collect.rs:173-192`: a web node that is not SHOWING is entered only when its bounds overlap the document's rectangle. When its bounds are unknown, the walker asks for them (`Miss::Bounds`) and does not go deeper. The unit test covers my probe case (5000 paragraphs above the viewport, then the on-screen heading): the result has 3 nodes, the heading is reached, and nothing is truncated.
- **L-1: fixed for text lines.** Each text node gets its document's rectangle in `text_clips`, and `lines_of` uses it. Off-screen controls are now pruned by H-1.
- **L-2: fixed.** `commands.rs:1183`: the name fill now skips ComboBox and any node that has a value.
- **L-3: fixed.** `flow.rs:1059-1070`: a name-only change is followed only when the node was first spoken without a name, or when its app is in `web_apps`.
  - `web_apps` comes only from `DocNotice::WebApp`. That notice is sent only when `is_gecko(info) || is_blink(info)` (`doc-source-atspi/factory.rs:255`).
  - VTE/GNOME Terminal is GTK, so it is not a web app. A title change can be echoed only if the terminal's first focus had a blank name.
  - Nit: also require `needs_name(role)` in that branch.
- **L-4: fixed.** `gorca_model::text::{is_icon_glyph, is_object_marker}` are now shared by browse, `name_is_blank` and `subtree_label`.
- **Paging up to 8192 children: bounded, does not block.**
  - At most 32 pages, read one after another. Each call carries its budget-class deadline, an error or an empty page stops the loop, and it runs in an async build task, not on an actor.
  - Each page fetches bounds (`want` includes BOUNDS when expanding for a miss), so off-screen children are pruned without extra calls.
  - **R2-1 (medium, follow-up):** the soft build budget is checked only between rounds, while the paging loop ignores it. On a slow but responsive app, one round can take 32 × 120 ms ≈ 3.8 s (Interactive class) or 32 × 2 s (Background class). Fix: give `expand` the build's start time and budget, and stop paging once the budget has run out.
  - Residual (low): an off-screen container with zero-size bounds, such as a wrapper of absolutely positioned children, now prunes its on-screen children if they lack SHOWING.
