# Orbit concepts

The vocabulary, and how the pieces nest. Read this once before setting Orbit up;
after that, use it to disambiguate a term rather than reading it end to end.

## The shape of it

```text
host (one machine, one identity, one task-id prefix)
└── workspace (logical identity with a registered checkout and owner)
    ├── tasks, frictions                ← what the work is
    ├── routines, auto-tasks            ← what fires on a schedule
    └── runs                            ← what actually executed
```

A host holds many workspaces. A workspace scopes the durable record. Job
executions become runs; ordinary tool calls have their own audit records.

## Places

**Machine** — one machine. It has an identity, the `[machine]` table in the
global `~/.orbit/config.toml`: a stable `id`, a renameable display `name`, and
an **immutable `task_prefix`** that namespaces every task ID this machine
allocates. Written once, at `orbit init`. `orbit config show` displays it; of
those identity fields, only `machine.name` is settable.

**Workspace** — a logical project registered with a local checkout, with
`.orbit/` at its root. A checkout declares an owner or replica role; the owner
machine is authoritative for mutations. A replica does not become an owner
merely by sharing Git history. Registered
in a machine-global registry, so commands can address it by name, by logical ID
(`ws_*`), or by absolute path. A linked Git worktree resolves to its registered
checkout rather than registering separately.

**Global root** (`~/.orbit/`) — the machine's own state: the store, the
workspace registry, machine identity, installed resources, logs. Never in version
control.

**Workspace `.orbit/`** — per-user checkout state, gitignored in full.
`config.toml`, `routines/`, `auto_tasks/`, and `resources/` are this owner's
settings; `orbit workspace init` / `sync` seed shipped defaults from the
binary. Everything under `.orbit/state/` is runtime evidence — job-run
bundles, audit events, diagnostics.

## Work

**Task** — the unit of change. Carries a description, acceptance criteria, a
plan, `context_files` selectors naming what it will modify, a lifecycle status,
and a full history. IDs are allocated by the store; never invented.

**Epic tag** — a size hint on one large task: work a top-tier crew takes on
whole. Crew selection reads it; admission ignores it, and a tagged task ships as
an ordinary leaf on its own declared context.

**Publication** — an explicitly published, validated task snapshot in a
dedicated Git repository. It has source/workspace/authority identity, a
generation, a commit, and attachment completeness labels. Inspection is not a
live task read; restore is an explicit same-authority operation.
See [publication.md](../../orbit-setup/references/publication.md).

**Friction** — a record of something that made the work harder than it should
have been. A ledger of experience, not a queue of work; see
[friction.md](friction.md).

## Task-flow analytics

Use `orbit task flow` to see whether a filtered task population is growing,
flat, or draining over time:

```sh
orbit task flow
orbit task flow --tag security-review --window 14d --buckets 4
orbit task flow --type bug --json
```

The report defaults to six 7-day buckets, with the most recent bucket last.
Choose `--window` with a duration in seconds, minutes, hours, days, or weeks
(`s`, `m`, `h`, `d`, or `w`); `--buckets` accepts 1 through 520. Filter by
task type (`feature`, `bug`, `refactor`, or `chore`), and use `--tag` to select
tags. Repeat `--tag` or pass comma-separated tags to require every selected
tag, matching task-list filtering. These options select task records by their
current type and tags; the report includes each selected task's creation and
status events within the requested span.

Each bucket covers its start instant through, but not including, its end
instant. The rows are oldest first. `FILED` counts task creations,
`REOPENED` counts transitions from a terminal status back to an open status,
`CLOSED` counts departures into `done`, and `DROPPED` counts departures into
`rejected` or `archived`. A terminal-to-terminal relabeling is not another
departure. For older terminal tasks with no status history, the report uses
their last-updated time as an approximate departure time. A task that closes
or drops and later reopens contributes both events in their respective
buckets.

`NET` is `FILED + REOPENED - CLOSED - DROPPED`; a negative value means more
tasks left the backlog than entered during that bucket. `OPEN AT END` is the
filtered population still open at that bucket's end. The overall verdict uses
the net across all reported buckets: negative is draining, zero is flat, and
positive is growing. `OPEN NOW` is the current open count, so it is a snapshot,
not another flow event. If all event totals and the current open count are
zero, the verdict says there is no data rather than calling the result flat.

With `--json`, each bucket includes its ISO `start` and `end`, the four event
counts, `net`, and `open_at_end`. `totals` contains those event totals, the
span's net, and `open_now`; `verdict` contains the overall interpretation.

To verify a report, run the same filters in JSON and table form. Confirm that
the bucket count and order match the requested window, each bucket's net uses
the formula above, event totals equal the sum of their bucket values, and total
net equals the sum of bucket nets. Check the verdict against the overall net,
treating the no-data message as its own case. Read `open_at_end` as a historical
snapshot and `open_now` as the current one; neither is added to the inflow or
outflow arithmetic. Run `orbit task flow --help` to confirm the available
options and examples on the installed version.

## Execution

**Activity** — one named step definition: `agent_implement`, `git_commit`,
`pr_open`, `worktree_setup`, `reserve_locks`. Activities are never invoked
directly; a job's step list references them.

**Job** — a deterministic multi-step pipeline composing activities. Discover
with `orbit job list` / `orbit job show <id>`.

**Run** — one execution of a job, with a `jrun-*` ID, a durable state bundle,
and an audit trail. Runs are submitted to a detached worker and are asynchronous
by default: the command returns once the run is durable, not once it finishes.

**Crew** — a named provider-and-model assignment, defined as a `[crews.<name>]`
table in `config.toml` (`orbit doctor providers` shows which provider CLIs this
machine can launch). A task's `crew` selects who executes it;
`workflow.default_crew` covers tasks that don't declare one. An unassigned
task draws `workflow.<tier>_complexity_crews` for its complexity, then the
default crew. An activity that names a purpose draws
`workflow.purpose_crews.<purpose>` first; an unset pool keeps that purpose's
earlier tier (`low` for the task pilot and auto-fix, `medium` for failure
recovery).

**Executor** — how a provider is actually invoked. Mostly infrastructure; you
choose crews, not executors.

**Policy / fsProfile** — the filesystem grant an activity runs under
(`reviewer`, `implementer`, `docs_writer`, `pure_compute`, `unrestricted`). An
activity that omits its profile falls back to `unrestricted`, so profiles are
declared explicitly.

## Scheduling

**Routine** — a definition (`.orbit/routines/*.yaml`) pointing at a
`job:<name>` target, with a retry and overlap policy. The trigger is cron, or
a state predicate (`preparation_eligible` for task-pilot,
`finding_unverified` for finding-verification, `pilot_warnings_held` for
pilot-warning triage). Definitions are per-user
checkout state. An owner checkout evaluates its routines. A replica checkout
schedules only cron routines whose job is `worktree_gc_pipeline` or
`store_gc_pipeline`.

**Sweep** — the stateless tick. `orbit clock tick` (alias `orbit sweep`)
evaluates due routines and auto-tasks on this host. An OS clock unit invokes
`orbit clock tick` every minute.

**Auto-task** — a definition (`.orbit/auto_tasks/*.yaml`) that *mints a task* on
its own schedule. The host clock tick evaluates due definitions directly, so
adding a recurring chore is a new definition, never new code or a new routine.

The distinction that matters: a **routine** runs a pipeline on a schedule; an
**auto-task** creates work on a schedule, which something else then ships.

## The rule that surprises people

Routine and auto-task *definitions* are per-user files under `.orbit/`. All
scheduler *state* — last fire times, pauses, locks, run history — lives in
the host's own store and never syncs. Two machines sharing a repo each own
their own definitions and independent state. See
[multi-host.md](../../orbit-setup/references/multi-host.md).
