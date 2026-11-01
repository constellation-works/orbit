---
title: Schedule recurring work
description: "Run Orbit unattended: the host scheduler clock, routines that fire jobs on a schedule, and auto-task definitions that mint tasks."
sidebar:
  order: 4
---

A per-machine **host scheduler clock** drives all scheduled work. On each tick, due
**routines** start job runs and due **auto-tasks** file tasks. Every routine
and auto-task Orbit seeds starts disabled. [Routines and Auto-Tasks](../../concepts/scheduling/) explains
the model; this page is the procedure.

:::tip[Let your agent set it up]
Ask your agent to **install the Orbit clock and turn on worktree GC**, or to
**file a dependency audit every Monday**. The `orbit-setup` skill installs the
clock and enables the routines and auto-tasks you ask for. After that, the
dashboard's **Automation** view toggles routines and auto-tasks, pauses,
enables, or retunes the clock, and has **Mint now** for any auto-task; see
[Use the Dashboard](../dashboard/). The commands below are the fallback.
:::

## 1. Install the clock

```bash
orbit routine init --install-clock
```

This installs a per-user unit (launchd on macOS, a systemd user timer on
Linux) that runs `orbit clock tick` every minute. Each tick reads routines and
auto-tasks from every registered owner checkout on this machine. Nothing is
scheduled until the clock runs.

```bash
orbit clock status
orbit clock pause                       # stop scheduled ticks
orbit clock enable
orbit clock set --cadence-seconds 300   # whole minutes only; a paused clock stays paused
```

A paused clock still allows a manual `orbit clock tick`, and pausing it leaves
each routine's own pause state alone.

The unit runs Orbit by absolute path. If that binary moves or is removed,
ticks stop while `orbit clock status` can still report the clock as enabled.
`orbit update` repairs the unit; run `orbit clock repair` after installing
Orbit some other way. Repair never resumes a clock you paused.

To try a change, run a tick by hand:

```bash
orbit clock tick --dry-run --verbose     # what would fire; writes nothing
orbit --workspace <name> clock tick      # only that workspace's schedules
```

## Scheduled credentials

Use this when a scheduled run needs a credential you normally export in your
login shell — a provider token, an API key, or a `GITHUB_TOKEN`. The clock
does not inherit that environment: launchd (macOS) or a systemd user timer
(Linux) starts `orbit clock tick` with no login shell, so a variable you
exported interactively never reaches the tick or the runs it starts. The
installed unit file never contains a secret.

Instead, the tick reads one machine-global file, `~/.orbit/clock.env`, beside
the global `config.toml`. It is not per-workspace, not in any checkout, and
never synced. Installing the clock creates it comment-only and owner-only;
reinstalling never overwrites an existing file:

```bash
orbit routine init --install-clock   # creates ~/.orbit/clock.env if absent
chmod 600 ~/.orbit/clock.env         # owner read/write only; the tick requires this
```

Edit it with one `NAME=value` per line, using placeholders here and real
values only in the file itself:

```ini
# ~/.orbit/clock.env — never commit or share this file
CLAUDE_CODE_OAUTH_TOKEN=<token-from-claude-setup-token>
ANTHROPIC_CREDITS_API_KEY_1=<api-key-for-the-credits-crew>
GITHUB_TOKEN=<github-token>
```

A name in the file reaches a run only when Orbit admits it, in one of two
ways:

- **Shared `pass` list.** Add the name to the effective
  [`execution.env.pass`](../../reference/config/#settable-keys) for the
  workspace. That list replaces the default rather than extending it, so
  restate the baseline names:

  ```bash
  orbit config set execution.env.pass \
    '["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "CLAUDE_CODE_OAUTH_TOKEN", "GITHUB_TOKEN"]'
  ```

  `execution.env.pass` is a security key: once a workspace `config.toml`
  exists, its list is the one that counts for that workspace, not the global
  one. A token stored under a name no workspace's `pass` admits is loaded
  for nobody.
- **Per-crew source names.** A crew's `env` entry (global config only) names
  a source the tick also reads from `clock.env`, without adding it to `pass`.
  With `env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]` on a crew,
  that crew's agents receive `ANTHROPIC_API_KEY` from the
  `ANTHROPIC_CREDITS_API_KEY_1` line above; they never receive the source
  name itself, and other crews are unaffected. Set it with
  `orbit config set --global crews.<name>.env '["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]'`.
  See [the `clock.env` reference](../../reference/config/#scheduled-credentials-clockenv).

Service and login environments combine like this: a non-empty value already
in the tick's process environment wins, an empty or absent one is filled
from `clock.env`, and the tick never changes its own environment — values
pass only as defaults to the child runs it starts. A dry run loads nothing,
so `orbit clock tick --dry-run` never reports loaded credentials. By
contrast, a drain or ship run you start by hand does not read `clock.env`:
start it from a login shell, or expect unset-name warnings on the run.

Verify with names only — no command prints a value:

```bash
orbit clock tick --json   # a real tick; its clock_env_loaded lists loaded names
orbit doctor              # env-pass and, on macOS, claude-worker-token rows
```

`orbit doctor`'s `env-pass` row checks the environment doctor itself runs
in. A refused `clock.env` — a symlink, a file owned by another user, or one
readable by group or others — is a load error on the tick: the tick still
runs, but loads nothing from the file, and the error tells you to
`chmod 600` it. Sandboxed agents cannot read `clock.env` itself; they only
receive the admitted names as environment variables.

### Claude on macOS

On macOS, give scheduled Claude runs a dedicated credential. Create a
long-lived token and store it as `CLAUDE_CODE_OAUTH_TOKEN`:

```bash
claude setup-token   # requires a Claude subscription; prints the token once
```

Put that token in `~/.orbit/clock.env` and admit `CLAUDE_CODE_OAUTH_TOKEN`
in `execution.env.pass`, as above. An `ANTHROPIC_API_KEY` admitted the same
way, or a per-crew `env` mapping that supplies either name, also satisfies
this. A Claude activity that receives neither is refused before `claude`
starts, rather than falling back to the Claude Desktop login, which the
Desktop revokes mid-run; a drain on such a host excludes those Claude crews
instead of claiming work it cannot run. `orbit doctor`'s
`claude-worker-token` row warns ahead of time, naming the `pass` list or
`clock.env` that lacks the token, or a crew credential source `clock.env`
lacks. On Linux this row is skipped: only macOS shares a Desktop login with
the `claude` CLI.

## 2. Turn on routines

`orbit workspace init` seeds the default routines into `.orbit/routines/`: task
pilot, finding verification, pilot-warning triage,
worktree GC, store GC (daily audit and run-state retention), the
CI-failure and dependency-alert sweeps, crew-performance refresh,
[task publication](../task-publication/#scheduled-publication) (weekly Sunday
at 04:47 host-local time, disabled until you opt into publishing task prose;
attachments default to `omit`), and the `ux-rules` routine (daily at 06:40,
which does no work until you grant its runner script and choose the surfaces
it measures).
All start disabled except `crew_performance_refresh`, which runs hourly.
With `workflow.crew_performance` set to `off`, that routine skips its scan and
writes no snapshot, but still records a job run. `orbit workspace sync`
refreshes the routines after an upgrade and keeps your edits.

Turn one on with its switch in **Automation → Routines**, or set
`enabled: true` in its YAML. Git ignores `.orbit/`, so the change applies to
this checkout on this machine. Start with worktree GC, and turn on the ship
sweep last (see [Ship unattended](#ship-unattended)).

![Automation → Routines: the next hour's fires on a timeline, then each routine with its switch, cadence, next fire, and last run.](../../../assets/dashboard/dashboard-automation.png)

```bash
orbit routine list            # enabled, paused, next due, last fire
orbit routine show <name>
```

### Write a routine

```yaml
schemaVersion: 1
name: ci_failure_sweep_myrepo
description: File failing CI runs as proposed
  repair tasks.
enabled: true
trigger:
  cron: "5 * * * *"     # 5-field cron, host-local time
  missed_run: skip      # or catch_up_once
target: job:ci_failure_sweep_pipeline
policy:
  timeout_minutes: 30
  overlap: forbid       # or allow
  retries:
    max: 0
    backoff_minutes: 15
```

- **`target`** must be a job (`job:<name>`). To schedule one activity, wrap it
  in a one-step job.
- **`missed_run`** handles slots missed while the machine slept. `skip`, the
  default, waits for the next slot and records each missed one with the reason
  `missed_run: skip` (`orbit routine list --json` shows it on `last_fire.detail`).
  `catch_up_once` fires one make-up run. The seeded worktree-gc routine uses
  `catch_up_once`, so a gap runs GC once. `orbit doctor` warns when an enabled
  routine's last success is older than three times its schedule interval.
- **`overlap: forbid`**, the default, skips a fire while the previous one is
  still running. After `timeout_minutes`, the stuck fire is recorded as timed
  out and stops blocking.
- **There is no host field.** Every machine with a registered owner checkout
  and its clock on evaluates the routine against its own store. To keep it
  off one machine, run `orbit routine pause <name>` there;
  `orbit routine resume <name>` undoes it. Pauses stay on that machine, are
  never synced, and survive reboots. To retire a routine, set
  `enabled: false`.

An invalid file never fires with defaults; Orbit skips it. Field-by-field
shapes for cron, state, and delivery triggers are in
[Scheduling YAML](../../reference/scheduling-yaml/).

## 3. Define recurring chores as auto-tasks

An auto-task is a task template that the clock files on a schedule. A new
chore is a new definition, never new code or a new routine. The tick files
tasks itself, so there is no routine to enable. The file's schedule,
template, dedupe, and preconditions are in
[Scheduling YAML](../../reference/scheduling-yaml/).

For ready-made repository audits, choose a
[built-in maintenance sweep](#built-in-maintenance-sweeps) before writing your own.

```bash
orbit auto-task add \
  --name weekly-dep-audit \
  --cron "0 9 * * 1" \
  --title "Audit outdated dependencies" \
  --body "Audit the lockfile and file follow-up tasks." \
  --criterion "List outdated packages and their latest versions." \
  --criterion "File follow-up tasks for known advisories." \
  --tag maintenance
```

Give exactly one trigger: `--cron`, `--every-minutes`, or
`--deliveries-landed '<JSON>'`, which fires once verified deliveries land on a
branch. A cron or interval definition is enabled as soon as you add it. A
delivery-triggered one starts disabled; check it with
`orbit auto-task show <name> --preview` before you enable it.

| Option | Effect |
|---|---|
| `--status` | Status of each filed task: `backlog` (default), or `proposed` to approve each one yourself. |
| `--dedupe` | `skip-if-open` (default) skips a fire while the previous task is still open, so a stalled backlog never collects copies. `always` files regardless. |
| `--crew` | Crew for filed tasks. |
| `--priority` | `low`, `medium` (default), `high`, or `critical`. |
| `--tag` | Extra tag on each filed task, beside the `auto-task:<name>` tag. Repeatable. |
| `--required-tools` | Exact canonical tool names copied to each filed task. |

### Mint one now

```bash
orbit auto-task mint weekly-dep-audit
```

This files one task immediately, like **Mint now** in the dashboard. It
ignores the schedule, the dedupe policy, and `enabled`, and it does not move
the next scheduled fire. Use it to see what a definition produces before you
leave it running.

### Change, turn off, or delete

```bash
orbit auto-task list --enabled
orbit auto-task show weekly-dep-audit
# Change only the fields you pass.
orbit auto-task update weekly-dep-audit --cron "0 9 * * 2"
# Keep the definition and its history when toggling it off.
orbit auto-task toggle weekly-dep-audit off
orbit auto-task delete weekly-dep-audit --reason "moved to Renovate"
```

`delete` removes the definition and its scheduler cursor, and writes an audit
record. It refuses while a task filed from the definition is still open and
names those tasks; `--force` deletes anyway and leaves them alone.

Deleting a shipped default, such as `code-review` in a repository with no
code, also records an opt-out: later `orbit workspace init --force` and
`orbit workspace sync` runs leave it out, and `orbit doctor` does not report it
missing. `orbit auto-task restore <name>` brings it back, disabled as shipped.

### Repair a delivery-triggered definition

A `--deliveries-landed` definition keeps a ledger of the deliveries it still
owes a review. `recover` and `reset` only preview until you pass `--reason`:

```bash
# Resume after a settings change; this keeps the debt.
orbit auto-task recover <name> --adopt-settings --reason "<why>"
# Reconcile a rebased branch; this keeps the debt.
orbit auto-task recover <name> --replay-history --reason "<why>"
# Forget the debt and re-baseline at the branch head.
orbit auto-task reset <name> --reason "<why>"
# Waive one settled failed batch.
orbit auto-task update <name> \
  --waive-batch <batch-id> \
  --waiver-reason "<why>"
```

`recover --reissue-action` re-files a settled action that closed without
accepted evidence. `reset --force` abandons an executing action instead of
cancelling it. Deleting the definition runs the same audited reset, so
`delete` refuses whenever `reset` would. The
[auto-task reference](https://github.com/constellation-works/orbit/blob/main/plugin/skills/orbit-setup/references/auto-tasks.md)
covers stalls and the shipped definitions.

## Built-in maintenance sweeps

Use these six shipped auto-task definitions to find maintenance work in your
repository. They discover its languages, manifests, conventions, and CI
configuration rather than assuming a particular toolchain. Start with the
sweep that matches the decision you need to make:

| Definition | Choose it when… | Shipped schedule |
|---|---|---|
| `code-health-sweep` | You need evidence of dead code, duplication, unnecessary complexity, local architectural drift, or compatibility paths ready for retirement. | Every 4,320 minutes (3 days). |
| `architecture-audit` | You need a package/module dependency graph and evidence for cycles, layering drift, misplaced abstractions, or package splits and merges. | Every 20,160 minutes (14 days). |
| `structure-sweep` | Oversized or misgrouped source and test files make navigation and maintenance difficult. | Every 10,080 minutes (7 days). |
| `dependency-freshness` | You need an inventory of outdated, unused, or duplicate dependencies and small upgrade/removal proposals. | `0 9 1 * *` — 09:00 on the first of each month. |
| `flaky-test-hunt` | Hosted CI tests fail and then pass without a relevant change; you need failure-log evidence for fixes. | `15 8 * * 1` — Monday at 08:15. |
| `ci-budget-watch` | You need hosted test-job timings compared with your repository's documented CI budget. | `45 1 * * *` — daily at 01:45. |

All six ship **disabled**, with `skip-if-open` dedupe and assessment tasks
entering `backlog`. Cron times use the host's local timezone; intervals are
elapsed minutes, not fixed weekdays. The clock files a task; an agent must
execute that task through your normal workflow to perform the assessment.
The assessments gather evidence and file follow-up work, without editing
your source, dependencies, tests, or CI configuration. They do not authorize
dispatch, merge, or release.

### Inspect, preview, try, then enable

Run these commands from your registered owner checkout. After an Orbit
upgrade, inspect and apply workspace synchronization to get the current
shipped definitions:

```bash
orbit workspace sync --check
orbit workspace sync
orbit auto-task list
orbit auto-task show dependency-freshness
orbit auto-task show dependency-freshness --json
orbit clock tick --dry-run --verbose
```

`sync --check` writes nothing and exits 3 when convergence is pending. The
JSON `show` output includes the full template, criteria, and required tools;
the text view shows the definition source, body management, settings, and
schedule. `clock tick --dry-run --verbose` previews scheduling decisions
without filing tasks. Disabled definitions do not fire. `show --preview`
adds baseline/source diagnostics for delivery-triggered definitions; these
six time-based definitions have no delivery baseline to preview.

Try one assessment before leaving it scheduled:

```bash
orbit auto-task mint dependency-freshness
# After reviewing the task it creates, optionally change the cadence.
orbit auto-task update dependency-freshness --cron "0 9 * * 1"
orbit auto-task toggle dependency-freshness on
orbit auto-task show dependency-freshness
# Stop future scheduled filings while keeping the definition and history.
orbit auto-task toggle dependency-freshness off
```

`mint` creates a real task immediately: it ignores the schedule, `enabled`,
and dedupe, and leaves the scheduler cursor unchanged. It does not execute
the assessment. An open manually minted task prevents later scheduled
copies under `skip-if-open`. Turning a definition off does not cancel tasks
already filed. Install and enable the [host clock](#1-install-the-clock)
before expecting scheduled filings.

Use `update` and `toggle` to customize a shipped definition. Operator
settings such as cadence, enabled state, crew, priority, and complexity are
kept separately from its managed body, so `workspace sync` can refresh the
shipped instructions while retaining those settings. Editing the body or
criteria creates a local fork that sync preserves; it no longer receives
automatic body refreshes. Inspect `show` and sync's report after upgrading.
Deleted shipped definitions stay opted out until you explicitly restore
them, as described under [Change, turn off, or delete](#change-turn-off-or-delete).

### Prerequisites and limits

The executing agent needs repository access, the installed Orbit skills,
and the definition's required tools. An unknown or human-only required tool
is refused at task creation; a registered but disabled tool produces a
warning and must be available for execution. Minting and scheduling use the
checkout's local definitions: inspection from a linked checkout can show an
owner definition without granting permission to mint or change it there.

The source-oriented sweeps read a pinned integration-branch revision and
discover your repository's own conventions. They do not run builds, tests,
or timing experiments. Compatibility retirement needs a documented support
window. Architecture changes need graph evidence; proposals that depend on
unmeasured benefits get measurement prerequisites. Layout-only findings
belong to `structure-sweep`, which uses source files over 1,000 lines,
logic-heavy module indexes over 400 lines, and test files over 2,000 lines
as size thresholds alongside language-specific layout checks.

Dependency freshness needs ecosystem tooling, registry access, and upstream
release notes to confirm upgrade targets. Missing tools are reported as
gaps; unavailable network/registry access produces an evidence-bearing
no-op. Advisory-covered dependencies belong to security work, not this sweep.

The two CI sweeps need hosted CI configuration and granted, authenticated
run-history tools; the flaky-test hunt also reads failure logs. They do not
rerun tests locally. No hosted test job or unreachable history produces a
report explaining the gap, not a claim that CI is healthy. Flake detection
uses a 14-day window. CI budget watch measures the last 20 completed runs
in the documented population; without a documented budget it records a
trend and files no repair task.

### Find the evidence and follow-up work

Find assessment tasks by provenance, for example:

```bash
orbit task list --tag auto-task:dependency-freshness
```

Open the assessment task's artifacts and execution summary in the
[dashboard](../dashboard/). The outputs differ by sweep:

- **Code health:** `code-health.json`, with dispositions and up to six
  proposed improvement tasks. Confirmed correctness bugs are filed
  separately and do not count toward that cap.
- **Architecture:** `architecture.json`, with up to three proposed
  improvement tasks, plus any measurement prerequisites.
- **Structure:** scan evidence and candidate dispositions in the execution
  summary, with up to four proposed tasks grouped by package or subsystem.
- **Dependencies:** `dependency-freshness.json`, with up to three proposed
  upgrade or removal tasks.
- **Flaky tests:** `flaky-test-hunt.json`, with up to four proposed
  `ci-flake` fixes, highest flake count first.
- **CI budget:** `ci-budget-watch/report.md`; an over-budget run comments on
  existing `ci-budget` repair work or files one proposed repair task.

The templates check existing work before filing and record covered,
deferred, and rejected candidates. A run with no actionable findings can
succeed with its evidence persisted. Missing evidence remains a stated
gap. Review and approve proposed follow-ups before they enter delivery;
enabling a sweep does not approve them for you.

## 4. Watch it work

A routine fire is an ordinary run, and a filed task is an ordinary task. The
**Automation** view shows each routine's and auto-task's next fire and last
result; `orbit routine list` and `orbit auto-task list` show the same. Find
what got filed by its tag:

```bash
orbit task list --tag maintenance
```

Approve anything filed as `proposed`, then let a
[delivery window](../continuous-delivery/) pick it up.

## Ship unattended

No routine ships work. To ship while you are away, start an `orbit run auto`
drain for a window, or run `orbit run ship` yourself. A drain without
`--complete` leaves shipped work in `review` for you. Turn on worktree GC
first.

The `ship-sweep` routine, `orbit run ship-sweep`, and the
`workflow.auto_ship` setting were removed. A workspace that still holds the
routine warns and never runs it, and a config that still sets the key loads
with a deprecation warning.

## If nothing fires

Check in this order:

1. The clock is enabled: `orbit clock status`.
2. This checkout is registered as an owner: `orbit workspace list`.
3. The definition has `enabled: true`.
4. The routine is not paused on this machine: `orbit routine list`.
5. Fires start but fail on a missing credential: check
   [Scheduled credentials](#scheduled-credentials) — `orbit doctor` names
   the unset `pass` names, and `orbit clock tick --json` lists the names
   `clock_env_loaded` from `~/.orbit/clock.env`.

`orbit clock tick --dry-run --verbose` shows the decision for every routine
and auto-task, and `orbit doctor` runs the host health checks. The
[CLI reference](../../reference/cli/#scheduler) lists every scheduler command.
