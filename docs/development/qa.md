## Safe Mutable CLI Fixtures

Fixtures and manual reproductions that mutate Orbit task, run, workspace or registry state run isolated from the process that launches them; authorized operator work keeps normal routing. Hand-run QA from a managed lane follows the recipe in [Manual QA from a managed lane](#manual-qa-from-a-managed-lane). Spawn the CLI as a child with absolute disposable paths, clearing inherited authority before setting `HOME` and `USERPROFILE`:

```rust
use std::path::Path;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;

fn fixture_orbit(work: &Path, home: &Path) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}
```

`clear_inherited_authority` removes the managed-run routing, identity and grant variables, including `ORBIT_WORKER_CONTEXT_REQUIRED`. [`tool_list.rs`](../../crates/orbit-cli/tests/tool/tool_list.rs) uses this pattern; [`isolated_cli_fixture.rs`](../../crates/orbit-cli/tests/support/isolated_cli_fixture.rs) is a shared disposable home, registry and workspace. Orbit core's libtest fixtures use `application::tests::run_isolated_test`, which re-executes one exact test this way and verifies it passed in the child. CLI MCP and output bodies that write databases or change process authority use [`isolated_test::run_in_child`](../../crates/orbit-cli/tests/support/isolated_test.rs) before any setup; it verifies exactly one passed test and supplies a private home under the checkout's `.orbit/tmp/`, outside Bubblewrap's `/tmp` overlay. It includes ignored entry points only when their parent has already been selected by the caller.

Runtime integration permutations use `scenario_table!` in the existing runtime binary. Each named row keeps its setup and assertions; the shared [table fixture](../../crates/orbit-core/tests/runtime/scenarios.rs) re-executes one isolated table with inherited authority cleared, gives each sequential row a fresh HOME/cwd, restores its environment, and verifies one passed child. Families that install process-wide worker substitutions retain a separate child per row. Keep self-reexecuting entry points as standalone tests, since their exact libtest names are part of their fixture protocol. Table failures report the original row identity, and sandbox deferrals still reach the gate output. Each row and child prints its elapsed time, and a fixture may copy setup from a per-process template under the [fixture template rules](../design-patterns/test_strategy.md#quality-and-the-400-second-budget).

Shared mutable runtime fixture constructors refuse to run in the libtest parent. `scenarios::require_isolated_mutable_fixture` panics unless the process is already a re-executed child. `dispatch_admission::isolated` ([`dispatch_admission.rs`](../../crates/orbit-core/tests/runtime/dispatch_admission.rs)) sets `ORBIT_TEST_DISPATCH_ADMISSION_CHILD`. `scenario_table!` sets `ORBIT_TEST_TASK_FIXTURE_CHILD` and `ORBIT_RUNTIME_SCENARIO_ROW`; a row re-executed with only `ORBIT_RUNTIME_SCENARIO_ROW` still counts. The panic names this rule and tells the author to call `dispatch_admission::isolated` or place the case in a `scenario_table!` row. Covered constructors include `resident_cycle::fixture`, `recovery_fixture` (`fixture`, `fixture_with_config`, `fixture_with_forge`), `resident_authority::runtime_at`, the decision, script, tag-instruction, external-merge, attention, audit-commit, sandbox-off and review-gate fixture builders, `human_crew_work`'s fixture, and the dispatch-admission runtime builders (`runtime`, `configured_runtime`, `gap_fit::build`, `submission_notices::throttled_runtime`). A custom child that calls one of them sets `ORBIT_TEST_TASK_FIXTURE_CHILD` or `ORBIT_TEST_DISPATCH_ADMISSION_CHILD` on that child. New mutable setup goes through a covered constructor, or calls `require_isolated_mutable_fixture` before `OrbitRuntime::from_roots`.

Runtime fixtures create write-heavy temporary roots with `bulk_write_temp_dir()`. Runtime tables use the host build budget's resolved test-thread count, without a binary-wide serialization group. Record both wall and summed time under matched host build-budget settings when measuring the binary. The owner-read retry fixture uses the feature-gated `with_owner_read_retry_wait` hook to exercise the same retries in milliseconds; production keeps its one- and three-second waits. Outcome fixtures may opt into `with_pipeline_poll_wait` while retaining real elapsed time, deadlines and child/process transitions. Immutable proc fixtures use `with_host_load_sampling_wait` only when sampling cannot gain tick data by waiting; raw reads, CPU unknowns and reservation behavior remain real, and native `/proc` keeps the production interval. These callbacks have independent thread-local scopes and restore the previous callback on return or unwind. Changing-proc, polling-cadence and deadline controls keep system waits.

Before adding tasks or starting runs, make the checkout a Git repository (`git_repo::init(&work)`), run `workspace init`, then check routing read-only: `workspace show --format json` must report `registered: true`, and `checkout.repo_root` and `checkout.orbit_dir` must equal the canonicalized fixture paths (the child reports physical paths). This catches a fixture routed to an ambient workspace before it mutates anything.

For committed Git fixtures, use `git_repo::SeededRepo::new(&work).build()` from `tests/support/git_repo.rs`. The builder pins `main` and commits a README by default; use `branch`, `readme`, `empty_commit`, `no_commit` and `identity` for specialized seeds. `host_default_branch` explicitly preserves plain `git init` semantics where any host default is acceptable. Use `git_repo::run` for checked Git commands and trimmed stdout; both initialization and commands scrub inherited repository authority.

For task-list, search and rendering setup, use `tests/support/task_workspace.rs`. Its shared `TestWorkspace` verifies routing and `seed_tasks` writes a batch through the public runtime in one isolated child, including statuses and tags, without starting an `orbit` process for each record. Use the CLI explicitly when creation or tag parsing is itself the behavior under test. Threshold tests should pass a small explicit `--limit`; the default-limit and bare-array JSON case keeps its 60-record batch.

Detached-worker tests use `tests/support/workflow_gate.rs` to hold a step at a socket rendezvous through the existing `local_shell` action. Wait for readiness, assert the CLI's wait or detach behavior, then release the waiter and observe the terminal run. Socket and action deadlines only bound failures; successful execution does not wait for a sleep to expire. Release the waiter after interrupting its owning worker as well.

- **Every fixture checkout gets its own Git repository,** negative controls included. Root discovery walks past a plain directory or empty `.git`, so with `TMPDIR` inside another checkout (including a managed `.orbit/tmp`) the fixture resolves the enclosing Orbit root and `workspace init` refuses, or fails on a managed run's read-only `.orbit`. A tmpfs `TMPDIR` avoids this only when it is outside the checkout; it does not relax the fixture's own-root requirement. Never answer that refusal with `--force`. Ordinary lookup and the plugin's SessionStart hook still accept an `.orbit` at any ancestor, so a control that needs "no workspace above" must check that precondition and report a skip.
- **`export HOME=/tmp/...` is not isolation.** A managed child can inherit `ORBIT_MANAGED_RUN_CONTEXT` and `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE`, which carry durable authority and override home discovery. Never run bare mutable CLI commands in a managed worker ([regression test](../../crates/orbit-cli/tests/workspace/ambient_authority_isolation.rs)).
- **A private home does not make the workspace private.** The working directory still discovers a workspace by walking up, so a fresh `HOME` run from an uninitialized directory inside a real checkout opens that checkout's `.orbit` under an empty generation authority. A probe like this advanced a live workspace's layout while that workspace's own clients were running. Composition now refuses a pending layout migration under an authority that does not protect the workspace ([upgrades runbook](../runbooks/upgrades.md#the-authority-that-protects-a-workspace)), but other reads and writes still reach the discovered workspace. Give every fixture its own initialized repository under its temp root, as `McpWorkspace::init` does. Before running a candidate, resolve and assert every authority, workspace, store and cwd path: each must be under the fixture's temp root, and none may be the real home's `.orbit`. To reproduce a two-authority case, use two fixtures, as [`upgrade.rs`](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/upgrade.rs) does in `a_foreign_authority_cannot_migrate_a_discovered_workspace_under_live_clients`. Never aim a candidate at a live checkout, and never delete admission files or layout markers to undo a mistake.
- **Build every fixture Git command, reads included, with `git_repo::command()`** (`crates/orbit-cli/tests/support/git_repo.rs`), which scrubs authority first; set another initial branch with a scrubbed `symbolic-ref` before the first commit. Inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, index or object-directory variables can amend the parent repository or force-push to its origin.
  The shared-helper containment regression (`cargo test -p orbit-cli --test workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy`) re-executes one small fixture body with these locators aimed at a disposable sentinel. It exercises repository setup, commits/refs, status, ignore rules, push and linked worktrees through the shared helper, then checks the decoy HEAD, refs, config, index, worktrees, sentinel bytes and remote refs. Table rows cover all locators together and `GIT_COMMON_DIR` alone; process-wide scrubbing does not replace the command builder. Each CLI suite runs its ordinary boundary fixtures once, without another per-binary containment wrapper.
- **In core, engine, store, exec and web tests, use the crate's `tests/support/git.rs::command()` builder** for every fixture Git command, including reads and expected failures. The unit-test crate and integration binaries include that same private source as `git_test_support`; it uses `orbit_common::test_env::clear_inherited_authority` before callers set deliberate environment or arguments. Core also provides a bounded `run` helper for text-output setup, preserving config isolation without production argv admission. Calls that exercise a production Git API as the subject still use that API. Keep specialized stdin, output, branch, identity, hooks/template and transport controls at the caller. A disposable child's initial scrub does not replace scrubbing each Git command, and deliberate hostile `GIT_*` inputs belong after the builder's scrub.
- **Engine fixtures that publish Git branches** create repositories from scratch, verify the root before writing, and set `GIT_ALLOW_PROTOCOL=file` plus a file-only protocol policy in the child's disposable `HOME/.gitconfig` (the engine's VCS adapter clears Git variables but keeps `HOME`). [`git_fixture.rs`](../../crates/orbit-engine/tests/engine/git_fixture.rs) refuses any effective push URL, after rewrites, that is not an absolute local path; fetch failures use local upload-pack programs. Keep this policy when a test replaces its global Git config.
- **Change process-wide state only in a re-executed child,** never through a guard in the parallel libtest parent. Pass tool stub directories through `Command::env("PATH", ...)` when re-executing the test, as the engine VCS fixtures do; a scoped environment lock does not protect sibling readers. This covers forge-outage Git wrappers on `PATH` (a sibling could resolve the wrapper after its directory is dropped), tracing's callsite-interest cache in the log-capture fixture, and the Bubblewrap environment and credential-mask fixtures, whose launcher reads `HOME` and `CARGO_HOME`: they run with a cleared environment and a private `HOME` outside `/tmp`, and receive fixture variables through the child command.
- **Workflow-dispatch fixtures wait for every detached worker** to reach a terminal state in the fixture's run history before dropping the temporary home; a successful submission proves only admission. Capture all submitted IDs before assertions, including control-workspace runs, and use a cleanup guard that releases rendezvous waiters and settles workers on assertion unwinds. Wait for the owning processes to exit too, since terminal history can precede their final cleanup. The replica routine fixture exercises this with a held owner while the replica finishes. Check that the expected runs are recorded and that a disposable parent registry exposed through the managed environment gains none, as the output-goldens task-pilot fixture does.

`cargo test -p orbit-cli --test tool plugin_secrets::secret_sinks::seeded_secrets_stay_out` seeds fake provider, SCM, cloud and database credentials before redactor initialization, drives task writes and provider output through the CLI, and reports each leak by sink: task bundles, audit rows and blobs, run logs, attached diagnostics, dashboard log views and the provider's environment. Raw task attachments keep their byte-preserving contract. Its executor runs with `sandbox: off`.

### Manual QA from a managed lane

A managed lane exports Orbit routing and authority: `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`, `ORBIT_ROOT`, `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ID` and about two dozen more. Unsetting a few of them, or overriding only `HOME`, still lets a dev binary resolve the live global registry. Run from a run worktree with the real home, a candidate build also opens the host's primary workspace `.orbit` and global store, and opening them applies the candidate's pending layout and schema migrations: a read-only command or `web serve` is enough. Once they advance, the installed binary is refused on that host. Orbit refuses a breaking migration from a build that is not the installed orbit ([upgrades runbook](../runbooks/upgrades.md#a-build-that-is-not-the-installed-orbit)), but that guard is a backstop for hosts with an installed binary, not isolation. Run every candidate command, reads and `web serve` included, against a disposable home and its own workspace: clear the whole environment and point `HOME` at a disposable directory.

Workspace selection depends on the working directory, not only on `HOME`. Without an explicit root, `crates/orbit-runtime/src/base/path/resolve.rs` can select the main checkout's `.orbit/` from a linked worktree or discover an ancestor workspace by walking up. Changing `HOME` does not stop linked-worktree discovery. A home under a managed checkout's `.orbit/tmp/` therefore does not isolate a command run inside it: even a read-only command can open the primary checkout's live state. An explicit `--root` takes precedence over both discovery paths and selects the registry root too. The wrapper below supplies it to every candidate command, and every command runs from an independent disposable directory that is its own Git repository under the run's `.orbit/tmp/`. `HOME` also remains disposable for any home-relative files.

Run the steps below as one shell session, from the managed checkout:

```bash
CHECKOUT="$(git rev-parse --show-toplevel)"
ORBIT_BIN="$CHECKOUT/target/debug/orbit"   # cargo build -p orbit-cli --bin orbit --locked
mkdir -p "$CHECKOUT/.orbit/tmp"
QA_BASE="$(mktemp -d "$CHECKOUT/.orbit/tmp/manual-qa.XXXXXX")"
QA_HOME="$QA_BASE/home"
QA_REPO="$QA_BASE/repo"
QA_CWD="$QA_BASE/cwd"
mkdir -p "$QA_HOME" "$QA_REPO" "$QA_CWD"
qa() { env -i PATH=/usr/bin:/bin HOME="$QA_HOME" "$ORBIT_BIN" --root "$QA_REPO/.orbit" "$@"; }

# Make each scratch directory its own Git repository, so linked-worktree discovery and the
# uninitialized ancestor walk stop there instead of reaching the checkout (ORB-22643).
for dir in "$QA_REPO" "$QA_CWD"; do
  env -i PATH=/usr/bin:/bin HOME="$QA_HOME" git -C "$dir" init -q
done

# Mint the scratch root's machine identity first: `workspace init` does not create one,
# and `host list` and the dashboard's /api/hosts refuse until `orbit init` has run.
# Run it from QA_CWD, outside QA_REPO, so QA_REPO stays free of Orbit state.
(cd "$QA_CWD" && qa init --non-interactive --machine-name manual-qa --task-prefix MQA)

# Initialize the workspace in the scratch repository.
cd "$QA_REPO"
qa workspace init --name manual-qa

# Check all routing before previews or destructive commands.
qa config show --scope global --format json | jq -r '.global_root'
qa config show --format json | jq -r '.global_root, .shared_root, .local_root'
qa workspace show --format json | jq -r '.checkout.repo_root, .checkout.orbit_dir'
qa workspace list --format json
qa host list --format json

# Dashboard QA: serve only the scratch registry, from the scratch repository.
# Run it in the foreground and stop it with Ctrl-C; it must exit before cleanup.
# A script that drives a browser uses the lifecycle below instead.
qa web serve --port 0 --no-open

# Cleanup: once the server has exited, see the managed-lane rule below.
cd "$CHECKOUT"
```

With this explicit-root recipe, `global_root`, `shared_root` and `local_root` must all print `$QA_REPO/.orbit`; the real `~/.orbit` must never appear. `host list` must succeed rather than report a missing `[machine]` identity. If any check fails or prints another path, stop before running another command. The workspace must report `$QA_REPO` as its checkout. The scratch registry's workspace list must contain only `manual-qa`, with its checkout under `$QA_REPO`; no live workspace identity may appear. Do not add `--confirm` until those checks match. Preview a destructive command first, then confirm it with the same `qa …` wrapper. Stderr warnings such as "no actor identity resolved" are expected in a cleared environment.

`orbit init` mints the machine identity in the scratch root's global `config.toml` (`$QA_REPO/.orbit` under `--root`); `workspace init` does not. The `qa init` step passes `--non-interactive` with an explicit `--machine-name` and `--task-prefix`, so it never prompts. It runs from `QA_CWD`, not `QA_REPO`: do not run any other Orbit command in `QA_REPO` before `workspace init`. The first such command writes a `.orbit/config.yaml` with a generated workspace identity, and `workspace init` then refuses with an identity conflict. `QA_BASE`, `QA_HOME`, `QA_REPO` and `QA_CWD` all stay under the run's `.orbit/tmp/`, the only scratch location the implement activity allows (never `/tmp`), and outside Bubblewrap's `/tmp` overlay, as the fixtures' homes are. `mktemp` takes that explicit path, so an inherited `TMPDIR` cannot redirect it. Every candidate command uses the cleared environment and explicit root in `qa`. `--root` is resolved before every discovery step, so `qa` commands resolve `$QA_REPO/.orbit` from any working directory. The `QA_REPO` and `QA_CWD` Git repositories also stop the linked-worktree and uninitialized ancestor walks. The initialized-only lookup (`RequireInitialized` in [`resolve.rs`](../../crates/orbit-runtime/src/base/path/resolve.rs)) walks past a Git boundary, so a command run without `--root` from these directories could reach the checkout's `.orbit/`. The recipe runs none.

Cleanup: once the QA is finished and the server has exited, an operator shell deletes `QA_BASE` with `rm -rf -- "${QA_BASE:?}"`, which removes `QA_REPO` and `QA_CWD` with it. A managed lane must not run `rm -rf`: its cleanup rule forbids raw removal. `QA_BASE` is ignored scratch under the run's `.orbit/tmp/`, which the lane clears with the worktree, so the lane needs no further step.

#### Scripted dashboard QA: spawn, capture and teardown

A script that starts `web serve` for screenshots, instead of a person pressing Ctrl-C, must own the exact child from the spawn onward. The incident this guards against (F2026-11-362): a harness spawned a detached server, awaited readiness before entering its `try/finally`, closed the browser before stopping the server and sent a fallback KILL without waiting for the exit. Two isolated listeners survived with PPID 1. The recipe below keeps the isolation above (`qa`'s cleared environment and explicit `--root`, run from `QA_REPO`) and changes only who owns the process:

- **Arm teardown before the spawn.** The `EXIT` trap is installed first, so a readiness timeout, a browser failure, `set -e` or SIGINT/SIGTERM all reach it.
- **Record the PID and its start-time token as the next statements, with signals deferred across that interval.** The server is spawned with `exec` in a backgrounded subshell, so `$!` is the server itself, not a wrapper. SIGINT/SIGTERM arriving between the spawn and the token capture is recorded, not acted on, until the `(pid, token)` pair exists; the start then fails at once and teardown runs. Teardown signals only a PID whose token still matches (`/proc/<pid>/stat` field 22, or `ps -o lstart=` where `/proc` is absent), never a name pattern, process group or port owner. While the token is still empty the recorded PID is this script's own unreaped child, so it cannot have been reused and `kill -0` alone identifies it; teardown may also recover that PID from `$!` when the assignment itself was interrupted, but only once the start has armed that recovery and only when `$!` is not the caller's own earlier background job: a `( )` subshell inherits the caller's last background PID in `$!`, and with an empty token existence alone would accept it, so a cancellation before the spawn would SIGTERM a job the script never started.
- **Stop the server on every path, after the browser attempt.** The browser's close is attempted whenever it was started, its failure is recorded without skipping the server stop, and the first nonzero status is the run's status.
- **Wait for the exit.** SIGTERM, a bounded wait for the exit, then SIGKILL (sent only to the verified PID) and a second bounded wait. A server that survives SIGKILL fails the run loudly and keeps `SERVER_PID` set for a retry; nothing blocks on an unbounded `wait`.
- **Never delete artifacts.** Teardown touches only the process. Screenshots under `QA_BASE` stay for `orbit.task.artifact` action `put`; the managed-lane rule above still forbids raw removal.

For a static Jobs screenshot, use the prepared Playwright kit and wait for the rendered panel instead of page or network quiescence. The capture deadline is 20 seconds from Chromium launch, including page creation and evidence writing. Browser cleanup has a separate five-second bound; the Node process exits after that bound even if Playwright retains open handles, allowing server teardown to start. Navigate with `waitUntil: "domcontentloaded"`, observe the real `/api/events/stream` response, then wait for `#runs-body` to show its settled content: a `#runs-body .runs-row` when runs are in flight, or a `#runs-body .empty-state` on an idle workspace, where `#runs-count` reads "nothing in flight". Then wait for `#runs-count` to leave its placeholders (`-`, `—`) and its loading value (`…`). That pair means the Jobs › History request rendered (its default Running filter lists the runs in flight, or reports none); the `EventSource` remains open. `browser.close()` runs in `finally`, then `qa_serve_stop` sends TERM, waits at most five seconds, escalates to KILL, reaps the exact server child and confirms its port closed. A screenshot from a static fixture that stubs `/api/events/stream` proves only the rendered state; keep event delivery, reconnect and resync checks on the real stream.

```bash
# Define once, after `qa workspace init`. Run the capture below as `bash script.sh` or in a ( ) subshell.
QA_LOG="$QA_BASE/serve.log"; SHOTS="$QA_BASE/shots"; mkdir -p "$SHOTS"
SERVE_CMD=(env -i PATH=/usr/bin:/bin HOME="$QA_HOME" "$ORBIT_BIN" --root "$QA_REPO/.orbit" web serve --port 0 --no-open)
SERVER_PID=; SERVER_TOKEN=; QA_PORT=; QA_URL=; QA_DEFERRED_SIGNAL=; QA_STOPPED_PID=; QA_SPAWN_ARMED=; QA_SPAWN_PRIOR=

# Start-time token: the (pid, token) pair identifies one process, so a reused PID never matches.
qa_token() {
  if [ -r "/proc/$1/stat" ]; then sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f20
  else ps -o lstart= -p "$1" 2>/dev/null; fi
}
qa_defer_signal() { QA_DEFERRED_SIGNAL=1; }
qa_alive() {
  [ -n "$SERVER_PID" ] || return 1
  # No token yet: the PID is still our own unreaped child, so it cannot have
  # been reused; existence alone identifies it.
  if [ -z "$SERVER_TOKEN" ]; then kill -0 "$SERVER_PID" 2>/dev/null; return; fi
  [ "$(qa_token "$SERVER_PID")" = "$SERVER_TOKEN" ]
}
qa_wait_gone() {  # bounded poll: $1 = tenths of a second
  for _ in $(seq "$1"); do qa_alive || return 0; sleep 0.1; done; ! qa_alive
}
qa_listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# Spawn: `exec` makes the child the server itself, so $! is its PID.
qa_serve_start() {  # $1 = readiness deadline in seconds
  : >"$QA_LOG"
  # Defer INT/TERM until the (pid, token) pair exists: an exiting handler in
  # this interval would run teardown with no PID, or with a PID and no token.
  QA_DEFERRED_SIGNAL=
  trap qa_defer_signal INT TERM
  # A ( ) subshell inherits the caller's last background PID in $!. Remember it
  # (only while that job is alive, so it cannot be our child) and arm the $!
  # recovery in qa_serve_stop only now: before this point no server exists.
  QA_SPAWN_PRIOR=; if [ -n "${!:-}" ] && kill -0 "$!" 2>/dev/null; then QA_SPAWN_PRIOR="$!"; fi
  QA_SPAWN_ARMED=1
  ( exec "${SERVE_CMD[@]}" ) >"$QA_LOG" 2>&1 </dev/null &
  SERVER_PID=$!
  SERVER_TOKEN="$(qa_token "$SERVER_PID")"
  trap 'exit 143' INT TERM
  if [ -n "$QA_DEFERRED_SIGNAL" ]; then echo "interrupted while capturing the server identity" >&2; return 143; fi
  for _ in $(seq $(($1 * 10))); do
    qa_alive || { echo "server exited before readiness" >&2; return 1; }
    QA_URL="$(sed -n 's/^Dashboard listening on \(http:\/\/[^ ]*\).*/\1/p' "$QA_LOG" | head -n1)"
    [ -n "$QA_URL" ] && { QA_PORT="${QA_URL##*:}"; return 0; }
    sleep 0.1
  done
  echo "no readiness announcement within $1s" >&2; return 1
}

# Teardown: idempotent; signals only the recorded (pid, token); waits for the exit.
qa_serve_stop() {
  local pid="$SERVER_PID" rc=0
  # Interrupted before the assignment above, after the spawn was armed: $! names
  # the spawned child, which is unreaped and identified by existence while it
  # has no token yet. Never adopt the caller's earlier job that a ( ) subshell
  # inherits in $!, nor any $! before the spawn is armed.
  if [ -z "$pid" ] && [ -n "$QA_SPAWN_ARMED" ] && [ -n "${!:-}" ] && [ "${!:-}" != "$QA_SPAWN_PRIOR" ] \
     && [ -z "$SERVER_TOKEN" ] && [ "${!:-}" != "$QA_STOPPED_PID" ]; then pid="$!"; SERVER_PID="$pid"; fi
  [ -n "$pid" ] || return 0
  if qa_alive; then
    kill -TERM "$pid" 2>/dev/null
    if ! qa_wait_gone "${QA_TERM_GRACE:-50}"; then
      echo "server ignored SIGTERM; sending SIGKILL" >&2
      qa_alive && kill -KILL "$pid" 2>/dev/null
      qa_wait_gone 50 || { echo "server $pid survived SIGKILL" >&2; rc=1; }
    fi
  fi
  [ "$rc" -eq 0 ] && { wait "$pid" 2>/dev/null || :; } # reaps it; TERM/KILL wait statuses are expected
  if [ "$rc" -eq 0 ] && qa_alive; then rc=1; fi
  if [ -n "$QA_PORT" ] && qa_listening "$QA_PORT"; then echo "port $QA_PORT still listening" >&2; rc=1; fi
  [ "$rc" -eq 0 ] && { SERVER_PID=; QA_STOPPED_PID="$pid"; }
  return "$rc"
}

browser_shoot() {  # $1 = dashboard URL; $2 = screenshot; $3 = elapsed-time evidence JSON
  . "$ORBIT_BROWSER_KIT_ENV"
  node --input-type=module - "$@" <<'NODE'
import { writeFile } from 'node:fs/promises';
import { writeSync } from 'node:fs';
import { inspect } from 'node:util';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
const [baseUrl, screenshotPath, evidencePath] = process.argv.slice(2);
const started = performance.now();
const deadline = Date.now() + 20_000;
const remaining = () => {
  const ms = deadline - Date.now();
  if (ms <= 0) throw new Error('20-second screenshot deadline expired');
  return ms;
};
let browser;
let streamClosed = false;
let captureTimer;
let cleanupTimer;
const capture = async () => {
  browser = await chromium.launch({ timeout: remaining(), args: ['--no-zygote', '--disable-gpu', '--disable-dev-shm-usage'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const streamPromise = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return url.pathname === '/api/events/stream' && (response.headers()['content-type'] || '').includes('text/event-stream');
  }, { timeout: remaining() });
  // Attach a rejection handler immediately, including when navigation fails first.
  void streamPromise.catch(() => {});
  await page.goto(`${baseUrl}/#jobs/history`, { waitUntil: 'domcontentloaded', timeout: remaining() });
  const stream = await streamPromise;
  if (stream.status() !== 200) throw new Error(`event stream returned ${stream.status()}`);
  void stream.finished().then(() => { streamClosed = true; }, () => { streamClosed = true; });
  // Settled means rows (runs in flight) or the empty state (an idle workspace):
  // the Running filter renders no .runs-row when nothing is in flight.
  await page.waitForSelector('#runs-body .runs-row, #runs-body .empty-state', { state: 'visible', timeout: remaining() });
  await page.waitForFunction(() => {
    const count = document.querySelector('#runs-count')?.textContent?.trim();
    return Boolean(count && !['-', '—', '…'].includes(count));
  }, undefined, { timeout: remaining() });
  await page.screenshot({ path: screenshotPath, fullPage: false, timeout: remaining() });
  if (streamClosed) throw new Error('dashboard event stream closed before screenshot capture');
  const evidence = {
    url: `${baseUrl}/#jobs/history`,
    readiness: '#runs-body .runs-row or .empty-state visible and #runs-count settled',
    event_stream_url: stream.url(),
    event_stream_status: stream.status(),
    event_stream_content_type: stream.headers()['content-type'],
    event_stream_open_at_screenshot: !streamClosed,
    viewport: { width: 1280, height: 900 },
    screenshot: screenshotPath,
    elapsed_ms: Math.round(performance.now() - started),
    deadline_ms: 20_000,
  };
  await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
  writeSync(1, `${JSON.stringify(evidence)}\n`);
};
let captureError;
let cleanupError;
try {
  await Promise.race([
    capture(),
    new Promise((_, reject) => {
      captureTimer = setTimeout(() => reject(new Error('20-second screenshot deadline expired')), remaining());
    }),
  ]);
} catch (error) {
  captureError = error;
} finally {
  clearTimeout(captureTimer);
  if (browser) {
    try {
      await Promise.race([
        browser.close(),
        new Promise((_, reject) => {
          cleanupTimer = setTimeout(() => reject(new Error('5-second browser cleanup deadline expired')), 5_000);
        }),
      ]);
    } catch (error) {
      cleanupError = error;
    } finally {
      clearTimeout(cleanupTimer);
    }
  }
}
if (captureError) writeSync(2, `Screenshot capture failed: ${inspect(captureError)}\n`);
if (cleanupError) writeSync(2, `Browser cleanup failed: ${inspect(cleanupError)}\n`);
// Playwright can retain handles after a timeout. Do not let them delay server teardown.
process.exit(captureError || cleanupError ? 1 : 0);
NODE
}

# Capture. The browser helper owns Chromium; the EXIT trap owns the server child.
trap qa_serve_stop EXIT                  # armed before the spawn
trap 'exit 143' INT TERM
cd "$QA_REPO"; rc=0
qa_serve_start 30 || rc=$?                                    # readiness timeout or early exit
if [ "$rc" -eq 0 ]; then
  browser_shoot "$QA_URL" "$SHOTS/jobs-history-running-dark-1280.png" "$QA_BASE/jobs-history-capture.json" || rc=$?
fi
qa_serve_stop || { st=$?; [ "$rc" -ne 0 ] || rc=$st; }
exit "$rc"
```

Calling `qa_serve_stop` explicitly before `exit` reports its failure in the run's status; the trap is the backstop for every earlier exit and is a no-op once the explicit call has cleared `SERVER_PID`. If the script itself is SIGKILLed no trap can run, so check for an orphan afterwards (below).

Verify teardown with probes on the exact child, with no `ps`, `pgrep`, `lsof` or `ss` pattern and nothing global, so they work inside the supported sandbox: record `pid=$SERVER_PID port=$QA_PORT` before stopping, then after it `kill -0 "$pid"` must fail (the child is reaped, not a zombie) and `qa_listening "$port"` must fail (a loopback connect, refused once the listener is closed). To rehearse a failure path, point `SERVE_CMD` at a stub: `(sh -c 'exec sleep 300')` never announces and exercises the readiness timeout; `(sh -c 'trap "" TERM; echo "Dashboard listening on http://127.0.0.1:1"; exec sleep 300')` ignores SIGTERM and exercises the fallback KILL with `QA_TERM_GRACE=5`; a bad flag such as `--port notaport` exits early. A rehearsal on Linux in a disposable workspace (success, browser-init failure, browser-close failure, readiness timeout, early exit and a SIGTERM-ignoring child, plus SIGTERM to the script mid-readiness) left each child gone and each listener closed, with captured files intact. The `dashboard_qa_recipe_reaps_child_when_interrupted_at_identity_boundaries` regression in `crates/orbit-cli/tests/process/web_serve_shutdown.rs` drives the extracted recipe deterministically. A Bash `DEBUG` hook (with `set -T`) sends SIGTERM just before the spawn, just before the PID assignment and just before the token assignment. The normal-path stub owns its listener: it binds an ephemeral loopback port, announces that port and keeps it open, so an unrelated local service on any fixed port cannot change the "still listening" teardown probe; the capture stub connects to the announced port, which proves the listener was open, and the run must still exit 0 once teardown has reaped the child. For the two post-spawn signals the stub blocks on a FIFO before publishing its PID, and the hook records `$!`, so the Linux subreaper parent can still prove that exact child was reaped. The same parent classifies an intentionally unreaped child — the recipe subshell SIGKILLed after a descendant exists, or the runner's timeout taken once that unpublished child is recorded — then terminates and reaps the owned process group, including a descendant that ignores SIGTERM. The recipe runs in a `( )` subshell with a caller background job started first. A `wrapper_stall` case stops the outer wrapper (`SIGSTOP`) right after it starts that job, so only the runner's own timeout ends it; the sweep must reap the wrapper and the caller's job so output collection reaches EOF. The parent asserts the harness result, that the caller's job is untouched on the paths that return to it, and that the capture directory survives.

