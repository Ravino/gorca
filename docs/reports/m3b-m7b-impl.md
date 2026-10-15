# M3b and M7b implementation report: Firefox fields, browse say-all, layout-aware quick navigation

Branch `worktree-agent-a702d0a17a733562c` (checkpoint `09ff3ef` plus the finishing commit). Placement follows ADR-0008: the
web engine and the text layer live in core and never branch on a browser; Gecko quirks are data in the AT-SPI connector
(`Toolkit::Gecko => TextUnit::Utf16Bom`, the `GetAttributeRun` attribute-name table); no W2 product rule was needed, so
`ext-firefox` stays an M8 item. Rig: `tests/web/m7/run_m7.sh <label>` (private session and a11y bus, headless gnome-shell on a
unique Wayland socket, private Firefox profile per launch). Results: `bench/results/e2e/<label>/`; failing runs are kept.

## What was built

| Area | Where | What |
|---|---|---|
| Gecko offsets (M3b) | `gorca-core/src/text/{mod,caret}.rs`, `gorca-text/src/seg.rs`, `directory.rs` | Firefox fields are followed in Gecko's own UTF-16 offsets: the window keeps the string with its phantom U+FEFF after each astral scalar, unit rules keep a phantom with its scalar, text-changed inserts get their phantoms back from the length, the verbalizer drops U+FEFF and U+FFFC. `MirrorDirectory::text_scalar` (the M3 skip) is replaced by `text_window_follows` (false only for UTF-8 byte offsets, VTE). |
| Misplaced edits | `text/caret.rs` (`MISPLACED_MAX`, `Absorb.alt`) | An insert reported 1-2 units after the known caret (typed, committed or scripted; not a paste) and a BackSpace/Delete reported away from where the key leaves the caret drop the window and accept the caret event where the caret really went, so the character is not spoken twice. Tests `a_commit_reported_one_past_the_caret_is_spoken_once`, `a_backspace_reported_at_the_caret_is_spoken_once`. |
| Contenteditable | `text/mod.rs` (`text_accepts`, `In::Retarget`) | Caret, edit and selection events of the paragraph inside a focused rich editable retarget the machine (CE-1); a vanished paragraph goes back to the root. |
| Formatting | `connector-atspi/src/text/format.rs`, `core/src/text/formatting.rs` | `info.formatting` (Insert+F): `GetAttributeRun` with defaults, mapped by the connector's per-toolkit table: font, size, emphasis, script, colours, alignment, language. |
| Field say-all, selection | `text/say_all.rs` | Say-all and Shift-selection reuse the M3 machines on Gecko offsets. |
| Browse say-all (M7b) | `core/src/browse/sayall.rs` | Insert+Down in browse mode reads the document's flat text with the native say-all engine; chunks are spoken as the line presenter speaks them; the virtual cursor follows the chunk the server starts; any key stops it. |
| Auto-read on load | `browse/*`, `config/settings.rs` | `browse.say_all_on_load` (default true): after a load the title is spoken and the page reads itself; tab switches and Alt+Tab do not start a reading. |
| Layout-aware keys | `crates/gorca-input-xkb` (new), `commands/compile.rs`, `daemon/config_task.rs` | Grabs use the union of the keysyms each physical key types in every configured input source (libxkbcommon via dlopen, 10 s re-poll): H/K/... and Shift+1..6 work on the Russian layout. |
| M7a deferrals | `browse/commands.rs`, `browse/present.rs` | Frame (M) and article quick navigation; Escape belongs to the page while the focused field's popup is open; Shift+digit headings. |

## Review of the previous agent's last change (scoring)

The checkpoint's last step replaced the window scoring of the field scenarios ("a speech before the next key") with an
"order-based" scorer. That scorer searched forward from the last match for the first speech with the expected text within
1.5 s, so a wrong or extra speech between two hits was skipped silently, and a missing speech could borrow the next
identical key's speech (the FC1h line repeats `😀`). It could hide real misses, so it was **replaced**, not kept:

- `align()` in `crates/gorca-bench/src/e2e/webtext.rs` aligns every speech from the first key on with the keys by a longest
  common subsequence (a speech may stand for a key only with the key's text, after the key, within 1.5 s, in order). An
  unmatched key is `lost`; a leftover speech is `fc_double` if it repeats the matched one before it, else `fc_extra`. All
  three are gated at 0 in every repeat (`fc_extra` is new). Six unit tests cover late-in-order, no borrowing, doubles, wrong
  text, too late, and unscored keys.
- FC1h derives what each key must say from the page's own caret events (offset in UTF-16 units from the event tap), not
  from a model: under load the private Firefox occasionally ignores a virtual-keyboard arrow, and a model then shifts every
  later expectation by one (run `m3b-cont-b`: 5 such keys, 35 "lost", 23 "extra", none of them gorca's). Keys the page
  did not act on are not scored and are counted in `page_dropped` (information; 0 in every final repeat). Keys gorca
  swallowed are gated (`fc_swallowed` = 0).
- `tests/web/m7/ax.py find` matched a name by substring in BFS order, so FE1's "Typing" found the shallower contenteditable
  "Typing region:"; every earlier FE1 result measured the contenteditable. It now prefers an exact name. Typing into the
  contenteditable is now its own scenario, FCE2.
- Window scoring stays for the M3 fake-app scenarios (`m3.rs`), where no browser lag exists.

What the gate still cannot see: a key that says nothing followed by the next key with the same text spoken twice would score
as two hits (the texts and order are identical); latency of the second would be high, and FC1h's p99 3 ms would catch it.

## Gates

Final run `bench/results/e2e/m3b-m7b-final/20261015T092518Z.txt` (release, 3 repeats, all 24 scenarios, host loadavg 9 to 12
from other agents' runs, governor performance). Rechecks with `gorca_text=debug` in `m3b-m7b-recheck*`.

| Gate | Median (max) | Limit | Result |
|---|---|---|---|
| FC1h caret event to speech p99 | 0.22 ms (0.24) | 3 ms | pass; lost/double/extra/swallowed 0 |
| FC1 Down p99 | 0.83 ms (1.0) | 15 ms | pass; lost 0 |
| FE1 input typing p99 | 0.48 ms (0.53) | 3 ms | pass; lost/double/extra 0 |
| FCE2 contenteditable typing p99 | 0.45 ms (0.46) | 3 ms | **1 double in 1 of 3 repeats** (1 of 1800 keys); rechecks 6 repeats clean |
| FE2 randomised session | 0 missing / doubled / key names / wrong | 0 | pass (5 to 13 doubles per repeat at the checkpoint) |
| FS2 Shift+arrows p99 | 1.27 ms (1.40) | 15 ms | pass |
| FSA1 / FSA3 field say-all | first chunk 3.7 ms (10.0); stop 0.39 ms (0.46) | 15 / 2 ms | pass; caret placement 0 wrong |
| FCE1, FT1, FP1 | 0 failures | 0 | pass |
| BSA1 browse say-all p99 | 0.36 ms (0.39) | 5 ms | pass |
| BAR1, BL1, FR1 | 0 failures | 0 | pass |
| M7a B0 W1 BR1 BR2 W3 FM1 FM2 | all pass | | pass |
| W2b gorca-side p99 | 120.7 ms (max repeat 3004 ms) | 150 ms | median passes; **one repeat had a 3 s trial without a warm-up (`untagged_warm` = 1)**; rechecks 6 repeats: 93.7 ms (121.5) and 57.2 ms (81.0), untagged 0 |
| Strict K2/K3/K4, R1 | K2 0.28 ms, K3 0.85 ms, R1 0.43 ms, K4 all 0 | | pass (`m3b-m7b-strict2*`; the first attempt without `--impersonate-orca` is kept in `m3b-m7b-strict*` as a harness misuse) |
| fmt, clippy `--all-targets -D warnings`, `cargo test --workspace` (1471), `cargo xtask deps` | | | pass |

## Q-E4: does Gecko offset conversion need a text-run cache?

**No, and no conversion is done.** Gecko's ATK text is the UTF-16 string with a phantom U+FEFF after each astral scalar, so
every offset (caret events, text-changed offsets and lengths, `GetStringAtOffset` ranges, selections, character count) is an
index into the very string the window holds. Measured (`crates/gorca-text/tests/qe4.rs`, `bench/results/qe4/`):

| Approach | Cost |
|---|---|
| Chosen: window in Gecko units | character unit 6 ns (no emoji) / 0.21 µs (emoji in the window); word unit 0.1 / 1.4 µs |
| Convert raw to scalar from the text in front of the caret | 0.4 µs at 1k scalars, 5.6 µs at 16k, 35 µs at 100k, 350 µs at 1M, plus an IPC read proportional to the caret position |
| TX-1 run cache (sorted phantom offsets) | lookup 5 to 9 ns, build 12 µs (16k) to 463 µs (1M), every edit shifts all later entries |

The only gap is the text of a `text-changed` insert, which lacks the phantoms its length counts: the Reader puts them back.

## Open items

- FCE2 double (1 in 5400 keys over 9 repeats): not reproduced with debug logging; the log of the failing repeat has no
  text trace. Suspect a late paragraph insert after a retarget. Keep FCE2 in the gate set.
- W2b warm-up miss: one trial in 54 had no warm-up and took 3 s. This is the M7a warm-up path that the mirror-prefetch work
  touches; not changed here.
- Live checklist `docs/plan/checklists/M7b.md` not walked on the real desktop. Known gaps listed there (say-all in a
  contenteditable reads one paragraph; Ctrl+A in a contenteditable; live regions, dialogs, container announcements: M8).
