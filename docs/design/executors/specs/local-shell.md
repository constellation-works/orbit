---
type: design
summary: "Spec: the local-shell executor and the local_shell deterministic action"
tags: ["executors", "activity-job"]
last_validated: 2026-09-05
---

# Spec: `local-shell`

Deterministic local command execution for v2 jobs. A `local_shell` activity runs
one child process and reports its stdout, stderr, and exact exit status. It is
the supported replacement for the v1 `cli_command` executor that
[ORB-10395] deleted along with the rest of the v1 executor stack.

A shell step is **not** an agent. It receives no prompt, no model, no tool
allowlist, and none of the Orbit registry/workspace identity variables a CLI
agent is launched with. Model and provider selection stay entirely on the
`agent_loop` path.

## Two moving parts

| Artifact | What it decides |
| --- | --- |
| The `local-shell` **executor definition** (`kind: Executor`) | The environment additions, the fallback program, a static argument prefix, the default timeout, and — if declared — the OS sandbox every shell step runs under. |
| The **activity** (`kind: Activity`, `type: deterministic`, `action: local_shell`) | The program and arguments for this step, its working directory, its explicit environment, and its timeout. |

The shipped definition declares no `command` and no `sandbox`, so out of the box
each activity names its own program and the child runs bare under Orbit's
process supervisor.

## Activity configuration

Everything below lives in the activity's `config:` block.

| Key | Type | Meaning |
| --- | --- | --- |
| `command` | string | `argv[0]`. Executed directly — no shell is involved, so nothing is word-split, globbed, or expanded. |
| `args` | string[] | Literal arguments, appended after the executor definition's static prefix. |
| `shell` | string | Interpreter for script execution, e.g. `/bin/sh`. Mutually exclusive with `command`. |
| `script` | string | Script text, handed to the interpreter as `<shell> -c <script>`. Required with `shell`; `args` is rejected alongside it. |
| `executor` | string | Executor definition to resolve. Defaults to `local-shell`. |
| `cwd` | string | Working directory, relative to the resolved workspace root unless absolute. Must stay inside that root. Defaults to the root itself. |
| `env` | map | Explicit entries layered onto the resolved `[execution.env]` baseline. A name already in the baseline is replaced, not duplicated. |
| `timeout_ms` | integer | Wall-clock budget, 1 ms to 3,600,000 ms. Falls back to the definition's `timeout_seconds`, then to 600,000 ms. |
| `allow_nonzero_exit` | boolean | When true a nonzero exit returns structured output instead of failing the step. Default false. |

### Argv execution and shell execution are separate declarations

`command` + `args` is an `execve`. `shell` + `script` is an explicit request for
an interpreter. Orbit never joins `args` into a command line and never wraps a
`command` in a shell, so a value that happens to contain `;`, `$(…)`, or a glob
is an ordinary argument byte string.

### Step input never reaches argv

The program and its arguments come from `config` only. A job step's `with:`
block is rendered from templates and can carry text an agent produced; letting
it reach `argv` would make every shell step an injection surface. Runtime input
contributes exactly one thing: `workspace_path`, the checkout to run in. This is
the same selector the deterministic VCS actions use, so a `local_shell` step and
a `git_commit` step in one job always agree on which checkout they are looking
at. Absent that key, the step runs in the registered repository root.

A job that needs a different command declares a different activity.

## Output

```json
{
  "success": true,
  "exit_code": 0,
  "stdout": "…",
  "stderr": "…",
  "duration_ms": 42,
  "timed_out": false,
  "timeout_ms": 60000,
  "argv": ["git", "status", "--porcelain"],
  "cwd": "/abs/path/to/checkout",
  "sandbox": "none"
}
```

`sandbox` names the backend the child actually ran under (`linux-bwrap`,
`macos-sandbox-exec`, `bare-fallback`, or `none`), so a run record never leaves
the containment question implicit.

A nonzero exit, a timeout, or a child terminated by a signal fails the step
unless `allow_nonzero_exit` is set; the failure message names the program, the
directory, and the status. A missing or non-executable program fails before
anything runs.

## Supervision

Execution goes through the same `orbit-exec` machinery the CLI agent runner
uses, not a second implementation:

- The child is spawned as a process-group leader.
- stdout and stderr are drained on background threads and subject to Orbit's
  output-capture limit.
- stdin is closed immediately. A shell step is non-interactive, and a child
  reading an open-but-silent stdin would only ever end at the deadline.
- The wall-clock deadline sends `SIGTERM` to the whole process group, waits out
  a grace period, then `SIGKILL`s it — background grandchildren included.
- A signal delivered to Orbit itself cancels the child the same way and is
  reported as exit status `128 + signal`.

## Filesystem authority

Two independent limits apply.

1. **`cwd` containment.** The resolved working directory is canonicalized and
   must stay under the resolved workspace root. This is enforced regardless of
   sandboxing, because the shipped definition declares none.
2. **The activity's `fsProfile`.** It is resolved against the active policy and
   handed to sandbox compilation exactly as it is for a CLI agent. If the
   executor definition declares a `sandbox`, the child runs inside it; if the
   OS primitive is unavailable, the step fails closed unless the definition sets
   `allow_fallback: true`.

To confine shell steps at the kernel, add a sandbox to the definition and give
the activity a profile:

```yaml
# kind: Executor, metadata.name: local-shell
spec:
  executor_type: local_shell
  sandbox: macos-sandbox-exec   # translated to linux-bwrap on Linux at seed time
  allow_fallback: false
```

## Working example

`crates/orbit-core/assets/activities/examples/local_shell_reference.yaml` ships
as the runnable reference:

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: local_shell_reference
spec:
  type: deterministic
  description: Deterministic local command execution reference.
  fsProfile: implementer
  action: local_shell
  config:
    command: git
    args: ["status", "--porcelain"]
    cwd: "."
    env:
      GIT_OPTIONAL_LOCKS: "0"
    timeout_ms: 60000
    allow_nonzero_exit: false
```

Script form:

```yaml
  config:
    shell: /bin/sh
    script: |
      set -eu
      make ci-fast
    timeout_ms: 900000
```

## Compatibility with `cli_command` definitions

`local_shell` is the current spelling of the executor family that older
definitions call `cli_command`. `ExecutorType` accepts `cli_command` as an
alias, so a bundled or user-authored definition written before [ORB-11294] loads
unchanged; it is re-serialized under the canonical `local_shell` name the next
time it is written.

Such a definition's fields keep working as step defaults:

- `command` → the program for a step that names none.
- `args` → a static prefix in front of the step's own arguments.
- `env` → environment additions.
- `timeout_seconds` → the default wall-clock budget.

Re-seeding never overwrites an installed `local-shell` definition, so operator
customizations survive upgrades.

Agent-shaped fields on a shell definition (`model_pair_override`, `model_flag`,
`stdout_format`) are inert: nothing on this path reads them. The two resolution
boundaries do not cross — an agent executor cannot back a `local_shell` step,
and a `local_shell` definition cannot back an `agent_loop` activity.
