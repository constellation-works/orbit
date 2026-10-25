---
title: Maintain and remove workspaces
description: "Inspect bindings and ship mode, synchronize shipped defaults while preserving local edits, inspect migrations, rebind source remotes, and safely remove or tear down registered workspaces."
sidebar:
  order: 7
---

After [installing Orbit](../../getting-started/install/) and registering a workspace with `orbit workspace init`, you maintain the workspace as your codebase evolves and Orbit releases updates.

This guide covers everyday workspace maintenance: inspecting bindings and ship mode, synchronizing shipped defaults after upgrading the CLI, inspecting schema and layout migrations, safely updating repository remotes, and choosing between deregistering a checkout and tearing down its artifacts.

## Inspect bindings and ship mode

To inspect the current checkout's registered workspace binding:

```bash
orbit workspace show
```

This displays the logical workspace name and ID (`ws_*`), checkout path, local role (`owner` or `replica`), ship mode (`pr` or `local`), and the configured base branch. To inspect all registered workspaces across the machine, run `orbit workspace list`.

### Change the delivery ship mode

Workspaces deliver completed tasks by opening a pull request (`pr`) or merging directly into the local base branch (`local`). You can inspect or change this setting without re-running initialization:

```bash
orbit workspace ship-mode
orbit workspace ship-mode local
orbit workspace ship-mode pr
```

Passing a mode updates the workspace registry entry directly and reports the previous mode. Managed defaults and configuration files remain untouched. To target a different workspace by name or ID, pass `--workspace`:

```bash
orbit --workspace my-project workspace ship-mode local
```

### Validate or reassert checkout roles

In multi-machine environments, one checkout acts as the `owner` while checkouts on follower machines act as `replica` checkouts. To validate or update a checkout's declared role:

```bash
orbit workspace role my-project owner
orbit workspace role my-project replica --owner hm_abcdef0123456789
```

Declaring a `replica` requires `--owner` with the stable machine ID of the owner host. Declaring an `owner` rejects `--owner`. Orbit validates the declaration before saving to prevent invalid configurations, such as claiming the owner role on a non-owner machine.

## Synchronize shipped defaults

When you upgrade the Orbit binary (see [Stay current](../../getting-started/install/#stay-current)), newer releases may include updated routine definitions, executors, policies, or prompt templates.

`orbit workspace sync` converges your workspace's managed artifacts on the defaults shipped by the installed binary while preserving your local edits:

```bash
orbit workspace sync
```

### Convergence and local edits

- **Preserves local modifications:** Managed files that you have customized are preserved. The sync report classifies actions by scope and outcome, highlighting items marked `Preserved` or `BindingDrift`.
- **Reconciles gitignore:** Sync updates the managed `.orbit/` block in the repository's `.gitignore` in place to keep workspace state untracked. Sync never runs `git` commands on your behalf; if `.orbit/` files were already tracked before initialization, run `orbit doctor` to identify them, then run `git rm -r --cached .orbit`.
- **Authority and scope:** Sync requires an initialized and registered workspace as well as an initialized machine identity. It refuses to run in unregistered directories.

### Preview pending convergence

To inspect what sync would change without modifying files:

```bash
orbit workspace sync --check
```

`--check` reports pending updates and exits with status `0` if the workspace is up to date, or status `3` if convergence changes are pending. This status code makes `--check` suitable for pre-flight scripts and CI checks.

## Inspect and apply migrations

Orbit manages two versioned migration ledgers: the SQLite store schema (inside `orbit.db` and task stores) and the workspace layout in `.orbit/`.

Both ledgers auto-apply whenever an Orbit command opens the runtime. To inspect pending migrations without applying them, run `orbit migrate` or `orbit migrate --dry-run`:

```bash
orbit migrate
```

`migrate` without `--confirm` connects without opening a runtime, listing the current and supported versions for both components:

```text
COMPONENT         CURRENT  SUPPORTED
workspace layout  1        1
store schema      1        1

workspace: /path/to/repo/.orbit (orbit 0.32.0)

Workspace is up to date.
```

If migrations are pending, `orbit migrate` lists them and exits with a nonzero status. To apply them explicitly:

```bash
orbit migrate --confirm
```

### Forward compatibility and upgrades

If a workspace was created or migrated by a newer Orbit binary:
- **Additive migrations:** If all newer migrations are additive, Orbit opens the workspace in a read-only forward-compatible mode. Read-only commands work, but write commands are refused until you upgrade Orbit.
- **Incompatible migrations:** If newer migrations are not forward-compatible, commands refuse to open the workspace. Upgrade your Orbit CLI binary to continue.

Before major upgrades, consider taking a snapshot of your workspace state as described in [Back up and move Orbit state](../state-and-backup/):

```bash
cp -a .orbit .orbit.bak
```

## Rebind a moved source Git remote

When you transfer a repository to a new organization, rename it on your forge, or change its remote URL, update the registered source identity so task publication, PR delivery, and checkout tracking continue to function.

Orbit identifies the source repository using a portable Git URL and refuses credentials, local filesystem paths, and remote aliases.

### Rebinding workflow

Follow these steps to rebind the remote:

1. **Inspect and record the current remote:**
   ```bash
   orbit workspace source-remote show --json
   ```
   Save the reported URL in case you need to roll back.

2. **Validate the rebind with a dry run:**
   ```bash
   orbit workspace source-remote rebind --remote git@github.com:example/new-repo.git --dry-run --json
   ```
   This confirms that the URL is valid and computes the new repository identity without changing the registry.

3. **Apply the rebind in the registry:**
   ```bash
   orbit workspace source-remote rebind --remote git@github.com:example/new-repo.git
   ```

4. **Update the checkout's Git origin:**
   Updating the Orbit registry does not change your Git configuration. Update your checkout's remote separately:
   ```bash
   git remote set-url origin git@github.com:example/new-repo.git
   ```
   Then verify the updated identity:
   ```bash
   orbit workspace source-remote show
   ```

5. **Rollback if needed:**
   To undo the transition, rebind back to the saved original URL using `orbit workspace source-remote rebind --remote <OLD_URL>`.

:::caution[Recreate publication bindings]
If this workspace uses task publication, existing publication bindings must be removed and recreated explicitly with `orbit workspace publication remove` and `orbit workspace publication bind`. Orbit never rewrites publication lineages or remote snapshot histories automatically. See [Publish and restore tasks](../task-publication/).
:::

## Remove or tear down a workspace

When you retire a project or move work to another machine, Orbit provides two distinct commands to unregister or delete workspace state: `orbit workspace remove` and `orbit workspace teardown`.

| Action | Command | Confirmation | Local `.orbit/` | Global task store |
|---|---|---|---|---|
| **Deregistration** | `orbit workspace remove <WORKSPACE>` | None (immediate) | Preserved | Retained for doctor recovery |
| **Teardown** | `orbit workspace teardown <WORKSPACE>` | Requires `--confirm` | Deleted | Deleted |

### Deregister with remove

`orbit workspace remove` deregisters the workspace from `~/.orbit/workspaces.json`:

```bash
orbit workspace remove my-project
```

- **Arguments and safety:** Takes only the workspace selector (name, `ws_*` ID, or absolute checkout path). It has no `--confirm` flag and does not provide an interactive preview prompt.
- **Local files preserved:** The `.orbit/` directory in your checkout is left untouched.
- **Task store retained:** The global task-store partition under `~/.orbit/tasks/` is preserved as checkout evidence so `orbit doctor` can classify it. If unarchived task bundles remain, the command reports the partition path and notes:
  ```text
  left task-store partition /home/user/.orbit/tasks/workspaces/ws_... (12 task bundle(s));
  the task-registry workspace binding is retained as checkout evidence so `orbit doctor` can classify it.
  Reclaim with `orbit doctor --fix-orphan-task-stores --confirm`.
  ```

Use `remove` when you want to unregister a checkout without losing task history or local settings.

### Delete artifacts with teardown

`orbit workspace teardown` completely removes Orbit artifacts associated with a workspace:

```bash
orbit workspace teardown my-project
```

Because teardown is destructive, Orbit enforces strict safety checks:
- **Safety refusals:** Teardown refuses if the current working directory belongs to a different registered checkout, preventing accidental deletion of the wrong project. It also refuses the global `~/.orbit/` directory and any data path not ending in `.orbit`.
- **Preview without confirmation:** Running teardown without `--confirm` prints the resolved plan and exits without making changes:
  ```text
  teardown is destructive. Resolved target:
  workspace:     my-project (ws_0123456789abcdef)
  repo root:     /path/to/my-project
  orbit dir:     /path/to/my-project/.orbit
  task stores:   1 partition(s)
  Pass --confirm to proceed.
  ```

To execute the teardown:

```bash
orbit workspace teardown my-project --confirm
```

When confirmed, teardown executes in order:
1. Deletes the task-store partition bound to the checkout from `~/.orbit/tasks/`.
2. Removes Orbit-owned skill links from `.agents/skills/` and `.claude/skills/`.
3. Deletes the checkout's `.orbit/` directory.
4. Deregisters the workspace from `~/.orbit/workspaces.json` last. Persisting deregistration only after deletions succeed ensures that any interrupted operation remains retryable with the same selector.

## Next steps

- [Install Orbit](../../getting-started/install/): Install the CLI and learn how binary updates apply to existing environments.
- [Back up and move Orbit state](../state-and-backup/): Protect authoritative machine identity, databases, and task bundles.
- [Publish and restore tasks](../task-publication/): Snapshot task backlogs to a dedicated Git repository.
