#!/usr/bin/env bash
# Make a filtered copy of the repository for the first public push, and verify it.
#
#   tools/release/public-export.sh [--src=DIR] [--ref=REF] [--out=DIR] [--keep]
#
# What it does, always on a FRESH CLONE in a scratch directory (the source repository is only read):
#   1. clones SRC (default: the repository this script is in) at REF (default: the checked-out branch);
#   2. runs git filter-repo on the clone to
#        - drop .claude/, CLAUDE.md and .toolchain from all history,
#        - rewrite author/committer/tagger email via a mailmap,
#        - scrub host paths and the login name from every blob (replace-text);
#   3. verifies the result and prints a summary (exit 1 if a check fails).
# It never adds a remote and never pushes. After a pass, review the clone, then push from it by hand.
# git filter-repo is never run on SRC; the script refuses OUT inside SRC.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
src=$(cd "$here/../.." && pwd)
ref=""
out=""
keep=0
NEW_IDENTITY_NAME="Evgeniy Nekrasov"
NEW_EMAIL="evgeniy@nekrasov.pw"
# Built from parts so that a text scrub of the repository cannot rewrite it.
OLD_EMAIL="evgeniynekrasov@""tutamail"".com"

for a in "$@"; do
  case "$a" in
    --src=*) src=$(cd "${a#--src=}" && pwd) ;;
    --ref=*) ref=${a#--ref=} ;;
    --out=*) out=${a#--out=} ;;
    --keep) keep=1 ;;
    -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

command -v git-filter-repo >/dev/null 2>&1 || {
  echo "git-filter-repo is not installed: sudo apt-get install --no-install-recommends git-filter-repo" >&2
  exit 2
}
[ -n "$ref" ] || ref=$(git -C "$src" rev-parse --abbrev-ref HEAD)

if [ -z "$out" ]; then
  out=$(mktemp -d "${TMPDIR:-/tmp}/gorca-public-export.XXXXXX")
else
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
fi
case "$out/" in
  "$src"/*) echo "refusing: --out must not be inside the source repository" >&2; exit 2 ;;
esac
[ -z "$(ls -A "$out")" ] || { echo "refusing: $out is not empty" >&2; exit 2; }

clone="$out/gorca-public"
echo "== clone $src ($ref) -> $clone"
git clone --quiet --no-local --no-hardlinks --branch "$ref" "$src" "$clone" 2>&1 | tail -2 || true
[ -d "$clone/.git" ] || { echo "clone failed" >&2; exit 2; }
cd "$clone"
git remote remove origin   # the clone must not point back at the source

cat > "$out/mailmap.txt" <<EOF
$NEW_IDENTITY_NAME <$NEW_EMAIL> <$OLD_EMAIL>
$NEW_IDENTITY_NAME <$NEW_EMAIL> $NEW_IDENTITY_NAME <$OLD_EMAIL>
EOF
cat > "$out/paths.txt" <<'EOF'
.claude
CLAUDE.md
.toolchain
gorca.err
EOF
cat > "$out/expressions.txt" <<EOF
regex:<repo>/\.claude/worktrees/[A-Za-z0-9_-]+==><repo>
regex:<repo>==><repo>
regex:/tmp/claude-[0-9]+/(-www-a11y/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/scratchpad/?)?==>/tmp/
regex:/home/evgeniy\.nekrasov==>/home/user
literal:$OLD_EMAIL==>$NEW_EMAIL
EOF

echo "== git filter-repo"
git filter-repo --force --quiet \
  --invert-paths --paths-from-file "$out/paths.txt" \
  --mailmap "$out/mailmap.txt" \
  --replace-text "$out/expressions.txt" 2>&1 | tail -3
git remote remove origin 2>/dev/null || true

# ---------------------------------------------------------------- verification
fail=0
check() { # check NAME COUNT [DETAIL]
  if [ "$2" -eq 0 ]; then printf 'PASS  %s\n' "$1"; else printf 'FAIL  %s (%s)\n' "$1" "$2"; [ -n "${3:-}" ] && printf '%s\n' "$3" | head -8 | sed 's/^/        /'; fail=1; fi
}
echo "== verification (all refs, all history unless noted)"

n=$(git log --all --name-only --format= | grep -cE '^(\.claude/|CLAUDE\.md$|\.toolchain)' || true)
check "no .claude/, CLAUDE.md or .toolchain in any commit" "$n"

ids=$(git log --all --format='%an <%ae>%n%cn <%ce>' | sort -u)
n=$(printf '%s\n' "$ids" | grep -vc "^$NEW_IDENTITY_NAME <$NEW_EMAIL>$" || true)
check "every author/committer is $NEW_IDENTITY_NAME <$NEW_EMAIL>" "$n" "$ids"

n=$(git log --all --format='%an %ae %cn %ce %B' | grep -ciE 'tuta[m]ail\.com' || true)
check "no old email in commit metadata or messages" "$n"
n=$(git log --all -G'tuta[m]ail\.com' --oneline | wc -l)
check "no old email in any file content (history)" "$n"

hist_pat='/home/[a-z][a-z0-9_]*\.[a-z]|<repo>|/tmp/claude-[0-9]'
n=$(git log --all -G"$hist_pat" --oneline | wc -l)
check "no host paths (/home/<login>., <repo>, session scratch dirs) in any file content (history)" "$n" "$(git log --all -G"$hist_pat" --oneline | head -3)"

login_hits=$(git grep -IlE 'evgeniy\.nekrasov|evgeniy[^@.]' HEAD -- . ':!docs/decisions.md' ':!docs/reports/public-push-prep.md' ':!CONTRIBUTING.md' ':!tools/release/public-export.sh' || true)
n=$(printf '%s' "$login_hits" | grep -c . || true)
check "no login name in the tip tree (outside the decision log)" "$n" "$login_hits"

n=$(git log --all --format=%B | grep -ciE '^co-authored-by|generated with|claude code' || true)
check "no AI attribution in commit messages" "$n"

secret_pat='-----BEGIN [A-Z ]*PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{35}|sk-[A-Za-z0-9]{32,}'
n=$(git log --all -G"$secret_pat" --oneline | wc -l)
check "no private keys or well-known token shapes (history)" "$n" "$(git log --all -G"$secret_pat" --oneline | head -3)"

n=$(git remote | wc -l)
check "clone has no remotes" "$n"

# Soft check: password-like assignments with long literals. The repository has test fixtures for
# secret redaction (crates/gorca-bench), so this is listed, not failed.
soft=$(git grep -InE "(password|passwd|secret|api[_-]?key|token)[A-Za-z_]*[[:space:]]*[:=][[:space:]]*[\"'][A-Za-z0-9+/_&-]{12,}[\"']" HEAD -- . ':!bench/results' ':!docs' | cut -c1-140 | head -10 || true)
echo "INFO  password-like literals in the tip tree (review by hand; redaction-test fixtures are expected):"
printf '%s\n' "${soft:-none}" | sed 's/^/        /'

echo "== summary"
echo "commits: $(git rev-list --all --count)   tip: $(git rev-parse --short HEAD)   files at tip: $(git ls-files | wc -l)"
echo "size: $(du -sh .git | cut -f1) in .git"
echo "licence files at tip: $(ls LICENSE* CONTRIBUTING.md README.md CHANGELOG.md THIRD-PARTY.md 2>/dev/null | tr '\n' ' ')"
echo "clone: $clone   (no remote configured; nothing was pushed)"
if [ "$fail" -ne 0 ]; then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
if [ "$keep" -eq 0 ]; then echo "(the clone stays in $out; delete it when done)"; fi
