# gorca-doc MB benches

conditions: loadavg `7.16 6.91 5.56 2/2806 1144511`, governor `powersave`, model name	: Intel(R) Core(TM) Ultra 9 285H, cores 16, quick=false

### 1000 nodes (1018 built, 1024 after update runs)

build 0.1 ms (144 ns/node), heap 0.2 MB (223 B/node), headings 29, links 174, lines 217, timer overhead 13 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 119 ns | 201 ns | 26.91 us | 120 ns |
| prev heading | 100000 | 122 ns | 198 ns | 16.15 us | 185 ns |
| next heading level 2 (filter) | 100000 | 89 ns | 142 ns | 6.10 us | 93 ns |
| next heading level 1-3 (filter) | 100000 | 92 ns | 159 ns | 8.09 us | 97 ns |
| next link | 100000 | 100 ns | 166 ns | 8.13 us | 104 ns |
| next unvisited link | 100000 | 100 ns | 169 ns | 5.39 us | 105 ns |
| next landmark/region | 100000 | 93 ns | 154 ns | 6.35 us | 97 ns |
| next form field | 100000 | 85 ns | 144 ns | 10.40 us | 88 ns |
| next table | 100000 | 85 ns | 141 ns | 1.44 us | 88 ns |
| next list item | 100000 | 92 ns | 150 ns | 1.92 us | 94 ns |
| next text run (dense category) | 100000 | 96 ns | 160 ns | 12.38 us | 102 ns |
| next link within landmark | 100000 | 160 ns | 246 ns | 5.86 us | 159 ns |
| next_in_landmark(link) | 100000 | 168 ns | 270 ns | 12.21 us | 173 ns |
| kth heading | 100000 | 14 ns | 28 ns | 1.64 us | 14 ns |
| rank (heading k of N) | 100000 | 118 ns | 204 ns | 5.73 us | 118 ns |
| cmp_order | 100000 | 104 ns | 162 ns | 11.91 us | 106 ns |
| landmark_of | 100000 | 30 ns | 54 ns | 2.76 us | 31 ns |
| text: offset_of(node) | 100000 | 56 ns | 92 ns | 4.56 us | 59 ns |
| text: at_offset | 100000 | 39 ns | 64 ns | 3.16 us | 42 ns |
| text: line_start(k) | 100000 | 35 ns | 61 ns | 7.37 us | 38 ns |
| text: line_of(offset) | 100000 | 63 ns | 105 ns | 2.40 us | 69 ns |
| text: line_range + read line | 100000 | 283 ns | 395 ns | 3.51 us | 288 ns |
| text: word at offset | 100000 | 369 ns | 480 ns | 9.33 us | 383 ns |
| text: next line | 100000 | 206 ns | 350 ns | 11.90 us | 205 ns |
| batch: apply (writer, back copy) | 300 | 69.25 us | 81.44 us | 95.54 us | - |
| batch: publish() only | 300 | 134 ns | 796 ns | 1.01 us | - |
| batch: apply + publish | 300 | 69.41 us | 81.53 us | 95.63 us | - |
| batch: replay on other copy (deferred) | 300 | 69.66 us | 85.27 us | 91.10 us | - |
| batch: replay + apply + publish (steady state) | 300 | 138.68 us | 164.71 us | 166.29 us | - |
| publish() after a 1-op change | 2000 | 79 ns | 80 ns | 4.89 us | - |
| reader: at_position + next heading, writer running batches | 7429631 | 166 ns | 368 ns | 100.99 us | - |

batch: 280 ops touching ~1012 nodes; writer waits 186 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 10000 nodes (10008 built, 4232 after update runs)

build 1.9 ms (189 ns/node), heap 3.5 MB (355 B/node), headings 294, links 1669, lines 2182, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 196 ns | 303 ns | 24.16 us | 188 ns |
| prev heading | 100000 | 200 ns | 310 ns | 10.08 us | 195 ns |
| next heading level 2 (filter) | 100000 | 174 ns | 274 ns | 3.90 us | 170 ns |
| next heading level 1-3 (filter) | 100000 | 187 ns | 291 ns | 3.21 us | 184 ns |
| next link | 100000 | 188 ns | 284 ns | 4.06 us | 185 ns |
| next unvisited link | 100000 | 189 ns | 283 ns | 3.08 us | 185 ns |
| next landmark/region | 100000 | 192 ns | 299 ns | 5.82 us | 188 ns |
| next form field | 100000 | 169 ns | 266 ns | 8.88 us | 165 ns |
| next table | 100000 | 168 ns | 267 ns | 4.90 us | 166 ns |
| next list item | 100000 | 180 ns | 290 ns | 8.15 us | 176 ns |
| next text run (dense category) | 100000 | 182 ns | 274 ns | 9.01 us | 179 ns |
| next link within landmark | 100000 | 316 ns | 447 ns | 9.07 us | 309 ns |
| next_in_landmark(link) | 100000 | 328 ns | 471 ns | 3.64 us | 336 ns |
| kth heading | 100000 | 49 ns | 83 ns | 13.37 us | 33 ns |
| rank (heading k of N) | 100000 | 253 ns | 382 ns | 13.09 us | 245 ns |
| cmp_order | 100000 | 183 ns | 257 ns | 7.63 us | 183 ns |
| landmark_of | 100000 | 51 ns | 86 ns | 9.79 us | 43 ns |
| text: offset_of(node) | 100000 | 101 ns | 157 ns | 11.52 us | 99 ns |
| text: at_offset | 100000 | 78 ns | 129 ns | 4.81 us | 78 ns |
| text: line_start(k) | 100000 | 73 ns | 128 ns | 3.55 us | 76 ns |
| text: line_of(offset) | 100000 | 134 ns | 220 ns | 4.59 us | 137 ns |
| text: line_range + read line | 100000 | 544 ns | 726 ns | 11.12 us | 530 ns |
| text: word at offset | 100000 | 661 ns | 850 ns | 9.88 us | 645 ns |
| text: next line | 100000 | 522 ns | 724 ns | 14.60 us | 521 ns |
| batch: apply (writer, back copy) | 300 | 92.42 us | 105.60 us | 108.46 us | - |
| batch: publish() only | 300 | 150 ns | 386 ns | 462 ns | - |
| batch: apply + publish | 300 | 92.60 us | 105.97 us | 108.85 us | - |
| batch: replay on other copy (deferred) | 300 | 101.94 us | 116.32 us | 123.56 us | - |
| batch: replay + apply + publish (steady state) | 300 | 196.12 us | 219.63 us | 222.30 us | - |
| publish() after a 1-op change | 2000 | 122 ns | 123 ns | 3.72 us | - |
| reader: at_position + next heading, writer running batches | 4847452 | 262 ns | 673 ns | 29.65 us | - |

batch: 281 ops touching ~1012 nodes; writer waits 1465 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 100000 nodes (100048 built, 91691 after update runs)

build 16.3 ms (163 ns/node), heap 28.7 MB (287 B/node), headings 2905, links 16704, lines 21843, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 330 ns | 501 ns | 30.50 us | 312 ns |
| prev heading | 100000 | 332 ns | 520 ns | 25.80 us | 321 ns |
| next heading level 2 (filter) | 100000 | 320 ns | 477 ns | 22.60 us | 309 ns |
| next heading level 1-3 (filter) | 100000 | 322 ns | 494 ns | 12.51 us | 310 ns |
| next link | 100000 | 306 ns | 489 ns | 16.15 us | 295 ns |
| next unvisited link | 100000 | 307 ns | 475 ns | 7.03 us | 297 ns |
| next landmark/region | 100000 | 321 ns | 502 ns | 12.31 us | 309 ns |
| next form field | 100000 | 316 ns | 475 ns | 8.93 us | 306 ns |
| next table | 100000 | 317 ns | 478 ns | 18.85 us | 308 ns |
| next list item | 100000 | 319 ns | 486 ns | 9.46 us | 309 ns |
| next text run (dense category) | 100000 | 289 ns | 447 ns | 7.86 us | 283 ns |
| next link within landmark | 100000 | 555 ns | 812 ns | 14.41 us | 541 ns |
| next_in_landmark(link) | 100000 | 581 ns | 837 ns | 20.02 us | 577 ns |
| kth heading | 100000 | 204 ns | 358 ns | 7.01 us | 199 ns |
| rank (heading k of N) | 100000 | 450 ns | 679 ns | 20.19 us | 445 ns |
| cmp_order | 100000 | 273 ns | 387 ns | 20.33 us | 272 ns |
| landmark_of | 100000 | 104 ns | 269 ns | 7.83 us | 78 ns |
| text: offset_of(node) | 100000 | 178 ns | 290 ns | 7.72 us | 174 ns |
| text: at_offset | 100000 | 166 ns | 334 ns | 13.70 us | 166 ns |
| text: line_start(k) | 100000 | 163 ns | 326 ns | 29.62 us | 166 ns |
| text: line_of(offset) | 100000 | 264 ns | 496 ns | 20.38 us | 258 ns |
| text: line_range + read line | 100000 | 801 ns | 1.39 us | 39.80 us | 808 ns |
| text: word at offset | 100000 | 920 ns | 1.52 us | 23.56 us | 943 ns |
| text: next line | 100000 | 803 ns | 1.24 us | 27.99 us | 841 ns |
| batch: apply (writer, back copy) | 300 | 214.93 us | 312.26 us | 339.12 us | - |
| batch: publish() only | 300 | 261 ns | 732 ns | 826 ns | - |
| batch: apply + publish | 300 | 215.16 us | 312.64 us | 339.56 us | - |
| batch: replay on other copy (deferred) | 300 | 272.51 us | 413.70 us | 457.66 us | - |
| batch: replay + apply + publish (steady state) | 300 | 488.11 us | 685.00 us | 797.23 us | - |
| publish() after a 1-op change | 2000 | 131 ns | 133 ns | 11.14 us | - |
| reader: at_position + next heading, writer running batches | 3957737 | 387 ns | 1.19 us | 58.59 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 639 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 1000000 nodes (1000031 built, 999352 after update runs)

build 170.0 ms (170 ns/node), heap 232.2 MB (232 B/node), headings 29057, links 166925, lines 218390, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 20000 | 777 ns | 1.82 us | 15.23 us | 574 ns |
| prev heading | 20000 | 658 ns | 1.89 us | 18.52 us | 582 ns |
| next heading level 2 (filter) | 20000 | 779 ns | 2.19 us | 15.42 us | 1.06 us |
| next heading level 1-3 (filter) | 20000 | 580 ns | 1.30 us | 21.39 us | 567 ns |
| next link | 20000 | 526 ns | 1.13 us | 10.66 us | 520 ns |
| next unvisited link | 20000 | 530 ns | 1.61 us | 15.09 us | 519 ns |
| next landmark/region | 20000 | 575 ns | 1.60 us | 10.67 us | 563 ns |
| next form field | 20000 | 626 ns | 1.90 us | 21.04 us | 631 ns |
| next table | 20000 | 664 ns | 2.15 us | 12.90 us | 663 ns |
| next list item | 20000 | 1.18 us | 2.88 us | 12.37 us | 610 ns |
| next text run (dense category) | 20000 | 498 ns | 939 ns | 6.75 us | 484 ns |
| next link within landmark | 20000 | 1.18 us | 2.42 us | 14.90 us | 937 ns |
| next_in_landmark(link) | 20000 | 1.02 us | 2.22 us | 9.77 us | 1.00 us |
| kth heading | 20000 | 1.31 us | 2.62 us | 15.15 us | 1.23 us |
| rank (heading k of N) | 20000 | 905 ns | 2.59 us | 13.50 us | 829 ns |
| cmp_order | 20000 | 456 ns | 1.00 us | 5.36 us | 449 ns |
| landmark_of | 20000 | 239 ns | 878 ns | 11.03 us | 184 ns |
| text: offset_of(node) | 20000 | 330 ns | 633 ns | 5.92 us | 326 ns |
| text: at_offset | 20000 | 468 ns | 1.74 us | 7.46 us | 441 ns |
| text: line_start(k) | 20000 | 472 ns | 1.64 us | 7.05 us | 444 ns |
| text: line_of(offset) | 20000 | 838 ns | 2.33 us | 7.59 us | 703 ns |
| text: line_range + read line | 20000 | 1.96 us | 3.49 us | 21.87 us | 2.00 us |
| text: word at offset | 20000 | 2.17 us | 3.77 us | 20.30 us | 2.22 us |
| text: next line | 20000 | 1.80 us | 3.36 us | 28.99 us | 1.67 us |
| batch: apply (writer, back copy) | 40 | 497.11 us | 1.21 ms | 1.21 ms | - |
| batch: publish() only | 40 | 447 ns | 1.99 us | 1.99 us | - |
| batch: apply + publish | 40 | 497.46 us | 1.21 ms | 1.21 ms | - |
| batch: replay on other copy (deferred) | 40 | 668.12 us | 1.16 ms | 1.16 ms | - |
| batch: replay + apply + publish (steady state) | 40 | 1.17 ms | 2.22 ms | 2.22 ms | - |
| publish() after a 1-op change | 2000 | 223 ns | 228 ns | 506 ns | - |
| reader: at_position + next heading, writer running batches | 534183 | 2.71 us | 5.13 us | 169.18 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 671 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)

