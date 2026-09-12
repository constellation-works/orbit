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

Commands and UI labels below were verified against current `orbit web`
help and the dashboard sources in this repository. Examples use sanitized
identifiers and are **illustrative**, not captured from a live host, unless a
caption says otherwise.

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
```

`ssh-host` is anything `ssh` accepts: a hostname, `user@host`, or a
`~/.ssh/config` alias. The command:

1. Forwards a local loopback port to the remote dashboard port (default
   `7878`).
2. Reuses a remote `orbit web serve` that already answers `/healthz`, or
   starts `orbit web serve --no-open --port <remote-port>` and owns that
   process.
3. Prints `http://localhost:<local-port>` and, unless `--no-open` is set,
   opens a browser.
4. On Ctrl-C, tears down only the SSH process this invocation started. A
   server it spawned is not left behind; a server it attached to is left
   running.

If local `7878` is busy and you did not pass `--port`, connect picks a free
ephemeral port. An explicit `--port` that is already bound fails instead.

`orbit web connect` does not accept `--root`. That flag used to name a remote
workspace; it now means a local data directory, which this command does not
read. Pass `--workspace <SELECTOR>` to preselect the remote workspace.

## Workspace scope

When more than one workspace is registered, the left rail shows a workspace
selector. One active workspace is the default: the `--workspace` value if it
resolved, otherwise the registered workspace containing the current
directory, otherwise the first active entry.

**All workspaces** is an explicit aggregate view. It lists tasks across the
served registry, but mutations are disabled there — including Operations
Enable/Disable, Mint now, clock changes, and auto-drain. Select one active
workspace before changing anything. Inactive registry entries appear as
`<name> (unavailable)` and cannot be selected.

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
| **Audit** | Recent events and a 24-hour summary. |
| **Diagnostics** | Recent runs, metrics, errors, incidents, reliability, and the scoreboard. |
| **Operations** | Routines, auto-tasks, auto-drain, and operation-mode grants. |
| **Knowledge** | Friction records. |

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

Status and crew dropdowns on the row are editable only with a concrete
workspace selected. In **All workspaces** they stay visible but are
read-only.

The right dock has two modes that share the same column width: **Status**
(files currently locked by tasks) and **Log** (a live `orbit.log` tail with
all / err / deny / warn filters).

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

Operations has three subtabs. All of them require a **single active
workspace**. In **All workspaces** the panels stay read-only and explain
why.

### Routines

Each card is one versioned routine from the selected workspace: name, enabled
/ blocked / disabled, target job, schedule, last and next evaluation, and last
fire.

**Enable** / **Disable** writes that routine's enabled field. The control is
disabled when:

- no single workspace is selected,
- or the session is not an authorized operator (see
  [Authorization](#authorization)).

Toggling a routine does not start or stop the host sweep clock.

### Host sweep clock

The clock panel is host-scoped (`orbit sweep` on this machine). **Start** /
**Stop** asks for confirmation and does not change any routine definition.
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

Each definition shows its schedule, dedupe policy, last evaluation or mint,
last minted task, and whether an open duplicate already exists.

- **Enable** / **Disable** writes the definition's `enabled` field after a
  confirm dialog. A disabled definition is skipped by the scheduler; it is
  not deleted.
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
orbit auto-task toggle "$NAME"
orbit auto-task mint "$NAME"
```

The CLI mint path is the same unconditional operation the dashboard button
calls.

### Auto-drain and operation mode

**Start bounded window** submits `orbit run auto` for the selected duration
and concurrency. Shipped tasks stay in `review` unless you opt into
automatic completion.

The completion checkbox is a governed operator action: it marks every task
the window ships as `done` (`review` → `done`), not only the ones visible at
submit time. If the session is not authorized for that option, the window
can still start with default review completion; the completion control stays
disabled and says why.

**Operation Mode** projects `orbit operation explain` for the workspace:
preset, caps, and the active grant. **Stop grant** and **Revoke grant** are
supported when a grant is active and the session is authorized.

**Enablement is not a dashboard action.** Creating a grant names a finite
task set and explicit rights; that decision stays on the CLI or operator MCP
surface. Do not treat a missing Enable control as a broken button.

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
   toggle, mint, clock, auto-drain completion, grant stop/revoke). The
   server resolves the same capability vocabulary as the CLI. A local
   interactive terminal counts as an operator session. A non-interactive
   process (a service unit, a script) does not, unless it is started with
   `ORBIT_OPERATOR=1`. That override is recorded in the audit trail.

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
| Enable / Mint now / clock buttons disabled | Select one active workspace. If the note mentions an authorized operator session, start `orbit web serve` from an interactive terminal or with `ORBIT_OPERATOR=1`. |
| Mint created a second open task | Expected: manual mint ignores dedupe. The card's **Open duplicate** field says so before you confirm. |
| Ship returns 409 `ship_run_in_flight` | That task already has a non-terminal ship run. Open the named `run_id`. |
| 409 `workspace_claim_held` | Another operator holds the workspace claim. Wait for expiry or inspect the holder; do not retry in a loop. |
| Top-bar **Failed runs** disagrees with Diagnostics → Errors | Different denominators. See [Runs and errors](#runs-and-errors). |
| Stale workspace list after `orbit workspace init` | A running server reloads the registry on request boundaries; click **Refresh**. A malformed refresh keeps the last good snapshot. |

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
