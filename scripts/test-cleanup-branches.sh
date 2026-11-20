#!/usr/bin/env bash
# Regression fixture for scripts/cleanup-branches.sh: it must finish when
# `git worktree list --porcelain` keeps writing after the primary record.
#
# The fixture is a throwaway repository. A `git` shim on PATH pads the porcelain
# listing past any pipe buffer with lines the script ignores; everything else
# runs the real git, so removal and branch deletion act only on the fixture.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/cleanup-branches.sh"
# Physical path: the script compares `pwd -P` results.
TMP="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$TMP"' EXIT

fail() {
  printf 'test-cleanup-branches: FAIL: %s\n' "$*" >&2
  exit 1
}

REAL_GIT="$(command -v git)"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid

mkdir "$TMP/bin"
cat >"$TMP/bin/git" <<SHIM
#!/usr/bin/env bash
# Emit the real listing, then 1 MiB of unrecognised lines, so the producer
# outlives any early-exiting reader regardless of scheduling.
if [[ "\${1:-}" == worktree && "\${2:-}" == list && "\${3:-}" == --porcelain ]]; then
  "$REAL_GIT" "\$@"
  head -c 1048576 /dev/zero | tr '\\0' 'x' | fold -w 80
  echo
  exit 0
fi
exec "$REAL_GIT" "\$@"
SHIM
chmod +x "$TMP/bin/git"

REPO="$TMP/primary"
"$REAL_GIT" init -q -b main "$REPO"
(
  cd "$REPO"
  "$REAL_GIT" commit -q --allow-empty -m init
  "$REAL_GIT" branch agent-main
  "$REAL_GIT" branch stale
  "$REAL_GIT" branch other
  "$REAL_GIT" worktree add -q -b invoking "$TMP/invoking"
  "$REAL_GIT" worktree add -q -b doomed "$TMP/doomed"
)

# Invoke from the linked tree; the primary is neither the invoking tree nor main-named by accident.
status=0
out="$(cd "$TMP/invoking" && PATH="$TMP/bin:$PATH" "$SCRIPT" 2>&1)" || status=$?
[[ "$status" -eq 0 ]] || fail "script exited $status on a long worktree listing: $out"
[[ "$out" == *"Cleanup complete."* ]] || fail "script did not reach completion: $out"

[[ -d "$REPO/.git" ]] || fail "primary checkout was removed"
[[ -d "$TMP/invoking" ]] || fail "invoking checkout was removed"
[[ ! -e "$TMP/doomed" ]] || fail "other linked worktree was not removed"

branches="$("$REAL_GIT" -C "$REPO" for-each-ref --format '%(refname:short)' refs/heads/ | sort | tr '\n' ' ')"
[[ "$branches" == "agent-main invoking main " ]] || fail "unexpected surviving branches: $branches"

printf 'test-cleanup-branches: ok\n'
