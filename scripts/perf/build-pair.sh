#!/usr/bin/env bash
# Build the release `orbit` binary for two revisions, for paired timing.
#
#   scripts/perf/build-pair.sh --base <rev> --candidate <rev> --out <builds-dir>
#                              [--keep] [--lane-wait <seconds>]
#   scripts/perf/build-pair.sh --base <rev> --candidate-worktree <path>
#                              --out <builds-dir> [--keep] [--lane-wait <seconds>]
#
# Pass exactly one of --candidate and --candidate-worktree. The worktree form
# takes the checkout root (git rev-parse --show-toplevel); a subdirectory is
# refused. It snapshots tracked and untracked files, excluding ignored ones, into a
# temporary commit (throwaway GIT_INDEX_FILE, write-tree, commit-tree -p HEAD)
# whose objects sit outside the checkout. HEAD, the index, the files and git
# metadata stay untouched. A worktree with no such changes resolves to HEAD,
# so the equal-commit refusal still fires.
#
# Each revision is exported with `git archive` into its own clean source dir
# and built with a private CARGO_TARGET_DIR, using the cargo invocation of
# `make install INSTALL_PROFILE=release` plus `--locked`, under
# scripts/build-budget.py. Writes <builds-dir>/{base,candidate}/orbit and
# <builds-dir>/provenance.json. Holds the host-wide perf lane for the whole
# run. See docs/runbooks/perf-timing-protocol.md.
set -euo pipefail

usage() {
  sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 64
}

refuse() {
  printf 'build-pair.sh: %s\n' "$*" >&2
  exit 2
}

base_rev=""
candidate_rev=""
candidate_worktree=""
out=""
keep=0
lane_wait="${ORBIT_PERF_LANE_WAIT_SECONDS:-900}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) base_rev="${2:?}"; shift 2 ;;
    --candidate) candidate_rev="${2:?}"; shift 2 ;;
    --candidate-worktree) candidate_worktree="${2:?}"; shift 2 ;;
    --out) out="${2:?}"; shift 2 ;;
    --keep) keep=1; shift ;;
    --lane-wait) lane_wait="${2:?}"; shift 2 ;;
    -h|--help) usage ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; usage ;;
  esac
done
if [[ -n "$candidate_rev" && -n "$candidate_worktree" ]]; then
  printf 'pass only one of --candidate and --candidate-worktree\n' >&2
  usage
fi
[[ -n "$base_rev" && -n "$out" ]] || usage
[[ -n "$candidate_rev" || -n "$candidate_worktree" ]] || usage

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="$(cd "$here/../.." && pwd -P)"
build_budget="$repo/scripts/build-budget.py"
git_repo="$repo"
if [[ -n "$candidate_worktree" ]]; then
  [[ -d "$candidate_worktree" ]] || refuse "--candidate-worktree is not a directory: $candidate_worktree"
  candidate_worktree="$(cd "$candidate_worktree" && pwd -P)"
  inside="$(git -C "$candidate_worktree" rev-parse --is-inside-work-tree 2>/dev/null || true)"
  [[ "$inside" == "true" ]] || refuse "--candidate-worktree is not a git worktree: $candidate_worktree"
  top="$(git -C "$candidate_worktree" rev-parse --show-toplevel)"
  top="$(cd "$top" && pwd -P)"
  [[ "$candidate_worktree" == "$top" ]] || refuse "--candidate-worktree must be the checkout root $top, not the subdirectory $candidate_worktree; a subdirectory snapshots and exports only part of the checkout"
  git_repo="$candidate_worktree"
fi

# ── disk gate (RB-11) ────────────────────────────────────────────────────────
disk_gate() {
  local target="$1" parent pcent
  parent="$(dirname "$target")"
  while [[ ! -e "$parent" && "$parent" != "/" ]]; do parent="$(dirname "$parent")"; done
  if [[ -n "${ORBIT_PERF_SIMULATE_DISK_PCENT:-}" ]]; then
    pcent="$ORBIT_PERF_SIMULATE_DISK_PCENT"
  else
    pcent="$(df --output=pcent "$parent" | tail -n 1 | tr -dc '0-9')"
  fi
  if (( pcent >= 80 )); then
    refuse "disk gate: $parent is at ${pcent}% (df --output=pcent), at or above 80%; created nothing for $target. Free space first (RB-11, ws_constellation runbooks)."
  fi
}

# Snapshot tracked and untracked files, excluding ignored ones, into a
# temporary commit. Objects and the index live under a throwaway directory,
# so a read-only git dir (a managed worktree) still works and the checkout's
# HEAD, index, files and git metadata are not written.
# Sets candidate_commit, candidate_tree, candidate_is_snapshot,
# snapshot_scratch, snapshot_objects and snapshot_alternates.
snapshot_worktree() {
  local wt="$1"
  local head head_tree git_dir common_dir real_objects index objects tree commit
  local src_index src_index_dir seed_git object_format shared dir name
  head="$(git -C "$wt" rev-parse --verify --quiet 'HEAD^{commit}' || true)"
  [[ -n "$head" ]] || refuse "cannot resolve HEAD to a commit in $wt"
  head_tree="$(git -C "$wt" rev-parse --verify --quiet 'HEAD^{tree}' || true)"
  [[ -n "$head_tree" ]] || refuse "cannot resolve HEAD to a tree in $wt"
  git_dir="$(git -C "$wt" rev-parse --git-dir)"
  if [[ "$git_dir" != /* ]]; then
    git_dir="$(cd "$wt/$git_dir" && pwd -P)"
  fi
  common_dir="$(git -C "$wt" rev-parse --git-common-dir)"
  if [[ "$common_dir" != /* ]]; then
    common_dir="$(cd "$wt/$common_dir" && pwd -P)"
  fi
  real_objects="$common_dir/objects"
  [[ -d "$real_objects" ]] || refuse "no object store at $real_objects"
  src_index="$(git -C "$wt" rev-parse --git-path index)"
  if [[ "$src_index" != /* ]]; then
    src_index="$wt/$src_index"
  fi
  object_format="$(git -C "$wt" rev-parse --show-object-format)"

  # mktemp places the temporary index and objects on TMPDIR's filesystem
  # (or /tmp), which need not be the builds directory. Refuse before creating
  # them. disk_gate "$out" still runs before that directory is created.
  local snapshot_fs="${TMPDIR:-}"
  if [[ -z "$snapshot_fs" ]]; then
    snapshot_fs="/tmp"
  fi
  disk_gate "$snapshot_fs/orbit-perf-worktree-snapshot"

  snapshot_scratch="$(mktemp -d)"
  index="$snapshot_scratch/index"
  objects="$snapshot_scratch/objects"
  mkdir -p "$objects"
  snapshot_objects="$objects"
  snapshot_alternates="$real_objects"
  local -a snap_env=(
    "GIT_INDEX_FILE=$index"
    "GIT_OBJECT_DIRECTORY=$objects"
    "GIT_ALTERNATE_OBJECT_DIRECTORIES=$real_objects"
  )
  # Seed the scratch index from a copy of the source index (and any split-index
  # sharedindex.* sidecars) inside a throwaway GIT_DIR so Git's split-index
  # freshening never updates mtimes in the checkout's real git directory.
  if [[ -f "$src_index" ]]; then
    seed_git="$snapshot_scratch/seed-git"
    mkdir -p "$seed_git/objects" "$seed_git/refs"
    printf 'ref: refs/heads/invalid\n' > "$seed_git/HEAD"
    if [[ "$object_format" != "sha1" ]]; then
      printf '[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectformat = %s\n' \
        "$object_format" > "$seed_git/config"
    fi
    cat "$src_index" > "$seed_git/index"
    src_index_dir="$(cd "$(dirname "$src_index")" && pwd -P)"
    # Copy each sidecar best-effort. ls-files below opens only the sidecar the
    # copied index references, so a retired sidecar that Git expires or that
    # cannot be read is skipped. If the referenced one is missing or unreadable,
    # ls-files fails and the snapshot refuses.
    for dir in "$git_dir" "$src_index_dir" "$common_dir"; do
      for shared in "$dir"/sharedindex.*; do
        [[ -f "$shared" ]] || continue
        name="${shared##*/}"
        if [[ ! -e "$seed_git/$name" ]] && ! cat "$shared" > "$seed_git/$name" 2>/dev/null; then
          rm -f "$seed_git/$name"
        fi
      done
    done
    env -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES \
      -u GIT_COMMON_DIR -u GIT_WORK_TREE \
      GIT_DIR="$seed_git" GIT_OBJECT_DIRECTORY="$real_objects" \
      git ls-files --stage -z \
      | env "${snap_env[@]}" git -C "$wt" -c core.splitIndex=false update-index -z --index-info \
      || refuse "failed to snapshot the working tree at $wt"
    rm -rf "$seed_git"
  fi
  env "${snap_env[@]}" git -C "$wt" -c core.splitIndex=false add --all -- . \
    || refuse "failed to snapshot the working tree at $wt"
  tree="$(env "${snap_env[@]}" git -C "$wt" -c core.splitIndex=false write-tree)" \
    || refuse "failed to snapshot the working tree at $wt"
  if [[ "$tree" == "$head_tree" ]]; then
    rm -rf "$snapshot_scratch"
    snapshot_scratch=""
    snapshot_objects=""
    snapshot_alternates=""
    candidate_commit="$head"
    candidate_tree="$head_tree"
    candidate_is_snapshot=0
    return
  fi
  commit="$(
    env "${snap_env[@]}" \
      GIT_AUTHOR_NAME="orbit-perf" \
      GIT_AUTHOR_EMAIL="orbit-perf@orbit.invalid" \
      GIT_COMMITTER_NAME="orbit-perf" \
      GIT_COMMITTER_EMAIL="orbit-perf@orbit.invalid" \
      git -C "$wt" commit-tree "$tree" -p "$head" -m "orbit perf: working-tree snapshot"
  )" || refuse "failed to commit the working-tree snapshot"
  candidate_commit="$commit"
  candidate_tree="$tree"
  candidate_is_snapshot=1
}

archive_into() {
  local commit="$1" dest="$2"
  if [[ -n "${snapshot_objects:-}" && "$commit" == "$candidate_commit" ]]; then
    env GIT_OBJECT_DIRECTORY="$snapshot_objects" \
      GIT_ALTERNATE_OBJECT_DIRECTORIES="$snapshot_alternates" \
      git -C "$git_repo" archive --format=tar "$commit" | tar -x -C "$dest"
  else
    git -C "$git_repo" archive --format=tar "$commit" | tar -x -C "$dest"
  fi
}

# ── serial lane ──────────────────────────────────────────────────────────────
passwd_home="$(getent passwd "$(id -u)" | cut -d: -f6)"
lane_lock="${ORBIT_PERF_LANE_LOCK:-$passwd_home/.orbit/cache/perf-lane/lane.lock}"
mkdir -p "$(dirname "$lane_lock")"
exec 9<>"$lane_lock"
if ! flock -w "$lane_wait" 9; then
  holder="$(head -c 4096 "$lane_lock" 2>/dev/null || true)"
  refuse "perf lane busy: ${holder:-<unknown holder>}"
fi
printf '{"entry": "build-pair.sh", "host": "%s", "pid": %d, "run_id": "%s", "started_at": "%s", "task_id": "%s"}\n' \
  "$(uname -n)" "$$" "${ORBIT_RUN_ID:-}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${ORBIT_TASK_ID:-}" >"$lane_lock"

success=0
work=""
snapshot_scratch=""
snapshot_objects=""
snapshot_alternates=""
candidate_is_snapshot=0
cleanup() {
  : >"$lane_lock" 2>/dev/null || true
  if [[ -n "$snapshot_scratch" && -d "$snapshot_scratch" ]]; then
    if [[ -z "$work" || "$snapshot_scratch" != "$work"/* ]]; then
      rm -rf "$snapshot_scratch"
    fi
  fi
  if [[ -z "$work" ]]; then
    return
  fi
  if (( keep )); then
    printf 'kept: %s\n' "$out" >&2
  elif (( success )); then
    rm -rf "$work"
  else
    rm -rf "$out"
  fi
}
trap cleanup EXIT

# ── revisions ────────────────────────────────────────────────────────────────
resolve() { git -C "$git_repo" rev-parse --verify --quiet "$1^{$2}" || refuse "cannot resolve $1 to a $2"; }
base_commit="$(resolve "$base_rev" commit)"
base_tree="$(resolve "$base_rev" tree)"
if [[ -n "$candidate_worktree" ]]; then
  snapshot_worktree "$git_repo"
else
  candidate_commit="$(resolve "$candidate_rev" commit)"
  candidate_tree="$(resolve "$candidate_rev" tree)"
  candidate_is_snapshot=0
fi
if [[ "$base_commit" == "$candidate_commit" ]]; then
  refuse "base and candidate resolve to the same commit $base_commit; nothing to compare"
fi

out="$(realpath -m "$out")"
if [[ -e "$out" ]] && [[ -n "$(ls -A "$out" 2>/dev/null)" || ! -d "$out" ]]; then
  refuse "$out already exists and is not empty"
fi
disk_gate "$out"
mkdir -p "$out"
work="$out/.work"
mkdir -p "$work"
if [[ -n "$snapshot_scratch" ]]; then
  mv "$snapshot_scratch" "$work/snapshot-git"
  snapshot_scratch="$work/snapshot-git"
  snapshot_objects="$snapshot_scratch/objects"
fi

build_one() {
  local role="$1" commit="$2" src="$work/src-$1" target="$work/target-$1" json bin
  mkdir -p "$src" "$out/$role"
  archive_into "$commit" "$src"
  # Test hook: hash the exported tree instead of compiling. Distinct trees
  # produce distinct bytes, so the identical-binary refusal still applies.
  # Never set ORBIT_PERF_BUILD_PAIR_STUB for a real measurement.
  if [[ "${ORBIT_PERF_BUILD_PAIR_STUB:-}" == "1" ]]; then
    python3 - "$src" "$out/$role/orbit" <<'PY'
import hashlib, os, sys
from pathlib import Path
src, dest = map(Path, sys.argv[1:3])
digest = hashlib.sha256()
for path in sorted(p for p in src.rglob("*") if p.is_file()):
    digest.update(path.relative_to(src).as_posix().encode())
    digest.update(b"\0")
    digest.update(path.read_bytes())
    digest.update(b"\0")
dest.write_bytes(digest.digest())
os.chmod(dest, 0o755)
PY
    return
  fi
  json="$work/cargo-$role.json"
  printf 'build-pair.sh: building %s %s\n' "$role" "$commit" >&2
  # The same cargo invocation as `make install INSTALL_PROFILE=release`, plus
  # --locked. GIT_CEILING_DIRECTORIES keeps the export from being read as part
  # of an enclosing checkout, so build.rs embeds no outside commit identity.
  (
    cd "$src"
    env CARGO_TARGET_DIR="$target" GIT_CEILING_DIRECTORIES="$work" \
      "$build_budget" -- cargo build -p orbit-cli --bin orbit --release --locked \
      --message-format=json-render-diagnostics >"$json"
  )
  bin="$(python3 -c "$CARGO_EXECUTABLE_FROM_JSON" <"$json")"
  [[ -x "$bin" ]] || refuse "cargo produced no orbit executable for $role"
  install -m 755 "$bin" "$out/$role/orbit"
}

CARGO_EXECUTABLE_FROM_JSON='
import json, sys
path = None
for raw in sys.stdin:
    raw = raw.strip()
    if not raw.startswith("{"):
        continue
    try:
        message = json.loads(raw)
    except json.JSONDecodeError:
        continue
    if message.get("reason") == "compiler-artifact" and message.get("executable") \
            and message.get("target", {}).get("name") == "orbit":
        path = message["executable"]
if not path:
    sys.exit("no orbit executable in cargo output")
print(path)
'

build_one base "$base_commit"
build_one candidate "$candidate_commit"

base_sha="$(sha256sum "$out/base/orbit" | cut -d' ' -f1)"
candidate_sha="$(sha256sum "$out/candidate/orbit" | cut -d' ' -f1)"
if [[ "$base_sha" == "$candidate_sha" ]]; then
  refuse "base $base_commit and candidate $candidate_commit built identical binaries (sha256 $base_sha); the candidate would silently measure the baseline. Pick revisions whose content differs."
fi

python3 - "$out" "$work" \
  "$base_commit" "$base_tree" "$base_sha" \
  "$candidate_commit" "$candidate_tree" "$candidate_sha" \
  "$base_rev" "$candidate_rev" "$candidate_worktree" \
  "$candidate_is_snapshot" "${ORBIT_PERF_BUILD_PAIR_STUB:-}" "$here" <<'PY'
import json, os, subprocess, sys
from pathlib import Path
(out, work, base_commit, base_tree, base_sha,
 candidate_commit, candidate_tree, candidate_sha,
 base_rev, candidate_rev, candidate_worktree, candidate_is_snapshot,
 build_stub, here) = sys.argv[1:15]
sys.path.insert(0, here)
import perf_common as common

out, work = Path(out), Path(work)
snapshot = candidate_is_snapshot == "1"
stub = build_stub == "1"
codegen_env = [
    "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "RUSTDOCFLAGS",
    "RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_TARGET", "RUSTC",
    "RUSTC_BOOTSTRAP", "CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS", "AR",
    "CARGO_INCREMENTAL", "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
    "CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_PROFILE_RELEASE_DEBUG",
    "CARGO_PROFILE_RELEASE_STRIP", "CARGO_PROFILE_RELEASE_PANIC",
]
def rev(role, commit, tree, sha, working_tree_snapshot):
    src = work / f"src-{role}"
    cargo_config = src / ".cargo" / "config.toml"
    return {
        "role": role,
        "commit": commit,
        "tree": tree,
        "working_tree_snapshot": working_tree_snapshot,
        "binary": f"{role}/orbit",
        "binary_sha256": sha,
        "cargo_config_sha256": common.sha256_file(cargo_config) if cargo_config.exists() else None,
        "cargo_lock_sha256": common.sha256_file(src / "Cargo.lock"),
    }
rustc = subprocess.run(["rustc", "-Vv"], check=True, capture_output=True, text=True).stdout
cargo = subprocess.run(["cargo", "-V"], check=True, capture_output=True, text=True).stdout.strip()
source_export = "git archive --format=tar <commit> | tar -x (clean dir per revision)"
if snapshot:
    source_export += (
        "; candidate is a working-tree snapshot "
        "(throwaway GIT_INDEX_FILE, write-tree, commit-tree -p HEAD; "
        "objects kept outside the checkout)"
    )
if stub:
    cargo_command = "ORBIT_PERF_BUILD_PAIR_STUB=1 (test hook; not a release build)"
else:
    cargo_command = (
        "scripts/build-budget.py -- cargo build -p orbit-cli --bin orbit "
        "--release --locked --message-format=json-render-diagnostics"
    )
provenance = {
    "schemaVersion": 1,
    "kind": "orbit-perf-build-pair",
    "created_at": common.now_iso(),
    "revisions": {
        "base": rev("base", base_commit, base_tree, base_sha, False),
        "candidate": rev("candidate", candidate_commit, candidate_tree, candidate_sha, snapshot),
    },
    "requested": {
        "base": base_rev,
        "candidate": candidate_rev or None,
        "candidate_worktree": candidate_worktree or None,
    },
    "profile": "release",
    "features": "default (no --features, --no-default-features or --all-features)",
    "cargo_command": cargo_command,
    "build_stub": stub,
    "source_export": source_export,
    "cargo_target_dir": "private per revision, removed after the build",
    "rustc_vv": rustc,
    "cargo_version": cargo,
    "codegen_env": {name: os.environ.get(name) for name in codegen_env},
    "rustc_wrapper": "the exported tree's .cargo/config.toml build.rustc-wrapper (scripts/rustc-compiler-cache.sh; plain rustc without sccache)",
    **common.host_identity(),
}
common.write_json(out / "provenance.json", provenance)
print(json.dumps({"builds": str(out), "base": provenance["revisions"]["base"]["binary_sha256"],
                  "candidate": provenance["revisions"]["candidate"]["binary_sha256"]}))
PY
success=1
