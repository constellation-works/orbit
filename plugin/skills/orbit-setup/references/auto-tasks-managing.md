# Auto-tasks: managing definitions and granting tools

## Contents

- [Managing definitions](#managing-definitions)
  - [Granting tools to every minted task](#granting-tools-to-every-minted-task)

## Managing definitions

```bash
orbit auto-task list                    # definitions with schedule and state (--all: plugin-off too)
orbit auto-task show <name>
orbit auto-task update <name> --cron "0 9 * * 2"   # present fields only
orbit auto-task toggle <name> on|off    # the kill-switch — preserved, not deleted
orbit auto-task mint <name>             # mint one right now
orbit auto-task delete <name> --reason "<why>"   # remove it for good
orbit auto-task restore <name>          # reinstate a deleted shipped default
```

In a linked worktree, `show` and `list` inspect checkout-local candidate
definitions first and fall back by name to the registered workspace's catalog.
`show` reports the root and file that supplied the body; its settings and plugin
provenance come from that same catalog. A disabled or invalid local definition
does not expose a shared replacement. Inspection accepts `.yaml` and `.yml`
(case-insensitive extensions) and reports the actual file path.

If multiple
files in one catalog have the same name, `show` refuses the ambiguous lookup,
and the scheduler, `list` and `orbit doctor` report every one of them as a load
error and fire none until only one remains.
Writes and `mint` still require the canonical `<name>.yaml` file and keep their
existing checkout scope; inspecting a shared definition does not authorize
changing or minting it from a worktree.

An empty worktree catalog can be a Linux sandbox
write anchor and does not hide workspace definitions.
`orbit doctor` diagnoses both catalogs with their own settings and manifests.

On a shipped default, `toggle` and `update` of settings fields — `enabled`,
the schedule, dedupe, the operator's granted tools, and the template's crew,
priority, complexity, and added tags — are stored in `.orbit/auto_tasks/.orbit-auto-task-settings.json` and
applied over the bundled body, which keeps receiving upstream fixes on
`orbit workspace sync`.

Changing any other field forks the definition: Orbit
then preserves the file as written and stops refreshing it, and `show` reports
`body: forked` with the differing fields. `orbit workspace sync` moves an older
fork that differs only in settings back under management.

### Granting tools to every minted task

A task's `granted_tools` is an operator's grant, and a minted task has no
operator at mint time. Put the grant on the definition instead:

```bash
orbit auto-task update <name> --granted-tools orbit.search
orbit auto-task show <name>
```

Each scheduled fire and manual `mint` copies the list to the new task's
`granted_tools` and records a `granted_tools_changed` entry in its history.
The list is the definition's top-level `granted_tools`, beside `template`,
not a template field: a `template` replacement or flag edit never changes
it. `--granted-tools ""` clears it. `orbit auto-task add` takes the same
flag.

Compatibility: an older binary refuses a definition or settings file that
sets `granted_tools`. Upgrade every host before setting a grant, then
inspect the stored grant with `orbit auto-task show <name>`.

Only an operator session may supply the field. Any other caller that
supplies it, an agent's `orbit.auto_task` call or a mutation proposal
included, is refused naming the missing operator authority, never silently
stripped. Names are checked like a task's grant: exact canonical names of
agent-facing, enabled tools, never an operator-only tool. A minted task
with a grant runs only on the owner host.

What a grant lets the task's
`agent_implement` run do is in the installed Orbit skill
`references/task-fields-tools.md`.

The definition file is per-user checkout state. Orbit checks authority on
the `add` and `update` calls, not on a hand edit of the YAML, so treat that
file as you treat the rest of `.orbit/`.

Tool names written before tool families (`orbit.task.artifact.put` rather
than `orbit.task.artifact:put`) are upgraded automatically. On the first open
after an upgrade, Orbit rewrites them in every stored auto-task definition
(managed, forked or workspace-local), in tasks' `required_tools` and
`granted_tools`, and in installed plugins' `permissions.orbit_tools`. It
rewrites again whenever a mint, scheduler tick or dispatch finds an old name.

`orbit workspace sync` rewrites old names in managed and workspace-local
definitions, and `orbit workspace sync --check` lists those pending rewrites as
`migrated`. Sync preserves a forked definition as written, so a fork's old names
wait for the next mint, scheduler tick or dispatch. A rewrite changes only the
list items, keeping comments and the definition's managed or forked state.

Each
rewrite is logged to `.orbit/state/tool-name-migrations.jsonl` (plugin
rewrites to `~/.orbit/state/`), and a task gets a
`tool_names_migrated` history entry.

Plugin rewrites preserve exact consent. Before changing a matching installed
manifest, Orbit durably retains its original bytes under
`~/.orbit/state/plugin-tool-name-originals/`, keyed by the original SHA-256.
If the rewritten file lands but its digest record and rollback do not, a
later ordinary open verifies the retained original against the recorded digest
and accepts only its exact tool-name rewrite. This works for mixed legacy and
canonical names, including repeated items.

The loader takes its snapshot
before migration, so the following open loads the repaired plugin.

For interrupted writes from older versions without a retained original,
Orbit checks the all-legacy spelling and mixed spellings of up to ten
convertible list items (at most 1,024 candidates). Each candidate must match
the recorded digest and reproduce the entire on-disk manifest exactly when
rewritten. Larger ambiguous old lists require reinstalling the reviewed
plugin with `orbit plugin add --force` and enabling it with the intended
grants.

An unrelated manifest edit never gains consent through recovery.
The plugin migration has its own versioned completion marker, so installs
marked complete by the older pass receive this repair once.

The dashboard's **Automation › Auto-tasks** row has an **edit** link on the
trigger that changes only the schedule (cron, a fixed interval or
`deliveries_landed`). It runs the schedule half of `update`, with the same
validation, and refuses the save if the schedule changed since the page read
it. Every other field stays read-only there.
