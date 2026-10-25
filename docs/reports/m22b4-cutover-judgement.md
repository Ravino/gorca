# M22b4 cut-over judgement

Note: `docs/decisions.md` cites this file for the sub-cut-over 2 (where-am-I) judgement, but it
was in neither main 1ddd46cc nor this branch when this round started. The file is created here.
The sub-cut-over 1 and 2 judgements are not reproduced.

## Sub-cut-over 3: extensions, extension ABI, explain (b473d24f on 1ddd46cc), r1 2026-10-25

**Verdict: APPROVE AFTER** the must-fix item below (F-1) and the doc/tracking items F-2, F-4 and F-6.
The ABI change and the secrecy closure are sound. The parity claim proves less than it says (F-2).

### Commands run (worktree, `CARGO_TARGET_DIR=/var/tmp/gorca-shared-target CARGO_INCREMENTAL=0`, loadavg ~12)

| Command | Result |
|---|---|
| `cargo test -p gorca-ext-host` | all pass (29+4+2+27+4+2+4) |
| `cargo test -p gorca-core --lib` | 1032 passed, 5 ignored |
| `cargo test -p gorca-daemon --test shell_parity` | 2 passed |
| `cargo run -p xtask -- cutover --check` | ok |
| `cargo run -p xtask -- legacy` | ok, 19 rows / 49 uses; host feature rule ok |
| Probe: main's 0.3 `ext_gnome_shell.wasm` (built 2026-10-24, imports `gorca:ext/*@0.3.0`) through `ExtHostBuilder::add_wasm` on the branch | `Err("cannot load component: no exported instance named `gorca:ext/hooks@0.4.0`")` (the probe test was removed afterwards) |
| e2e evidence `bench/results/e2e/m22b4d-p1/p1.md` (untracked) | 31 PASS |

### Findings (ranked)

**F-1 (medium, must fix): GTK InfoBar is no longer read as a notice in any application.**
`present/notification.rs` `is_notice_node` / `is_notice_role` and `app_actor.rs:1095` now ask the
fused role. AT-SPI `INFO_BAR` (102) fuses to `SemRole::Status` (`gorca-sem-atspi/src/roles.rs:80`,
`54 | 102 => Status`), while the legacy map made it `Role::Alert`
(`gorca-connector-atspi/src/role_map.rs:15`, asserted by the test at :82). Failure: a GTK 3 app
shows an info bar (for example "file changed on disk"). Before, `app_actor` admitted the showing
event as an alert and the Reader spoke it as a notice. Now `alert` is false, the node is known, and
the event returns at `app_actor.rs:1098`: nothing is said. No corpus row, deviation row or decision
covers this. Fix: let the notice question accept the InfoBar origin (a raw-role fact, or a
`Status` that came from engine role 102). Or record it as an intended deviation that the owner
accepts. Add a test either way.

**F-2 (medium, doc): the "99216 facts" gate is runtime parity, not legacy parity.** `shell_parity`
feeds the same `PolicyView` grid to three runtimes of the new code. It cannot show that the Shell
rules fire on the same nodes as before. Mapping AT-SPI to legacy `Role` against AT-SPI to `SemRole`
for the rule sets gives these unrecorded changes:
- `DIALOGS` gains FILE_CHOOSER (19) and `alertdialog`.
- `LEVELS` gains DIAL (15), RATING (114) and SCROLL_BAR (48).
- `WINDOW` gains DESKTOP_FRAME (14) and INPUT_METHOD_WINDOW (89).

The comment in `extensions/ext-gnome-shell/src/lib.rs` ("map like the legacy mapping did: ... a
scroll bar is a slider") is false: the legacy map had no ScrollBar arm, so a scroll bar was
`Unknown`. A Shell scroll bar with a 0..1 range now gets `shell.level-percent` and
`shell.osd-level`. Real Shell exposure is low, but the claim is wrong. Fix: correct the comment,
drop SCROLLBAR unless it is intended, and record the widened sets in the decision row.

**F-3 (medium, explain correctness): explain lists the origins of a recomposition, not of the
words spoken.** `handlers.rs` `explain_ir` composes `Reason::Focus` from the node's entry as it is
now. No policy or extension edit is applied, so `via` is always 0. The last utterance may have been
where-am-I, a notice, a window title, extension words (`VIA_EXT`) or a policy `Say`/`Value`.
Failure: the Shell slider speaks "25 percent" (policy `Value`), yet explain lists
`kind=value via=0` with the compose value text. Fix: keep the redacted IR of each utterance at
speak time (the bench present log has it) and list that. If that is not done, the field lines and
the decision row must say "the focus composition now".

**F-4 (medium, tracking): legacy extension text paths remain after "deleted for good".**
`on-event rewrite` (`app_actor.rs:1144`), `host.speak` and `command-result::speak`
(`ext.rs:286/316`, `reader.rs:762`) still go as flat text to `say`/`announce`, bypassing PresIr and
render. ADR-0022 §2.9 maps them to an IR replacement and content utterances through render. No
`legacy.allow` row tracks them, so rule 15 cannot see them. Fix: add rows (or a plan item with an
expiry) before the tag `legacy/ext-last` claims the extension paths are gone.

**F-5 (low/medium, ABI): refusal of old components is clear but has silent consequences.** A 0.3
component is refused at load with the wasmtime export error above, logged at `warn`. This is
consistent with ADR-0019 V-1 (pre-1.0, no promise) and `WIT_PACKAGE`. But main's `target/ext-wasm`
holds 0.3 builds, and `load_dir` has no native fallback (`--shell-policy native` only), so a
deployment that does not rerun `cargo xtask build-ext` runs with no Shell policy and stays silent
about it. Fix: rebuild the components as part of applying the commit. Better, print "built for
`gorca:ext@X`, host is 0.4.0" in the refusal.

**F-6 (low, ABI direction): two accessor vocabularies.** 0.4 adds `read-group` /
`group-value {code, bits, number, text}`. The 0.10 draft has `model.prop` / `prop-value` with
`absent`, `position`, `grid`, `area` and `truth`. 0.4's shape is a strict subset under different
names, and it cannot say "known, no value". ADR-0026 §2.8 serves 0.3/0.4 only until `ext-gnome-shell`
and `ext-vte` are ported. Recommendation: freeze 0.4 as the legacy world (no further growth). If it
must grow again, rename it to `prop`/`prop-value` with `absent` so that the port to 0.10 is
mechanical. Do not keep two growing ABIs.

**F-7 (low, secrecy): explain field `chars` can carry a secret's length.** `FieldLine.chars` is
counted for every field whose own flag is not `SECRET`. Compose flags a field secret only by the
fused role (`compose.rs:422`). For a node that is secret by the native or record role only, the
value field's length is logged (the text is withheld, because `log_report` gates `with_text` on the
node-level `secret`). This is dev-only, and `SegLine.chars` already had the same behaviour before
this commit. Fix: set `chars` to 0 for every field when the node-level `secret` is true.

**F-8 (low): "enforced by type" holds for explain (`Redacted`) but not for the extension
boundary.** `ExtNode` and `PolicyView` have public, mutable text fields (`on_level_read` writes
`view.name`). Today every core construction goes through `ExtNode::new` with the full secrecy
question, and the label is filtered by `is_secret()`. That is correct by inspection, not by the
compiler. Fix: make the text fields private, with a `with_label` setter that checks secrecy.

**F-9 (low, doc): the decision row miscounts.** "Rows removed: 4 ext-snapshot, 14 policy-facts"
should read 2 ext-snapshot rows (4 uses) and 16 policy-facts rows (33 uses): 37 to 19 rows, 86 to
49 uses.

### Questions asked

1. **ABI break.** All first-party extensions are rebuilt against 0.4 (ext-vte unchanged in source).
   Old components are refused with a clear error (probe), not trapped. There is no negotiation, and
   ADR-0019 V-1 needs none. There is no conflict with ADR-0026 as long as 0.4 is the frozen legacy
   world (F-6).
2. **Secrecy.** The extension boundary closes rule 14 correctly: `ExtNode::new` asks
   `is_secret_entry || is_sensitive` (all three signals), and `get-node`, `read-group` (every text
   group, value text unexposed), `present` and `policy` read through it. The exemption removal is
   exact. Residuals are F-7 and F-8 (low). No path that shows a secret's text was found.
3. **Explain.** It is free of secret text (`Redacted`, per-field and node-level gates; the test
   holds). The origins are not those of the spoken utterance (F-3).
4. **Single commit.** It is justified for the WIT: a world cannot export 0.3 and 0.4 hooks at once,
   and the core policy path has a single caller set. The deletion is exact: 18 rows removed, rule
   15 passes at 19/49, and `role_to_ext`, `ext_role` and the WIT enum are gone. "No legacy path
   left" is false for the text paths (F-4).
5. **Hidden information.** No deviation rows were added. The information is lost without a row
   rather than hidden behind one: F-1 (InfoBar) and F-2 (widened role sets).
