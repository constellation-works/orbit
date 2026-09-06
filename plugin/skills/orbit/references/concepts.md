# Orbit concepts

The vocabulary, and how the pieces nest. Read this once before setting Orbit up;
after that, use it to disambiguate a term rather than reading it end to end.

## The shape of it

```text
host (one machine, one identity, one task-id prefix)
└── workspace (logical identity with a registered checkout and owner)
    ├── tasks, docs, frictions          ← what the work is
    ├── routines, auto-tasks            ← what fires on a schedule
    └── runs                            ← what actually executed
```

A host holds many workspaces. A workspace scopes the durable record. Job
executions become runs; ordinary tool calls have their own audit records.

## Places

**Host** — one machine. It has an identity (`~/.orbit/host.toml`): a host ID
used to pin routines, a machine ID, and an **immutable task prefix** that
namespaces every task ID this machine allocates. Chosen once, at `orbit init`.

**Workspace** — a logical project registered with a local checkout, with
`.orbit/` at its root. A checkout declares an owner or replica role; the owner
machine is authoritative for mutations. A replica does not become an owner
merely by sharing Git history. Registered
in a machine-global registry, so commands can address it by name, by logical ID
(`ws_*`), or by absolute path. A linked Git worktree resolves to its registered
checkout rather than registering separately.

**Global root** (`~/.orbit/`) — the machine's own state: the store, the
workspace registry, host identity, installed resources, logs. Never in version
control.

**Workspace `.orbit/`** — split by durability. `config.toml`, `routines/`,
`auto_tasks/`, and `resources/` are definitions and belong in git. Everything
under `.orbit/state/` is runtime evidence — job-run bundles, audit events,
diagnostics — and does not.

## Work

**Task** — the unit of change. Carries a description, acceptance criteria, a
plan, `context_files` selectors naming what it will modify, a lifecycle status,
and a full history. IDs are allocated by the store; never invented.

**Epic** — a task with descendants, shipped through a pipeline that gives the
whole family one worktree and one branch, instead of one per child.

**Publication** — an explicitly published, validated task snapshot in a
dedicated Git repository. It has source/workspace/authority identity, a
generation, a commit, and attachment completeness labels. Inspection is not a
live task read; restore is an explicit same-authority operation.
See [publication.md](setup/publication.md).

**Doc** — reviewed markdown under the configured docs roots, indexed for
retrieval by concept rather than filename. Historical decision documents can be indexed as ordinary docs; there is no
separate decision store. Retrieved history does not override current
requirements, code, or explicit instructions.

**Friction** — a record of something that made the work harder than it should
have been. A ledger of experience, not a queue of work; see
[friction.md](friction.md).

## Execution

**Activity** — one named step definition: `agent_implement`, `git_commit`,
`pr_open`, `worktree_setup`, `reserve_locks`. Activities are never invoked
directly; a job's step list references them.

**Job** — a deterministic multi-step pipeline composing activities. Discover
with `orbit job list` / `orbit job show <id>`.

**Run** — one execution of a job, with a `jrun-*` ID, a durable state bundle,
and an audit trail. Runs are submitted to a detached worker and are asynchronous
by default: the command returns once the run is durable, not once it finishes.

**Crew** — a named provider-and-model assignment (`orbit executor list` shows the
available providers). A task's `crew` selects who executes it;
`workflow.default_crew` covers tasks that don't declare one, and
`workflow.system_crew` covers Orbit's own bounded activities like failure
recovery and triage.

**Executor** — how a provider is actually invoked. Mostly infrastructure; you
choose crews, not executors.

**Policy / fsProfile** — the filesystem grant an activity runs under
(`reviewer`, `implementer`, `docs_writer`, `pure_compute`, `unrestricted`). A
read-only activity that omits its profile silently falls back to workspace
writes, so profiles are declared explicitly.

## Scheduling

**Routine** — a git-versioned cron trigger (`.orbit/routines/*.yaml`) pointing at
a `job:<name>` target, pinned to specific hosts, with a retry and overlap
policy. Definitions sync through git.

**Sweep** — the stateless tick. `orbit sweep` fires whatever routine is due on
this host, and an OS clock unit invokes it every minute.

**Auto-task** — a definition (`.orbit/auto_tasks/*.yaml`) that *mints a task* on
its own schedule. One generic routine drives all of them, so adding a recurring
chore is a new definition, never new code or a new routine.

The distinction that matters: a **routine** runs a pipeline on a schedule; an
**auto-task** creates work on a schedule, which something else then ships.

## The rule that surprises people

Routine and auto-task *definitions* are versioned files and sync across
machines. All scheduler *state* — last fire times, pauses, locks, run history —
lives in the host's own store and never syncs. Two machines sharing a repo run
the same definitions against completely independent state. See
[multi-host.md](setup/multi-host.md).
