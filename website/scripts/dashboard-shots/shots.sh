#!/bin/sh
# Regenerate the dashboard screenshots in website/src/assets/dashboard/ from a
# throwaway demo workspace, so no real task data reaches the docs.
#
#   website/scripts/dashboard-shots/shots.sh [--with-run]
#
# --with-run ships one task through a real agent first, for the run-detail
# shot. It needs a signed-in agent CLI and takes a few minutes.
# See README.md for the Playwright setup.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$HERE/../../src/assets/dashboard}
PORT=${PORT:-7899}
WITH_RUN=${1:-}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/orbit-shots.XXXXXX")
ROOT=$WORK/home/.orbit
REPO=$WORK/notes-cli
export ORBIT_ACTOR=human:you
orbit() { command orbit --root "$ROOT" "$@"; }

SERVER=
cleanup() {
  [ -n "$SERVER" ] && kill "$SERVER" 2>/dev/null || true
  [ -n "${KEEP:-}" ] || rm -rf "$WORK"
}
trap cleanup EXIT

# A small sample project.
mkdir -p "$REPO/notes"
cd "$REPO"
printf '# notes-cli\n\nA small command-line notebook: add, list, and search plain-text notes.\n' > README.md
printf '.orbit/\n__pycache__/\n' > .gitignore
printf '"""A small command-line notebook."""\n' > notes/__init__.py
cat > notes/__main__.py <<'EOF'
import sys

from notes.store import NoteStore


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    store = NoteStore()
    if not argv:
        print("usage: notes add|list|search ...")
        return 2
    command, *rest = argv
    if command == "add":
        store.add(" ".join(rest))
    elif command == "list":
        for i, note in enumerate(store.all(), 1):
            print(f"{i}. {note}")
    elif command == "search":
        for note in store.search(" ".join(rest)):
            print(note)
    else:
        print(f"unknown command: {command}")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
EOF
cat > notes/store.py <<'EOF'
from pathlib import Path

DEFAULT_PATH = Path.home() / ".notes.txt"


class NoteStore:
    def __init__(self, path=DEFAULT_PATH):
        self.path = Path(path)

    def all(self):
        if not self.path.exists():
            return []
        return [line for line in self.path.read_text().splitlines() if line]

    def add(self, text):
        with self.path.open("a") as f:
            f.write(text + "\n")

    def search(self, term):
        return [note for note in self.all() if term.lower() in note.lower()]
EOF
git init -q -b main
git add -A
git -c user.name=demo -c user.email=demo@example.com commit -q -m "notes-cli: add, list, search"

orbit init --non-interactive --machine-name laptop --task-prefix NOTE >/dev/null
orbit workspace init --ship-mode local --base-branch main --task-id-start 41 >/dev/null

# Four approved tasks in the backlog, two awaiting approval.
add() { orbit task add --workspace . "$@"; }
V=$(add --title "Add a --version flag to the notes CLI" \
  --description "Print the package version and exit when the CLI is run with --version." \
  --acceptance-criteria "python -m notes --version prints the version string and exits 0." \
  --acceptance-criteria "The version is defined once, in notes/__init__.py." \
  --complexity low --context file:notes/__main__.py --context file:notes/__init__.py)
D=$(add --title "Add a delete command" \
  --description "Let users delete a note by its list number." \
  --acceptance-criteria "python -m notes delete 2 removes the second note." \
  --acceptance-criteria "Deleting a number that doesn't exist prints an error and exits 1." \
  --complexity medium --context file:notes/__main__.py --context file:notes/store.py)
W=$(add --title "Match whole words with search --word" \
  --description "Add a --word option to search that matches whole words only." \
  --acceptance-criteria "search --word cat matches 'a cat' but not 'concatenate'." \
  --complexity low --context file:notes/__main__.py --context file:notes/store.py)
J=$(add --title "Store notes as JSON lines" \
  --description "Move the store to JSON lines so notes can carry a created-at timestamp, migrating the plain-text file on first read." \
  --acceptance-criteria "New notes are written as one JSON object per line with text and created_at." \
  --acceptance-criteria "An existing plain-text ~/.notes.txt is migrated without losing notes." \
  --complexity medium --context file:notes/store.py)
add --title "Document where notes are stored" \
  --description "The README should say notes live in ~/.notes.txt and how to point the store elsewhere." \
  --acceptance-criteria "README.md has a Storage section naming ~/.notes.txt." \
  --complexity low --context file:README.md >/dev/null
add --title "Create orbit-hello.txt" \
  --description "Add orbit-hello.txt at the repository root containing the text 'hello from orbit'." \
  --acceptance-criteria "orbit-hello.txt exists at the repository root." \
  --acceptance-criteria "orbit-hello.txt contains the text 'hello from orbit'." \
  --complexity low --context file:orbit-hello.txt --allow-missing-context >/dev/null
for t in "$V" "$D" "$W" "$J"; do
  orbit task update "$t" --approve --note "Scope reviewed." >/dev/null
done

if [ "$WITH_RUN" = "--with-run" ]; then
  RUN=$(orbit run ship "$V" | sed -n 's/^Run ID: //p')
  echo "shipping $V as $RUN"
  while :; do
    STATE=$(orbit run show "$RUN" | sed -n 's/^State: //p')
    case "$STATE" in
      succeeded|success) break ;;
      failed|cancelled|timeout|interrupted) echo "run $RUN ended $STATE" >&2; exit 1 ;;
    esac
    sleep 10
  done
  orbit task update "$V" --approve --note "Reviewed the change." >/dev/null
fi

orbit web serve --port "$PORT" --operator --no-open >/dev/null 2>&1 &
SERVER=$!
until curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1; do sleep 1; done

node "$HERE/capture.mjs" "http://127.0.0.1:$PORT/?workspace=ws_notes-cli" "$OUT"
