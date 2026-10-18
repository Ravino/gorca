# Review: Firefox SPA stale title and wrong-target Enter (b9ef9030)

Date: 2026-10-19. Reviewer round r1. Branch `worktree-agent-a6afeefd1474b60fc`, head b9ef9030, against `main` (196f6e7e).

## Gates (run on b9ef9030, CARGO_BUILD_JOBS=4)

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` | 126 test binaries, 2306 passed, 0 failed |
| `cargo xtask strings` | ok (332 literals, 113 allowed) |
| e2e M8-CL x3, M8-AF/SY/AN/SPA/APP/FM x1 | see "E2E rerun" below |

## Findings (ranked)

### F-1 (medium-high): the role+name retry clicks a neighbour when siblings share a name

`crates/gorca-doc-source-atspi/src/source.rs`, `reresolve_and_retry` (~line 1585). After the refresh,
the guard checks only that the logical node now maps to an object with the same role and name, and a
new path. The identity ladder (`identity.rs::match_leftovers`, rung IL-3a) pairs duplicate full keys
**in sibling order**. When the page removes an item above the target and re-creates the rest, the
target's logical id moves to the next duplicate. The guard passes, and the click lands on a
different element without any warning. The doc comment ("an action is never redirected to a
different element") and the report claim the opposite.

Probe (`docs/reports/ff-spa-title-activation-review-probe.patch`, test
`probe_review_duplicate_names_retry_clicks_the_neighbour` in `tests/fake_peer.rs`): a list of three
items all named "Reply" (message actions, "Delete" buttons and avatar links are typical). The page
drops the first and re-renders the other two without DOM ids. A click on the second (old `lis[1]`,
now `f1`) reports `Ok(())` and acts on `f2`:

```
PROBE r=Ok(()) acted=[7] f1=6 f2=7
clicked a different element: [7] (f1=6, f2=7)
```

Fix: retry only when the pairing is unambiguous. Either the new object was matched by IL-1 or IL-2
(the same fresh DOM id), or the old sibling list held exactly one sibling with this (role, name, tag)
signature and so does the new list. Otherwise return `Gone`. Add the probe as a regression test.

### F-2 (medium): a removed item under the cursor still means a silent click on a neighbour

The owner's complaint is "Enter opens the wrong thing". In the branch's own run
`bench/results/e2e/cl-fix2` (`CL.gone.enter`), Bob is removed under the cursor, position restore
moves the cursor to a neighbour without saying anything, and Enter opens "Dave" (reader log
`Opened chat Dave (7)`). The step is written to accept this (`.min(1)`, any chat but Bob). The new
"that item is gone" path is never exercised end to end, because the mirror heals before the click.
The report lists this as a known risk ("as NVDA does"). It is the same user-visible failure class
the task set out to fix.
Fix (choose one): (a) when restore moves the browse cursor off a node that was removed while the
user was idle on it, mark the cursor "displaced". The next Enter then says the new item (or "that
item is gone") instead of clicking, and a second Enter clicks. (b) At least speak the new cursor
line when restore moves it in the focused document. Add an e2e step that asserts the speech, not
only that Bob and the menu were not opened.

### F-3 (medium): "lost to document" fires on any root focus after a same-document control

`crates/gorca-core/src/browse/commands.rs:1011-1023`. The condition (Browse on this doc, previous
focus a different node of this doc, new focus the root) cannot tell a re-render of one list from an
SPA route change that replaces the whole view: Alt+Left/Back inside an SPA, a router that calls
`document.body.focus()`, a modal that closes and lets the focus fall, or a mouse click on the page
background. All of these are now silent, and the cursor stays wherever position restore put it,
possibly in content that no longer exists. Before the change they announced the document and
started at the top. I found no test that covers the route-change case. It was not reproduced in
the rig (analysis only).
Fix: keep the cursor only while its node (or its identity successor) is still live in the mirror
and inside the same view. When the cursor node itself was removed, fall back to the old behaviour,
or at least speak the new cursor position. Add a unit test: focus on a link, the subtree holding
the cursor is removed, root focus, then assert that something is spoken.
Not affected (checked): new page loads (new DocId), tab switches (previous focus in another doc),
Alt+Tab away and back (`FocusAway` clears `focus_node`), browser chrome with a known chain (clears
`focus_node`), focus mode (`WebMode::Focus`, rule not applied), Chromium nested frame documents
(not the root).

### F-4 (low): a failing result directory was deleted

`docs/reports/ff-spa-title-activation.md` ("Not done / risks"): the removed `cl-title-nofix*`
dirs held "passing old-code runs plus one cold-start flake". The flake run is a failing result.
CLAUDE.md forbids deleting failing results. The dirs were never committed, so they cannot be
recovered from git. Nothing else is missing: `git status` shows no deletions, and `cl-base1`,
`cl-base2`, `cl-fix1` and `cl-fix2` are committed.
Fix: say so in the report (conditions and what failed). Do not repeat this.

### F-5 (low): a click timeout says "can't activate that" although the click may still happen

`doc_actor/mod.rs` `spawn_action`: any result that is not `Ok`, including the 500 ms
`ACTION_DEADLINE` timeout (`None`), raises `ActionFailed { gone: false }`. A busy SPA (Telegram
while re-rendering) can run the queued `DoAction` later, so the user hears "can't activate that"
and then the chat opens. Fix: for a timeout, say nothing or use a softer "no response yet", and
keep "can't activate" for definite bus errors.

### F-6 (low): `ActionFailed { gone: true }` for nodes without a source id

`doc_actor/mod.rs:~657`: a Click on a synthetic node with no `src` says "that item is gone". The
click targets come from `up_to(LINK|FORM_FIELD|CLICKABLE|FOCUSABLE)`, so this is rare, but the
wording is wrong for a node that never had an id. `gone: false` would fit better.

### F-7 (info): gorca+T latency and the never-block rule

`commands/handlers.rs::answer_or_refetch`. Every gorca+T now issues one `Interactive` fetch, capped
at 120 ms (`gorca-rt/src/deadline.rs:26`). On a timeout or error it answers from the cache. It never
blocks the reader, and there are no new `unwrap`s or non-exhaustive matches. Costs: the answer is
one bus round trip slower for every app, and when the app hangs it is up to 120 ms late. If a
focus change or keypress advances the epoch during the fetch, the reply is dropped as stale (E-3)
and nothing is spoken. That is consistent with superseded speech. The fix for the reported symptom
was not reproduced (the old code also passed in the rig), so it is contract-tested only. Acceptable.

## E2E rerun (b9ef9030 release, private rig)

Load average 19 (shared host, performance governor), memory 17 GB available. Dirs are kept in the worktree:
`bench/results/e2e/rv-cl/20261018T213625Z*` and `bench/results/e2e/rv-focus/20261018T213823Z*`.

| Scenario | Repeats | Fail (median/max) | Notes |
|---|---|---|---|
| M8-CL | 3 | 0 / **2** | one repeat failed `CL.foc.enter` and `CL.bump.title` |
| M8-AF, SY, AN, SPA, APP, FM | 1 | 0 / 0 | all pass |

### F-0 (high): the main fix is flaky under load, the original bug came back once in 3 runs

`CL.foc.enter` failed in one repeat. The reader heard: "Account menu, button, banner landmark,
button, Account menu, button, Re-render same, ... | Account menu opened". The browse cursor went to
the top of the page and Enter clicked the account menu, which is exactly the owner's bug. The
lost-to-document guard did not fire. In the reader log
(`rv-cl/reader-20261018T213337Z.log` lines 76-78), two focus events arrive at 21:35:18.884 and
the menu click lands 80 ms later. The guard depends on `focus_node` holding the link that the page
focused. If that focus event is coalesced or dropped, or arrives after the document focus, under
load, `prev_focus` is the root or the trigger button, and the old path runs. The mechanism is not
verified, because the phase log does not record `prev_focus` or the decision. Fix: decide from the
browse cursor rather than from focus history. A root focus while browsing this document, with no
action of ours and the cursor on a live node that is not the root, keeps the cursor. Combine this
with F-3 (fall back when the cursor node is gone). Log the decision in the `web_focus` phase line.
Rerun M8-CL at least 5 times under load.

### F-0b (medium): gorca+T said "window title not available" once

`CL.bump.title`, same repeat. `window_title` returns `None` only when the chain has no
`Window`/`Dialog`, so the answer was built from a chain without the frame. Candidates: the mirror
missed the focused node and the ancestry has a gap (old path), or the new window fetch answered and
`answer_node` re-walked an ancestry that broke under load. Not reproduced, cause unverified. Fix:
when the window fetch fails or yields no title, use the last good cached title. The cache fallback
exists only on the error branch today. Add a counter or log line for which branch answered.
