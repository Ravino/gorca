# P3 e2e: legacy/review-last vs the cut-over (2026-10-25)

Both runs: `tests/shell/run_p3.sh`, 62 steps each (object moves, flat review by line, word and character on GTK4,
Firefox and GNOME Shell), all pass. Spoken text per step identical except:

* Shell flat review: `panel unavailable` -> `panel` on every line of the static panel (gate row `unavailable-static`,
  SM-5); the `unavailable` word on its own (next word) becomes `panel`.
* Shell `open quick settings`: the legacy run spoke an extra `application not responding` toast (timing of the Shell, not review).
* Firefox `focus: check box`: legacy one utterance `Subscribe, check box, not checked`, cut-over two (`check box, not
  checked`, then `Subscribe`): the focus path with a late name, not touched by this change (the legacy binary predates the merge of main).

Known failures, same on both (not part of this change, see the backlog row in docs/decisions.md): GTK4 `next` at the end of
the list, Firefox `next` and `previous` from the check box say `not responding`; Shell flat review reads `layout unavailable`.
Flat review lines, words and characters otherwise read the same text.
