# Pre-existing test failures seen during M22b4 (2026-10-25)

Run alone, 3 times each, on main (681a8eb7, source tree exported with `git archive`) and on the M22b4 branch.
Conditions: governor `powersave`, load average 6 to 14 (shared host, other agents building).

| test | main | M22b4 branch |
|---|---|---|
| `gorca-core` `doc_actor::fuse::tests::storm::a_streaming_leaf_costs_a_bounded_number_of_fusions` | FAIL 3/3 | FAIL 3/3 |
| `gorca-ext-host` `--test ext settings_are_per_extension_and_need_the_capability` | pass 3/3 | pass 3/3 (failed once inside the full workspace run: load flake) |

The storm test is a timing limit (nested items about 1.1 to 2.2 ms per rename against the test's limit) that the
`powersave` governor and the host load break; it fails the same way on main, so it is not caused by M22b4.
