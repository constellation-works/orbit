# Configuration: layers, seeding and scheduled publication

## Contents

- [Where config lives](#where-config-lives)
- [Seeding policy](#seeding-policy)
  - [Changing an existing workspace](#changing-an-existing-workspace)
- [Scheduled task publication](#scheduled-task-publication)

What is tunable and where. `orbit config keys` and `orbit config show` expose
the installed version's supported keys and effective values without a source
checkout. Check `orbit --version` before applying version-dependent CLI advice.
To choose values for a repository, in dependency order and from its own CI,
build and backlog evidence, follow [workspace tuning](workspace-tuning-policy.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local | Hand-authored, optional |
| `~/.orbit/config.toml` | Global / user | `orbit init` |

Ordinary settings inherit per key: workspace overrides global, global fills
omissions, built-in defaults fill the rest. Tables layer down to individual
settings; scalars and arrays replace wholesale.

**Three security-sensitive settings deliberately do not inherit** once a
workspace policy file exists — `execution.codex.sandbox`,
`execution.codex.approval_policy`, and `execution.env.pass`. If the workspace
file omits one, its built-in default applies rather than the user's global
value. This keeps repository sandboxing deterministic instead of dependent on
whoever's machine is running.

A file containing only `[plugin_enablement]`
is stripped before layering and does not count as a workspace policy layer:
those three settings still inherit global values.

```bash
orbit config show                  # effective merged view, with source provenance
orbit config show --scope global   # one physical file
orbit config get <key>
orbit config set <key> <value>
orbit config keys                  # every registered key with its allowed values; machine.id and machine.task_prefix are listed and read-only
orbit config path
```

`orbit config show` labels configured values by source: `workspace`, `global`,
or `environment`; `workflow.default_crew` shows `environment` when the
`CONSTELLATION_DEFAULT_PROVIDER` variable supplies it (no provider detection
runs at config load). A value from a compiled-in default shows `default`; a key with
no configured value and no compiled-in default shows `unset`. `machine.id` and
`machine.task_prefix` render as identity, not as settable rows.

The text rows
do not include file paths: see the `Layers (later
wins)` banner for layer paths, or use `orbit config show --json` for per-key
path provenance. Reach for it before assuming a value. Use `orbit config show
--all` to include unset advanced keys;
the defaults [below](configuration-settings-workflow.md#common-settings) are built-ins, not necessarily this host's effective policy.

## Seeding policy

`seeding.routines` and `seeding.auto_tasks` choose which shipped defaults a
workspace carries, independently of presets. Both default to `"all"`. Set them
in `<workspace>/.orbit/config.toml` for one workspace, or use `orbit config set
--global` for `~/.orbit/config.toml`. Each workspace key overrides its global
counterpart; a name list replaces the inherited list rather than adding to it.

| Value | Effect |
|---|---|
| `"all"` | Seed every shipped default of that kind, including additions in later releases. |
| `"none"` | Seed none of that kind. |
| A TOML name list | Seed exactly those shipped defaults; later additions stay excluded. Routine names are file stems such as `["worktree_gc", "store_gc"]`, not the rendered names from `orbit routine list`. Auto-task names use names such as `["doc-duties", "qa-sweep"]`. |

A name Orbit has never shipped is refused by config loading, `orbit config
set` and seeding commands; the error names the key, bad name and shipped
choices. A name a later release retired or demoted still loads but seeds
nothing. This
policy selects Orbit's shipped defaults, not user-authored or plugin definitions.

The policy applies to `orbit workspace init`, `orbit workspace init --force`,
`orbit workspace sync`, and the create-only seeding that `orbit init` performs
over registered workspaces after an upgrade. Use workspace sync to reconcile
an existing workspace: the upgrade's create-only pass leaves existing files
alone and does not retire them.

### Changing an existing workspace

Run from the intended registered checkout. Inspect effective values and their
sources, then save the current config and any local definition edits before
narrowing. Review the definitions you intend to keep; this example selects
garbage collection and the crew snapshot refresh, plus two auto-tasks whose
shipped defaults are disabled:

```bash
orbit workspace show
orbit config keys
orbit config show --all
orbit config get seeding.routines
orbit config get seeding.auto_tasks
orbit routine list
orbit auto-task list

orbit config set seeding.routines '["worktree_gc", "store_gc", "task_pilot"]'
orbit config set seeding.auto_tasks '["doc-duties", "qa-sweep"]'
orbit config get seeding.routines
orbit config get seeding.auto_tasks
orbit workspace sync --check --json
```

The two config writes are separate, not an atomic preset application. Check
their effective values before applying. `sync --check` writes nothing and
exits 3 when changes are pending. Review the whole report, including warnings
and host-global asset changes, before running:

```bash
orbit workspace sync --json
orbit config show --all
orbit workspace show
orbit workspace show --json
orbit doctor
```

- **Narrowing retires managed defaults by recorded digest.** Sync removes an
  unedited file Orbit seeded. It moves a locally edited excluded default under
  `.orbit/.retired-managed/` and warns to migrate it under a new user-authored
  name. A file Orbit never recorded is left alone. Save and review edits before
  sync; preservation does not keep the excluded definition active.
- **Widening reseeds on the next sync.** Add a name to the list or set a key
  back to `all`, then preview and apply sync again. This seeds the shipped
  definition, not the preserved edited copy. Selecting a default is separate
  from enabling it: shipped auto-tasks start disabled, including `host-health`.

  The shipped `worktree-gc` and `task-pilot` routines seed enabled, as does
  one dormant experiment routine that mints nothing unless a running
  experiment has a memory arm; other shipped routines seed disabled.
  Preserve existing enablement choices.
- **Auto-task deletion remains an opt-out.** An opt-out recorded by `orbit
  auto-task delete` survives exclusion and later widening, so sync still leaves
  that default absent. First include it in `seeding.auto_tasks`, then use
  `orbit auto-task restore <name>` to clear the opt-out and restore its shipped
  content with the shipped `enabled` value: disabled for every default,
  including `host-health`. Restore refuses an excluded name or an existing definition.

  See [Deleting a definition](auto-tasks-deleting.md#deleting-a-definition). For routines,
  a name-list policy is the durable per-name opt-out.

**Verify the result.** `orbit config show --all` reports the effective keys and
their provenance. `orbit workspace show` reports both policies and the defaults
left out; JSON carries `seeding.routines`, `seeding.auto_tasks` and
`seeding.left_out`, with the last applied preset separately under `preset`.
These describe policy, so also inspect `orbit routine list`, `orbit auto-task
list` and sync warnings for actual definitions and enablement.

`orbit doctor`
does not report excluded defaults as missing. Its `seeding-policy` row explains
non-default policies; no such row is emitted when both keys are `all`.

For applying policy together with config and plugin enables from a reviewed
file, retain the [workspace preset procedure](first-run-workspace.md#workspace-presets).

## Scheduled task publication

The disabled weekly `task-publication-<workspace>` routine publishes only its
owned workspace and always omits attachments; it has no settings. Manual
publication keeps its CLI attachment policy.
See [publication-bind.md](publication-bind.md#weekly-publication-routine) for the schedule,
owner-only behavior, credentials and enablement.
