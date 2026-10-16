# Speech lane: priority-aware slot overwrite

Follow-up to the open item in `2026-10-16-perf-engineer-k2-loss-under-load.md`.

## Priority classes in use (`gorca_model::Prio`, derived `Ord`, declaration order)
`Interrupt < Message < Text < Progress < Notification`. `Interrupt` means "cuts running speech",
not "most important"; the order only drives FIFO pop order in `queue.rs`.

| Prio | Producers |
|---|---|
| Interrupt | key echo, typed/erased/selection/caret speech (`text/caret.rs`), paste echo, focus presentation (`present/utterance.rs:97`), `Reader::say` confirmations, browse commands |
| Message | state-change presentation (`utterance.rs:224`, not surviving); `Reader::announce` (survives); daemon notices `notice_text` (survives, `gorca-daemon/src/lib.rs`) |
| Text | deferred typed word / non-paste insert echo, say-all chunks |
| Progress, Notification | none yet (SSIP lowering only) |

Slot admission: `prio == Interrupt || epoch > inflight_epoch`. Before the change the overwrite
ignored class entirely.

## Risk reproduced
A survivor notice (e.g. "application not responding", an extension notice) pending in the slot
was replaced by the next echo (Interrupt) and counted only as `superseded`; a notice arriving
late behind a newer-epoch slot occupant was dropped as stale although it survives focus changes.
Rejected: the "newer replaces only if prio >= pending" rule, because Interrupt (echo) outranks
Message numerically, so it would not protect the notice and would block echoes behind
lower-class items.

## Rule (`crates/gorca-speech/src/lane.rs`, `is_protected`)
Protected = `Prio::Message && survives_focus_change`.
- Pending protected + newcomer of another class: newcomer takes the slot (speaks at once, no echo
  delay), the notice is pushed to the FIFO front and spoken after it. Not counted superseded.
- Late protected arrival behind a newer slot occupant: queued at the FIFO front, not stale-dropped.
- Message vs Message, anything else: unchanged latest-wins, counted `superseded`.
- FIFO full: the notice is dropped and counted in `inbox_dropped` (conservation holds).
Cost: one extra predicate and, rarely, one mutex-guarded push; hot echo path unchanged.

## Accounting
Every utterance still ends in exactly one of spoken / superseded / stale_dropped / inbox_dropped /
muted_dropped, so `account.rs` conservation needs no change.

## Tests (`lane.rs`, deterministic detached-handle tests)
notice not overwritten by echo; notice survives epoch advance and goes ahead of queued speech;
late notice follows instead of dropping (ordinary late utterance still dropped); same-class and
plain-Message latest-wins with `superseded == 3`; full FIFO counts `inbox_dropped`; end-to-end
notice+echo accounted.

## Gates
fmt, clippy -D warnings, `cargo test --workspace`, `cargo xtask deps`: all clean.
Strict K2/K3/K4/R1, `--impersonate-orca`, GORCA_M1/M4 strict, 3 repeats, load 24 to 28 (16 cores, governor performance, other agents active; the stock `run.sh` could not be used because the worktree `.toolchain` is a stub, so the same build and `run-isolated.sh` steps ran with the ambient toolchain): every gated row PASS. K2/K3/R1 lost 0, not_consumed 0; K3 p99 1.24 ms (max over repeats 7.7), R1 p99 0.50 ms; K4 10k switches app_lost/doubled/leaked/forged 0; superseded K2 max 1, K3 max 9, R1 0. Results: `bench/results/e2e/speech-lane-priority/20261016T010707Z.{json,txt}`.
