# Summary of bench/results/web-prep/run3

```
date: 2026-10-14T00:54:01Z
kernel: 7.1.8+deb13-rt-amd64
cpus: 16
governor: performance
loadavg_start: 3.74 5.25 7.96
firefox: Mozilla Firefox 153.4.0esr
chromium: Google Chrome for Testing 153.0.8010.12 
loadavg_end: 6.16 7.47 7.98
```

## Build (M-9 / WM-1)

### firefox

| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large1k | 1009 | default frontier c4 w32 stateful | 14.8 | 20.4 | 5.0 | 11.5 | 4.7 | 6.3 | 2.9 | 1528 | 0.3 |
| large1k | 1009 | whole c4 w32 stateful | 13.7 | 14.1 | 4.1 | 10.8 | 4.1 | 6.6 | 3.1 | 1429 | 3.8 |
| large1k | 1009 | frontier c4 w32 states-all | 19.6 | 23.0 | 5.1 | 11.9 | 4.8 | 6.7 | 7.3 | 2118 | 0.3 |
| large1k | 1009 | frontier c4 w32 no-states | 12.4 | 12.7 | 5.0 | 11.9 | 4.8 | 6.8 | 0.0 | 1109 | 0.3 |
| large1k | 1009 | frontier c4 w32 stateful +text | 23.5 | 37.1 | 5.3 | 12.5 | 4.9 | 7.1 | 3.3 | 2454 | 0.3 |
| large1k | 1009 | frontier c1 w32 stateful | 25.4 | 25.6 | 7.2 | 19.5 | 6.8 | 12.2 | 5.4 | 1528 | 0.1 |
| large1k | 1009 | frontier c2 w32 stateful | 19.5 | 20.3 | 5.8 | 14.9 | 5.4 | 9.0 | 4.1 | 1528 | 0.2 |
| large1k | 1009 | frontier c8 w32 stateful | 15.6 | 16.2 | 5.1 | 11.9 | 4.8 | 6.6 | 3.0 | 1528 | 0.6 |
| large1k | 1009 | frontier c4 w8 stateful | 17.7 | 18.1 | 5.5 | 13.5 | 5.0 | 7.8 | 3.6 | 1528 | 0.3 |
| large1k | 1009 | frontier c4 w16 stateful | 17.9 | 20.2 | 5.6 | 13.8 | 5.1 | 8.1 | 3.6 | 1528 | 0.3 |
| large1k | 1009 | frontier c4 w64 stateful | 18.2 | 19.0 | 5.6 | 13.9 | 5.2 | 8.3 | 3.7 | 1528 | 0.4 |
| large1k | 1009 | frontier c4 w128 stateful | 18.6 | 19.2 | 5.5 | 14.1 | 5.1 | 8.6 | 4.0 | 1528 | 0.4 |
| large10k | 10233 | default frontier c4 w32 stateful | 148.2 | 159.0 | 50.9 | 115.2 | 49.8 | 63.2 | 32.0 | 15408 | 0.5 |
| large10k | 10233 | whole c4 w32 stateful | 141.4 | 145.2 | 44.0 | 108.6 | 44.0 | 64.1 | 31.9 | 14551 | 41.7 |
| large10k | 10233 | frontier c4 w32 states-all | 198.8 | 256.7 | 53.0 | 121.1 | 51.8 | 67.1 | 76.8 | 21324 | 0.5 |
| large10k | 10233 | frontier c4 w32 no-states | 123.4 | 125.9 | 52.8 | 122.3 | 51.4 | 69.0 | 0.0 | 11091 | 0.5 |
| large10k | 10233 | frontier c4 w32 stateful +text | 238.8 | 242.7 | 53.1 | 125.8 | 51.6 | 72.0 | 34.5 | 24760 | 0.5 |
| large10k | 10233 | frontier c1 w32 stateful | 256.1 | 257.4 | 70.2 | 196.8 | 67.7 | 125.6 | 57.2 | 15408 | 0.2 |
| large10k | 10233 | frontier c2 w32 stateful | 195.6 | 200.1 | 58.5 | 151.3 | 57.1 | 92.0 | 42.4 | 15408 | 0.3 |
| large10k | 10233 | frontier c8 w32 stateful | 152.5 | 154.4 | 52.1 | 118.7 | 50.9 | 66.0 | 32.8 | 15408 | 0.9 |
| large10k | 10233 | frontier c4 w8 stateful | 171.9 | 175.1 | 54.8 | 133.7 | 53.3 | 77.4 | 37.8 | 15408 | 0.5 |
| large10k | 10233 | frontier c4 w16 stateful | 177.2 | 188.8 | 56.3 | 137.8 | 55.0 | 81.6 | 38.8 | 15408 | 1.3 |
| large10k | 10233 | frontier c4 w64 stateful | 185.7 | 192.8 | 56.5 | 139.8 | 55.1 | 82.7 | 40.4 | 15408 | 1.1 |
| large10k | 10233 | frontier c4 w128 stateful | 187.2 | 190.3 | 57.5 | 144.3 | 55.7 | 85.7 | 41.1 | 15408 | 1.0 |
| large100k | 102305 | default frontier c4 w32 stateful | 1541.5 | 1570.8 | 567.3 | 1210.3 | 496.7 | 638.0 | 324.1 | 153923 | 5.4 |
| large100k | 102305 | whole c4 w32 stateful | 1548.9 | 2449.8 | 520.4 | 1200.3 | 520.4 | 667.0 | 335.2 | 145492 | 494.0 |
| large100k | 102305 | frontier c4 w32 states-all | 2169.1 | 3505.1 | 602.6 | 1294.6 | 527.6 | 686.0 | 791.9 | 213042 | 8.5 |
| large100k | 102305 | frontier c4 w32 no-states | 1321.8 | 1341.1 | 591.4 | 1312.3 | 520.5 | 705.3 | 0.0 | 110737 | 5.9 |
| large100k | 102305 | frontier c4 w32 stateful +text | 2632.0 | 2701.4 | 614.2 | 1378.2 | 542.4 | 748.7 | 375.3 | 247403 | 8.4 |
| large100k | 102305 | frontier c1 w32 stateful | 2650.9 | 2697.0 | 760.6 | 2045.3 | 689.1 | 1273.9 | 581.0 | 153923 | 7.7 |
| large100k | 102305 | frontier c2 w32 stateful | 2020.1 | 2107.7 | 644.0 | 1577.0 | 569.6 | 924.7 | 436.0 | 153923 | 5.1 |
| large100k | 102305 | frontier c8 w32 stateful | 1662.8 | 1716.7 | 604.4 | 1302.6 | 525.7 | 699.2 | 357.2 | 153923 | 7.2 |
| large100k | 102305 | frontier c4 w8 stateful | 1862.1 | 1951.4 | 626.8 | 1451.2 | 553.0 | 823.3 | 394.5 | 153923 | 4.5 |
| large100k | 102305 | frontier c4 w16 stateful | 1829.5 | 1847.6 | 615.4 | 1427.4 | 539.6 | 814.9 | 392.5 | 153923 | 6.0 |
| large100k | 102305 | frontier c4 w64 stateful | 1932.7 | 1974.0 | 639.0 | 1502.7 | 563.2 | 850.9 | 427.6 | 153923 | 5.8 |
| large100k | 102305 | frontier c4 w128 stateful | 1895.1 | 1929.9 | 617.1 | 1471.9 | 540.9 | 849.8 | 412.9 | 153923 | 5.1 |

First block (viewport first): time until the top of the document is complete

| page | block nodes | subtrees left | p50 ms | p95 ms |
|---|---|---|---|---|
| large1k | 444 | 48 | 9.0 | 44.1 |
| large10k | 444 | 806 | 10.7 | 132.0 |
| large100k | 444 | 8380 | 80.5 | 1145.0 |

### chromium

| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large1k | 2093 | default frontier c4 w32 stateful | 32.2 | 38.0 | 13.9 | 28.4 | 13.3 | 13.7 | 3.2 | 2612 | 0.8 |
| large1k | 2093 | whole c4 w32 stateful | 25.8 | 26.0 | 8.9 | 22.5 | 8.9 | 13.6 | 3.3 | 2513 | 8.2 |
| large1k | 2093 | frontier c4 w32 states-all | 39.7 | 39.9 | 9.3 | 23.4 | 9.0 | 14.1 | 15.8 | 4286 | 0.5 |
| large1k | 2093 | frontier c4 w32 no-states | 24.6 | 24.9 | 9.5 | 24.0 | 9.1 | 14.4 | 0.0 | 2193 | 0.6 |
| large1k | 2093 | frontier c4 w32 stateful +text | 43.3 | 43.6 | 9.7 | 24.5 | 9.2 | 14.7 | 3.5 | 4539 | 0.6 |
| large10k | 21203 | default frontier c4 w32 stateful | 292.1 | 298.9 | 106.8 | 255.1 | 100.5 | 146.5 | 34.5 | 26378 | 1.5 |
| large10k | 21203 | whole c4 w32 stateful | 287.5 | 294.8 | 103.3 | 251.9 | 103.3 | 147.8 | 35.2 | 25521 | 97.5 |
| large10k | 21203 | frontier c4 w32 states-all | 437.8 | 449.7 | 106.4 | 261.4 | 100.2 | 154.9 | 176.4 | 43264 | 0.8 |
| large10k | 21203 | frontier c4 w32 no-states | 269.6 | 271.1 | 107.4 | 268.1 | 100.7 | 159.9 | 0.0 | 22061 | 0.8 |
| large10k | 21203 | frontier c4 w32 stateful +text | 470.0 | 472.4 | 108.2 | 269.9 | 101.6 | 159.7 | 37.3 | 45867 | 0.8 |

First block (viewport first): time until the top of the document is complete

| page | block nodes | subtrees left | p50 ms | p95 ms |
|---|---|---|---|---|
| large1k | 416 | 68 | 7.5 | 51.8 |
| large10k | 416 | 826 | 12.5 | 246.3 |

## Churn (WM-4 update latency, M-13 identity)

### spa-list-200 / chromium / use_dom_id=True

initial build 463.1 ms, first block 44.0 ms (410 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 4.2 | 32.4 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 5.4 | 34.5 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 4.2 | 32.2 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 1.8 | 28.7 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 3.1 | 30.0 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 3.4 | 30.7 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 19.3 | 572.5 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 0.67 |
| rewrite in place (unkeyed) | 692 | 19.7 | 74.3 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 4.4 | 84.4 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 0.50 |

### spa-list-200 / chromium / use_dom_id=False

initial build 464.4 ms, first block 47.0 ms (410 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 4.3 | 32.1 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 5.3 | 34.7 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 3.8 | 33.7 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 1.9 | 28.6 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 4.1 | 31.0 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 3.3 | 30.3 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 20.5 | 716.8 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 692 | 19.5 | 76.0 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 4.3 | 87.4 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### large10k / firefox / use_dom_id=True

initial build 837.3 ms, first block 271.9 ms (444 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mut.rename 1 | 5 | 2.8 | 29.1 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1 | 2 | 3.0 | 31.4 | 3 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 100 | 101 | 8.7 | 48.9 | 300 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 100 | 499 | 6.3 | 44.6 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.removeCards 100 | 101 | 9.5 | 42.3 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1000 | 1001 | 29.4 | 157.0 | 3000 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 1000 | 3665 | 49.7 | 317.2 | 12336 | True | None | 7/7 | 0 | 0 | 0 | True | 0.00 |
| mut.route | 1858 | 33.6 | 117.5 | 2 | True | None | 1/7 | 0 | 6 | 0 | False | 1.00 |

### spa-list-200 / firefox / use_dom_id=True

initial build 465.9 ms, first block 54.8 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 3.4 | 29.9 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 4.4 | 35.7 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 2.5 | 31.7 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 1.9 | 28.8 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 2.6 | 31.6 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 11.1 | 72.8 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.52 |
| shuffle (keyed DOM moves) | 718 | 29.3 | 91.1 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 1.9 | 28.7 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 12.1 | 67.9 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 2308 | 8.4 | 83.3 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.49 |
| route swap (200 cards) | 15 | 7.3 | 73.2 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 0.00 |

### spa-list-200 / firefox / use_dom_id=False

initial build 474.9 ms, first block 60.4 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 3.2 | 30.5 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 3.0 | 38.9 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 2.0 | 32.0 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 1.9 | 28.8 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 1.7 | 30.8 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 8.5 | 72.6 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| shuffle (keyed DOM moves) | 718 | 9.2 | 75.5 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| remove 10 in the middle | 11 | 2.3 | 29.2 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 12.1 | 75.0 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 2308 | 7.7 | 79.8 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.97 |
| route swap (200 cards) | 15 | 12.1 | 110.3 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### spa-list-2000 / firefox / use_dom_id=True

initial build 882.7 ms, first block 308.3 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 22.3 | 52.1 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 10.8 | 42.9 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 2.8 | 35.8 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 2.9 | 30.4 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 5.1 | 34.8 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 39.3 | 369.4 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.50 |
| shuffle (keyed DOM moves) | 6110 | 49.0 | 405.4 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 4.4 | 32.9 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 82.4 | 413.5 | 4062 | True | None | 2035/2035 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 20308 | 100.6 | 603.7 | 4073 | True | None | 2035/2035 | 0 | 0 | 0 | True | 0.50 |
| route swap (200 cards) | 15 | 27.9 | 110.5 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 0.00 |

### spa-list-2000 / firefox / use_dom_id=False

initial build 882.3 ms, first block 311.1 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 6.5 | 36.7 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 6.3 | 38.9 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 4.3 | 37.3 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 3.0 | 30.6 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 5.7 | 34.7 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 60.3 | 394.9 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 1.00 |
| shuffle (keyed DOM moves) | 6110 | 46.0 | 406.7 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 1.00 |
| remove 10 in the middle | 11 | 4.9 | 33.8 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 47.9 | 381.9 | 4062 | True | None | 2035/2035 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 20308 | 67.0 | 577.1 | 4073 | True | None | 2035/2035 | 0 | 0 | 0 | True | 1.00 |
| route swap (200 cards) | 15 | 11.8 | 85.9 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 1.00 |

## Scaling (M-15)

### large100k: 102301 nodes, 8420 GetTree replies, 22.0 MB, 16 cores

| decode threads | p50 ms | speedup |
|---|---|---|
| 1 | 30.8 | 1.00 |
| 2 | 15.8 | 1.94 |
| 4 | 8.3 | 3.73 |
| 8 | 5.1 | 6.05 |
| 16 | 2.9 | 10.63 |

| P2P connections | fetch total p50 ms |
|---|---|
| 1 | 2075.4 |
| 2 | 1694.6 |
| 4 | 1577.5 |
| 8 | 1604.6 |
| 16 | 1632.1 |

### large10k: 10229 nodes, 846 GetTree replies, 2.2 MB, 16 cores

| decode threads | p50 ms | speedup |
|---|---|---|
| 1 | 3.2 | 1.00 |
| 2 | 1.6 | 1.95 |
| 4 | 1.0 | 3.33 |
| 8 | 0.8 | 4.00 |
| 16 | 0.8 | 3.85 |

| P2P connections | fetch total p50 ms |
|---|---|
| 1 | 187.8 |
| 2 | 160.9 |
| 4 | 148.9 |
| 8 | 144.6 |
| 16 | 146.6 |

## Research (TBD-WEB-1/2/3)

### firefox

| page | step | rows before->after | kept | same path | id stable | wrong node | positional reassigned | nodes persisting | state flips (showing) | with event | events |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large10k | window scroll 20000 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 10233 | 67 | 0 |  |
| large10k | window scroll 200000 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 10233 | 68 | 0 |  |
| large10k | window scroll back | 0->0 | 0 | 0 | 0 | 0 | 0 | 10233 | 65 | 0 |  |
| spa-list-200 | reverse (keyed DOM moves) | 0->0 | 0 | 0 | 0 | 0 | 0 | 11 | 0 | 0 | children-added=200, children-removed=200, text-delete=1, text-insert=1 |
| spa-list-200 | shuffle | 0->0 | 0 | 0 | 0 | 0 | 0 | 11 | 0 | 0 | children-added=200, children-removed=200, text-delete=189, text-insert=1 |
| spa-list-200 | window scroll 400 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 411 | 63 | 0 |  |
| spa-list-200 | window scroll back | 0->0 | 0 | 0 | 0 | 0 | 0 | 411 | 63 | 0 |  |
| virt-cdk | scrollBy +64 px (2 rows) | 19->19 | 19 | 19 | 19 | 0 | 0 | 23 | 4 | 0 |  |
| virt-cdk | scrollBy +640 px (20 rows) | 19->23 | 1 | 1 | 1 | 0 | 0 | 5 | 0 | 0 | children-added=22, children-removed=18, text-delete=1, text-insert=1 |
| virt-cdk | scrollTo row 500 | 23->23 | 0 | 0 | 0 | 0 | 23 | 4 | 0 | 0 | children-added=23, children-removed=23, text-delete=1, text-insert=1 |
| virt-cdk | scrollBy -3200 px (back 100 rows) | 23->23 | 0 | 0 | 0 | 0 | 23 | 4 | 0 | 0 | children-added=23, children-removed=23, text-delete=1, text-insert=1 |
| virt-cdk | scrollTo row 5000 (jump) | 23->23 | 0 | 0 | 0 | 0 | 23 | 4 | 0 | 0 | children-added=23, children-removed=23, text-delete=1, text-insert=1 |
| virt-cdk | scrollTo row 0 (jump back) | 23->16 | 0 | 0 | 0 | 0 | 16 | 4 | 0 | 0 | children-added=16, children-removed=23, text-delete=1, text-insert=1 |
| virt-tanstack-index | scrollBy +64 px (2 rows) | 16->18 | 16 | 16 | 16 | 0 | 0 | 20 | 4 | 0 | children-added=2, text-insert=1 |
| virt-tanstack-index | scrollBy +640 px (20 rows) | 18->19 | 0 | 0 | 0 | 0 | 18 | 4 | 0 | 0 | children-added=19, children-removed=18, text-delete=1, text-insert=1 |
| virt-tanstack-index | scrollTo row 500 | 19->19 | 0 | 0 | 0 | 0 | 19 | 4 | 0 | 0 | children-added=19, children-removed=19, text-delete=1, text-insert=1 |
| virt-tanstack-index | scrollBy -3200 px (back 100 rows) | 19->19 | 0 | 0 | 0 | 0 | 19 | 4 | 0 | 0 | children-added=19, children-removed=19, text-delete=1, text-insert=1 |
| virt-tanstack-index | scrollTo row 5000 (jump) | 19->19 | 0 | 0 | 0 | 0 | 19 | 4 | 0 | 0 | children-added=19, children-removed=19, text-delete=1, text-insert=1 |
| virt-tanstack-index | scrollTo row 0 (jump back) | 19->16 | 0 | 0 | 0 | 0 | 16 | 4 | 0 | 0 | children-added=16, children-removed=19, text-delete=1, text-insert=1 |
| virt-tanstack-slot | scrollBy +64 px (2 rows) | 16->18 | 16 | 16 | 16 | 0 | 0 | 20 | 4 | 0 | children-added=2, text-insert=1 |
| virt-tanstack-slot | scrollBy +640 px (20 rows) | 18->19 | 0 | 0 | 0 | 0 | 14 | 18 | 5 | 0 | children-added=5, children-removed=4, name-changed=14, text-delete=15, text-insert=15 |
| virt-tanstack-slot | scrollTo row 500 | 19->19 | 0 | 0 | 0 | 0 | 17 | 21 | 4 | 0 | children-added=2, children-removed=2, name-changed=17, text-delete=18, text-insert=18 |
| virt-tanstack-slot | scrollBy -3200 px (back 100 rows) | 19->19 | 0 | 0 | 0 | 0 | 15 | 19 | 6 | 0 | children-added=4, children-removed=4, name-changed=15, text-delete=16, text-insert=16 |
| virt-tanstack-slot | scrollTo row 5000 (jump) | 19->19 | 0 | 0 | 0 | 0 | 3 | 7 | 0 | 0 | children-added=16, children-removed=16, name-changed=3, text-delete=5, text-insert=4 |
| virt-tanstack-slot | scrollTo row 0 (jump back) | 19->16 | 0 | 0 | 0 | 0 | 11 | 15 | 6 | 0 | children-added=5, children-removed=8, name-changed=11, text-delete=12, text-insert=12 |
| virt-vue-recycle | scrollBy +64 px (2 rows) | 19->21 | 19 | 19 | 19 | 0 | 0 | 44 | 8 | 0 | children-added=2, text-insert=1 |
| virt-vue-recycle | scrollBy +640 px (20 rows) | 21->26 | 6 | 6 | 6 | 0 | 15 | 18 | 0 | 0 | children-added=35, children-removed=30, name-changed=15, text-delete=16, text-insert=17 |
| virt-vue-recycle | scrollTo row 500 | 26->52 | 26 | 26 | 26 | 0 | 0 | 58 | 26 | 0 | children-added=26, text-insert=1 |
| virt-vue-recycle | scrollBy -3200 px (back 100 rows) | 52->52 | 26 | 26 | 26 | 0 | 26 | 110 | 0 | 0 | children-added=26, children-removed=26, name-changed=26, text-delete=26, text-insert=26 |
| virt-vue-recycle | scrollTo row 5000 (jump) | 52->52 | 26 | 26 | 26 | 0 | 26 | 110 | 0 | 0 | children-added=26, children-removed=26, name-changed=26, text-delete=26, text-insert=26 |
| virt-vue-recycle | scrollTo row 0 (jump back) | 52->52 | 37 | 37 | 37 | 0 | 15 | 80 | 10 | 0 | children-added=30, children-removed=30, name-changed=15, text-delete=16, text-insert=16 |
| virtual-grid | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 73 | 16 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=2 |
| virtual-grid | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 5 | 0 | 0 | children-added=21, children-removed=20, text-delete=2, text-insert=2 |
| virtual-grid | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-grid | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-grid | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-grid | scrollTo row 0 (jump back) | 20->17 | 0 | 0 | 0 | 0 | 17 | 5 | 0 | 0 | children-added=18, children-removed=21, text-delete=2, text-insert=2 |
| virtual-recycle | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 22 | 4 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=2 |
| virtual-recycle | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 24 | 2 | 0 | children-added=21, children-removed=20, name-changed=19, text-delete=20, text-insert=21 |
| virtual-recycle | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 25 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=21, text-insert=21 |
| virtual-recycle | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 25 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=21, text-insert=21 |
| virtual-recycle | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 25 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=21, text-insert=21 |
| virtual-recycle | scrollTo row 0 (jump back) | 20->20 | 3 | 3 | 3 | 0 | 17 | 25 | 6 | 0 | children-added=18, children-removed=18, name-changed=17, text-delete=18, text-insert=18 |
| virtual-unmount | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 22 | 4 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=2 |
| virtual-unmount | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 5 | 0 | 0 | children-added=21, children-removed=20, text-delete=2, text-insert=2 |
| virtual-unmount | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-unmount | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-unmount | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 5 | 0 | 0 | children-added=21, children-removed=21, text-delete=2, text-insert=2 |
| virtual-unmount | scrollTo row 0 (jump back) | 20->17 | 0 | 0 | 0 | 0 | 17 | 5 | 0 | 0 | children-added=18, children-removed=21, text-delete=2, text-insert=2 |

Attribute names seen on list items, rows and cells:

* large10k: child-item-count, display, explicit-name, posinset, setsize, table-cell-index, tag
* spa-list-200: child-item-count, display, id, posinset, setsize, tag
* virt-cdk: class, display, explicit-name, id, posinset, setsize, tag, xml-roles
* virt-tanstack-index: child-item-count, display, explicit-name, id, posinset, setsize, tag, xml-roles
* virt-tanstack-slot: child-item-count, display, explicit-name, id, posinset, setsize, tag, xml-roles
* virt-vue-recycle: class, display, explicit-name, id, posinset, setsize, tag, xml-roles
* virtual-grid: child-item-count, class, colcount, colindex, display, explicit-name, id, posinset, rowcount, rowindex, setsize, table-cell-index, tag, xml-roles
* virtual-recycle: class, display, explicit-name, id, posinset, setsize, tag, xml-roles
* virtual-unmount: class, display, explicit-name, id, posinset, setsize, tag, xml-roles

### chromium

| page | step | rows before->after | kept | same path | id stable | wrong node | positional reassigned | nodes persisting | state flips (showing) | with event | events |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large10k | window scroll 20000 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 21203 | 131 | 0 |  |
| large10k | window scroll 200000 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 21203 | 140 | 0 |  |
| large10k | window scroll back | 0->0 | 0 | 0 | 0 | 0 | 0 | 21203 | 131 | 0 |  |
| spa-list-200 | reverse (keyed DOM moves) | 0->0 | 0 | 0 | 0 | 0 | 0 | 616 | 144 | 0 |  |
| spa-list-200 | shuffle | 0->0 | 0 | 0 | 0 | 0 | 0 | 616 | 120 | 0 |  |
| spa-list-200 | window scroll 400 px | 0->0 | 0 | 0 | 0 | 0 | 0 | 616 | 93 | 0 |  |
| spa-list-200 | window scroll back | 0->0 | 0 | 0 | 0 | 0 | 0 | 616 | 93 | 0 |  |
| virt-cdk | scrollBy +64 px (2 rows) | 19->19 | 19 | 19 | 19 | 0 | 0 | 44 | 8 | 0 |  |
| virt-cdk | scrollBy +640 px (20 rows) | 19->23 | 1 | 1 | 1 | 0 | 18 | 44 | 0 | 0 | children-added=4, name-changed=36 |
| virt-cdk | scrollTo row 500 | 23->23 | 0 | 0 | 0 | 0 | 23 | 46 | 4 | 0 | children-added=3, children-removed=3, name-changed=40 |
| virt-cdk | scrollBy -3200 px (back 100 rows) | 23->23 | 0 | 0 | 0 | 0 | 23 | 46 | 16 | 0 | children-added=3, children-removed=3, name-changed=40 |
| virt-cdk | scrollTo row 5000 (jump) | 23->23 | 0 | 0 | 0 | 0 | 23 | 46 | 16 | 0 | children-added=3, children-removed=3, name-changed=40 |
| virt-cdk | scrollTo row 0 (jump back) | 23->16 | 0 | 0 | 0 | 0 | 16 | 38 | 12 | 0 | children-removed=7, name-changed=32 |
| virt-tanstack-index | scrollBy +64 px (2 rows) | 17->19 | 17 | 16 | 16 | 0 | 0 | 54 | 12 | 0 | children-added=2 |
| virt-tanstack-index | scrollBy +640 px (20 rows) | 19->20 | 1 | 0 | 1 | 0 | 18 | 6 | 0 | 0 | children-added=19, children-removed=18 |
| virt-tanstack-index | scrollTo row 500 | 20->20 | 1 | 0 | 1 | 0 | 19 | 6 | 0 | 0 | children-added=19, children-removed=19 |
| virt-tanstack-index | scrollBy -3200 px (back 100 rows) | 20->20 | 1 | 0 | 1 | 0 | 19 | 6 | 0 | 0 | children-added=19, children-removed=19 |
| virt-tanstack-index | scrollTo row 5000 (jump) | 20->20 | 1 | 0 | 1 | 0 | 19 | 6 | 0 | 0 | children-added=19, children-removed=19 |
| virt-tanstack-index | scrollTo row 0 (jump back) | 20->17 | 1 | 0 | 0 | 0 | 16 | 6 | 0 | 0 | children-added=16, children-removed=19 |
| virt-tanstack-slot | scrollBy +64 px (2 rows) | 17->19 | 17 | 16 | 16 | 0 | 0 | 54 | 12 | 0 | children-added=2 |
| virt-tanstack-slot | scrollBy +640 px (20 rows) | 19->20 | 1 | 0 | 0 | 0 | 14 | 48 | 15 | 0 | children-added=5, children-removed=4, name-changed=28 |
| virt-tanstack-slot | scrollTo row 500 | 20->20 | 1 | 0 | 1 | 0 | 17 | 57 | 12 | 0 | children-added=2, children-removed=2, name-changed=34 |
| virt-tanstack-slot | scrollBy -3200 px (back 100 rows) | 20->20 | 1 | 0 | 1 | 0 | 15 | 51 | 18 | 0 | children-added=4, children-removed=4, name-changed=30 |
| virt-tanstack-slot | scrollTo row 5000 (jump) | 20->20 | 1 | 0 | 1 | 0 | 14 | 48 | 18 | 0 | children-added=5, children-removed=5, name-changed=28 |
| virt-tanstack-slot | scrollTo row 0 (jump back) | 20->17 | 1 | 0 | 0 | 0 | 11 | 39 | 18 | 0 | children-added=5, children-removed=8, name-changed=22 |
| virt-vue-recycle | scrollBy +64 px (2 rows) | 19->21 | 19 | 19 | 19 | 0 | 0 | 64 | 12 | 0 | children-added=2 |
| virt-vue-recycle | scrollBy +640 px (20 rows) | 21->26 | 6 | 6 | 6 | 0 | 15 | 55 | 5 | 0 | children-added=20, children-removed=15, name-changed=15 |
| virt-vue-recycle | scrollTo row 500 | 26->52 | 26 | 26 | 20 | 0 | 0 | 85 | 39 | 0 | children-added=26 |
| virt-vue-recycle | scrollBy -3200 px (back 100 rows) | 52->52 | 26 | 26 | 26 | 0 | 26 | 137 | 0 | 0 | children-added=26, children-removed=26, name-changed=26 |
| virt-vue-recycle | scrollTo row 5000 (jump) | 52->52 | 26 | 26 | 26 | 0 | 26 | 137 | 0 | 0 | children-added=26, children-removed=26, name-changed=26 |
| virt-vue-recycle | scrollTo row 0 (jump back) | 52->52 | 37 | 37 | 37 | 0 | 15 | 148 | 34 | 0 | children-added=15, children-removed=15, name-changed=15 |
| virtual-grid | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 125 | 24 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=1 |
| virtual-grid | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 6 | 0 | 0 | children-added=21, children-removed=20, text-delete=1, text-insert=1 |
| virtual-grid | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-grid | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-grid | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-grid | scrollTo row 0 (jump back) | 20->17 | 0 | 0 | 0 | 0 | 17 | 6 | 0 | 0 | children-added=18, children-removed=21, text-delete=1, text-insert=1 |
| virtual-recycle | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 40 | 7 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=1 |
| virtual-recycle | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 25 | 2 | 0 | children-added=21, children-removed=20, name-changed=19, text-delete=1, text-insert=1 |
| virtual-recycle | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 26 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=1, text-insert=1 |
| virtual-recycle | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 26 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=1, text-insert=1 |
| virtual-recycle | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 26 | 0 | 0 | children-added=21, children-removed=21, name-changed=20, text-delete=1, text-insert=1 |
| virtual-recycle | scrollTo row 0 (jump back) | 20->20 | 3 | 3 | 3 | 0 | 17 | 29 | 5 | 0 | children-added=18, children-removed=18, name-changed=17, text-delete=1, text-insert=1 |
| virtual-unmount | scrollBy +64 px (2 rows) | 17->19 | 17 | 17 | 17 | 0 | 0 | 40 | 7 | 0 | children-added=3, children-removed=1, text-delete=1, text-insert=1 |
| virtual-unmount | scrollBy +640 px (20 rows) | 19->20 | 0 | 0 | 0 | 0 | 19 | 6 | 0 | 0 | children-added=21, children-removed=20, text-delete=1, text-insert=1 |
| virtual-unmount | scrollTo row 500 | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-unmount | scrollBy -3200 px (back 100 rows) | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-unmount | scrollTo row 5000 (jump) | 20->20 | 0 | 0 | 0 | 0 | 20 | 6 | 0 | 0 | children-added=21, children-removed=21, text-delete=1, text-insert=1 |
| virtual-unmount | scrollTo row 0 (jump back) | 20->17 | 0 | 0 | 0 | 0 | 17 | 6 | 0 | 0 | children-added=18, children-removed=21, text-delete=1, text-insert=1 |

Attribute names seen on list items, rows and cells:

* large10k: display, explicit-name, level, name-from, posinset, setsize, table-cell-index, tag, text-align
* spa-list-200: display, id, level, posinset, setsize, tag, text-align
* virt-cdk: class, display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles
* virt-tanstack-index: display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles
* virt-tanstack-slot: display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles
* virt-vue-recycle: class, display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles
* virtual-grid: class, colcount, display, explicit-name, id, name-from, rowcount, rowindex, table-cell-index, tag, text-align, xml-roles
* virtual-recycle: class, display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles
* virtual-unmount: class, display, explicit-name, id, level, name-from, posinset, setsize, tag, text-align, xml-roles

