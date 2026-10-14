# Summary of bench/results/web-prep/run4-churn

## Build (M-9 / WM-1)

## Churn (WM-4 update latency, M-13 identity)

### spa-list-200 / chromium / use_dom_id=True

initial build 464.3 ms, first block 45.4 ms (410 nodes), initial mirror consistent: True; build detail {'nodes': 616, 'warm_ms': 33.919, 'frontier_ms': 0.5826, 'get_tree_ms': 4.0692, 'roles_ms': 3.4461, 'states_ms': 2.8295, 'total_ms': 51.4699}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 4.6 | 33.0 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 7.1 | 37.3 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 4.0 | 34.1 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 1.8 | 28.8 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 3.5 | 30.2 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 5.2 | 32.3 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 24.8 | 515.0 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 0.67 |
| rewrite in place (unkeyed) | 692 | 24.1 | 87.9 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 5.1 | 89.8 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 0.50 |

### spa-list-200 / chromium / use_dom_id=False

initial build 474.4 ms, first block 57.7 ms (410 nodes), initial mirror consistent: True; build detail {'nodes': 616, 'warm_ms': 37.5829, 'frontier_ms': 0.6821, 'get_tree_ms': 6.0077, 'roles_ms': 3.5698, 'states_ms': 9.2517, 'total_ms': 63.763}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 1 | 5.2 | 33.0 | 3 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 20 | 6.7 | 37.4 | 60 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 20 | 5.3 | 35.4 | 60 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 2 | 2.4 | 29.3 | 1 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| rename 10 buttons | 10 | 3.9 | 31.0 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| shuffle (keyed DOM moves) | 0 | - | - | 0 | False | True | 245/245 | 0 | 0 | 0 | False | 0.00 |
| remove 10 in the middle | 10 | 4.2 | 31.4 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 57.4 | 664.1 | 693 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 692 | 19.7 | 75.0 | 709 | True | None | 5/235 | 230 | 0 | 230 | True | 0.33 |
| route swap (200 cards) | 12 | 4.1 | 84.1 | 1403 | False | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### large10k / firefox / use_dom_id=True

initial build 804.5 ms, first block 251.9 ms (444 nodes), initial mirror consistent: True; build detail {'nodes': 10233, 'warm_ms': 243.2529, 'frontier_ms': 0.9427, 'get_tree_ms': 2.7874, 'roles_ms': 3.2574, 'states_ms': 1.2943, 'total_ms': 391.3784}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| mut.rename 1 | 5 | 3.0 | 28.7 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1 | 2 | 4.4 | 32.9 | 3 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 100 | 101 | 7.9 | 45.4 | 300 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 100 | 499 | 6.4 | 47.8 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.removeCards 100 | 101 | 9.6 | 40.6 | 0 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.addCards 1000 | 1001 | 30.6 | 174.1 | 3000 | True | None | 7/7 | 0 | 0 | 0 | False | 0.00 |
| mut.rename 1000 | 3665 | 24.5 | 282.9 | 12336 | True | None | 7/7 | 0 | 0 | 0 | True | 0.00 |
| mut.route | 1858 | 31.7 | 117.2 | 2 | True | None | 1/7 | 0 | 6 | 0 | False | 1.00 |

### spa-list-200 / firefox / use_dom_id=True

initial build 486.0 ms, first block 76.6 ms (403 nodes), initial mirror consistent: True; build detail {'nodes': 411, 'warm_ms': 61.3288, 'frontier_ms': 0.8671, 'get_tree_ms': 6.8544, 'roles_ms': 2.7667, 'states_ms': 3.7225, 'total_ms': 77.2833}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 1.6 | 29.0 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 2.7 | 31.1 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 2.9 | 32.4 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 1.5 | 28.5 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 1.7 | 30.2 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 10.9 | 70.7 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.52 |
| shuffle (keyed DOM moves) | 718 | 10.5 | 86.7 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 1.1 | 28.6 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 11.8 | 69.0 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 2308 | 7.2 | 77.5 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.97 |
| route swap (200 cards) | 15 | 7.7 | 75.9 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 0.00 |

### spa-list-200 / firefox / use_dom_id=False

initial build 486.3 ms, first block 73.3 ms (403 nodes), initial mirror consistent: True; build detail {'nodes': 411, 'warm_ms': 56.803, 'frontier_ms': 0.5487, 'get_tree_ms': 8.2841, 'roles_ms': 3.779, 'states_ms': 3.0508, 'total_ms': 73.7939}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 4.3 | 31.9 | 2 | True | None | 204/204 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 3.0 | 31.8 | 40 | True | None | 205/205 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 4.6 | 34.1 | 40 | True | None | 225/225 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 3.8 | 32.3 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 5.2 | 35.9 | 0 | True | None | 245/245 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 484 | 13.4 | 67.1 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| shuffle (keyed DOM moves) | 718 | 12.4 | 76.8 | 482 | True | None | 245/245 | 0 | 0 | 0 | False | 1.00 |
| remove 10 in the middle | 11 | 4.2 | 30.8 | 0 | True | None | 235/245 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 464 | 14.2 | 77.9 | 462 | True | None | 235/235 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 2308 | 24.0 | 110.8 | 473 | True | None | 235/235 | 0 | 0 | 0 | True | 0.97 |
| route swap (200 cards) | 15 | 8.2 | 76.7 | 802 | True | None | 2/235 | 0 | 233 | 0 | False | 1.00 |

### spa-list-2000 / firefox / use_dom_id=True

initial build 907.3 ms, first block 332.6 ms (403 nodes), initial mirror consistent: True; build detail {'nodes': 4011, 'warm_ms': 309.3139, 'frontier_ms': 2.2305, 'get_tree_ms': 14.0443, 'roles_ms': 2.8098, 'states_ms': 3.4381, 'total_ms': 486.2626}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 5.4 | 36.1 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 6.4 | 39.3 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 4.6 | 37.1 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 3.0 | 29.4 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 3.2 | 32.2 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 46.5 | 381.3 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.50 |
| shuffle (keyed DOM moves) | 6110 | 46.8 | 409.4 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.50 |
| remove 10 in the middle | 11 | 5.1 | 34.2 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 47.7 | 385.8 | 4062 | True | None | 2035/2035 | 0 | 0 | 0 | False | 0.50 |
| rewrite in place (unkeyed) | 20308 | 41.1 | 535.6 | 4073 | True | None | 2035/2035 | 0 | 0 | 0 | True | 1.00 |
| route swap (200 cards) | 15 | 10.8 | 85.6 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 0.00 |

### spa-list-2000 / firefox / use_dom_id=False

initial build 904.1 ms, first block 325.9 ms (403 nodes), initial mirror consistent: True; build detail {'nodes': 4011, 'warm_ms': 300.7659, 'frontier_ms': 1.9364, 'get_tree_ms': 14.6738, 'roles_ms': 3.5711, 'states_ms': 4.2504, 'total_ms': 490.3114}

| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| prepend 1 | 2 | 4.9 | 34.9 | 2 | True | None | 2004/2004 | 0 | 0 | 0 | False | 0.00 |
| prepend 20 | 21 | 5.2 | 39.6 | 40 | True | None | 2005/2005 | 0 | 0 | 0 | False | 0.00 |
| append 20 | 21 | 3.0 | 37.1 | 40 | True | None | 2025/2025 | 0 | 0 | 0 | False | 0.00 |
| edit item 5 | 5 | 0.9 | 28.8 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| rename 10 buttons | 60 | 3.0 | 33.2 | 0 | True | None | 2045/2045 | 0 | 0 | 0 | False | 0.00 |
| reverse (keyed DOM moves) | 4084 | 56.6 | 404.6 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 1.00 |
| shuffle (keyed DOM moves) | 6110 | 45.3 | 412.2 | 4082 | True | None | 2045/2045 | 0 | 0 | 0 | False | 1.00 |
| remove 10 in the middle | 11 | 5.9 | 35.8 | 0 | True | None | 2035/2045 | 0 | 10 | 0 | False | 0.00 |
| recreate all (key reset) | 4064 | 65.2 | 393.6 | 4062 | True | None | 2035/2035 | 0 | 0 | 0 | False | 1.00 |
| rewrite in place (unkeyed) | 20308 | 63.2 | 552.7 | 4073 | True | None | 2035/2035 | 0 | 0 | 0 | True | 1.00 |
| route swap (200 cards) | 15 | 13.4 | 93.5 | 802 | True | None | 2/2035 | 0 | 2033 | 0 | False | 1.00 |

## Scaling (M-15)

## Research (TBD-WEB-1/2/3)

