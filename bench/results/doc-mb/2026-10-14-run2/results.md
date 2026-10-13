# gorca-doc MB benches

conditions: loadavg `7.93 7.01 5.55 3/2843 1144282`, governor `powersave`, model name	: Intel(R) Core(TM) Ultra 9 285H, cores 16, quick=false

### 1000 nodes (1018 built, 1024 after update runs)

build 0.1 ms (146 ns/node), heap 0.2 MB (223 B/node), headings 29, links 174, lines 217, timer overhead 13 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 119 ns | 205 ns | 458.23 us | 128 ns |
| prev heading | 100000 | 206 ns | 469 ns | 14.56 us | 105 ns |
| next heading level 2 (filter) | 100000 | 88 ns | 141 ns | 5.53 us | 93 ns |
| next heading level 1-3 (filter) | 100000 | 92 ns | 156 ns | 6.34 us | 97 ns |
| next link | 100000 | 100 ns | 167 ns | 13.07 us | 105 ns |
| next unvisited link | 100000 | 100 ns | 169 ns | 4.92 us | 104 ns |
| next landmark/region | 100000 | 93 ns | 153 ns | 4.65 us | 97 ns |
| next form field | 100000 | 85 ns | 144 ns | 5.87 us | 89 ns |
| next table | 100000 | 86 ns | 141 ns | 4.63 us | 89 ns |
| next list item | 100000 | 92 ns | 150 ns | 6.75 us | 95 ns |
| next text run (dense category) | 100000 | 96 ns | 160 ns | 2.34 us | 102 ns |
| next link within landmark | 100000 | 159 ns | 246 ns | 10.39 us | 158 ns |
| next_in_landmark(link) | 100000 | 167 ns | 266 ns | 5.83 us | 172 ns |
| kth heading | 100000 | 15 ns | 28 ns | 1.90 us | 14 ns |
| rank (heading k of N) | 100000 | 119 ns | 203 ns | 5.80 us | 119 ns |
| cmp_order | 100000 | 105 ns | 164 ns | 4.66 us | 106 ns |
| landmark_of | 100000 | 31 ns | 55 ns | 16.94 us | 31 ns |
| text: offset_of(node) | 100000 | 57 ns | 93 ns | 2.49 us | 59 ns |
| text: at_offset | 100000 | 39 ns | 64 ns | 1.75 us | 42 ns |
| text: line_start(k) | 100000 | 35 ns | 62 ns | 770 ns | 39 ns |
| text: line_of(offset) | 100000 | 64 ns | 106 ns | 1.38 us | 69 ns |
| text: line_range + read line | 100000 | 286 ns | 393 ns | 8.35 us | 294 ns |
| text: word at offset | 100000 | 373 ns | 485 ns | 13.78 us | 383 ns |
| text: next line | 100000 | 206 ns | 352 ns | 7.15 us | 207 ns |
| batch: apply (writer, back copy) | 300 | 69.42 us | 85.10 us | 93.51 us | - |
| batch: publish() only | 300 | 134 ns | 773 ns | 1.06 us | - |
| batch: apply + publish | 300 | 69.51 us | 85.47 us | 93.63 us | - |
| batch: replay on other copy (deferred) | 300 | 70.04 us | 85.63 us | 98.61 us | - |
| batch: replay + apply + publish (steady state) | 300 | 139.05 us | 169.30 us | 186.38 us | - |
| publish() after a 1-op change | 2000 | 79 ns | 80 ns | 5.08 us | - |
| reader: at_position + next heading, writer running batches | 7240141 | 167 ns | 432 ns | 33.02 us | - |

batch: 280 ops touching ~1012 nodes; writer waits 214 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 10000 nodes (10008 built, 4232 after update runs)

build 1.9 ms (186 ns/node), heap 3.5 MB (355 B/node), headings 294, links 1669, lines 2182, timer overhead 12 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 194 ns | 300 ns | 26.98 us | 188 ns |
| prev heading | 100000 | 199 ns | 311 ns | 8.13 us | 194 ns |
| next heading level 2 (filter) | 100000 | 173 ns | 275 ns | 5.74 us | 172 ns |
| next heading level 1-3 (filter) | 100000 | 187 ns | 293 ns | 18.31 us | 185 ns |
| next link | 100000 | 188 ns | 286 ns | 15.74 us | 186 ns |
| next unvisited link | 100000 | 188 ns | 283 ns | 8.19 us | 186 ns |
| next landmark/region | 100000 | 190 ns | 297 ns | 9.28 us | 188 ns |
| next form field | 100000 | 167 ns | 265 ns | 4.69 us | 168 ns |
| next table | 100000 | 168 ns | 264 ns | 3.90 us | 164 ns |
| next list item | 100000 | 178 ns | 286 ns | 3.23 us | 175 ns |
| next text run (dense category) | 100000 | 181 ns | 275 ns | 9.42 us | 178 ns |
| next link within landmark | 100000 | 314 ns | 446 ns | 8.27 us | 309 ns |
| next_in_landmark(link) | 100000 | 328 ns | 474 ns | 15.52 us | 338 ns |
| kth heading | 100000 | 50 ns | 89 ns | 4.06 us | 37 ns |
| rank (heading k of N) | 100000 | 250 ns | 380 ns | 3.79 us | 245 ns |
| cmp_order | 100000 | 182 ns | 256 ns | 9.25 us | 182 ns |
| landmark_of | 100000 | 49 ns | 85 ns | 4.50 us | 43 ns |
| text: offset_of(node) | 100000 | 100 ns | 156 ns | 1.74 us | 99 ns |
| text: at_offset | 100000 | 78 ns | 131 ns | 3.63 us | 78 ns |
| text: line_start(k) | 100000 | 72 ns | 128 ns | 6.58 us | 77 ns |
| text: line_of(offset) | 100000 | 133 ns | 220 ns | 14.88 us | 138 ns |
| text: line_range + read line | 100000 | 531 ns | 708 ns | 10.68 us | 533 ns |
| text: word at offset | 100000 | 655 ns | 847 ns | 13.25 us | 651 ns |
| text: next line | 100000 | 523 ns | 725 ns | 10.88 us | 522 ns |
| batch: apply (writer, back copy) | 300 | 92.28 us | 103.19 us | 111.91 us | - |
| batch: publish() only | 300 | 151 ns | 387 ns | 423 ns | - |
| batch: apply + publish | 300 | 92.42 us | 103.32 us | 112.26 us | - |
| batch: replay on other copy (deferred) | 300 | 102.07 us | 117.09 us | 128.04 us | - |
| batch: replay + apply + publish (steady state) | 300 | 196.13 us | 217.58 us | 227.56 us | - |
| publish() after a 1-op change | 2000 | 122 ns | 123 ns | 3.79 us | - |
| reader: at_position + next heading, writer running batches | 4850070 | 259 ns | 710 ns | 128.47 us | - |

batch: 281 ops touching ~1012 nodes; writer waits 1461 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 100000 nodes (100048 built, 91691 after update runs)

build 17.0 ms (170 ns/node), heap 28.7 MB (287 B/node), headings 2905, links 16704, lines 21843, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 331 ns | 511 ns | 38.26 us | 313 ns |
| prev heading | 100000 | 333 ns | 517 ns | 5.44 us | 319 ns |
| next heading level 2 (filter) | 100000 | 320 ns | 478 ns | 26.50 us | 310 ns |
| next heading level 1-3 (filter) | 100000 | 323 ns | 498 ns | 18.27 us | 311 ns |
| next link | 100000 | 307 ns | 496 ns | 25.37 us | 296 ns |
| next unvisited link | 100000 | 309 ns | 488 ns | 27.13 us | 296 ns |
| next landmark/region | 100000 | 323 ns | 514 ns | 10.29 us | 312 ns |
| next form field | 100000 | 315 ns | 476 ns | 7.82 us | 307 ns |
| next table | 100000 | 318 ns | 481 ns | 13.12 us | 308 ns |
| next list item | 100000 | 319 ns | 492 ns | 8.87 us | 310 ns |
| next text run (dense category) | 100000 | 291 ns | 459 ns | 16.56 us | 285 ns |
| next link within landmark | 100000 | 560 ns | 817 ns | 20.98 us | 543 ns |
| next_in_landmark(link) | 100000 | 585 ns | 861 ns | 15.51 us | 582 ns |
| kth heading | 100000 | 212 ns | 447 ns | 13.53 us | 202 ns |
| rank (heading k of N) | 100000 | 449 ns | 702 ns | 13.13 us | 441 ns |
| cmp_order | 100000 | 272 ns | 384 ns | 28.70 us | 273 ns |
| landmark_of | 100000 | 105 ns | 257 ns | 8.94 us | 78 ns |
| text: offset_of(node) | 100000 | 179 ns | 296 ns | 8.99 us | 173 ns |
| text: at_offset | 100000 | 174 ns | 355 ns | 26.60 us | 171 ns |
| text: line_start(k) | 100000 | 167 ns | 348 ns | 9.68 us | 171 ns |
| text: line_of(offset) | 100000 | 270 ns | 523 ns | 33.98 us | 262 ns |
| text: line_range + read line | 100000 | 812 ns | 1.27 us | 46.68 us | 827 ns |
| text: word at offset | 100000 | 927 ns | 1.44 us | 25.78 us | 997 ns |
| text: next line | 100000 | 810 ns | 1.23 us | 30.20 us | 819 ns |
| batch: apply (writer, back copy) | 300 | 212.78 us | 239.42 us | 251.43 us | - |
| batch: publish() only | 300 | 270 ns | 688 ns | 729 ns | - |
| batch: apply + publish | 300 | 212.97 us | 239.73 us | 251.65 us | - |
| batch: replay on other copy (deferred) | 300 | 271.27 us | 308.74 us | 317.76 us | - |
| batch: replay + apply + publish (steady state) | 300 | 486.52 us | 541.58 us | 557.70 us | - |
| publish() after a 1-op change | 2000 | 130 ns | 133 ns | 10.79 us | - |
| reader: at_position + next heading, writer running batches | 3925576 | 388 ns | 1.22 us | 126.63 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 634 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 1000000 nodes (1000031 built, 999352 after update runs)

build 166.9 ms (167 ns/node), heap 232.2 MB (232 B/node), headings 29057, links 166925, lines 218390, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 20000 | 650 ns | 1.51 us | 20.77 us | 546 ns |
| prev heading | 20000 | 626 ns | 1.62 us | 8.84 us | 541 ns |
| next heading level 2 (filter) | 20000 | 636 ns | 1.93 us | 7.29 us | 592 ns |
| next heading level 1-3 (filter) | 20000 | 598 ns | 1.48 us | 9.31 us | 654 ns |
| next link | 20000 | 583 ns | 1.39 us | 13.34 us | 536 ns |
| next unvisited link | 20000 | 546 ns | 1.39 us | 9.79 us | 505 ns |
| next landmark/region | 20000 | 570 ns | 1.27 us | 20.78 us | 531 ns |
| next form field | 20000 | 630 ns | 1.76 us | 6.38 us | 585 ns |
| next table | 20000 | 744 ns | 2.12 us | 10.73 us | 627 ns |
| next list item | 20000 | 618 ns | 1.73 us | 20.89 us | 605 ns |
| next text run (dense category) | 20000 | 506 ns | 1.19 us | 20.25 us | 480 ns |
| next link within landmark | 20000 | 1.54 us | 3.03 us | 25.91 us | 1.40 us |
| next_in_landmark(link) | 20000 | 1.25 us | 2.58 us | 29.78 us | 1.06 us |
| kth heading | 20000 | 1.25 us | 2.57 us | 15.94 us | 1.25 us |
| rank (heading k of N) | 20000 | 923 ns | 2.39 us | 22.99 us | 765 ns |
| cmp_order | 20000 | 455 ns | 851 ns | 11.54 us | 447 ns |
| landmark_of | 20000 | 240 ns | 677 ns | 3.59 us | 192 ns |
| text: offset_of(node) | 20000 | 343 ns | 672 ns | 4.38 us | 335 ns |
| text: at_offset | 20000 | 592 ns | 1.80 us | 14.92 us | 531 ns |
| text: line_start(k) | 20000 | 441 ns | 1.71 us | 12.58 us | 437 ns |
| text: line_of(offset) | 20000 | 754 ns | 2.26 us | 15.61 us | 667 ns |
| text: line_range + read line | 20000 | 1.93 us | 3.42 us | 23.33 us | 2.32 us |
| text: word at offset | 20000 | 2.20 us | 3.88 us | 13.05 us | 2.19 us |
| text: next line | 20000 | 1.77 us | 3.28 us | 20.45 us | 1.84 us |
| batch: apply (writer, back copy) | 40 | 429.57 us | 614.19 us | 614.19 us | - |
| batch: publish() only | 40 | 318 ns | 636 ns | 636 ns | - |
| batch: apply + publish | 40 | 429.86 us | 614.68 us | 614.68 us | - |
| batch: replay on other copy (deferred) | 40 | 563.92 us | 836.31 us | 836.31 us | - |
| batch: replay + apply + publish (steady state) | 40 | 988.02 us | 1.45 ms | 1.45 ms | - |
| publish() after a 1-op change | 2000 | 183 ns | 185 ns | 390 ns | - |
| reader: at_position + next heading, writer running batches | 562598 | 2.53 us | 5.03 us | 324.83 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 599 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)

