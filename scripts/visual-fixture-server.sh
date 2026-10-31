#!/bin/sh
# Fixture server for the view manifest (visual-review 2_design.md §3.3).
#
#   scripts/visual-fixture-server.sh <fixture>
#
# Seeds a disposable Orbit root and workspace from
# crates/orbit-web/visual/fixtures/<fixture>.json, serves the real
# dashboard on port zero, and prints ORBIT_VISUAL_URL=<loopback URL>
# (plus ORBIT_VISUAL_TASK=<first seeded task id>) once listening.
# The caller owns the process group and stops it.
#
# Frozen server clock: when libfaketime's `faketime` is on PATH the
# server runs under it pinned to the fixture instant — a test-only
# seam of this script, never a shipped environment variable read by
# the product. Without faketime the browser clock is still frozen by
# the runner (Playwright) and every seeded timestamp is created in
# one seeding pass; relative ages then drift with wall time, which
# the results record as server_clock=faketime-absent.
set -eu

fixture="${1:-busy-workspace}"
# The runner's empty state asks for "<fixture>:empty": same seed file,
# no rows seeded.
seed_rows=1
case "$fixture" in
  *:empty) fixture="${fixture%:empty}"; seed_rows=0 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
orbit="${ORBIT_VISUAL_ORBIT:-orbit}"
frozen="${ORBIT_VISUAL_FROZEN_INSTANT:-2026-01-01T12:00:00Z}"
scratch="${ORBIT_SCRATCH_DIR:-$repo_root/.orbit/tmp}/visual-fixture-$fixture-$$"
root="$scratch/root"
home_dir="$scratch/home"
work="$scratch/work"
seed="$repo_root/crates/orbit-web/visual/fixtures/$fixture.json"

if [ ! -f "$seed" ]; then
  echo "visual-fixture-server: no seed at $seed" >&2
  exit 1
fi
mkdir -p "$root" "$home_dir" "$work"
export HOME="$home_dir"
export ORBIT_SKIP_HOST_PREREQUISITES=1

run_orbit() {
  "$orbit" --root "$root" "$@"
}

run_orbit init --non-interactive --machine-name visual-fixture \
  --task-prefix VIS --skip-host-prerequisites >/dev/null

(
  cd "$work"
  git init -q . 2>/dev/null || true
  HOME="$home_dir" run_orbit workspace init >/dev/null 2>&1 || true
)

first_task=""
if [ "$seed_rows" = "1" ]; then
  # One task per line of "<title>\t<complexity>\t<status>".
  rows="$(node -e '
    const fs = require("fs");
    const seed = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    for (const task of seed.tasks ?? []) {
      process.stdout.write([task.title, task.complexity ?? "low", task.status ?? "proposed"].join("\t") + "\n");
    }
  ' "$seed")"
  printf '%s\n' "$rows" | while IFS="$(printf '\t')" read -r title complexity status; do
    [ -n "$title" ] || continue
    added="$(cd "$work" && run_orbit --json task add --title "$title" --complexity "$complexity" 2>/dev/null)" || continue
    id="$(printf '%s' "$added" | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{process.stdout.write(JSON.parse(d).id??"")}catch{}})')"
    [ -n "$id" ] || continue
    printf '%s\n' "$id" >> "$scratch/task-ids"
    case "$status" in
      proposed) ;;
      *) (cd "$work" && run_orbit task update --id "$id" --status "$status" >/dev/null 2>&1) || true ;;
    esac
  done
  first_task="$(head -n 1 "$scratch/task-ids" 2>/dev/null || true)"
fi

if command -v faketime >/dev/null 2>&1; then
  server_clock="faketime"
  set -- faketime -f "$scratch/faketime.rc" "$orbit" --root "$root" web serve --host 127.0.0.1 --port 0 --no-open
  printf '@%s\n' "$(date -u -d "$frozen" +%s 2>/dev/null || echo 1767268800)" > "$scratch/faketime.rc"
else
  server_clock="faketime-absent"
  set -- "$orbit" --root "$root" web serve --host 127.0.0.1 --port 0 --no-open
fi

echo "visual-fixture-server: fixture=$fixture seed_rows=$seed_rows server_clock=$server_clock frozen=$frozen" >&2

# The dashboard prints "Dashboard listening on <url>"; translate it to
# the manifest protocol, then keep serving in the foreground.
log="$scratch/server.log"
"$@" >"$log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' INT TERM EXIT
url=""
attempt=0
while [ "$attempt" -lt 120 ]; do
  if [ -f "$log" ]; then
    url="$(grep -o 'http://127\.0\.0\.1:[0-9]*' "$log" | head -n 1 || true)"
    [ -n "$url" ] && break
    url="$(grep -o 'http://\[::1\]:[0-9]*' "$log" | head -n 1 || true)"
    [ -n "$url" ] && break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    echo "visual-fixture-server: server exited; log follows" >&2
    cat "$log" >&2 || true
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 1
done
if [ -z "$url" ]; then
  echo "visual-fixture-server: server did not announce a URL; log follows" >&2
  cat "$log" >&2 || true
  exit 1
fi
printf 'ORBIT_VISUAL_URL=%s\n' "$url"
[ -n "$first_task" ] && printf 'ORBIT_VISUAL_TASK=%s\n' "$first_task"
wait "$server_pid"
