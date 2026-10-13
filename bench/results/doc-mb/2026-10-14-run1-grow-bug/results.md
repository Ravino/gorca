# gorca-doc MB benches

conditions: loadavg `12.65 7.04 4.80 1/2568 1110734`, governor `powersave`, model name	: Intel(R) Core(TM) Ultra 9 285H, cores 16, quick=false

### 1000 nodes (1018 built, 171341 after update runs)

build 0.2 ms (149 ns/node), heap 0.2 MB (223 B/node), headings 29, links 174, lines 217, timer overhead 13 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 109 ns | 186 ns | 19.58 us | 113 ns |
| prev heading | 100000 | 129 ns | 202 ns | 34.31 us | 160 ns |
| next heading level 2 (filter) | 100000 | 166 ns | 403 ns | 15.48 us | 92 ns |
| next heading level 1-3 (filter) | 100000 | 92 ns | 158 ns | 3.50 us | 97 ns |
| next link | 100000 | 101 ns | 168 ns | 7.51 us | 104 ns |
| next unvisited link | 100000 | 100 ns | 169 ns | 4.37 us | 105 ns |
| next landmark/region | 100000 | 93 ns | 155 ns | 3.04 us | 97 ns |
| next form field | 100000 | 85 ns | 146 ns | 1.56 us | 88 ns |
| next table | 100000 | 86 ns | 141 ns | 6.42 us | 89 ns |
| next list item | 100000 | 92 ns | 151 ns | 7.57 us | 95 ns |
| next text run (dense category) | 100000 | 97 ns | 162 ns | 9.05 us | 102 ns |
| next link within landmark | 100000 | 158 ns | 245 ns | 6.04 us | 158 ns |
| next_in_landmark(link) | 100000 | 169 ns | 268 ns | 1.95 us | 173 ns |
| kth heading | 100000 | 14 ns | 28 ns | 1.70 us | 14 ns |
| rank (heading k of N) | 100000 | 118 ns | 204 ns | 9.75 us | 118 ns |
| cmp_order | 100000 | 106 ns | 164 ns | 7.17 us | 107 ns |
| landmark_of | 100000 | 30 ns | 53 ns | 255 ns | 31 ns |
| text: offset_of(node) | 100000 | 56 ns | 93 ns | 7.06 us | 60 ns |
| text: at_offset | 100000 | 39 ns | 62 ns | 9.02 us | 41 ns |
| text: line_start(k) | 100000 | 35 ns | 62 ns | 506 ns | 41 ns |
| text: line_of(offset) | 100000 | 63 ns | 106 ns | 3.85 us | 69 ns |
| text: line_range + read line | 100000 | 283 ns | 401 ns | 31.82 us | 300 ns |
| text: word at offset | 100000 | 377 ns | 496 ns | 8.66 us | 388 ns |
| text: next line | 100000 | 206 ns | 351 ns | 9.40 us | 207 ns |
| batch: apply (writer, back copy) | 300 | 228.33 us | 548.28 us | 1.19 ms | - |
| batch: publish() only | 300 | 210 ns | 488 ns | 497 ns | - |
| batch: apply + publish | 300 | 228.55 us | 548.77 us | 1.19 ms | - |
| batch: replay on other copy (deferred) | 300 | 276.53 us | 545.04 us | 698.14 us | - |
| batch: replay + apply + publish (steady state) | 300 | 511.46 us | 1.06 ms | 1.34 ms | - |
| publish() after a 1-op change | 2000 | 174 ns | 180 ns | 5.89 us | - |
| reader: at_position + next heading, writer running batches | 1480879 | 868 ns | 2.56 us | 70.61 us | - |

batch: 270 ops touching ~1018 nodes; writer waits 344 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 10000 nodes (10008 built, 196179 after update runs)

build 1.4 ms (138 ns/node), heap 3.5 MB (355 B/node), headings 294, links 1669, lines 2182, timer overhead 26 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 422 ns | 658 ns | 89.84 us | 415 ns |
| prev heading | 100000 | 446 ns | 703 ns | 6.97 us | 436 ns |
| next heading level 2 (filter) | 100000 | 384 ns | 617 ns | 20.36 us | 380 ns |
| next heading level 1-3 (filter) | 100000 | 414 ns | 646 ns | 31.18 us | 409 ns |
| next link | 100000 | 415 ns | 629 ns | 12.86 us | 412 ns |
| next unvisited link | 100000 | 416 ns | 631 ns | 19.28 us | 413 ns |
| next landmark/region | 100000 | 422 ns | 658 ns | 22.69 us | 418 ns |
| next form field | 100000 | 372 ns | 597 ns | 18.87 us | 366 ns |
| next table | 100000 | 369 ns | 594 ns | 28.69 us | 368 ns |
| next list item | 100000 | 393 ns | 643 ns | 11.01 us | 391 ns |
| next text run (dense category) | 100000 | 400 ns | 612 ns | 17.53 us | 396 ns |
| next link within landmark | 100000 | 696 ns | 996 ns | 22.12 us | 690 ns |
| next_in_landmark(link) | 100000 | 728 ns | 1.05 us | 18.66 us | 751 ns |
| kth heading | 100000 | 107 ns | 190 ns | 17.95 us | 74 ns |
| rank (heading k of N) | 100000 | 559 ns | 850 ns | 20.76 us | 543 ns |
| cmp_order | 100000 | 407 ns | 575 ns | 20.31 us | 407 ns |
| landmark_of | 100000 | 109 ns | 187 ns | 10.96 us | 94 ns |
| text: offset_of(node) | 100000 | 222 ns | 345 ns | 36.97 us | 221 ns |
| text: at_offset | 100000 | 169 ns | 279 ns | 18.00 us | 168 ns |
| text: line_start(k) | 100000 | 159 ns | 281 ns | 14.05 us | 169 ns |
| text: line_of(offset) | 100000 | 296 ns | 488 ns | 16.43 us | 308 ns |
| text: line_range + read line | 100000 | 1.19 us | 1.58 us | 15.63 us | 1.19 us |
| text: word at offset | 100000 | 1.48 us | 1.91 us | 19.72 us | 1.46 us |
| text: next line | 100000 | 1.15 us | 1.62 us | 34.72 us | 529 ns |
| batch: apply (writer, back copy) | 300 | 365.69 us | 3.88 ms | 10.62 ms | - |
| batch: publish() only | 300 | 462 ns | 1.95 us | 2.10 us | - |
| batch: apply + publish | 300 | 366.18 us | 3.88 ms | 10.62 ms | - |
| batch: replay on other copy (deferred) | 300 | 501.73 us | 2.90 ms | 17.07 ms | - |
| batch: replay + apply + publish (steady state) | 300 | 882.83 us | 8.04 ms | 17.58 ms | - |
| publish() after a 1-op change | 2000 | 215 ns | 225 ns | 5.29 us | - |
| reader: at_position + next heading, writer running batches | 785827 | 1.74 us | 4.17 us | 444.54 us | - |

batch: 270 ops touching ~1018 nodes; writer waits 338 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 100000 nodes (100048 built, 293853 after update runs)

build 20.8 ms (208 ns/node), heap 28.7 MB (287 B/node), headings 2905, links 16704, lines 21843, timer overhead 24 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 714 ns | 1.12 us | 26.68 us | 682 ns |
| prev heading | 100000 | 747 ns | 1.22 us | 42.94 us | 712 ns |
| next heading level 2 (filter) | 100000 | 724 ns | 1.54 us | 609.68 us | 824 ns |
| next heading level 1-3 (filter) | 100000 | 706 ns | 1.08 us | 14.48 us | 684 ns |
| next link | 100000 | 657 ns | 1.06 us | 18.03 us | 639 ns |
| next unvisited link | 100000 | 659 ns | 1.04 us | 16.67 us | 642 ns |
| next landmark/region | 100000 | 695 ns | 1.15 us | 22.52 us | 430 ns |
| next form field | 100000 | 302 ns | 501 ns | 17.32 us | 306 ns |
| next table | 100000 | 303 ns | 481 ns | 13.71 us | 311 ns |
| next list item | 100000 | 313 ns | 581 ns | 16.31 us | 360 ns |
| next text run (dense category) | 100000 | 316 ns | 737 ns | 17.29 us | 468 ns |
| next link within landmark | 100000 | 684 ns | 1.97 us | 138.13 us | 593 ns |
| next_in_landmark(link) | 100000 | 781 ns | 2.30 us | 181.26 us | 654 ns |
| kth heading | 100000 | 196 ns | 416 ns | 16.89 us | 199 ns |
| rank (heading k of N) | 100000 | 450 ns | 772 ns | 29.35 us | 454 ns |
| cmp_order | 100000 | 257 ns | 381 ns | 13.34 us | 269 ns |
| landmark_of | 100000 | 91 ns | 263 ns | 4.52 us | 77 ns |
| text: offset_of(node) | 100000 | 165 ns | 283 ns | 9.07 us | 172 ns |
| text: at_offset | 100000 | 155 ns | 361 ns | 15.89 us | 166 ns |
| text: line_start(k) | 100000 | 150 ns | 347 ns | 14.52 us | 168 ns |
| text: line_of(offset) | 100000 | 273 ns | 778 ns | 21.02 us | 270 ns |
| text: line_range + read line | 100000 | 1.05 us | 3.84 us | 252.23 us | 1.53 us |
| text: word at offset | 100000 | 1.39 us | 2.59 us | 23.25 us | 1.38 us |
| text: next line | 100000 | 981 ns | 1.87 us | 20.37 us | 946 ns |
| batch: apply (writer, back copy) | 300 | 298.04 us | 402.56 us | 12.41 ms | - |
| batch: publish() only | 300 | 293 ns | 946 ns | 1.14 us | - |
| batch: apply + publish | 300 | 298.35 us | 402.79 us | 12.41 ms | - |
| batch: replay on other copy (deferred) | 300 | 396.32 us | 531.36 us | 15.80 ms | - |
| batch: replay + apply + publish (steady state) | 300 | 697.62 us | 953.89 us | 16.11 ms | - |
| publish() after a 1-op change | 2000 | 148 ns | 152 ns | 7.49 us | - |
| reader: at_position + next heading, writer running batches | 634092 | 2.15 us | 5.19 us | 347.31 us | - |

batch: 270 ops touching ~1018 nodes; writer waits 605 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 1000000 nodes (1000031 built, 1028876 after update runs)

build 172.8 ms (173 ns/node), heap 232.2 MB (232 B/node), headings 29057, links 166925, lines 218390, timer overhead 22 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 20000 | 813 ns | 1.91 us | 11.09 us | 597 ns |
| prev heading | 20000 | 942 ns | 2.24 us | 15.35 us | 750 ns |
| next heading level 2 (filter) | 20000 | 810 ns | 2.35 us | 11.53 us | 811 ns |
| next heading level 1-3 (filter) | 20000 | 882 ns | 2.33 us | 13.19 us | 1.04 us |
| next link | 20000 | 637 ns | 1.78 us | 10.76 us | 805 ns |
| next unvisited link | 20000 | 766 ns | 1.91 us | 15.50 us | 883 ns |
| next landmark/region | 20000 | 995 ns | 2.29 us | 18.51 us | 967 ns |
| next form field | 20000 | 1.28 us | 2.98 us | 20.56 us | 1.15 us |
| next table | 20000 | 1.19 us | 2.93 us | 18.35 us | 713 ns |
| next list item | 20000 | 1.09 us | 2.61 us | 16.17 us | 992 ns |
| next text run (dense category) | 20000 | 662 ns | 1.64 us | 22.53 us | 819 ns |
| next link within landmark | 20000 | 2.01 us | 3.72 us | 20.89 us | 1.92 us |
| next_in_landmark(link) | 20000 | 1.99 us | 3.58 us | 17.79 us | 1.95 us |
| kth heading | 20000 | 1.92 us | 3.59 us | 20.95 us | 2.23 us |
| rank (heading k of N) | 20000 | 2.05 us | 4.17 us | 21.98 us | 1.52 us |
| cmp_order | 20000 | 508 ns | 1.33 us | 16.07 us | 462 ns |
| landmark_of | 20000 | 223 ns | 685 ns | 11.30 us | 179 ns |
| text: offset_of(node) | 20000 | 376 ns | 1.03 us | 13.64 us | 333 ns |
| text: at_offset | 20000 | 756 ns | 2.02 us | 13.21 us | 585 ns |
| text: line_start(k) | 20000 | 602 ns | 1.85 us | 20.88 us | 738 ns |
| text: line_of(offset) | 20000 | 1.60 us | 3.25 us | 14.27 us | 1.46 us |
| text: line_range + read line | 20000 | 3.52 us | 5.78 us | 23.00 us | 3.92 us |
| text: word at offset | 20000 | 3.82 us | 6.49 us | 23.66 us | 4.08 us |
| text: next line | 20000 | 3.40 us | 6.36 us | 38.19 us | 3.11 us |
| batch: apply (writer, back copy) | 40 | 504.51 us | 596.41 us | 596.41 us | - |
| batch: publish() only | 40 | 513 ns | 974 ns | 974 ns | - |
| batch: apply + publish | 40 | 505.23 us | 596.83 us | 596.83 us | - |
| batch: replay on other copy (deferred) | 40 | 637.85 us | 746.70 us | 746.70 us | - |
| batch: replay + apply + publish (steady state) | 40 | 1.17 ms | 1.27 ms | 1.27 ms | - |
| publish() after a 1-op change | 2000 | 201 ns | 208 ns | 15.89 us | - |
| reader: at_position + next heading, writer running batches | 389351 | 3.70 us | 6.74 us | 82.31 us | - |

batch: 270 ops touching ~1018 nodes; writer waits 519 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)

