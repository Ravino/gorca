# Regression results: gorca key source vs Mutter 48.7 NULL-modifiers crash

Private headless shell (`tests/shell/run_shell.sh`), host loaded (loadavg 13-32, swap pressure; the
load of each run is in its JSON line). Driver `tools/platform-probes/shell-segfault/regress.py`
with `crates/gorca-input-mutter/examples/key_session` (gorca's real `MutterKeySource`).

## Old source (HEAD 196f6e7e)
- stall2: shell crashed at run 0 (1/1). stall: crashed at run 1 (1/2 runs before the crash).
  stallkeep: crashed at run 0 (1/1). Each is the kernel line `segfault at 8` in libmutter and the
  shell process gone. (Before the zbus method_timeout fix the first "new" build was in fact still
  the old binary; those logs are the old-source evidence.)

## New source
| mode | runs | crashes | notes |
|---|---|---|---|
| stall2 | 20 | 0 | 40 queued grab changes, A SIGKILLed, B started, shell continued |
| stall | 8 | 0 | |
| stallkeep | 5 | 0 | 7 s shell stall: A keeps its ONE session (0 lost, 1 active) |
| det | 8 | 0 | A killed, B starts at once |
| setg | 3 | 0 | B starts while A holds the name; B never became active until A quit |

Intermediate failure kept for the record: with the zbus connection `method_timeout` at 2 s the
stallkeep runs ended the session on "I/O error: timed out" (3/3 "A restarted its session on a mere
stall"); the shell survived. Fixed by raising the backstop to 10 s (above GRAB_DEADLINE, asserted
at compile time).
