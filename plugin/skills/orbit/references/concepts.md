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
allocates. Written once, at `orbit init`. `orbit config show` displays it; only
`machine.name` is settable.

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
`workflow.default_crew` covers tasks that don't declare one, and
`workflow.system_crew` covers Orbit's own bounded activities like failure
recovery and the task pilot.

**Executor** — how a provider is actually invoked. Mostly infrastructure; you
choose crews, not executors.

**Policy / fsProfile** — the filesystem grant an activity runs under
(`reviewer`, `implementer`, `docs_writer`, `pure_compute`, `unrestricted`). A
read-only activity that omits its profile silently falls back to workspace
writes, so profiles are declared explicitly.

## Scheduling

**Routine** — a cron trigger (`.orbit/routines/*.yaml`) pointing at a
`job:<name>` target, with a retry and overlap policy. Definitions are
per-user checkout state and are evaluated by every host holding an owner
checkout of that workspace.

**Sweep** — the stateless tick. `orbit sweep` fires whatever routine is due on
this host, and an OS clock unit invokes it every minute.

**Auto-task** — a definition (`.orbit/auto_tasks/*.yaml`) that *mints a task* on
its own schedule. One generic routine drives all of them, so adding a recurring
chore is a new definition, never new code or a new routine.

The distinction that matters: a **routine** runs a pipeline on a schedule; an
**auto-task** creates work on a schedule, which something else then ships.

## The rule that surprises people

Routine and auto-task *definitions* are per-user files under `.orbit/`. All
scheduler *state* — last fire times, pauses, locks, run history — lives in
the host's own store and never syncs. Two machines sharing a repo each own
their own definitions and independent state. See
[multi-host.md](../../orbit-setup/references/multi-host.md).
