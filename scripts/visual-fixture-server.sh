#!/bin/sh
# Fixture server for the view manifest (archived visual-review 2_design.md §3.3).
#
#   scripts/visual-fixture-server.sh <fixture>
#
# Seeds a disposable Orbit root and workspace from
# crates/orbit-web/visual/fixtures/<fixture>.json. ORBIT_VISUAL_ORBIT
# (or orbit on PATH) is the API backend on port zero. The workspace
# front serves the admitted checkout's dashboard assets in front of
# that backend and prints ORBIT_VISUAL_URL=<loopback URL> only after
# declared assets load and API route presence is established. Proxied
# requests stay on that backend's loopback origin; a network-path or
# absolute-form target is refused before dispatch. Stdout
# also names the source revision, renderer hash, asset hash and backend
# identity separately. The caller owns the process group and stops it.
#
# Frozen server clock: when libfaketime's `faketime` is on PATH the
# server runs under it with its wall clock frozen at the fixture
# instant (ORBIT_VISUAL_FROZEN_INSTANT, default 2026-01-01T12:00:00Z)
# — a test-only seam of this script, never a shipped environment
# variable read by the product. The monotonic clock is not faked, so
# startup/readiness timers still advance in real time. Without
# faketime the browser clock is still frozen by the runner
# (Playwright) and every seeded timestamp is created in one seeding
# pass; relative ages then drift with wall time, which the results
# record as server_clock=faketime-absent.
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
# Fail before seeding when checkout assets are missing or unreadable.
# The front refuses again at serve time; this only makes that visible
# before the disposable backend starts.
export ORBIT_FIXTURE_REPO="$repo_root"
if ! node "$repo_root/scripts/visual-fixture-front.mjs" --check-assets; then
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
  git init -q .
  run_orbit workspace init >/dev/null
)

first_task=""
if [ "$seed_rows" = "1" ]; then
  # One task per line of "<title>\t<complexity>\t<status>".
  rows="$(node -e '
    const fs = require("fs");
    const seed = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (!Array.isArray(seed.tasks)) throw new Error("seed.tasks must be an array");
    for (const task of seed.tasks) {
      for (const value of [task.title, task.complexity ?? "low", task.status ?? "proposed"]) {
        if (typeof value !== "string" || !value.trim() || /[\t\r\n]/.test(value)) {
          throw new Error("seed task fields must be non-empty single-line strings");
        }
      }
      process.stdout.write([task.title, task.complexity ?? "low", task.status ?? "proposed"].join("\t") + "\n");
    }
  ' "$seed")"
  printf '%s\n' "$rows" | while IFS="$(printf '\t')" read -r title complexity status; do
    [ -n "$title" ] || continue
    added="$(cd "$work" && run_orbit --json task add --title "$title" --complexity "$complexity")"
    id="$(printf '%s' "$added" | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{process.stdout.write(JSON.parse(d).id??"")}catch{}})')"
    if [ -z "$id" ]; then
      echo "visual-fixture-server: task add returned no ID for $title" >&2
      exit 1
    fi
    printf '%s\n' "$id" >> "$scratch/task-ids"
    case "$status" in
      proposed) ;;
      # This operator override affects only the disposable fixture workspace.
      *) (cd "$work" && run_orbit task update "$id" --status "$status" --force >/dev/null) ;;
    esac
    actual="$(cd "$work" && run_orbit --json task show "$id")"
    printf '%s' "$actual" | node -e '
      let d = ""; process.stdin.on("data", c => d += c).on("end", () => {
        const actual = JSON.parse(d).status;
        if (actual !== process.argv[1]) {
          console.error(`visual-fixture-server: ${process.argv[2]} expected ${process.argv[1]}, got ${actual}`);
          process.exitCode = 1;
        }
      });
    ' "$status" "$id"
  done
  first_task="$(head -n 1 "$scratch/task-ids" 2>/dev/null || true)"
fi

# Open fleet notices, one per line of "<kind>\t<title>\t<body>", posted
# through the real CLI so the Notices view reads them from the backend.
# Under faketime they are posted at the frozen instant the server reads,
# so their age and expiry stay in range. Notices are operator writes;
# ORBIT_OPERATOR=1 grants that only to these seeding commands against the
# disposable fixture root.
if [ "$seed_rows" = "1" ]; then
  notice_rows="$(node -e '
    const fs = require("fs");
    const seed = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const notices = seed.notices ?? [];
    if (!Array.isArray(notices)) throw new Error("seed.notices must be an array");
    for (const notice of notices) {
      for (const value of [notice.kind, notice.title, notice.body]) {
        if (typeof value !== "string" || !value.trim() || /[\t\r\n]/.test(value)) {
          throw new Error("seed notice fields must be non-empty single-line strings");
        }
      }
      process.stdout.write([notice.kind, notice.title, notice.body].join("\t") + "\n");
    }
  ' "$seed")"
  printf '%s\n' "$notice_rows" | while IFS="$(printf '\t')" read -r kind title body; do
    [ -n "$title" ] || continue
    if command -v faketime >/dev/null 2>&1; then
      seed_ts="$(date -u -d "$frozen" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || printf '%s' "$frozen" | sed -e 's/T/ /' -e 's/Z$//')"
      (cd "$work" && env ORBIT_OPERATOR=1 TZ=UTC faketime -f "$seed_ts" "$orbit" --root "$root" notice post --kind "$kind" --title "$title" --body "$body" >/dev/null)
    else
      (cd "$work" && ORBIT_OPERATOR=1 run_orbit notice post --kind "$kind" --title "$title" --body "$body" >/dev/null)
    fi
  done
fi

# Resident mandates, one per `residents` row: the tagged assignment and its
# `resident/mandate.json`, then any agent-raised grant requests (left open)
# and an optional operator control action. The mandate's
# workspace and crew are this fixture's registered workspace and default crew.
if [ "$seed_rows" = "1" ]; then
  workspace_name="$(basename "$work")"
  default_crew="$(sed -n 's/^default_crew *= *"\([^"]*\)".*/\1/p' "$root/config.toml" | head -n 1)"
  resident_count="$(node -e '
    const seed = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const rows = seed.residents ?? [];
    if (!Array.isArray(rows)) throw new Error("seed.residents must be an array");
    process.stdout.write(String(rows.length));
  ' "$seed")"
  index=0
  while [ "$index" -lt "$resident_count" ]; do
    field() {
      node -e '
        const seed = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
        const row = seed.residents[Number(process.argv[2])];
        const value = process.argv[3].split(".").reduce((node, key) => node?.[key], row);
        if (value !== undefined) process.stdout.write(typeof value === "string" ? value : JSON.stringify(value));
      ' "$seed" "$index" "$1"
    }
    title="$(field title)"
    added="$(cd "$work" && run_orbit --json task add --title "$title" --complexity medium --tag resident)"
    id="$(printf '%s' "$added" | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{try{process.stdout.write(JSON.parse(d).id??"")}catch{}})')"
    if [ -z "$id" ]; then
      echo "visual-fixture-server: resident task add returned no ID for $title" >&2
      exit 1
    fi
    mandate_file="$work/resident-mandate-$index.json"
    node -e '
      const fs = require("fs");
      const seed = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
      const mandate = { schema: "resident.mandate/v1", workspaces: [process.argv[3]], crew: process.argv[4], ...seed.residents[Number(process.argv[2])].mandate };
      fs.writeFileSync(process.argv[5], JSON.stringify(mandate, null, 2));
    ' "$seed" "$index" "$workspace_name" "$default_crew" "$mandate_file"
    (cd "$work" && run_orbit task artifact put "$id" "$mandate_file" --path resident/mandate.json >/dev/null)
    request_count="$(field requests | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>process.stdout.write(String(d?JSON.parse(d).length:0)))')"
    request=0
    while [ "$request" -lt "$request_count" ]; do
      set --
      for right in $(field "requests.$request.add" | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>process.stdout.write(JSON.parse(d).join(" ")))'); do
        set -- "$@" --add "$right"
      done
      if [ "$(field "requests.$request.workspace")" = "true" ]; then
        set -- "$@" --add-workspace "$workspace_name"
      fi
      # An agent caller only raises a resident_authority request; it stays
      # open for the view's answer buttons.
      (cd "$work" && ORBIT_AGENT_NAME=visual-fixture-relay run_orbit resident grant "$id" "$@" --quote "$(field "requests.$request.quote")" >/dev/null)
      request=$((request + 1))
    done
    control_action="$(field control.action)"
    if [ -n "$control_action" ]; then
      (cd "$work" && ORBIT_OPERATOR=1 run_orbit resident control "$id" "$control_action" --reason "$(field control.reason)" >/dev/null)
    fi
    index=$((index + 1))
  done
fi

first_experiment=""
if [ "$seed_rows" = "1" ] && [ "$fixture" = "experiments" ]; then
  first_experiment="$(node -e 'const f=require(process.argv[1]); process.stdout.write(f.experiment.id)' "$seed")"
fi

if command -v faketime >/dev/null 2>&1; then
  server_clock="faketime"
  # `faketime -f` passes its argument directly as FAKETIME; it does not
  # read a file, so a pathname is rejected as a timestamp. The frozen
  # (stopped-clock) form is the absolute "YYYY-MM-DD hh:mm:ss" with no
  # "@" prefix ("@..." would start at the instant and keep running).
  # Convert the ISO-8601 UTC instant to that form, pin TZ=UTC so the
  # wall clock freezes at that UTC instant on any host, and leave
  # CLOCK_MONOTONIC real so readiness/startup timers still advance.
  frozen_ts="$(date -u -d "$frozen" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || printf '%s' "$frozen" | sed -e 's/T/ /' -e 's/Z$//')"
  set -- env TZ=UTC FAKETIME_DONT_FAKE_MONOTONIC=1 faketime -f "$frozen_ts" "$orbit" --root "$root" web serve --host 127.0.0.1 --port 0 --no-open
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
# Checkout assets are served by the workspace front. The backend remains
# the API, including for fixtures without an overlay. Overlays stay in
# the front and are not a separate process.
export ORBIT_FIXTURE_UPSTREAM="$url"
export ORBIT_FIXTURE_SEED="$seed"
export ORBIT_FIXTURE_TASK="${first_task:-}"
export ORBIT_FIXTURE_EXPERIMENT="${first_experiment:-}"
export ORBIT_FIXTURE_BACKEND="$orbit"
export ORBIT_FIXTURE_PROVENANCE="$scratch/fixture-provenance.json"
export ORBIT_FIXTURE_LAUNCHER="$repo_root/scripts/visual-fixture-server.sh"
if [ -n "$first_experiment" ]; then
  export ORBIT_FIXTURE_OVERLAY=experiment
elif [ "$fixture" = "implementers-scorecard" ]; then
  export ORBIT_FIXTURE_OVERLAY=scorecard
else
  export ORBIT_FIXTURE_OVERLAY=none
fi
node "$repo_root/scripts/visual-fixture-front.mjs" &
front_pid=$!
trap 'kill "$front_pid" "$server_pid" 2>/dev/null || true' INT TERM EXIT
wait "$front_pid"
