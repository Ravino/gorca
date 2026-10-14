# Summary of bench/results/web-prep/run2

```
date: 2026-10-14T00:35:41Z
kernel: 7.1.8+deb13-rt-amd64
cpus: 16
governor: performance
loadavg_start: 7.23 8.51 9.62
firefox: Mozilla Firefox 153.4.0esr
chromium: Google Chrome for Testing 153.0.8010.12 
loadavg_end: 2.76 6.66 9.10
```

## Build (M-9 / WM-1)

### firefox

| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large1k | 1009 | default frontier c4 w32 stateful | 16.7 | 22.3 | 5.4 | 12.5 | 4.9 | 7.3 | 3.4 | 1528 | 0.3 |
| large1k | 1009 | whole c4 w32 stateful | 14.9 | 15.5 | 4.1 | 11.3 | 4.1 | 7.1 | 3.4 | 1429 | 3.8 |
| large1k | 1009 | frontier c4 w32 states-all | 20.4 | 24.8 | 5.1 | 12.3 | 4.8 | 7.1 | 7.5 | 2118 | 0.3 |
| large1k | 1009 | frontier c4 w32 no-states | 13.1 | 14.0 | 5.4 | 12.7 | 5.0 | 7.3 | 0.0 | 1109 | 0.3 |
| large1k | 1009 | frontier c4 w32 stateful +text | 25.3 | 38.0 | 5.3 | 13.1 | 5.0 | 7.7 | 3.6 | 2454 | 0.3 |
| large1k | 1009 | frontier c1 w32 stateful | 26.0 | 29.1 | 7.3 | 19.9 | 7.0 | 12.5 | 5.6 | 1528 | 0.1 |
| large1k | 1009 | frontier c2 w32 stateful | 19.6 | 21.7 | 5.8 | 15.1 | 5.5 | 9.2 | 4.2 | 1528 | 0.2 |
| large1k | 1009 | frontier c8 w32 stateful | 17.1 | 18.0 | 5.2 | 13.1 | 4.8 | 7.8 | 3.6 | 1528 | 0.7 |
| large1k | 1009 | frontier c4 w8 stateful | 17.5 | 17.9 | 5.5 | 13.5 | 5.2 | 7.8 | 3.6 | 1528 | 0.3 |
| large1k | 1009 | frontier c4 w16 stateful | 18.8 | 24.5 | 5.7 | 14.2 | 5.3 | 8.5 | 4.0 | 1528 | 0.4 |
| large1k | 1009 | frontier c4 w64 stateful | 19.3 | 21.2 | 5.6 | 14.4 | 5.2 | 8.8 | 4.0 | 1528 | 0.4 |
| large1k | 1009 | frontier c4 w128 stateful | 19.7 | 22.3 | 5.6 | 14.8 | 5.3 | 9.0 | 4.4 | 1528 | 0.4 |
| large10k | 10233 | default frontier c4 w32 stateful | 157.5 | 171.1 | 53.5 | 121.3 | 52.4 | 67.2 | 34.0 | 15408 | 0.5 |
| large10k | 10233 | whole c4 w32 stateful | 151.3 | 169.7 | 44.2 | 114.4 | 44.2 | 70.4 | 35.4 | 14551 | 42.6 |
| large10k | 10233 | frontier c4 w32 states-all | 211.1 | 213.7 | 54.5 | 126.0 | 53.0 | 71.2 | 83.4 | 21324 | 0.5 |
| large10k | 10233 | frontier c4 w32 no-states | 128.0 | 130.6 | 54.4 | 127.2 | 53.5 | 72.2 | 0.0 | 11091 | 0.5 |
| large10k | 10233 | frontier c4 w32 stateful +text | 243.8 | 264.0 | 54.9 | 128.8 | 53.7 | 73.0 | 36.1 | 24760 | 0.5 |
| large10k | 10233 | frontier c1 w32 stateful | 263.4 | 287.4 | 73.5 | 201.9 | 72.4 | 127.5 | 59.1 | 15408 | 0.3 |
| large10k | 10233 | frontier c2 w32 stateful | 202.0 | 204.9 | 60.6 | 154.9 | 59.1 | 94.1 | 45.9 | 15408 | 0.3 |
| large10k | 10233 | frontier c8 w32 stateful | 163.2 | 173.3 | 53.3 | 124.7 | 52.2 | 70.5 | 36.3 | 15408 | 0.9 |
| large10k | 10233 | frontier c4 w8 stateful | 182.8 | 186.2 | 57.4 | 139.6 | 55.9 | 81.8 | 41.0 | 15408 | 0.5 |
| large10k | 10233 | frontier c4 w16 stateful | 189.0 | 194.9 | 58.8 | 145.1 | 57.0 | 85.4 | 42.5 | 15408 | 1.1 |
| large10k | 10233 | frontier c4 w64 stateful | 191.3 | 200.3 | 58.8 | 147.1 | 56.8 | 85.8 | 44.0 | 15408 | 1.2 |
| large10k | 10233 | frontier c4 w128 stateful | 193.8 | 200.4 | 59.7 | 148.5 | 58.2 | 88.6 | 43.6 | 15408 | 0.7 |
| large100k | 102305 | default frontier c4 w32 stateful | 1540.5 | 1596.4 | 571.5 | 1210.2 | 497.8 | 636.6 | 322.7 | 153923 | 4.4 |
| large100k | 102305 | whole c4 w32 stateful | 1541.1 | 1600.0 | 524.2 | 1202.2 | 524.2 | 670.0 | 336.5 | 145492 | 497.4 |
| large100k | 102305 | frontier c4 w32 states-all | 2187.6 | 2202.8 | 613.9 | 1334.0 | 537.1 | 714.5 | 833.8 | 213042 | 6.7 |
| large100k | 102305 | frontier c4 w32 no-states | 1372.7 | 1412.9 | 617.0 | 1360.4 | 542.0 | 730.5 | 0.0 | 110737 | 5.5 |
| large100k | 102305 | frontier c4 w32 stateful +text | 2495.5 | 2678.4 | 596.7 | 1329.6 | 524.6 | 724.9 | 367.8 | 247403 | 5.6 |
| large100k | 102305 | frontier c1 w32 stateful | 2898.6 | 3060.1 | 817.3 | 2256.5 | 738.8 | 1342.1 | 619.2 | 153923 | 2.7 |
| large100k | 102305 | frontier c2 w32 stateful | 2049.2 | 2138.2 | 661.4 | 1592.4 | 589.0 | 932.2 | 451.8 | 153923 | 0.4 |
| large100k | 102305 | frontier c8 w32 stateful | 1600.8 | 1629.6 | 582.5 | 1257.8 | 505.0 | 663.9 | 333.9 | 153923 | 6.0 |
| large100k | 102305 | frontier c4 w8 stateful | 1883.6 | 1903.8 | 633.4 | 1451.3 | 556.3 | 807.8 | 413.7 | 153923 | 5.7 |
| large100k | 102305 | frontier c4 w16 stateful | 1898.9 | 1924.2 | 639.2 | 1469.9 | 567.1 | 819.2 | 417.3 | 153923 | 12.7 |
| large100k | 102305 | frontier c4 w64 stateful | 1855.5 | 1921.8 | 615.1 | 1438.9 | 540.9 | 825.8 | 402.3 | 153923 | 5.4 |
| large100k | 102305 | frontier c4 w128 stateful | 1889.1 | 2434.2 | 619.1 | 1470.3 | 545.5 | 844.0 | 407.9 | 153923 | 6.2 |

First block (viewport first): time until the top of the document is complete

| page | block nodes | subtrees left | p50 ms | p95 ms |
|---|---|---|---|---|
| large1k | 444 | 48 | 9.7 | 44.6 |
| large10k | 444 | 806 | 11.1 | 127.6 |
| large100k | 444 | 8380 | 66.6 | 1239.6 |

### chromium

| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large1k | 2093 | default frontier c4 w32 stateful | 26.6 | 28.0 | 9.5 | 22.8 | 9.1 | 13.3 | 3.2 | 2612 | 0.6 |
| large1k | 2093 | whole c4 w32 stateful | 26.5 | 28.5 | 9.4 | 23.5 | 9.4 | 13.4 | 3.1 | 2513 | 8.0 |
| large1k | 2093 | frontier c4 w32 states-all | 39.1 | 54.8 | 9.4 | 23.2 | 8.9 | 13.6 | 15.6 | 4286 | 0.5 |
| large1k | 2093 | frontier c4 w32 no-states | 24.5 | 24.9 | 9.8 | 24.0 | 9.4 | 14.3 | 0.0 | 2193 | 0.6 |
| large1k | 2093 | frontier c4 w32 stateful +text | 42.3 | 42.4 | 9.4 | 24.0 | 9.0 | 14.5 | 3.4 | 4539 | 0.5 |
| large10k | 21203 | default frontier c4 w32 stateful | 284.8 | 296.2 | 106.5 | 250.1 | 99.6 | 142.5 | 33.8 | 26378 | 0.8 |
| large10k | 21203 | whole c4 w32 stateful | 295.4 | 299.6 | 110.0 | 259.1 | 110.0 | 148.2 | 35.5 | 25521 | 97.7 |
| large10k | 21203 | frontier c4 w32 states-all | 437.5 | 438.5 | 107.1 | 259.2 | 100.9 | 150.7 | 174.2 | 43264 | 0.8 |
| large10k | 21203 | frontier c4 w32 no-states | 265.3 | 266.3 | 108.0 | 263.8 | 101.8 | 154.5 | 0.0 | 22061 | 0.8 |
| large10k | 21203 | frontier c4 w32 stateful +text | 471.7 | 474.0 | 109.1 | 268.7 | 102.3 | 160.1 | 39.0 | 45867 | 0.8 |

First block (viewport first): time until the top of the document is complete

| page | block nodes | subtrees left | p50 ms | p95 ms |
|---|---|---|---|---|
| large1k | 416 | 68 | 7.4 | 51.6 |
| large10k | 416 | 826 | 12.1 | 250.3 |

## Churn (WM-4 update latency, M-13 identity)

### spa-list-200 / chromium / use_dom_id=True

initial build 464.2 ms, first block 45.4 ms (410 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 5.4 | 33.2 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 6.3 | 34.4 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 4.8 | 34.3 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 2.5 | 29.5 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 4.4 | 31.6 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 4.5 | 31.9 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 25.7 | 550.9 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 0.67 |
| rewrite in place (unkeyed) | 692 | 22.2 | 95.5 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 4.4 | 87.9 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 0.50 |

### spa-list-200 / chromium / use_dom_id=False

initial build 464.7 ms, first block 42.1 ms (410 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 5.2 | 32.9 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 6.4 | 35.9 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 5.0 | 34.5 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 2.3 | 29.1 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 4.5 | 31.5 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 3.8 | 30.9 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 26.0 | 740.5 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 692 | 26.5 | 90.4 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 5.2 | 95.8 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### large10k / firefox / use_dom_id=True

initial build 1386.4 ms, first block 574.7 ms (444 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mut.rename 1 | 5 | 6.2 | 32.7 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1 | 2 | 6.3 | 38.5 | 3 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 100 | 101 | 11.3 | 66.9 | 300 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 100 | 499 | 27.0 | 87.3 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.removeCards 100 | 101 | 14.6 | 51.6 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1000 | 1001 | 86.2 | 374.2 | 3000 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 1000 | 3665 | 32.5 | 708.4 | 12336 | True | None | 7/7 | 0 | 0 | 0 | True | 0.00 |
| mut.route | 1858 | 49.2 | 199.4 | 2 | True | None | 1/7 | 0 | 6 | 0 | False | 1.00 |

### spa-list-200 / firefox / use_dom_id=True

initial build 531.5 ms, first block 114.8 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 5.1 | 32.9 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 8.6 | 41.5 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 7.7 | 40.3 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 5.0 | 32.1 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 5.3 | 40.1 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 19.6 | 116.9 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.52 |
| shuffle (keyed DOM moves) | 718 | 16.5 | 136.3 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 5.3 | 33.3 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 18.9 | 109.5 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 2308 | 14.1 | 241.6 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.49 |
| route swap (200 cards) | 15 | 21.1 | 157.7 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 0.00 |

### spa-list-200 / firefox / use_dom_id=False

initial build 542.9 ms, first block 130.8 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 5.9 | 35.1 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 8.4 | 44.8 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 5.1 | 41.9 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 2.2 | 28.9 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 3.6 | 35.9 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 41.4 | 222.1 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| shuffle (keyed DOM moves) | 718 | 41.0 | 132.4 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| remove 10 in the middle | 11 | 3.9 | 31.5 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 19.7 | 148.6 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 2308 | 32.4 | 155.5 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.97 |
| route swap (200 cards) | 15 | 11.2 | 127.0 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### spa-list-2000 / firefox / use_dom_id=True

initial build 1320.2 ms, first block 558.5 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 11.1 | 45.6 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 23.4 | 64.3 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 17.0 | 59.2 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 17.4 | 45.0 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 4.4 | 44.4 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 105.4 | 848.9 | 4082 | True | None | 1395/2045 | 650 | 0 | 0 | False | 0.33 |
| shuffle (keyed DOM moves) | 6110 | 107.8 | 902.5 | 4082 | True | None | 42/2045 | 2003 | 0 | 0 | False | 0.33 |
| remove 10 in the middle | 11 | 6.4 | 37.7 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 98.1 | 838.9 | 4062 | True | None | 1397/2035 | 638 | 0 | 0 | False | 0.33 |
| rewrite in place (unkeyed) | 20308 | 98.6 | 1634.9 | 8146 | True | None | 2035/2035 | 0 | 0 | 0 | True | 0.25 |
| route swap (200 cards) | 15 | 15.9 | 161.1 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 0.00 |

### spa-list-2000 / firefox / use_dom_id=False

initial build 1376.9 ms, first block 612.9 ms (403 nodes), initial mirror consistent: True

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 8.0 | 41.5 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 18.7 | 59.0 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 13.0 | 56.5 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 7.8 | 38.9 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 4.6 | 37.9 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 114.3 | 908.2 | 4082 | True | None | 1595/2045 | 450 | 0 | 0 | False | 0.67 |
| shuffle (keyed DOM moves) | 6110 | 121.6 | 908.9 | 4082 | True | None | 42/2045 | 2003 | 0 | 0 | False | 0.67 |
| remove 10 in the middle | 11 | 13.0 | 48.0 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 93.2 | 868.2 | 4062 | True | None | 1197/2035 | 838 | 0 | 0 | False | 0.67 |
| rewrite in place (unkeyed) | 20308 | 99.3 | 1686.9 | 8146 | True | None | 2035/2035 | 0 | 0 | 0 | True | 0.50 |
| route swap (200 cards) | 15 | 21.9 | 155.5 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 1.00 |

## Scaling (M-15)

### large100k: 102301 nodes, 8420 GetTree replies, 22.0 MB, 16 cores

| decode threads | p50 ms | speedup |
|---|---|---|
| 1 | 46.9 | 1.00 |
| 2 | 53.9 | 0.87 |
| 4 | 16.6 | 2.82 |
| 8 | 12.3 | 3.81 |
| 16 | 14.9 | 3.14 |

| P2P connections | fetch total p50 ms |
|---|---|
| 1 | 4741.2 |
| 2 | 1709.1 |
| 4 | 1583.9 |
| 8 | 1540.5 |
| 16 | 1531.0 |

### large10k: 10229 nodes, 846 GetTree replies, 2.2 MB, 16 cores

| decode threads | p50 ms | speedup |
|---|---|---|
| 1 | 4.8 | 1.00 |
| 2 | 2.4 | 1.98 |
| 4 | 1.8 | 2.71 |
| 8 | 1.6 | 3.11 |
| 16 | 2.5 | 1.95 |

| P2P connections | fetch total p50 ms |
|---|---|
| 1 | 518.6 |
| 2 | 360.3 |
| 4 | 333.2 |
| 8 | 327.6 |
| 16 | 327.0 |

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
| virt-tanstack-index | scrollTo row 0 (jump back) | 20->17 | 1 | 0 | 1 | 0 | 16 | 6 | 0 | 0 | children-added=16, children-removed=19 |
| virt-tanstack-slot | scrollBy +64 px (2 rows) | 17->19 | 17 | 16 | 16 | 0 | 0 | 54 | 12 | 0 | children-added=2 |
| virt-tanstack-slot | scrollBy +640 px (20 rows) | 19->20 | 1 | 0 | 0 | 0 | 14 | 48 | 15 | 0 | children-added=5, children-removed=4, name-changed=28 |
| virt-tanstack-slot | scrollTo row 500 | 20->20 | 1 | 0 | 0 | 0 | 17 | 57 | 12 | 0 | children-added=2, children-removed=2, name-changed=34 |
| virt-tanstack-slot | scrollBy -3200 px (back 100 rows) | 20->20 | 1 | 0 | 1 | 0 | 15 | 51 | 18 | 0 | children-added=4, children-removed=4, name-changed=30 |
| virt-tanstack-slot | scrollTo row 5000 (jump) | 20->20 | 1 | 0 | 1 | 0 | 14 | 48 | 18 | 0 | children-added=5, children-removed=5, name-changed=28 |
| virt-tanstack-slot | scrollTo row 0 (jump back) | 20->17 | 1 | 0 | 0 | 0 | 11 | 39 | 18 | 0 | children-added=5, children-removed=8, name-changed=22 |
| virt-vue-recycle | scrollBy +64 px (2 rows) | 19->21 | 19 | 19 | 19 | 0 | 0 | 64 | 12 | 0 | children-added=2 |
| virt-vue-recycle | scrollBy +640 px (20 rows) | 21->26 | 6 | 6 | 6 | 0 | 15 | 55 | 5 | 0 | children-added=20, children-removed=15, name-changed=15 |
| virt-vue-recycle | scrollTo row 500 | 26->52 | 26 | 26 | 17 | 0 | 0 | 85 | 39 | 0 | children-added=26 |
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

