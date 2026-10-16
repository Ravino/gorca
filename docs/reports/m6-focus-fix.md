# M6 focus fix: terminal output after a focus change, before any key

Closes m6-review finding F3 and B-1 adoption finding F-1 for terminals. Code commit: `fix(terminal): earn the focus protection on the repeated focus event, drop the key workaround`.

## Bug (reproduced on main)
After a terminal gets the focus for the first time (a new window, a new tab) its output was silent until a key was pressed.
Reproduced on main (with the key workaround) by the new real-terminal steps: `vv_focus_failures` = 2 (new window: heard only the focus speech; new tab: same), `bench/results/e2e/main-new-step`.
With the workaround removed and nothing else changed, VV fails badly (`vv_failures` 10, `vv_screen_failures` 7, `bench/results/e2e/nokey*`): the protection is never earned.

## Root cause (measured, reader trace of a real gnome-terminal-server 3.56 / VTE 0.80)
The real VTE sends two focus events a few ms apart on a first focus. The read after the first one answers without `FOCUSED` (`present_with ... source=MissReply states=FOCUSABLE|SHOWING|VISIBLE|ENABLED`). The second event, which comes after the state is true, falls inside `DUPLICATE_FOCUS_WINDOW` (150 ms) and was dropped by `begin_focus`. A fresh read is not revalidated, so nothing re-read the state, `FocusedApp` was never published, and the app actor discarded the terminal's text events. (Cached "Hit" presentations were already repaired by B-1 RR-2: their re-read shows FOCUSED. Only fresh reads of a first-focused node were left.) The key workaround masked it by publishing on the first key.

## Fix
`present/flow.rs`: a repeated focus event for the node in focus calls `reconfirm_focus`: if the node's application is not the current protected one (the previous app's grace does not count) it reads the state once per focus change (`FetchKind::StateChange`); if the first presentation is still in flight it remembers (`Reconfirm::Pending`) and asks when that presentation ends without FOCUSED. The reply earns the protection only if it shows FOCUSED (`on_state_reply`, RR-2), so a forged focus still earns nothing. `terminal.rs`: the publish in `term_key` is removed. The fix is generic (any toolkit with this order), not terminal-specific. Cost: at most one extra GetState per first focus of such a node.

## Tests
- 4 unit tests in `reader.rs` (repeated event after unfocused read asks and earns; repeated event while the first presentation is in flight; nothing asked when earned; a repeated event alone earns nothing and is asked once).
- `VW` (fake VTE, `fakeapp.py focus_window`): another app has the focus, the terminal's window is activated and focused with window activation before/after the focus event, FOCUSED hidden from GetState for 0/8/160/400 ms with the `focused=1` signal at the same time, and a tab-like bounce between two nodes; output with no key must be spoken (`vw_lost` 0, `vw_ms` p99 <= 35 ms). Against main's files the bounce case loses 2/2 outputs. Note: the non-bounce VW cases also pass on main in the fake (the fake app emits a StateChanged that the S1 path already uses); the real terminal is the reproducer.
- `VV` real terminal (`vv_focus`, `vv_focus_failures`): new window of a second application, new tab, Alt+Tab back with commands typed to the compositor only. No key reaches the reader. `GORCA_VV_NO_FLOOD=1` skips the floods, `GORCA_VV_KEEP_DIR` keeps the reader trace.

## Results
Pre-rebase head 1554a69 (3 repeats, load 14-23): all M6 fake gates (V1 VT VH V2 V3 VP VR VF VB VW), strict K2 K2s K3 K4 R1 with `--impersonate-orca`, VV (incl. floods, `vv_focus_failures` 0): PASS. VW p99 32.3 ms.
Head 8ec61e0 (on a456491, 3 repeats, load 8-13): K gates and VV PASS (`vv_focus_failures` 0); fmt, clippy -D warnings, `cargo test --workspace`, `cargo xtask deps` clean. M6 fake: VW PASS (`vw_lost` 0), but V1 p99 37.1 ms (gate 35) and VT 2 missing/2 extra in one repeat FAIL.
A/B V1+VT vs the three core files at the base, alternating, 2 rounds: main clean twice (V1 33.7/32.6; VT 0), fix V1 38.5/36.8 and VT 3/8 misses. Then VT-only: fix 2 misses, main clean. Not explained by code: DIAG instrumentation showed the app is current at every key (0 warnings), and with the key publish restored VT still fails (9-19 misses). On a host at load 45-64 main itself fails VT (8-17 misses per repeat, `vtmain`). Host load from other agents was 11-65 during these runs, so the V1/VT misses are treated as load-induced but NOT proven independent of the change: rerun VT/V1 on a quiet host (`bench/b1/quiet-host.sh`) before relying on them. All runs kept in `bench/results/e2e/` (`ab-*`, `vtmain`, `vtdiag`, `mf*`).

## Not covered
Window activation is only modelled in the fake (the reader does not consume it for protection); a toolkit that never reports FOCUSED stays unprotected (fail closed).
