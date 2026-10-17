# Public push preparation (2026-10-17)

Status after the owner decisions of 2026-10-17 (`docs/decisions.md`). Nothing was pushed and no
remote was added.

## What is published, what is not

| Item | Decision |
|---|---|
| `.claude/`, `CLAUDE.md`, `.toolchain`, `gorca.err` | Not published; dropped from all history by the export script |
| `bench/results/`, `docs/reports/` | Published after tidy-up: index pages (`bench/results/README.md`, `docs/reports/README.md`), conditions labelled, host paths and login scrubbed |
| Commit identity | Rewritten from the old tutamail address to `Evgeniy Nekrasov <evgeniy@nekrasov.pw>` by a mailmap in the export |
| Licence | Core and built-in extensions GPL-3.0-or-later; SDK (`gorca-ext-api`, `gorca-i18n`, WIT files) MIT OR Apache-2.0; section 7 permission in `LICENSE-EXCEPTION` (draft, needs legal review) |
| Contributions | DCO (`CONTRIBUTING.md`), no CLA |

## Done in the tree

- Licence files, SPDX lines on every `.rs` and `.wit` file, `license`/`repository` in the
  manifests (`gorca-ext-api` and `gorca-i18n` carry their own `LICENSE-MIT` and `LICENSE-APACHE`).
  `cargo tree -p gorca-ext-api` shows one workspace dependency: `gorca-i18n`, which in turn has
  only `thiserror`.
- Host paths (`<repo>`, agent worktree paths, scratch directories) and the login name replaced
  by `<repo>` / `user` in 2,600 tracked text files; the code that used them now computes paths from
  the script location (`tools/platform-probes`, `tests/web/m7`, `tests/web/spa`) or uses `%h`
  (`contrib/systemd/gorca.service`). Test strings in `ext-vte` use `alice`.
- The two sentences of `tests/web/m8/pages/wiki.html` that followed Wikipedia are reworded.
- Name, login and old email removed from `docs/reports/2026-10-12-voicego-p0-fixes.md`.

## Still open

- 46 files under `docs/`, `tools/` and `tests/` mention the orchestrator, agent roles or
  `worktree-agent-*` branch names. They are accurate project history; publish as is or reword.
- `docs/decisions.md` rows that cite commit hashes (for example `98cb234`) point to nothing after
  the history rewrite; the hashes change.
- Trademarks: preset names `nvda` and `orca`, the Orca bus-name impersonation and the project name.
  Use "NVDA-style" and "Orca-style" in user text.
- `bench/results/` is about 300 MB on disk (31 MB packed in the export); consider a smaller subset if
  the repository size matters.

## Export

```sh
tools/release/public-export.sh [--src=DIR] [--ref=REF] [--out=DIR]
```

The script clones the repository into a scratch directory, runs `git filter-repo` on the clone
only (dropping the paths above, applying the mailmap and a text replacement of host paths, login and
old email in all history), verifies the result and prints a summary. It adds no remote and does not
push. Run `git filter-repo` only on such a clone, never on the working repository. Dry run against
`main` on 2026-10-17: all history checks passed; the only failure was the `evgeniy` test string
in `extensions/ext-vte/src/prompt.rs`, which is fixed on the preparation branch (which passes).
