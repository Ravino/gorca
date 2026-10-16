# M8 review, round 2 (head 14a82da)

Reviewer, 2026-10-16. Worktree `.claude/worktrees/m8-review`, detached at `14a82da`. Probes are in
`docs/reports/m8-rereview-probes.patch` (apply on 14a82da). Results are in `bench/results/e2e/m8-rereview-*`.

## Verdict: MERGE AFTER FIXES

The round-1 items are fixed as described: the caret-follow substitute is gone, the state words are fixed, the alert
is kept through a bulk re-render, live speech has a TTL and goes through `fair.rs`, virtual selection is the default,
and core has no Gecko rules. Gates at 14a82da are green: `cargo fmt --check` passes,
`clippy --workspace --all-targets -D warnings` passes, and `cargo test --workspace` gives 1911 passed, 0 failed.

The new D-2 Tab design brings two new defects that must be fixed before merge (R-1, R-2). It also has a
should-fix regression in the live path (R-3).

## Findings (ranked)

### R-1 HIGH: a held Tab swallows its own release after a focus-to-browse switch
`browse/commands.rs:604-626` (Tab in the grab set) and `gorca-input-mutter/src/source.rs:504-601`. The grab deferral
covers grabbed **modifiers** only (P-7). A keystroke grab added while that key is down is applied at once.

Mutter 48.7 (`git show 48.7:src/backends/meta-a11y-manager.c`, `notify_clients`) works like this:
- An autorepeat of the held key now matches `should_grab_keypress`.
- The keysym goes into `grabbed_keypresses`.
- The release is then consumed (`events.c:273`, `CLUTTER_EVENT_STOP`).

So the application got the press but never gets the release.

Scenario: the user holds Tab to walk a form in focus mode. Firefox moves focus onto a link, so gorca switches to
browse mode and grabs Tab (a target follows). The next compositor repeat is grabbed, and the release never reaches
Firefox. Wayland clients repeat keys on their own side, so this is expected to become a runaway Tab until the next
key event. The runaway is inferred, not measured on real Mutter.

Measured on the 48.7 emulation (`fake_mutter.py`, same table logic), run `m8-rereview-prb-sy`:
- `PRB.f.down` Tab press: consumed false.
- `PRB.f.link` focus on the link: "browse mode, Link three".
- `PRB.f.rep` repeat: **consumed true**.
- `PRB.f.up` release: **consumed true**.

Before D-2, Tab was never grabbed, so this path is new.

Fix: extend `held`/`deferred` to non-modifier keystrokes. Track keys that are down and passed to the app, and drop
(or defer) any new keystroke grab for those keysyms until their release. Add an e2e step like the PRB.f.* probe.

### R-2 HIGH: the Tab grab cache goes stale and leaves the grab set inverted with no time limit
`web_sync_tab_grabs` (`commands.rs:2115`) only pushes when `b.tab_grab` changes. But every other `SetGrabSet` push
uses the live availability without updating `tab_grab`. Those are: mode changes at lines 754/764/864, the letters
toggle at 2186, `commands/mode.rs:16` and `handlers.rs:646`. `BrowseState::new` also starts it at `(true,true)`.

Unit probe `probe_rr_stale_tab_grab_after_a_foreign_grab_push`:
1. Settle at the heading, so `tab_grab=(true,false)`.
2. Focus the edit (focus mode), then Escape: browse at the edit pushes `(false,true)`.
3. Wait 5 s, Ctrl+Home, settle: no push.

Result: at the top, Tab is not grabbed although a link follows, so it goes to Firefox. Shift+Tab is grabbed although
nothing precedes it: it says "no previous focusable element" and the key is swallowed. This is not a 150 ms race. It
lasts until the availability flips again.

Fix: record the availability wherever the grab set is built (for example, a `push_grabs()` helper that sets
`tab_grab` and pushes `current_grabs()`), or compare against the last pushed set.

### R-3 MEDIUM: an alert waiting for the owner's speech budget is replaced by later polite news
`present/fair.rs:257-269` (`e.waiting.replace(u)` does not look at priority) is fed by `web_live` → `notice_speak`
(new in 9c17363). Probe `probe_rr_alert_waiting_for_budget_is_replaced_by_later_polite_news`: 3 polite messages,
then an alert 20 ms later, then 1 polite message 20 ms after that. Spoken over 4 s: `news 1..4`. **The alert is never
spoken** and is only counted as dropped. That undoes the purpose of M-3 under light ticker traffic.

Fix: an assertive item never gets replaced by a polite one (keep the higher-priority item, or one waiting slot per
politeness), or assertive news bypasses the bucket.

### R-4 MEDIUM: Shift+Tab steps out to the outermost focusable ancestor, including iframe documents
`focusable_around` (`commands.rs:518`) returns the outermost focusable ancestor other than the root, and
`tab_target(Prev)` searches backwards from there.
- Unit probe: inside `<p tabindex=-1>`, Shift+Tab after the link says "no previous focusable element" instead of
  focusing the link. Think of the common `<main tabindex=-1>`.
- e2e `m8-rereview-prb-if` `PRB.if.stab`: the cursor is inside the nested frames. Shift+Tab was not grabbed
  (consumed false) because the outermost frame document has nothing before it.
- Native selection is also disabled everywhere inside a frame or a focusable container, because
  `inside_focusable` uses the same function.

Fix: use the nearest focusable ancestor, and treat embedded document roots (`Cat::FRAME` children) as not
focusable for D-2.

### R-5 MEDIUM: Tab stops on every nested frame document and never defers to the real tab order
`PRB.if.tab1..5`: "Nested level one, document", "a link", "Level two frame, document", "Level three frame, document",
"Level four frame, document".

NVDA does not override the tab order when the last caret move came from a focus change (#5227) or when the caret is
inside the focused object (`refs/nvda/source/browseMode.py:2132-2149`). gorca always walks `Cat::FOCUSABLE` in
document order. That includes `tabindex=-1` items such as toolbar roving buttons, and ignores positive `tabindex`.

Fix: once the cursor is on or inside the focused node, or the last cursor move was focus-driven, leave Tab
ungrabbed. Excluding frame documents (R-4) also covers the iframe case.

### R-6 LOW
- **Silent Tab swallow on a failed focus.** `GrabFocus` false or an error (`doc-source-atspi/src/source.rs:1313`,
  `r.map(|_| ())`) gives no feedback after a grabbed Tab.
- **Native-selection toggle silent outside the web.** `browse.native_selection_toggle` has `ContextMask::ANY`; with no
  web state the toggle returns silently.
- **"half checked" for non-checkboxes.** MIXED words are not limited to check boxes in `present_change` /
  `present_with`. Gecko sets MIXED on indeterminate progress bars (`HTMLFormControlAccessible.cpp:788`, ARIA
  `eIndeterminateIfNoValue`) and on `aria-pressed=mixed`. GTK sets it on tri-state pressed
  (`gtkatspicontext.c:271`). So these say "half checked" (and a toggle also says "not pressed").
- **No focus-mode Tab pass-through step.** The harness has no step that checks Tab passes through in focus mode now
  that FM.pass.tab is removed. Add one.

## Round-1 items

| Item | Status |
|---|---|
| H-1 caret follow | fixed: no `SetCaret`/`SetSelection` from reading (unit `reading_never_sets_a_page_caret_or_moves_focus`, SY.nomove.k/.sel). D-2 is a new design, see R-1, R-2, R-4, R-5 |
| M-1 / M-2 | fixed: decode test covers GTK Expandable/Expanded and Indeterminate. Change words are one tri-state (unit tests pass). R-6 is a residual |
| M-3 | fixed: the alert is spoken with 0, 63 and 500 extra touched nodes. R-3 is a new loss path |
| M-4 | TTL is applied at dequeue and when the queue is full. Expiring items are evicted before same-priority notices. TTL starts at speech-queue entry, not at the mutation (time waiting in `fair` does not count), so news can play later than 5 s after the change |
| M-5 | fixed: virtual by default, gorca+Shift+F10 toggles native, stale page selection is collapsed. R-4 limits it |
| M-6 | fixed: no engine names in core code outside comments and tests |

**Grab-set churn.** At most one `SetKeyGrabs` per 150 ms settle, and only when Tab availability flips. On normal
pages that happens only near the first or last focusable. The modifier set never changes, so this adds no exposure
to the stuck-modifier bug in `live-keys-swallowed.md`. The keystroke-held hazard is R-1.

**Harness.** Removing FM.pass.tab and FM.pass.shift_tab is justified by D-2 (their premise is inverted), and the
failing runs are kept. The SY.tab* steps press keys through the emulated Mutter and check the real focus event
(honest). SY.tab.out only checks the emulated verdict.

## M7 set: interleaved A/B (no regression shown)

The base binary is the merge base `08a11b3` (main as merged into the branch). It was built with `git archive`. Its
rev label says 14a82da because it was built inside the worktree, so the two binaries are told apart by a marker
string and their sha1. The branch binary is `14a82da`. Both arms use the same e2e harness. The order was
base, branch, branch, base, base, branch, with `--only FC1h,FS2,FE2,L1 --repeats 1`. Memory pressure stayed
low (some avg10 ≤ 1.33). Log: `bench/results/e2e/m8-rereview-ab.log`.

| Run | Load (1-min) | Red rows |
|---|---|---|
| base r1 | 14.1 | FC1h page_dropped 5, FS2 fc_extra 5, lost 1, p99 16.6, FE2 e2_double 2, e2_missing 1, e2_resync 9 |
| branch r2 | 12.7 | FS2 fc_extra 2, lost 1, FE2 e2_resync 8 |
| branch r3 | 10.5 | FE2 e2_wrong 1, e2_resync 10, L1 lost 3 |
| base r4 | 11.6 | FC1h fc_extra 7, fc_late 1, page_dropped 8 |
| base r5 | 11.3 | none |
| branch r6 | 8.3 | FS2 fc_late 1 |

FC1h, FS2 and FE2 fail in single repeats on both arms, and base is no better. L1 `lost 3` happened in one branch
run only. L1 is the generic event path, which M8 does not touch, and earlier main-era runs show L1 lost too. This is
not evidence of a regression, but it has not been cleared at low load.

## Merge with current main (03e6f9b)
`git merge-tree`: one conflict, in `docs/decisions.md` (keep both sides' rows). `reader.rs`, `present/flow.rs`,
`harness_focus.rs` and `daemon/tests/e2e.rs` merge automatically. The merged tree was not compiled.
