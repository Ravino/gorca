# M3b + M7b review (Firefox caret/typing echo, browse say-all, layout-aware quick nav)

Reviewer, 2026-10-15. Branch `worktree-agent-a702d0a17a733562c`, head `00bdce8`, base `932d611`.
Verdict: **MERGE AFTER FIXES** (one high-severity core bug, one scoring hole that the report wrongly says the
latency gate covers, and a typing double that now shows up in FE2 as well as FCE2).

## What I ran

| What | Where / how | Result |
|---|---|---|
| `cargo fmt --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` | branch worktree, head 00bdce8, clean tree | all pass; 1471 passed, 0 failed, 16 ignored |
| Release build | same | ok |
| e2e FE2, FCE2, FC1h, 3 repeats | `tests/web/m7/run_m7.sh m3b-m7b-review --repeats 3 --only FE2,FCE2,FC1h --no-fail`, private session, rev 00bdce8, `git_dirty=false`. Load 15.5 at start, 6.5 at end (the M6 review e2e and the B-1 bench ran alongside), governor performance | **FE2.e2_double FAIL (max 1)**; FCE2 and FC1h pass. `bench/results/e2e/m3b-m7b-review/20261015T102235Z.txt` |
| Unit probes | `bench/results/e2e/m3b-m7b-review/probes/` (inserted temporarily, then `git checkout`) | 3 of 4 probes show defects (below) |
| Trial merges | `git merge-tree` against `main` and the B-1 branch | see F-9 |

Note on the implementer's evidence: the "final" run `m3b-m7b-final/20261015T092518Z.txt` was taken at `git_rev=09ff3ef`,
`git_dirty=true`, not at head, and it has **two FAIL rows**: `FCE2.fc_double` (max 1) and `W2b.untagged_warm` (max 1).
My run at the clean head is the only gate evidence for 00bdce8 itself.

## Findings (ranked)

### F-1 HIGH: a rich editable is stolen by any text change elsewhere in the same app, and that text is spoken
`crates/gorca-core/src/text/mod.rs:582-616` (`text_accepts`, `text_outside`). While a contenteditable is focused
(`t.hosts`), an event from **any** node of the same app is accepted and retargets the machine unless the mirror
*knows* the node hangs outside the root. `Lookup::Miss` and `Lookup::Gone` count as inside. Text-changed events
are accepted the same way as caret events.

Failure scenario: you type in a contenteditable (a mail composer or a chat box) while the page updates a clock, a
counter or a chat log somewhere else. Gecko sends `text-changed` for that node. It is new or not mirrored, so it is a Miss.
The machine moves to that node and the Reader says `"12:01"` (inserted-text echo). The paragraph's window, caret and selection
are dropped.

Probe `probe_unrelated_text_change_does_not_steal_the_rich_editable` (probes/probe_text.rs): the result is
`spoken = ["12:01"]` and `tracked = LocalId(500)` (the clock); the expected result is nothing spoken with `LocalId(96)`
(the paragraph) still tracked.

Fix: only a **caret** event may retarget on a Miss (Gecko fires caret-moved only for the focused editable's
descendants). Edit and selection events from a node that is not the tracked one are accepted only when the mirror
confirms ancestry (`Hit` chain reaching `root`). Otherwise drop them, or ask the connector for the parent chain under a deadline.
Add the probe as a regression test. This interacts with B-1: the focus-window prefetch changes which nodes are a Hit, so how often this
fires changes after B-1 merges, but the hole exists either way.

### F-2 MEDIUM: the strict scorer still hides "silent key + doubled next key", and the latency gate does not catch it
`crates/gorca-bench/src/e2e/webtext.rs:424-504` (`align`), `:409` (`score_aligned`). The implementer admits the blind spot
and says "latency of the second would be high, and FC1h's p99 3 ms would catch it". **That is false.** For the
`from_event` scorers, latency is measured from the *last* page caret event between the key and the speech. For the
borrowed first copy, that is the *next* key's event, so both copies come out at sub-millisecond latency. For the
key-based scorers (FE1/FCE2), one 110 ms outlier among 600 samples does not move p99.

Probe `probe_align_silent_key_then_double_is_hidden` (probes/probe_align.rs): keys `x`,`x` 140 ms apart. The first is silent and
the second is said twice, 0.3 ms apart. The score is `lost 0, doubled 0, extra 0, lat [0.3, 0.6]`.

Exposure: FC1h has adjacent equal expectations by construction (positions 4 and 6 are both the emoji), and FE1/FCE2 hit
equal adjacent letters with probability 1/26. FC1 and FE2 are not affected (FE2 uses its own per-action model scorer).

Cheap fix (about 10 lines): measure each match's latency from **its own** key's first page event (`p.t_ev`, already
computed) for `from_event`, or from its own key otherwise. Then gate a new count `fc_late` (matches with latency > 50 ms)
at 0. Gorca-side echo is 0.2 to 0.5 ms here, so any match 50 ms late is either a borrowed speech or a real latency
failure. Both should fail.

### F-3 MEDIUM: intermittent double speech while typing reproduces at head, now in FE2 as well
In my run at 00bdce8, FE2 failed in 1 of 3 repeats: `#260 Space: wanted "space" once, said ["space","space"]`, after
Home and then BackSpace at caret 0 (a no-op). The implementer's final run had FCE2 `fc_double` 1 (a stray `"m"` 9.3 ms after
key #24). This makes **2 doubles in about 4,000 field-typing actions over 6 repeats** across the two scenarios. FE2's
scorer is a per-action model check and cannot invent a double, so this is gorca output, not a harness flake.

A unit replay of the FE2 sequence (probes/probe_fe2_double.rs, GTK and Gecko rigs, gaps of 100, 500 and 1000 ms) speaks once, so
it is a timing race the rig does not model. The reader logs of both failing runs are at `warn`, so there is no trace.
In the real desktop, keys reach gorca (Mutter monitor) and Firefox in parallel. That gives more reordering than the rig,
where gorca sees the key first, so live frequency may be higher.

Recommendation: do not label these flakes. Keep FE2 and FCE2 gated. Before merge, run FE2 and FCE2 with
`RUST_LOG=...,gorca_text=debug` until one double is captured (about 10 repeats at the observed rate), then fix it or
explain it.

### F-4 MEDIUM: the Gecko insert-offset quirk is hard-coded in core for every toolkit (ADR-0008 non-conformance), and it can swallow a real move
`crates/gorca-core/src/text/caret.rs:46` (`MISPLACED_MAX`), `insert()`, and `delete()` (`alt`). The quirk is real. The implementer's
debug logs show Gecko reporting `Insert{offset:5}` with the caret at 4 and then `caret 5`: 13 of 461 inserts in
`m3b-cont-c`, 24 of 1050 in `m3b-cont-b`, and 0 in the contenteditable run `m3b-cont-d`. But ADR-0008 puts per-engine quirks in
connector **data rows** with ids and `disabled_quirks`. Here the tolerance applies to GTK, St, Qt and any future engine.

Probe `probe_scripted_insert_after_caret_swallows_a_real_move`: in a GTK entry with the caret at 2, a script inserts `X` at 3
(no key). A mouse or uncorrelated caret move to 3 within 50 ms is then **silent**. The baseline probe without the insert speaks.

Fix: put the slack in the app profile (`Toolkit::Gecko => insert_offset_slack: 2`, others 0) and read it in core as data, with a
quirk id in the log. The Idle-path risk is narrow (50 ms), so this is medium on architecture and low on user impact.

### F-5 MEDIUM: W2b in the final run has a 3 s gorca-side trial and an `untagged_warm` FAIL
`m3b-m7b-final`: `W2b.w2b_gorca_ms.max` is 3004 ms in one repeat and `W2b.untagged_warm` FAIL. No recorded phase exceeds
116 ms, so the 3 s sits outside every instrumented phase (it looks like a deadline expiry). The median-over-repeats
gate hides it. The branch adds `DocNotice::Loaded` and page auto-read (`say_all_on_load` default **true**) on the
same load path. I did not verify whether W2b runs with auto-read enabled or whether that path contributes. The warm-up
itself is M7a code that the B-1 branch (agent-a259b4477ef251616) rewrites, so a fix belongs there. Before merge, record
it as a known open gate failure with an owner, not as "median passes".

### F-6 LOW: FC1h exempts speech after a page-dropped key, and `page_dropped` is not gated
`webtext.rs` fc1h and the `owner` check in `align`. FC1h's expectations come from Firefox's own caret events. Gorca cannot move
that caret in FC1h (no say-all or review), so this is **not circular**, and I agree with deriving the expectations this way. But a key with no page caret
event is unscored, and anything spoken after it is ignored. Gate `page_dropped` at a small bound (for example 2 per repeat) and
accept only nothing or the edge character after a dropped key.

### F-7 LOW: the verbalizer drops U+FFFC everywhere
`crates/gorca-speech/src/verbalize.rs` `without_phantoms` is applied in `unit()` and `spell()`. A character unit that is an embedded
object (an inline image or link inside a contenteditable paragraph, or an embedded child in a GTK TextView) becomes empty and
is spoken as "blank". Fix: say the object's role and name, or at least "object", for a lone U+FFFC. Strip only U+FEFF.

### F-8 LOW: smaller items
- `crates/gorca-core/src/browse/sayall.rs` `web_loaded`/`web_auto_read`: `loaded` is keyed by **app**. A `load-complete` of
  another tab or an iframe of the same Firefox within 4 s makes a still-loading page start reading at once. Found by code reading
  only; I did not probe it.
- `crates/gorca-core/src/commands/compile.rs` `grabs_where`: browse mode keeps a keysym typed by an unbound key of another layout
  (Shift+' on US is swallowed in browse mode when `ru` is configured). This is documented and deliberate, but it removes a key from the page.
- `crates/gorca-daemon/src/config_task.rs` layout poll: every 10 s it spawns 4 `gsettings` processes and recompiles the keymaps.
  `timeout` does not cancel the `spawn_blocking`, so a hung keymap compile leaks one blocking thread per tick. A timeout also
  silently swaps the grabs to the US-only table until the next tick. Re-probe only the `sources` key, and keep the
  previous table on timeout.
- `tests/web/m7/ax.py find`: the exact name is tried first, then it silently falls back to a substring match. The FE1/FCE2 mix-up can
  recur if Firefox changes a name. Log the resolved name in the notes, or fail when the exact match misses.
- `docs/decisions.md`: a blank line before the new rows splits the table, so they render as plain text.
- `crates/gorca-input-xkb`: crate-level `#![allow(unsafe_code)]` with SAFETY comments on each block. This is acceptable for dlopen.
  Prefer `#[allow]` on the FFI module only.

### F-9 INFO: merge mechanics
- `git merge-tree main <branch>` conflicts only in `docs/decisions.md` (main moved after 932d611), so it needs a rebase.
- With the B-1 branch: `fetch/caller.rs` **auto-merges** (M3b appends `attribute_run`, and B-1 adds `children_of_capped` above it, so there
  is no semantic overlap). The conflicts are `.claude/agent-memory/rust-engineer/MEMORY.md`, `crates/gorca-core/tests/pipeline.rs` and
  `docs/decisions.md`, all from main drift, not from M3b code.
- Semantic interactions with B-1: F-1 depends on mirror Hit/Miss coverage, and F-5 depends on the warm-up path.

## Claims checked

| Claim | Verdict |
|---|---|
| The strict LCS scorer cannot hide misses | Mostly true: a maximum matching with every leftover gated. The one admitted hole is real and is **not** covered by latency (F-2). |
| FC1h expectations from page caret events are not circular | Agreed. The residual masking is through unscored dropped keys (F-6). |
| Folding the next caret move into a misplaced insert | Fixes the measured Gecko quirk (FE2 doubles 5 to 13 per repeat down to 0 or 1). It is generic in core and can swallow a real move (F-4). |
| Harness substring bug fixed | Yes (exact name first). The fallback remains (F-8). |
| FCE2/W2b failures are flakes | No. FCE2 and FE2 doubles are gorca output (F-3). The W2b 3 s stall is unexplained (F-5). |
| Q-E4: no Gecko offset cache needed | Agreed. The phantom U+FEFF is confirmed in `refs/firefox/accessible/atk/DOMtoATK.cpp` (`AddBOMs`). `crates/gorca-text/tests/qe4.rs` measures the right alternatives. The only caveat is that it is a microbenchmark of the segmenter, not of the IPC read that conversion would need. |
| No unwrap in non-test code; deadlines on IPC | Yes. The only new `unwrap` is in xkb tests. `attribute_run` goes through `Caller::call` with a `Deadline`, and the keymap build is under `KEYMAP_DEADLINE`. |
| fmt / clippy / tests | Pass (reproduced). |

## Must fix before merge
1. F-1 (retarget guard), with the probe added as a test.
2. F-2 (`fc_late` gate), with the probe added as a unit test.
3. F-3: capture one double with debug tracing, then fix it or explain it. Until then, FE2 and FCE2 stay gated and red-on-failure.
4. Rebase on main (decisions.md) and fix the table blank line.

Should fix: F-4 (move to a quirk row), F-5 (owner and issue, coordinated with B-1), F-6, F-7.
