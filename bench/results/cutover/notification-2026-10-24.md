# Cut-over gate: notification

result: PASS
surface: notification
date: 2026-10-24
languages: en, ru
rev: 12c1fb2010e099cf6ec21d60917a228defe5d5c5
dirty: yes (gate ran before the commit)
inputs: a05336c181ce3ad1
shuffle_seed: 0x5eed
loadavg: 9.71 11.63 9.18 17/1784 2152223
governor: powersave
records-en: 1dbd31fc9b0e9e35
records-ru: b4ff257f66dafa25

| surface | lang | corpora | compared | equal | intended | unclassified | dropped | unexplained drops | deterministic |
|---|---|---|---|---|---|---|---|---|---|
| notification | en | 4/4 families | 66 | 64 | 2 | 0 | 7041 | 0 | yes |
| notification | ru | 4/4 families | 66 | 64 | 2 | 0 | 7041 | 0 | yes |

The gate runs the replay once per UI language (English and Russian, the owner's) and passes only when both pass. Nodes the replay did not compare are counted per reason below; a drop with no explanation fails the gate. A row that rests on a fused model fact the source record contradicts is not accepted as intended.

## Language: en (PASS)

compared 66 nodes: 64 equal (including 0 where both presenters say nothing), 2 intended, 0 unclassified; 7041 dropped, 0 unexplained.

### Corpora (en)

| family | status | compared | equal | intended | unclassified |
|---|---|---|---|---|---|
| gtk4 | covered (1 corpora) | 4 | 4 | 0 | 0 |
| firefox | covered (5 corpora) | 20 | 20 | 0 | 0 |
| chromium | covered (5 corpora) | 20 | 18 | 2 | 0 |
| gnome-shell | covered (7 corpora) | 22 | 22 | 0 | 0 |

### Dropped nodes and cases (en)

| family | reason | count | explanation |
|---|---|---|---|
| chromium | not-a-notice-role | 2105 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| electron | not-a-notice-role | 746 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| firefox | not-a-notice-role | 1376 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gnome-shell | not-a-notice-role | 1585 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gtk3 | not-a-notice-role | 101 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gtk4 | not-a-notice-role | 373 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| telegram | not-a-notice-role | 357 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| vte | not-a-notice-role | 398 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |

total dropped: 7041; unexplained: 0 (must be 0 to PASS).

### Determinism (en)

Two plain runs and one run with shuffled arrival order and batching produced byte-identical records.

### Intended differences (en, deviations.toml, epoch change)

- `role-word-improved@notification [m22b4]`: 2

### Unclassified differences (en)

None.

## Language: ru (PASS)

compared 66 nodes: 64 equal (including 0 where both presenters say nothing), 2 intended, 0 unclassified; 7041 dropped, 0 unexplained.

### Corpora (ru)

| family | status | compared | equal | intended | unclassified |
|---|---|---|---|---|---|
| gtk4 | covered (1 corpora) | 4 | 4 | 0 | 0 |
| firefox | covered (5 corpora) | 20 | 20 | 0 | 0 |
| chromium | covered (5 corpora) | 20 | 18 | 2 | 0 |
| gnome-shell | covered (7 corpora) | 22 | 22 | 0 | 0 |

### Dropped nodes and cases (ru)

| family | reason | count | explanation |
|---|---|---|---|
| chromium | not-a-notice-role | 2105 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| electron | not-a-notice-role | 746 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| firefox | not-a-notice-role | 1376 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gnome-shell | not-a-notice-role | 1585 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gtk3 | not-a-notice-role | 101 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| gtk4 | not-a-notice-role | 373 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| telegram | not-a-notice-role | 357 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |
| vte | not-a-notice-role | 398 | a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so |

total dropped: 7041; unexplained: 0 (must be 0 to PASS).

### Determinism (ru)

Two plain runs and one run with shuffled arrival order and batching produced byte-identical records.

### Intended differences (ru, deviations.toml, epoch change)

- `role-word-improved@notification [m22b4]`: 2

### Unclassified differences (ru)

None.

