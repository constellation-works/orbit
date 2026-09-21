---
title: Use the Dashboard
description: "Connect to the Orbit operator dashboard locally or over SSH, select a workspace, inspect tasks and runs, and use Operations controls with current authorization rules."
sidebar:
  order: 6
---

The **Orbit dashboard** is the operator UI for a live host: tasks, runs,
errors, and Operations. It is not a login portal and it is not a replica of
another machine's store. Open it on loopback, or reach a remote host over an
authenticated SSH tunnel.

Identifiers in the examples below are placeholders.

## Open it locally

From any directory, serve every workspace registered in the current Orbit
root:

```bash
orbit web serve --no-open
```

The default URL is `http://127.0.0.1:7878`. Omit `--no-open` to let the
process try to open a browser. Useful flags:

```bash
orbit web serve --port 8080 --no-open
orbit web serve --workspace orbit --no-open
orbit web serve --operator --no-open
orbit --root /path/to/orbit-root web serve --no-open
```

`--workspace` preselects a registered name, logical `ws_*` ID, or local
checkout path. If the selector does not match an active workspace, the UI
opens on **All workspaces** instead of erroring.

`--root` chooses which registry is served — `<root>/workspaces.json` and
nothing from the machine-global `~/.orbit/workspaces.json`. That is the same
root isolation every other command uses. `--global` still parses but is a
no-op: `orbit web serve` always serves every workspace in that registry.

The dashboard refuses non-loopback binds. `--host 0.0.0.0` is not a remote
access path; use [`orbit web connect`](#open-it-over-ssh) instead.

## Open it over SSH

```bash
orbit web connect my-server
orbit web connect my-server --remote-port 7878 --port 9000
orbit web connect my-server --workspace orbit
orbit web connect my-server --no-operator
```

`ssh-host` is anything `ssh` accepts: a hostname, `user@host`, or a
`~/.ssh/config` alias. The command:

1. Forwards a local loopback port to the remote dashboard port (default
   `7878`).
2. Reuses a remote `orbit web serve` that already answers `/healthz`, or
   starts `orbit web serve --no-open --operator --port <remote-port>` and
   owns that process. Pass `--no-operator` to spawn the previous read-only
   Operations surface instead.
3. Prints `http://localhost:<local-port>` and, unless `--no-open` is set,
   opens a browser.
4. On Ctrl-C, tears down only the SSH process this invocation started. A
   server it spawned is not left behind; a server it attached to is left
   running.

If local `7878` is busy and you did not pass `--port`, connect picks a free
ephemeral port. An explicit `--port` that is already bound fails instead.

When connect **attaches** to a remote server that is already running without
operator capability, it prints a notice and leaves that process as-is: it
cannot upgrade a server it did not start. Restart the remote with
`orbit web serve --operator`, or stop it and reconnect so this command can
spawn one.

`orbit web connect` does not accept `--root`. That flag used to name a remote
workspace; it now means a local data directory, which this command does not
read. Pass `--workspace <SELECTOR>` to preselect the remote workspace.

## Workspace scope

When more than one workspace is registered, the left rail shows a workspace
selector. One active workspace is the default: the `--workspace` value if it
resolved, otherwise the registered workspace containing the current
directory, otherwise the first active entry.

**All workspaces** is an explicit aggregate view. It lists tasks across the
served registry, but it is not a write target. Operations Enable/Disable, Mint
now, clock changes, auto-drain, and per-workspace panels are read-only there.
Task-row and task-detail mutations are allowed only when a row carries its
explicit owning workspace; aggregate task rows normally do, and writes are
sent to that owner. If a row has no owner, its controls stay disabled. Select
one active workspace before changing anything else. Inactive registry entries
appear as `<name> (unavailable)` and cannot be selected.

The selected workspace and time window live in the page URL, so a reload or
copied link restores the same scope. **Diagnostics → Reliability** is
fleet-wide: it ignores the selected workspace and says so in the rail.

A dashboard aggregate or metric is not proof that a particular task or run
succeeded. Confirm the selected workspace before acting.

## Inspect tasks, runs, and errors

The left rail is the section map:

| Rail | What it shows |
|---|---|
| **Tasks** | Backlog and other statuses for the selected workspace (or the aggregate list). |
| **Auto-drain** | Under Work beside Tasks: the bounded auto-delivery window. The rail count is the number of tasks eligible now. |
| **Audit** | Recent events and a 24-hour summary. |
| **Diagnostics** | Recent runs, metrics, errors, incidents, reliability, and the scoreboard. |
| **Operations** | Three rail subtabs: **Routines** (with the sweep clock), **Auto-tasks**, and **Jobs**. |
| **Knowledge** | Friction records. |
| **Config** | The effective `config.toml` for the selected workspace, with five rail subtabs: **Effective**, **Workspace file**, **Global file**, **Crews**, and **Keys**. |

### Tasks

Search by ID or title, filter by status, or type an ID into **Jump to
ORB-NNNNN** in the top bar. Opening a task shows detail plus the actions
that apply to its current status:

| Control | When it is present |
|---|---|
| **ship** | `backlog` only. Dispatches the workspace's configured ship mode (`pr` or `local`); there is no extra PR/local toggle on the button. Disabled while a ship run is already in flight for that task. |
| **approve** | `proposed` or `review`. |
| **reject** | `proposed`, `review`, or `backlog`. |
| **archive** | Any status except `archived`. |
| **comment** | Always. |

Status and crew dropdowns on the row are editable with a concrete workspace
selected, or for an aggregate row that includes its explicit owner. A row
without that owner stays read-only in **All workspaces**.

The status dropdown lists every lifecycle status, including for `done` and
`archived` tasks. Targets the [lifecycle table](../../concepts/tasks/#transition-rules)
allows are the ordinary path and still prompt for the evidence they require (a
plan, a completion summary). Every other target sits under a **force
(off-table)** group marked with ⚠: picking one asks for a single confirmation
naming the move, then applies it as the operator override — the same escape
hatch as `orbit task update <id> --status <status> --force`, recorded in task
history as a `forced` event. Agents have no equivalent: `orbit.task.update` and
the MCP surface cannot force.

The right dock has two modes that share the same column width: **Status**
(files currently locked by tasks) and **Log** (a live `orbit.log` tail with
all / err / deny / warn filters).

#### Edit task metadata inline

In **Tasks**, expand a row to open its detail. The five shipped inline editors
are split across the two detail columns:

| Field | Location and behavior |
|---|---|
| **description** | Left detail column, always shown for an editable task. Click **edit** to open the Markdown editor. |
| **acceptance criteria** | Left detail column, in the collapsed-by-default **acceptance criteria** section. Click **edit**; enter one criterion per line. |
| **complexity** | Right detail column, in the **properties** card. Choose **low**, **medium**, **hard**, or **xhard**; the change saves immediately. **unassessed** is displayed when already stored but is not an option. |
| **tags** | Right detail column, in the **properties** card. Click the card-header **edit** button and enter comma- or newline-separated tags. |
| **context files** | Right detail column, in the **context files** section. Click **edit**; enter one selector per line. The section header shows the current count. |

The text editors use the current lowercase **save** and **cancel** buttons.
**save** shows `saving…`, then returns to the read-only view with a field-saved
notice. **cancel** discards the draft and makes no request. If a text-field
save fails, the editor stays open with its text intact, the inline error is
shown, and **save** / **cancel** are enabled again. Complexity has no
save/cancel editor: changing its select saves immediately and reports
`complexity saved` or an inline update error. These metadata edits update the
task record; they do not dispatch work.

For **context files**, use the displayed selector forms `file:…`, `dir:…`, or
`symbol:…`. The server validates the selector kind, an existing filesystem
anchor, and whether that anchor is a file or directory as requested. A
`symbol:` name and kind are not looked up. If the task is deliberately about
to create a target, check **allow missing context** before **save**; otherwise
the rejected save leaves the draft in place so it can be corrected.

### Runs and errors

**Diagnostics → Recent runs** lists job runs for the selected workspace.
Click a row for run detail: metadata, steps, events, and a timing chart when
the run has that data.

Supported run actions, when the run's state allows them:

- **cancel** — `pending` or `running`.
- **Resume** — `failed`, `interrupted`, or `timeout`; starts from the first
  non-successful step.
- **Replay run** — submits a new run of the same job.

Those buttons are absent or disabled when the state does not allow the
action. A 409 from ship or another governed start means a conflicting run
or workspace claim is already held; refresh and inspect the named run
instead of retrying blindly.

**Diagnostics → Errors** is the step/event failure list for the current
month. It is not the same counter as the top-bar **Failed runs** tile
(failed, timeout, and interrupted job runs in the selected health window)
or Recent runs' **failed** filter (durable `Failed` state, no window). Use
the view that matches the question you are asking.

```bash
# CLI equivalents for the same facts. Identifiers are placeholders.
orbit task show ORB-NNNNN
orbit run show jrun-YYYYMMDD-HHMM-NN
orbit audit list
```

## Operations: mint, toggle, clock, drain

Operations has three subtabs in the rail: **Routines**, which also holds the
sweep clock bar; **Auto-tasks**; and **Jobs**. **Auto-drain** is its own
destination under Work (`#auto-drain`; the older `#operations/auto-drain`
link still resolves). All of them require a **single active workspace**. In
**All workspaces** the panels stay read-only and explain why.

Routines, auto-tasks and jobs share one row layout: the thing, what triggers
it, when it fires next, what happened last time, and one control at the
edge. Rows are grouped by whether they will fire (Active / Paused; On a
schedule / On delivery / Disabled), and each row's **Details** disclosure
carries the long fields. Below 720px the column headers go and each cell
labels itself.

### Routines

The pane opens with a **Next hour** strip: every routine whose next slot
falls in the coming hour, drawn solid when it will fire and hollow when the
routine is paused (its slot is skipped, not queued). Each row is one
versioned routine from the selected workspace: a switch, the name and the
job it runs (linked into **Jobs**), its cadence in words with the cron under
it, the next fire as a relative time, and the last run with its outcome,
duration and run link. A routine that is enabled but not effective carries
a **blocked** pill.

The switch writes that routine's enabled field. It is disabled when:

- no single workspace is selected,
- or the session is not an authorized operator (see
  [Authorization](#authorization)).

Toggling a routine does not start or stop the host sweep clock.

### Sweep clock

The clock bar above the routines is host-scoped (`orbit clock tick` on this
machine): health, provider, whether the service is enabled, cadence, last
and next tick, with **Pause clock** / **Enable clock** and the cadence picker
at the right. **Start** / **Stop** asks for confirmation and does not change
any routine definition.
**Apply cadence** reloads the native clock interval without changing whether
the service is enabled. Both need the same operator authorization as routine
toggles.

CLI equivalents:

```bash
orbit routine list
orbit clock status
orbit clock pause
orbit clock enable
orbit clock set --cadence-seconds 300
```

### Auto-tasks

A stats strip leads: definitions, how many are enabled, the next scheduled
mint, and how many definitions have an open duplicate (the scheduler skips
those slots). If the workspace defines an `auto_task_scheduler` routine and
it is paused, the pane says so, because scheduled definitions will not mint
until it runs.

Rows are grouped **On a schedule**, **On delivery** (minted after landed
deliveries, not on a clock) and **Disabled**. Each shows the switch, the
name with its template crew and priority, the trigger and dedupe policy, the
next mint, and the last minted task with its status; an open duplicate is
called out on the row. **Details** keeps the template, the scheduler cursor,
delivery coverage and the mint warning.

- The switch writes the definition's `enabled` field after a confirm
  dialog. A disabled definition is skipped by the scheduler; it is not
  deleted.
- **Mint now** creates one task immediately. It **ignores** the definition's
  schedule, enabled flag, and scheduler dedupe policy. The UI warns before
  the request: `Manual mint ignores this definition's schedule, enabled flag,
  and scheduler dedupe policy.` If an open instance already exists, mint
  still creates another.

Mint and toggle are separate operations. Minting a disabled definition is
supported and intentional; it is the on-demand escape hatch, not a
scheduler fire.

```bash
orbit auto-task list
orbit auto-task toggle "$NAME" off   # or on
orbit auto-task mint "$NAME"
```

The CLI mint path is the same unconditional operation the dashboard button
calls.

### Jobs

The Jobs subtab is the catalogue of job definitions the workspace uses,
projected from what the dashboard already serves: every `job:` target a
routine names plus every job id in the workspace's recent runs
(`/api/job-runs`). There is no job endpoint yet, so the pane is read-only.

- **Running now** lists in-flight runs with their job, role, crew, start
  time and run link.
- The catalogue is grouped **Sweeps** (housekeeping and intake, safe to run
  by hand) and **Delivery** (task and workspace pipelines, normally started
  by a ship or a drain). Each row shows which routines schedule the job and
  at what cadence, the last run with outcome, duration and link, and how
  many runs are active.
- **Run ▸** is offered on every row but disabled; its tooltip and the row's
  **Details** carry the exact CLI command, with a copy button:

```bash
orbit run job worktree_gc_pipeline --workspace orbit
```

The button will submit the same run once a job endpoint lands.

### Auto-drain

The pane reads top to bottom in the order you act: the window controls, the
slot picture, a summary strip, then task readiness.

**Start … window** submits `orbit run auto` for the selected duration
(a segmented picker) and concurrency (blank means the runtime default). The
line beside the button says what the window would admit — the eligible count
against free slots — and the button label carries the chosen duration.
Shipped tasks stay in `review` unless you opt into automatic completion.

**Slots** draws one tile per leaf slot. Occupied tiles name the task the run
carries and its phase; free tiles are dashed. Distributed drain can raise the
slot count to twenty, so the tiles wrap.

**Task readiness** groups the server's readiness rows by what you can do
about them:

- **Eligible now** — admitted in listed order when the window starts.
- **Blocked by a running task** — grouped by the task holding the lock, with
  the contested files as chips and the holder's slot phase, for both context
  locks and same-wave deferrals.
- **Waiting on dependencies** — collapsed to the tasks the chain bottoms out
  on and the chain laid out by depth; **Show all** expands the full rows.
- **Other reasons** — every remaining server reason with its evidence, so no
  row is hidden.

The snapshot is read-only: nothing is reserved or started until you start a
window, and the counts cover the bounded snapshot, not the workspace.

The completion checkbox is a governed operator action: it marks every task
the window ships as `done` (`review` → `done`), not only the ones visible at
submit time. If the session is not authorized for that option, the window
can still start with default review completion; the completion control stays
disabled and says why.

## Config: read and edit config.toml

**Manage → Config** (`#config/effective`) shows what this workspace actually
runs on. It is the browser view of `orbit config show`: the same layering,
the same sections, and the same registry descriptions, read from the server
rather than re-derived in the page.

Each rail subtab is its own hash route:

| Subtab | Hash | What it shows |
|---|---|---|
| **Effective** | `#config/effective` | The layered result — global under workspace under the built-in defaults. |
| **Workspace file** | `#config/workspace-file` | `.orbit/config.toml` resolved on its own (`orbit config show --scope workspace`). |
| **Global file** | `#config/global-file` | `~/.orbit/config.toml` resolved on its own. Edits here write global. |
| **Crews** | `#config/crews` | The crew table alone. |
| **Keys** | `#config/keys` | Every settable key with its type, section, description, and accepted values. |

The Effective view opens with a **layers strip** naming both files, then one
panel per section — Delivery, Crews, Execution, Operation mode,
Housekeeping — and a read-only **Paths** grid. Each row carries its value,
the layer that supplied it, and the registry's one-line description. The
source chip is the provenance: `workspace`, `global`, `default` (no file
sets it; the built-in value is in force), `unset` (no value at all), or
`registry` for a fact that comes from the workspace registry rather than
from `config.toml`.

Two facts the value alone cannot tell you are written on the row:

- **A shadowed lower layer** — `overrides global: trunk` means the workspace
  file won and names what it replaced.
- **A global `execution.*` value that was not inherited** — a workspace
  `config.toml` must restate the security keys
  (`execution.codex.sandbox`, `execution.codex.approval_policy`,
  `execution.env.pass`) to keep them. When one is dropped, the row says
  `global sets … — not inherited while a workspace file exists`, and the
  Execution panel carries a badge. The strip's warning appears only when a
  workspace file exists, because that is when the rule applies.

**Set only** hides keys nothing sets. A row whose lower layer was overridden
or not inherited stays visible at every filter setting; **All keys** adds the
rest, and expands a section that collapsed because every key in it is unset.

When the checkout is registered, a **registry strip** shows the registered
`base_branch` and `ship_mode`. Delivery reads those, not
`workflow.base_branch`, so the strip flags a disagreement between the two.
Changing them is an `orbit workspace` operation; the strip only displays
them.

### Editing a value

Click a row (or its pencil) to edit it in place. The editor is typed by the
registry: a choice list for a key with fixed values, a toggle for a boolean,
a number field for an integer, and a chip list for a string array. Each row
saves on its own — there is no staged batch.

**Saves go to the workspace file** (`.orbit/config.toml`, per-user and
git-ignored) from the Effective view; only the Global file subtab writes
`~/.orbit/config.toml`. A save takes the same admission path as `orbit config
set`, so a refused value comes back with the CLI's own message printed on the
row, and the saved row re-renders with the layer it now comes from.

If the workspace has no `config.toml` yet, the first save is refused on
purpose — creating that file moves the security keys off global policy — and
the row offers the two explicit choices `orbit config set` has: copy the
current global policy (`--seed-from-global`) or start empty (`--fresh`).

**Crews** render one row per crew with its provider, model, effort, tags, and
the layer it came from; a crew named by `workflow.default_crew` or
`workflow.system_crew` is annotated and tinted. Add, edit, and delete are
inline. Deleting a crew one of those keys still names is refused, with the
key in the message. A file that defines any `[crews.*]` table must also
resolve `workflow.default_crew` within itself, so adding the first crew to a
file that names no default crew is refused with that admission error.

Config writes need an **operator session** like the Operations controls, and
each one records a `config.set` audit event with the key, both values, and
the file it wrote. Without that capability, the rows render read-only.

Not editable here: routines, auto-task definitions, and the workspace
registry. Multi-workspace comparison is out of scope — select one workspace.

## Authorization

The dashboard has **no application login**. It binds loopback only. Origin
checks mitigate browser CSRF; they are not an access-control boundary.
Anyone who can reach the forwarded port can call the same mutation endpoints
the browser uses, with the server process's authority. Keep that port inside
the intended operator boundary.

Two independent gates still apply:

1. **Workspace scope.** Aggregate view and inactive workspaces are
   read-only, even for an operator.
2. **Operator capability** for Operations controls (routine/auto-task
   toggle, mint, clock, auto-drain completion) and for
   Config writes. The
   server resolves the same capability vocabulary as the CLI. A local
   interactive terminal counts as an operator session. `orbit web serve
   --operator` grants the same capability without a TTY or
   `ORBIT_OPERATOR`. `orbit web connect` passes `--operator` by default:
   Orbit is a single-user tool, and the SSH login is the operator act.
   `ORBIT_OPERATOR=1` remains the escape hatch for a non-interactive
   process that is not started with `--operator`; that override is
   recorded in the audit trail. Pass `orbit web connect --no-operator`
   for a read-only remote Operations surface. MCP is unchanged:
   `orbit mcp serve --operator` is still the only operator path there.

When a control is unauthorized, it is **disabled** and the card states
`Controls require an authorized operator session.` A click that still
reaches the API returns `403` with `code: authorization_denied`. That is
supported refusal, not a missing feature.

Task **ship** / **approve** / **reject** / **archive** and run cancel /
resume / replay are not the Operations operator gate. They still require a
concrete active workspace, and they still fail closed on conflicts (in-flight
ship, workspace claim held).

## Troubleshooting

| Symptom | What to check |
|---|---|
| Browser never opens, or `connection refused` | Is `orbit web serve` still running? Probe `curl -s http://127.0.0.1:7878/healthz` — a live server returns `ok`. |
| `refusing to bind dashboard to non-loopback address` | Bind `127.0.0.1` or `::1`. For another machine, use `orbit web connect`, not `--host 0.0.0.0`. |
| `orbit web connect does not accept --root` | Pass `--workspace <SELECTOR>`. |
| Connect waits then fails readiness | SSH must work non-interactively to that host, and `orbit` must be on the remote `PATH`. The remote process has about 30 seconds to answer `/healthz`. |
| Workspace selector missing | Only one servable workspace is registered; the UI has nothing to switch. |
| Enable / Mint now / clock buttons disabled | Select one active workspace. If the note mentions an authorized operator session, start `orbit web serve --operator` (or `orbit web connect`, which does that by default). A local interactive terminal still counts. `ORBIT_OPERATOR=1` remains the env override. Connecting to a pre-existing non-operator remote cannot upgrade it in place. |
| Mint created a second open task | Expected: manual mint ignores dedupe. The card's **Open duplicate** field says so before you confirm. |
| Ship returns 409 `ship_run_in_flight` | That task already has a non-terminal ship run. Open the named `run_id`. |
| 409 `workspace_claim_held` | Another operator holds the workspace claim. Wait for expiry or inspect the holder; do not retry in a loop. |
| Top-bar **Failed runs** disagrees with Diagnostics → Errors | Different denominators. See [Runs and errors](#runs-and-errors). |
| Config save returns "no workspace config exists yet" | Expected on the first write: choose **Copy global policy** or **Start empty** on the row. |
| Config save returns an admission error | The value is refused by the same rule `orbit config set` applies; the message names the accepted values. |
| Stale workspace list after `orbit workspace init` | A running server reloads `workspaces.json` on the next request after that file's mtime or length changes; click **Refresh**. A malformed refresh keeps the last good snapshot. |

Readiness with per-workspace store and log-sink checks:

```bash
curl -s 'http://127.0.0.1:7878/healthz?detailed=true'
```

HTTP 200 means every check passed; 503 means at least one failed. Point
uptime monitoring at the detailed form. Plain `/healthz` is liveness only.

The rail foot shows connection state next to the workspace selector. Orange
**connecting…** at idle after startup usually means the first API request
failed; **Refresh** or the `/healthz` probe distinguishes a dead process
from a slow workspace.

## Next

- [Run a Task Lifecycle](../task-lifecycle/) — create, ship, and review from
  the CLI.
- [Schedule Recurring Work](../recurring-work/) — routines, the sweep clock,
  and auto-task definitions.
- [Run a Continuous Delivery Window](../continuous-delivery/) — bounded
  `orbit run auto`, including `--complete`.
- [Set Up MCP](../mcp-integration/) — the tool surface agents use; distinct
  from this operator UI.
