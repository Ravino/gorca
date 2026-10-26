# gorca-input RLIMIT_RTTIME SIGXCPU demotions (2026-10-26)

## Symptom
Live journal (`journalctl --user -u gorca`): 6 SIGXCPU lines (2026-10-22 22:38, 10-23 09:01, 10-23 22:32,
10-24 08:58, 10-25 14:14, 10-25 20:32 UTC); four are followed 50-110 ms later by
`lane ran flat out ... lane=gorca-input` (the watchdog saw ~100% CPU on the input thread). The other two
were demoted by the SIGXCPU handler alone. Key rate at the time was normal (200-400 keys/min); no
key-channel overflow, no Mutter notices. The 20:32:57 case sits in a burst of `web_focus` phases 30 ms
apart (a held arrow/modifier while browsing Telegram in Firefox).

## Root cause
`crates/gorca-input-mutter/src/source.rs`, session loop. The stale arm
`sleep_until(stale_at), if stale_at.is_some()` is armed from
`deferred.and(down.iter().map(|d| d.at + DOWN_STALE).min())`. `down` only forgets a key on its release.
Sequence: a grab-set change is deferred because a gorca modifier is held (`held` non-empty); the modifier
stays down longer than `DOWN_STALE` (1.5 s; holding the screen-reader key while navigating). `stale_at` is
now in the past, the arm fires, `take_ready` returns None (modifier still held), `continue`, and the next
pass computes the same past deadline: a busy loop with no blocking syscall until the key is released.
On the SCHED_RR lane that is exactly "100 ms without blocking" (RTTIME). The thread never sleeps, so the
kernel counter never resets; the watchdog then sees 100% duty.

It is not a Mutter D-Bus flood, libei, key-repeat storm or pre-wake spin: the key pump, `keys.next()` and
the other arms all block in epoll; only this arm can be ready forever without a state change.

## Fix
`stale_deadline()` forgets stale keys (`down.retain(age < DOWN_STALE)`) before computing the deadline, so the
result is always in the future or None. Behaviour is unchanged otherwise (`keystroke_conflict` already
ignored stale keys; a deferred set held back by a modifier is still released by the modifier's release
event). Test: `stale_deadline_is_never_in_the_past`.

## Other findings
- Re-promotion: none. `demote_tid` is one-way until the daemon restarts, so one transient spin leaves the
  input lane SCHED_OTHER for the rest of the run. Open item (needs an rtkit re-request with backoff).
- Other RT lanes (reader, speech-io, drain) are covered by the same watchdog + SIGXCPU net; none of the six
  events names them. I did not audit their loops for the same "ready arm that does not make progress" shape.
- Reproduction: not run in a private session (the loop needs a live Mutter manager); covered by the pure
  helper test.
