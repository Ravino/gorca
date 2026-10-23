# Doubled typing echo (Telegram study F2, F6): root cause

Symptom: every typed key spoken twice (STOP between), Firefox and Chromium, plain `<input>`;
Chromium also "new line" twice on send.

Root cause: not the text layer. Mutter 48.7 delivers each KeyEvent of a key that passes through the
Wayland input method (browser editables use text-input) to the keyboard monitor twice, identical
(keycode, keysym, unichar, state, pressed), about 1 ms apart, different D-Bus serials, same
sender and destination. The lane classified both as presses, so the text machine echoed twice
(second as an autorepeat). Super (no focused window) and the text layer's own logic were clean.
Evidence: `RUST_LOG=gorca_dbg=debug` trace in `/var/tmp/ge-echo5`, `refs/mutter/src/core/events.c`
(notify before the input method) vs forwarded event re-entering `meta_display_handle_event`.
Reproduced with a binary built before M22b1 (`<repo>/target/release/gorca`, 00:03), so the
shadow presenter and C3/C3b are not involved.

Fix: `InputLane::is_mirrored` drops an event identical to the latest event of the same keycode
within `MIRROR_WINDOW` (8 ms; autorepeat starts after hundreds of ms and repeats every >=10 ms).
Counted in `LaneStats::mirrored`. Tests: lane unit tests, `tg_drive.py` echo scenario
(`echo-once` verdicts) and chat scenario (`new line` at most once) on both browsers.
Not found a regression of leaf_diff; the existing Reader tests
`typing_in_an_editor_that_rebuilds_its_text_node_is_spoken_once_per_key` already assert one
utterance per key for key-echo plus the insertion diff.
