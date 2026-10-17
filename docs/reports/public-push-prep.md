# Public push preparation (2026-10-17)

Scope: what to exclude or clean before the first public push to github.com/Ravino/gorca. Nothing
was deleted, no remote was added and nothing was pushed. Findings are from scanning the tracked
files of this branch (`git ls-files`, 5,900+ files, about 41 MiB packed) and the commit metadata.

## 1. Must be excluded from the published history

| Path or item | Why | Evidence |
|---|---|---|
| `.claude/` (147 tracked files: `agent-memory/`, `agents/`, `skills/`, `settings.json`) | Internal agent prompts and memory; the owner said it will not be published | `git ls-files .claude` |
| `CLAUDE.md` (root) | Orchestration and agent instructions, live-session rules, chat-language notes. Decide: drop, or replace with a short `CONTRIBUTING.md` carrying the build and gate rules | file content |
| Commit author and committer email `evgeniy@nekrasov.pw` on every commit | Becomes public with the history. Keep only if the owner accepts it; otherwise rewrite with a mailmap to a GitHub no-reply address | `git log --format='%ae %ce'`: a single identity |
| `bench/results/` (5,773 tracked files, about 297 MB on disk) | Raw benchmark output with host paths (`<repo>/...` in 1,900+ files) and the author's login (`/home/user` in `bench/results/m9`). Too large and noisy for a public repo | `du -sh bench/results`; scan below |
| `docs/reports/ext-gnome-shell-wasm-data/` (1.1 MB logs) and `docs/reports/m4b-shell-probe-logs/` (0.7 MB) | Raw logs; the first contains the login name (`gates/test.log`) and many `<repo>` paths | scan |
| History blobs of the `.toolchain` symlink | Committed and untracked twice (`52ffc67f`, `197bade2`); harmless symlink, but the incident shows it must never enter the public repo | `git log --all -- .toolchain` |
| Untracked, never to be added: `gorca.err`, `target/`, `.toolchain`, `.venv/`, `refs/` | Logs and local toolchains. `gorca.err` is not in this checkout; check the main checkout before copying anything | `.gitignore` |

Keep `bench/results/` as a curated subset if numbers should be public: the final Phase 2
acceptance summary and the few result files cited from `docs/decisions.md`.

## 2. Files with personal data that need an edit, not removal

| File | Problem | Suggested fix |
|---|---|---|
| `docs/reports/2026-10-12-voicego-p0-fixes.md` | Full name, login and `tutamail` address | Replace with the project name or remove the lines |
| `extensions/ext-vte/src/prompt.rs:193,201` | Test strings `"[sudo] password for evgeniy: "` (English and Russian) | Change to a neutral user name (`alice`); keep the tests passing (`cargo test -p ext-vte`) |
| `contrib/systemd/gorca.service` | `ExecStart` and `Documentation` use `<repo>/...`; README tells users to edit it | Use `%h`-relative or `/usr/local/bin/gorca` paths and a template note |
| `tools/platform-probes/adr6/*.py` (15 files), `tests/web/spa`, `tests/web/m7`, `research/web-graph` | Hard-coded `<repo>/...` paths | Use `os.path.dirname(__file__)`; or drop `tools/platform-probes/` if not needed |
| `docs/reports/*.md` (7 files, listed by `grep -rl <repo> docs`), `docs/plan/phase-3.md` | Host paths in prose | Optional: sed `<repo>/` to a repo-relative path |
| `crates/gorca-core/src/terminal/*`, `crates/gorca-daemon/src/*` | `/home/u`, `/home/me` only (test fixtures, not personal) | None |

Secret scan (tokens, keys, `password=`, GitHub and AWS key shapes): no real secret found. The only
matches are test fixtures (`secret = "Zq7-tr0ub4dor&3"` in `crates/gorca-bench/src/e2e/m3.rs`,
`m6.rs`) used to check that secrets are redacted, and the identifier `CancellationToken`.

## 3. Internal process text to review

46 files under `docs/`, `tools/` and `tests/` mention the orchestrator, agent roles
or `worktree-agent-*` branch names (for example `docs/decisions.md`, ADR-0001, bake-off
judgements, `docs/plan/phase-3.md`). They are accurate project history but read as internal.
Options: publish as is (honest, noisy); or run a light `sed` to replace role names with "maintainer"
and drop branch names. `docs/decisions.md` row 2026-10-17 about the `.toolchain` incident is
internal and can be dropped.

## 4. Other pre-publication items (from `licence-provenance.md`)

- `tests/web/m8/pages/wiki.html`: two sentences follow Wikipedia (CC BY-SA 4.0). Reword or
  add an attribution comment; credited in `THIRD-PARTY.md` for now.
- `repository` in `Cargo.toml` is set to `https://github.com/Ravino/gorca`.
- Trademarks: preset names `nvda` and `orca`, the Orca bus-name impersonation and the project
  name. Describe presets as "NVDA-style" and "Orca-style" in user text.
- `LICENSE-EXCEPTION` needs legal review. `crates/gorca-bench*`, `extensions/*` and
  `bench/wasm-hook/wit/quirk.wit` are GPL-3.0-or-later like the core; only `crates/gorca-ext-api`
  is MIT OR Apache-2.0. Say if the first-party extensions should be permissive too.

## 5. Recommended filter plan

Keep the local repository untouched. Work on a separate mirror clone.

```sh
git clone --mirror /path/to/local/gorca /tmp/gorca-public.git      # separate clone, then:
cd /tmp/gorca-public.git

# 1. Paths to drop from all history (one path per line in paths.txt)
cat > paths.txt <<'EOF'
.claude
CLAUDE.md
bench/results
docs/reports/ext-gnome-shell-wasm-data
docs/reports/m4b-shell-probe-logs
.toolchain
gorca.err
EOF
git filter-repo --invert-paths --paths-from-file paths.txt

# 2. Rewrite the author identity, if wanted (mailmap: new <new-email> <old-email>)
git filter-repo --mailmap mailmap.txt

# 3. Replace text in remaining blobs (expressions.txt, one "old==>new" per line):
#      evgeniy.nekrasov==>user
#      <repo>/==>
#      evgeniy@nekrasov.pw==>noreply@users.noreply.github.com
#      evgeniy==>alice
git filter-repo --replace-text expressions.txt

# 4. Verify before any push
git log --format='%an <%ae> %cn <%ce>' | sort -u
git grep -nIE 'evgeniy|nekrasov|tutamail|/home/[a-z]+\.|<repo>' $(git rev-list --all) | head
```

After the rewrite: run the tests in a fresh checkout of the filtered clone, then add the remote
and push there only. All commit hashes change; references to hashes in `docs/` (for example
`98cb234` in `docs/decisions.md`) will point to nothing, so either keep the references as
history notes or drop them. The commit message of the squashed M4b merge already notes that the
original history contained the login name and 9 MB of probe logs, so a rewrite of history is
consistent with earlier practice.

If a rewrite of history is not wanted, the alternative is a single squashed initial commit of the
cleaned tree. It is simpler and removes all history leaks at once, but it loses the log.

## 6. Done in this change

LICENSE, LICENSE-MIT, LICENSE-APACHE (also in `crates/gorca-ext-api/`), LICENSE-EXCEPTION (draft),
SPDX headers on 441 Rust and WIT files (`GPL-3.0-or-later`; the SDK crate `MIT OR Apache-2.0`),
`license` and `repository` in `Cargo.toml`, symbol table headers, README, THIRD-PARTY.md,
`about.toml`, `about.hbs`, CHANGELOG.md. Not covered by SPDX headers: Python, shell, JavaScript and
TOML files; Cargo metadata and the README carry the licence for them. `cargo metadata`,
`cargo fmt --check` and `cargo test -p gorca-speech -p gorca-ext-api` pass.
