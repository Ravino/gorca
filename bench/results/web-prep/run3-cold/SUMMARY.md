# Summary of bench/results/web-prep/run3-cold

## Build (M-9 / WM-1)

### firefox

| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large1k | 1009 | default frontier c4 w32 stateful | 15.5 | 21.4 | 5.1 | 12.1 | 4.8 | 6.9 | 3.1 | 1528 | 0.3 |
| large1k | 1009 | whole c4 w32 stateful | 14.3 | 17.4 | 4.1 | 11.1 | 4.1 | 7.0 | 3.2 | 1429 | 3.8 |
| large1k | 1009 | frontier c4 w32 states-all | 21.1 | 23.8 | 5.2 | 12.8 | 4.9 | 7.5 | 8.0 | 2118 | 0.3 |
| large1k | 1009 | frontier c4 w32 no-states | 13.1 | 20.6 | 5.4 | 12.6 | 5.0 | 7.5 | 0.0 | 1109 | 0.3 |
| large1k | 1009 | frontier c4 w32 stateful +text | 26.2 | 38.4 | 5.5 | 13.6 | 5.2 | 8.0 | 3.8 | 2454 | 0.3 |
| large10k | 10233 | default frontier c4 w32 stateful | 160.2 | 167.7 | 52.4 | 124.0 | 51.0 | 71.7 | 33.5 | 15408 | 0.5 |
| large10k | 10233 | whole c4 w32 stateful | 149.7 | 162.0 | 44.5 | 114.5 | 44.5 | 68.9 | 33.8 | 14551 | 42.0 |
| large10k | 10233 | frontier c4 w32 states-all | 206.3 | 219.0 | 53.5 | 124.2 | 52.2 | 70.7 | 79.3 | 21324 | 0.5 |
| large10k | 10233 | frontier c4 w32 no-states | 131.7 | 140.3 | 53.8 | 130.6 | 52.6 | 74.5 | 0.0 | 11091 | 0.6 |
| large10k | 10233 | frontier c4 w32 stateful +text | 249.6 | 253.2 | 54.8 | 129.0 | 53.2 | 73.7 | 36.4 | 24760 | 0.5 |
| large100k | 102305 | default frontier c4 w32 stateful | 1602.7 | 1639.0 | 578.7 | 1255.4 | 505.3 | 672.5 | 337.7 | 153923 | 3.1 |
| large100k | 102305 | whole c4 w32 stateful | 1584.0 | 1642.5 | 516.6 | 1226.8 | 516.6 | 705.5 | 357.2 | 145492 | 490.2 |
| large100k | 102305 | frontier c4 w32 states-all | 2164.2 | 2208.6 | 598.6 | 1333.9 | 523.1 | 728.0 | 825.3 | 213042 | 4.5 |
| large100k | 102305 | frontier c4 w32 no-states | 1348.6 | 1388.4 | 604.7 | 1339.3 | 530.5 | 735.2 | 0.0 | 110737 | 6.0 |
| large100k | 102305 | frontier c4 w32 stateful +text | 2538.8 | 2599.8 | 599.3 | 1346.1 | 522.0 | 748.6 | 371.2 | 247403 | 1.5 |

First block (viewport first): time until the top of the document is complete

| page | block nodes | subtrees left | p50 ms | p95 ms |
|---|---|---|---|---|
| large1k | 444 | 48 | 10.2 | 48.8 |
| large10k | 444 | 806 | 9.3 | 125.1 |
| large100k | 444 | 8380 | 80.6 | 1123.5 |

Cold first block (fresh connection, cache domains not yet active; one sample per launch)

| page | total ms | warm-up ms | frontier ms | GetTree ms | block nodes |
|---|---|---|---|---|---|
| large1k | 70.8 | 59.5 | 0.3 | 3.7 | 444 |
| large10k | 281.6 | 272.0 | 1.4 | 2.9 | 444 |
| large100k | 2458.6 | 2377.8 | 71.7 | 3.4 | 444 |

## Churn (WM-4 update latency, M-13 identity)

### large10k / firefox / use_dom_id=True

initial build 826.7 ms, first block 273.8 ms (444 nodes), initial mirror consistent: True; build detail {'nodes': 10233, 'warm_ms': 261.4713, 'frontier_ms': 1.0036, 'get_tree_ms': 2.5723, 'roles_ms': 6.8241, 'states_ms': 1.5379, 'total_ms': 411.2117}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mut.rename 1 | 5 | 2.8 | 29.4 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1 | 2 | 3.8 | 32.4 | 3 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 100 | 101 | 7.8 | 45.2 | 300 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 100 | 499 | 13.5 | 53.1 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.removeCards 100 | 101 | 9.2 | 40.4 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1000 | 1001 | 28.8 | 164.4 | 3000 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 1000 | 3665 | 28.5 | 285.6 | 12336 | True | None | 7/7 | 0 | 0 | 0 | True | 0.00 |
| mut.route | 1858 | 31.3 | 115.5 | 2 | True | None | 1/7 | 0 | 6 | 0 | False | 1.00 |

### spa-list-200 / firefox / use_dom_id=True

initial build 463.1 ms, first block 49.2 ms (403 nodes), initial mirror consistent: True; build detail {'nodes': 411, 'warm_ms': 37.7334, 'frontier_ms': 0.3613, 'get_tree_ms': 5.457, 'roles_ms': 2.7333, 'states_ms': 2.5057, 'total_ms': 49.85}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 2.2 | 29.5 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 3.5 | 32.9 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 2.8 | 32.2 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 0.9 | 29.5 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 2.2 | 31.4 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 8.6 | 69.3 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.52 |
| shuffle (keyed DOM moves) | 718 | 8.1 | 66.6 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 2.5 | 29.5 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 9.6 | 69.7 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 2308 | 6.8 | 95.4 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.49 |
| route swap (200 cards) | 15 | 7.0 | 68.3 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 0.00 |

## Scaling (M-15)

## Research (TBD-WEB-1/2/3)

