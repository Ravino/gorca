# gorca-doc MB benches

conditions: loadavg `7.45 6.96 5.56 3/2842 1144404`, governor `powersave`, model name	: Intel(R) Core(TM) Ultra 9 285H, cores 16, quick=false

### 1000 nodes (1018 built, 1024 after update runs)

build 0.1 ms (141 ns/node), heap 0.2 MB (223 B/node), headings 29, links 174, lines 217, timer overhead 13 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 118 ns | 202 ns | 20.63 us | 120 ns |
| prev heading | 100000 | 124 ns | 198 ns | 9.32 us | 248 ns |
| next heading level 2 (filter) | 100000 | 96 ns | 216 ns | 2.28 us | 93 ns |
| next heading level 1-3 (filter) | 100000 | 91 ns | 157 ns | 10.28 us | 97 ns |
| next link | 100000 | 100 ns | 167 ns | 2.93 us | 105 ns |
| next unvisited link | 100000 | 100 ns | 171 ns | 9.60 us | 104 ns |
| next landmark/region | 100000 | 92 ns | 152 ns | 5.29 us | 97 ns |
| next form field | 100000 | 84 ns | 145 ns | 9.25 us | 88 ns |
| next table | 100000 | 87 ns | 142 ns | 9.39 us | 89 ns |
| next list item | 100000 | 92 ns | 151 ns | 4.01 us | 95 ns |
| next text run (dense category) | 100000 | 96 ns | 160 ns | 6.92 us | 103 ns |
| next link within landmark | 100000 | 160 ns | 247 ns | 8.69 us | 158 ns |
| next_in_landmark(link) | 100000 | 168 ns | 267 ns | 13.76 us | 173 ns |
| kth heading | 100000 | 14 ns | 28 ns | 12.01 us | 14 ns |
| rank (heading k of N) | 100000 | 118 ns | 204 ns | 8.60 us | 119 ns |
| cmp_order | 100000 | 104 ns | 163 ns | 4.44 us | 106 ns |
| landmark_of | 100000 | 30 ns | 54 ns | 89 ns | 31 ns |
| text: offset_of(node) | 100000 | 56 ns | 93 ns | 3.29 us | 59 ns |
| text: at_offset | 100000 | 39 ns | 64 ns | 3.81 us | 42 ns |
| text: line_start(k) | 100000 | 35 ns | 59 ns | 5.56 us | 40 ns |
| text: line_of(offset) | 100000 | 64 ns | 106 ns | 5.41 us | 69 ns |
| text: line_range + read line | 100000 | 282 ns | 390 ns | 5.83 us | 290 ns |
| text: word at offset | 100000 | 375 ns | 489 ns | 12.97 us | 406 ns |
| text: next line | 100000 | 206 ns | 350 ns | 9.41 us | 206 ns |
| batch: apply (writer, back copy) | 300 | 69.57 us | 82.71 us | 90.98 us | - |
| batch: publish() only | 300 | 137 ns | 851 ns | 1.10 us | - |
| batch: apply + publish | 300 | 69.71 us | 82.96 us | 91.12 us | - |
| batch: replay on other copy (deferred) | 300 | 70.02 us | 84.54 us | 86.44 us | - |
| batch: replay + apply + publish (steady state) | 300 | 139.38 us | 162.99 us | 175.44 us | - |
| publish() after a 1-op change | 2000 | 79 ns | 81 ns | 5.17 us | - |
| reader: at_position + next heading, writer running batches | 7353258 | 167 ns | 421 ns | 38.30 us | - |

batch: 280 ops touching ~1012 nodes; writer waits 232 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 10000 nodes (10008 built, 4232 after update runs)

build 2.0 ms (203 ns/node), heap 3.5 MB (355 B/node), headings 294, links 1669, lines 2182, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 196 ns | 308 ns | 29.86 us | 192 ns |
| prev heading | 100000 | 207 ns | 325 ns | 6.11 us | 193 ns |
| next heading level 2 (filter) | 100000 | 174 ns | 275 ns | 18.29 us | 171 ns |
| next heading level 1-3 (filter) | 100000 | 188 ns | 294 ns | 9.68 us | 185 ns |
| next link | 100000 | 189 ns | 286 ns | 5.22 us | 185 ns |
| next unvisited link | 100000 | 189 ns | 286 ns | 13.46 us | 186 ns |
| next landmark/region | 100000 | 192 ns | 301 ns | 6.20 us | 189 ns |
| next form field | 100000 | 169 ns | 265 ns | 5.11 us | 166 ns |
| next table | 100000 | 166 ns | 259 ns | 2.16 us | 163 ns |
| next list item | 100000 | 179 ns | 286 ns | 3.02 us | 177 ns |
| next text run (dense category) | 100000 | 182 ns | 277 ns | 10.05 us | 179 ns |
| next link within landmark | 100000 | 316 ns | 446 ns | 12.41 us | 310 ns |
| next_in_landmark(link) | 100000 | 329 ns | 472 ns | 13.33 us | 337 ns |
| kth heading | 100000 | 50 ns | 93 ns | 3.96 us | 33 ns |
| rank (heading k of N) | 100000 | 254 ns | 388 ns | 7.24 us | 247 ns |
| cmp_order | 100000 | 183 ns | 258 ns | 8.67 us | 183 ns |
| landmark_of | 100000 | 51 ns | 86 ns | 7.29 us | 42 ns |
| text: offset_of(node) | 100000 | 101 ns | 157 ns | 10.41 us | 100 ns |
| text: at_offset | 100000 | 79 ns | 131 ns | 2.16 us | 80 ns |
| text: line_start(k) | 100000 | 73 ns | 130 ns | 6.11 us | 77 ns |
| text: line_of(offset) | 100000 | 135 ns | 222 ns | 8.56 us | 138 ns |
| text: line_range + read line | 100000 | 539 ns | 717 ns | 10.27 us | 533 ns |
| text: word at offset | 100000 | 660 ns | 848 ns | 10.71 us | 651 ns |
| text: next line | 100000 | 524 ns | 724 ns | 5.87 us | 523 ns |
| batch: apply (writer, back copy) | 300 | 92.55 us | 105.94 us | 110.75 us | - |
| batch: publish() only | 300 | 167 ns | 420 ns | 547 ns | - |
| batch: apply + publish | 300 | 92.73 us | 106.12 us | 111.02 us | - |
| batch: replay on other copy (deferred) | 300 | 104.32 us | 118.96 us | 126.26 us | - |
| batch: replay + apply + publish (steady state) | 300 | 198.41 us | 224.41 us | 227.69 us | - |
| publish() after a 1-op change | 2000 | 122 ns | 123 ns | 3.98 us | - |
| reader: at_position + next heading, writer running batches | 4861501 | 260 ns | 699 ns | 85.12 us | - |

batch: 281 ops touching ~1012 nodes; writer waits 1270 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 100000 nodes (100048 built, 91691 after update runs)

build 16.4 ms (164 ns/node), heap 28.7 MB (287 B/node), headings 2905, links 16704, lines 21843, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 100000 | 329 ns | 505 ns | 19.58 us | 315 ns |
| prev heading | 100000 | 333 ns | 511 ns | 28.42 us | 320 ns |
| next heading level 2 (filter) | 100000 | 319 ns | 475 ns | 11.77 us | 309 ns |
| next heading level 1-3 (filter) | 100000 | 323 ns | 493 ns | 11.93 us | 309 ns |
| next link | 100000 | 306 ns | 498 ns | 8.40 us | 296 ns |
| next unvisited link | 100000 | 308 ns | 479 ns | 13.58 us | 298 ns |
| next landmark/region | 100000 | 322 ns | 499 ns | 23.32 us | 309 ns |
| next form field | 100000 | 316 ns | 473 ns | 18.19 us | 306 ns |
| next table | 100000 | 317 ns | 478 ns | 13.69 us | 307 ns |
| next list item | 100000 | 318 ns | 492 ns | 8.77 us | 307 ns |
| next text run (dense category) | 100000 | 290 ns | 448 ns | 25.18 us | 283 ns |
| next link within landmark | 100000 | 557 ns | 822 ns | 21.61 us | 543 ns |
| next_in_landmark(link) | 100000 | 585 ns | 865 ns | 16.27 us | 577 ns |
| kth heading | 100000 | 210 ns | 370 ns | 8.75 us | 199 ns |
| rank (heading k of N) | 100000 | 448 ns | 660 ns | 8.12 us | 441 ns |
| cmp_order | 100000 | 272 ns | 384 ns | 17.50 us | 271 ns |
| landmark_of | 100000 | 104 ns | 262 ns | 11.07 us | 77 ns |
| text: offset_of(node) | 100000 | 178 ns | 335 ns | 10.68 us | 174 ns |
| text: at_offset | 100000 | 168 ns | 340 ns | 7.71 us | 169 ns |
| text: line_start(k) | 100000 | 164 ns | 439 ns | 13.24 us | 168 ns |
| text: line_of(offset) | 100000 | 267 ns | 522 ns | 20.96 us | 260 ns |
| text: line_range + read line | 100000 | 807 ns | 1.38 us | 27.10 us | 844 ns |
| text: word at offset | 100000 | 933 ns | 1.62 us | 29.56 us | 971 ns |
| text: next line | 100000 | 818 ns | 1.32 us | 23.50 us | 843 ns |
| batch: apply (writer, back copy) | 300 | 213.73 us | 327.91 us | 376.71 us | - |
| batch: publish() only | 300 | 302 ns | 846 ns | 889 ns | - |
| batch: apply + publish | 300 | 214.08 us | 328.66 us | 376.97 us | - |
| batch: replay on other copy (deferred) | 300 | 272.36 us | 430.11 us | 500.00 us | - |
| batch: replay + apply + publish (steady state) | 300 | 488.50 us | 739.97 us | 876.97 us | - |
| publish() after a 1-op change | 2000 | 130 ns | 133 ns | 10.15 us | - |
| reader: at_position + next heading, writer running batches | 3922022 | 390 ns | 1.20 us | 28.47 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 568 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)


### 1000000 nodes (1000031 built, 999352 after update runs)

build 174.4 ms (174 ns/node), heap 232.2 MB (232 B/node), headings 29057, links 166925, lines 218390, timer overhead 11 ns (subtracted)

| operation | samples | p50 | p99 | max | bulk mean |
|---|---|---|---|---|---|
| next heading | 20000 | 725 ns | 1.67 us | 4.80 us | 536 ns |
| prev heading | 20000 | 633 ns | 1.42 us | 4.80 us | 549 ns |
| next heading level 2 (filter) | 20000 | 640 ns | 1.93 us | 16.92 us | 572 ns |
| next heading level 1-3 (filter) | 20000 | 620 ns | 1.65 us | 18.63 us | 575 ns |
| next link | 20000 | 535 ns | 1.29 us | 11.96 us | 507 ns |
| next unvisited link | 20000 | 523 ns | 1.47 us | 15.22 us | 507 ns |
| next landmark/region | 20000 | 632 ns | 1.90 us | 16.10 us | 562 ns |
| next form field | 20000 | 811 ns | 2.11 us | 15.75 us | 750 ns |
| next table | 20000 | 633 ns | 1.75 us | 10.33 us | 625 ns |
| next list item | 20000 | 627 ns | 1.73 us | 10.01 us | 630 ns |
| next text run (dense category) | 20000 | 516 ns | 1.52 us | 10.59 us | 472 ns |
| next link within landmark | 20000 | 1.13 us | 2.56 us | 14.96 us | 991 ns |
| next_in_landmark(link) | 20000 | 1.07 us | 2.32 us | 18.64 us | 1.26 us |
| kth heading | 20000 | 1.46 us | 2.84 us | 11.49 us | 1.22 us |
| rank (heading k of N) | 20000 | 970 ns | 2.35 us | 10.78 us | 947 ns |
| cmp_order | 20000 | 457 ns | 782 ns | 10.29 us | 451 ns |
| landmark_of | 20000 | 241 ns | 679 ns | 3.25 us | 190 ns |
| text: offset_of(node) | 20000 | 335 ns | 658 ns | 4.23 us | 324 ns |
| text: at_offset | 20000 | 475 ns | 1.71 us | 5.12 us | 504 ns |
| text: line_start(k) | 20000 | 469 ns | 1.66 us | 21.85 us | 597 ns |
| text: line_of(offset) | 20000 | 791 ns | 2.30 us | 19.98 us | 619 ns |
| text: line_range + read line | 20000 | 1.88 us | 3.34 us | 13.75 us | 1.94 us |
| text: word at offset | 20000 | 2.38 us | 4.10 us | 21.29 us | 2.47 us |
| text: next line | 20000 | 1.91 us | 3.68 us | 20.38 us | 2.04 us |
| batch: apply (writer, back copy) | 40 | 422.45 us | 472.82 us | 472.82 us | - |
| batch: publish() only | 40 | 335 ns | 417 ns | 417 ns | - |
| batch: apply + publish | 40 | 422.84 us | 473.08 us | 473.08 us | - |
| batch: replay on other copy (deferred) | 40 | 521.92 us | 561.45 us | 561.45 us | - |
| batch: replay + apply + publish (steady state) | 40 | 947.00 us | 1.03 ms | 1.03 ms | - |
| publish() after a 1-op change | 2000 | 162 ns | 165 ns | 357 ns | - |
| reader: at_position + next heading, writer running batches | 502453 | 2.90 us | 5.29 us | 39.60 us | - |

batch: 281 ops touching ~1018 nodes; writer waits 697 (slow >1 ms: 0), fallback clones 0 (batch phase) / 0 (total)

