# gorca-doc MB results

`cargo bench -p gorca-doc --bench doc_mb -- --out <absolute dir>` (cargo runs benches with the crate
directory as cwd, so pass an absolute path).

- `2026-10-14-run1-grow-bug`: kept as recorded. Harness bug: the update phase inserted 720 nodes per
  batch and never removed them, so the document grew during the measurement (1k to 171k nodes). Query
  numbers in it are valid but taken at load 12; update and reader numbers are not comparable.
- `2026-10-14-run2` .. `run4`: the fixed harness (previous batch's insertions are removed), three
  consecutive runs on a loaded host (loadavg 7-8, governor powersave).

Report: `docs/reports/2026-10-14-gorca-doc-core.md`.
