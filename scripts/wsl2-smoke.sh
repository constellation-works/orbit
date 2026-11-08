#!/usr/bin/env bash
# Run the Orbit WSL2 smoke check inside a fresh Ubuntu distribution.
set -u
# Drop inherited Orbit bindings, provider credentials, npm settings and Git config.
# Preserve only the Windows executable path needed for the host-side HTTP probe.
if [[ ${ORBIT_SMOKE_CLEAN_ENV:-} != 1 ]]; then
  powershell_path=$(command -v powershell.exe || true)
  exec env -i ORBIT_SMOKE_CLEAN_ENV=1 ORBIT_SMOKE_POWERSHELL="$powershell_path" ORBIT_SMOKE_PARENT_HOME="${HOME:-/tmp}" \
    PATH="/usr/local/bin:/usr/bin:/bin" USER="${USER:-smoke}" \
    TMPDIR="${TMPDIR:-/tmp}" bash "$0" "$@"
fi
exec 3>&1
if ! command -v python3 >/dev/null 2>&1; then
  printf '%s\n' '{"schema_version":1,"status":"fail","steps":[{"step":"prerequisites","status":"fail","detail":"python3 is required"}]}' >&3
  exit 1
fi
scratch=$(mktemp -d "${ORBIT_SMOKE_PARENT_HOME:-${TMPDIR:-/tmp}}/.orbit-wsl2-smoke.XXXXXXXX") || {
  printf '%s\n' '{"schema_version":1,"status":"fail","steps":[{"step":"scratch","status":"fail","detail":"Could not create scratch directory"}]}' >&3
  exit 1
}
cleanup() {
  python3 - "$scratch" <<'PY'
import shutil, sys
shutil.rmtree(sys.argv[1])
PY
}
trap cleanup EXIT
results="$scratch/results.jsonl"
: > "$results"
record() {
  python3 - "$results" "$1" "$2" "$3" <<'PY'
import json, sys
with open(sys.argv[1], 'a', encoding='utf-8') as output:
    output.write(json.dumps(dict(step=sys.argv[2], status=sys.argv[3], detail=sys.argv[4])) + '\n')
PY
}
report() {
  python3 - "$results" >&3 <<'PY'
import json, sys
with open(sys.argv[1], encoding='utf-8') as source:
    steps = [json.loads(line) for line in source]
failed = any(s['status'] == 'fail' for s in steps)
print(json.dumps(dict(schema_version=1, status='fail' if failed else 'pass', steps=steps)))
sys.exit(failed)
PY
}
run_step() {
  local name=$1
  shift
  if "$@" >"$scratch/$name.log" 2>&1; then
    record "$name" pass "$(tail -c 2000 "$scratch/$name.log")"
    return 0
  fi
  record "$name" fail "$(tail -c 2000 "$scratch/$name.log")"
  return 1
}

kernel=$(uname -r)
kernel_lower=$(printf "%s" "$kernel" | tr "[:upper:]" "[:lower:]")
is_wsl=false
if [[ $kernel_lower == *microsoft* ]]; then
  is_wsl=true
  if [[ $kernel_lower == *wsl2* || $kernel_lower == *microsoft-standard* ]]; then
    record wsl pass "WSL2 kernel: $kernel"
  else
    record wsl fail "WSL detected but WSL2 could not be confirmed: $kernel"
  fi
  if [[ -r /etc/os-release ]] && ( . /etc/os-release; [[ ${ID:-} == ubuntu ]] ); then
    record distro pass 'Ubuntu detected'
  else
    record distro fail 'Expected an Ubuntu distribution'
  fi
elif [[ $(uname -s) == Linux ]]; then
  record wsl skipped 'Not running under WSL'
  record distro skipped 'Not running under WSL'
else
  record wsl fail 'This check requires Linux or WSL2 Ubuntu'
  record distro skipped 'Not running under WSL'
  report
  exit $?
fi

# Keep all Orbit and npm state away from the user's existing installation.
export HOME="$scratch/home" USERPROFILE="$scratch/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$HOME" "$scratch/npm" "$scratch/repo"
unset ORBIT_ROOT ORBIT_HOME ORBIT_WORKSPACE ORBIT_TASK_ID ORBIT_RUN_ID ORBIT_WORKER_CONTEXT_REQUIRED
export npm_config_prefix="$scratch/npm"
export PATH="$scratch/stub:$scratch/npm/bin:$PATH"
mkdir -p "$scratch/stub"
cat > "$scratch/stub/codex" <<'STUB'
#!/bin/sh
# This executable has no provider access; it implements only this fixture.
if [ "${1:-}" = login ]; then printf '%s\n' 'Logged in'; exit 0; fi
cat > /dev/null
printf '%s\n' 'Smoke implementation' > smoke-result.txt
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"summary":"Created the smoke result.","execution_summary":"Wrote smoke-result.txt."},"error":null}'
STUB
chmod +x "$scratch/stub/codex"

node_ok() {
  command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1 &&
    node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 18 ? 0 : 1)' &&
    [[ $(command -v node) != /mnt/* && $(command -v npm) != /mnt/* ]]
}
if node_ok; then
  record node pass "$(node --version); npm $(npm --version)"
else
  # Isolated Node install: never change the host's packages or global prefix.
  case $(uname -m) in
    x86_64) arch=x64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) arch=unsupported ;;
  esac
  if [[ $arch != unsupported ]] && command -v curl >/dev/null 2>&1 && command -v tar >/dev/null 2>&1 &&
    curl -fsSL "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt" -o "$scratch/SHA256SUMS" 2>"$scratch/node.log"; then
    archive=$(awk -v suffix="-linux-$arch.tar.xz" '$2 ~ suffix"$" {print $2; exit}' "$scratch/SHA256SUMS")
    if [[ -n $archive ]] && curl -fsSL "https://nodejs.org/dist/latest-v22.x/$archive" -o "$scratch/$archive" 2>>"$scratch/node.log" &&
      (cd "$scratch" && awk -v file="$archive" '$2 == file' SHA256SUMS | sha256sum -c - >>node.log 2>&1) &&
      mkdir -p "$scratch/node" && tar -xJf "$scratch/$archive" -C "$scratch/node" --strip-components=1 >>"$scratch/node.log" 2>&1; then
      export PATH="$scratch/node/bin:$PATH"
    fi
  fi
  if node_ok; then
    record node pass "Installed isolated $(node --version); npm $(npm --version)"
  else
    record node fail "Node 18+ and npm unavailable; $(tail -c 1500 "$scratch/node.log" 2>/dev/null)"
  fi
fi
if node_ok && run_step install npm install -g @orbit-tools/cli; then
  run_step version orbit --version || true
  if (cd "$scratch/repo" && git init -q -b main && git config user.name 'Orbit smoke' && git config user.email 'smoke@example.invalid' && printf 'Orbit smoke\n' > README.md && git add README.md && git commit -qm seed); then
    if (cd "$scratch/repo" && run_step init orbit init --non-interactive --machine-name wsl-smoke --task-prefix WSM --skip-host-prerequisites); then
      if (cd "$scratch/repo" && run_step workspace orbit workspace init --name wsl-smoke --base-branch main --ship-mode local); then
        # Workspace init creates .gitignore; local delivery requires a clean base.
        if (cd "$scratch/repo" && git add .gitignore && git commit -qm 'initialize workspace' && orbit config set --global crews.sol.enabled true >/dev/null 2>&1); then
          if python3 - "$scratch/repo" >"$scratch/ship.log" 2>&1 <<'PYSHIP'
import json, pathlib, subprocess, sys, time
repo = pathlib.Path(sys.argv[1])
def call(*args):
    return subprocess.run(["orbit", *args, "--json"], cwd=repo, text=True, capture_output=True, timeout=30, check=True)
created = json.loads(call("task", "add", "--title", "Smoke delivery", "--description", "Create smoke-result.txt.", "--acceptance-criteria", "smoke-result.txt exists.", "--complexity", "low", "--status", "backlog", "--crew", "sol", "--context", "file:smoke-result.txt", "--allow-missing-context", "--plan", "Create the file.").stdout)
task_id = created["id"]
submitted = json.loads(call("run", "ship", task_id, "--mode", "local", "--complete").stdout)
run_id = submitted["run_id"]
terminal = False
try:
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        run = json.loads(call("run", "show", run_id).stdout)["run"]
        state = run["state"]
        if state in ("success", "failed", "cancelled", "interrupted"):
            terminal = True
            if state != "success":
                raise RuntimeError(f"shipment {run_id}: {state}: {run.get('error_message')}")
            break
        time.sleep(1)
    else:
        raise TimeoutError(f"shipment {run_id} did not finish in 180 seconds")
    task = json.loads(call("task", "show", task_id).stdout)
    if task["status"] != "done" or (repo / "smoke-result.txt").read_text(encoding="utf-8") != "Smoke implementation\n":
        raise RuntimeError(f"shipment finished without delivering {task_id}: {task['status']}")
    print(f"{task_id} shipped to done and its file landed ({run_id})")
finally:
    if not terminal:
        # The cancellation command waits for the worker and its child runs.
        cancelled = json.loads(call("run", "cancel", run_id, "--confirm").stdout)
        if cancelled.get("unstopped_leaves") or cancelled.get("unstopped_children"):
            raise RuntimeError(f"shipment cancellation left workers: {cancelled}")
PYSHIP
          then
            record ship pass "$(cat "$scratch/ship.log")"
          else
            record ship fail "$(tail -c 3000 "$scratch/ship.log")"
          fi
        else
          record ship fail 'Could not prepare a clean workspace or enable the stub crew'
        fi
        (cd "$scratch/repo" && run_step doctor orbit doctor) || true
        if (cd "$scratch/repo" && orbit doctor providers --json >"$scratch/providers.json" 2>"$scratch/sandbox.log"); then
          if python3 - "$scratch/providers.json" >"$scratch/readiness.log" <<'PYREADY'
import json, sys
rows = json.load(open(sys.argv[1], encoding="utf-8"))
checked = [row for row in rows if row.get("sandbox") == "linux-bwrap"]
for row in checked:
    print(f"{row.get('name')}: ready={row.get('sandbox_ready')} detail={row.get('sandbox_readiness_detail')}")
if not checked:
    print("No linux-bwrap executor was reported; readiness could not be established")
sys.exit(0 if checked and all(row.get("sandbox_ready") is True for row in checked) else 1)
PYREADY
          then
            record sandbox pass "$(cat "$scratch/readiness.log")"
          else
            record sandbox fail "$(cat "$scratch/readiness.log")"
          fi
        else
          record sandbox fail "$(tail -c 2000 "$scratch/sandbox.log")"
        fi
        if python3 - "$scratch/repo" "$is_wsl" "$ORBIT_SMOKE_POWERSHELL" >"$scratch/dashboard.log" 2>&1 <<'PYWEB'
import socket, subprocess, sys, time, urllib.request
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
process = subprocess.Popen(["orbit", "web", "serve", "--host", "127.0.0.1", "--port", str(port), "--no-open"], cwd=sys.argv[1], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError("dashboard exited before it was ready")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                break
        except OSError:
            time.sleep(.1)
    else:
        raise RuntimeError("dashboard did not become ready")
    if sys.argv[2] == "true":
        if not sys.argv[3]:
            raise RuntimeError("powershell.exe is unavailable; Windows localhost forwarding was not checked")
        command = f"$ErrorActionPreference='Stop'; (Invoke-WebRequest -UseBasicParsing -Uri 'http://localhost:{port}/' -TimeoutSec 10).StatusCode"
        response = subprocess.run([sys.argv[3], "-NoProfile", "-NonInteractive", "-Command", command], capture_output=True, text=True, timeout=20, check=True)
        if response.stdout.strip() != "200":
            raise RuntimeError(f"Windows localhost returned {response.stdout!r}")
        print(f"Windows PowerShell fetched http://localhost:{port}/ (HTTP 200)")
    else:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=10) as response:
            if response.status != 200:
                raise RuntimeError(f"dashboard returned HTTP {response.status}")
        print(f"Linux fetched http://127.0.0.1:{port}/ (HTTP 200); Windows forwarding skipped")
finally:
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
PYWEB
          then
            record dashboard pass "$(cat "$scratch/dashboard.log")"
          else
            record dashboard fail "$(tail -c 2000 "$scratch/dashboard.log")"
          fi
        if $is_wsl; then
          if python3 - >"$scratch/windows-fs.log" 2>&1 <<'PYFS'
import pathlib, subprocess, tempfile
with tempfile.TemporaryDirectory(prefix="orbit-wsl-smoke-", dir="/mnt/c") as tmp:
    path = pathlib.Path(tmp)
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    (path / "smoke.txt").write_text("smoke\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(path), "add", "smoke.txt"], check=True)
    result = subprocess.run(["git", "-C", str(path), "status", "--porcelain"], capture_output=True, text=True, check=True)
    if "smoke.txt" not in result.stdout:
        raise RuntimeError("Git did not report the staged file")
print("Git can stage a file on /mnt/c; DrvFs remains unsupported for Orbit state due to permissions, case and locking limits. Keep Orbit workspaces and state on the Linux filesystem.")
PYFS
          then
            record windows_fs pass "$(cat "$scratch/windows-fs.log")"
          else
            record windows_fs fail "$(tail -c 2000 "$scratch/windows-fs.log")"
          fi
        else
          record windows_fs skipped '/mnt/c requires WSL'
        fi
      fi
    fi
  else
    record repository fail 'Could not initialize temporary Git repository'
  fi
else
  record install fail 'Node/npm or package installation failed'
fi
report
