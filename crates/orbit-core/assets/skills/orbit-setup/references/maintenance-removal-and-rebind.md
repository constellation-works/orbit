# Maintenance: removing, re-targeting and rebinding a workspace

## Contents

- [Removing or re-targeting a workspace](#removing-or-re-targeting-a-workspace)
  - [Rebind a moved checkout](#rebind-a-moved-checkout)

## Removing or re-targeting a workspace

Choose by intent:

- To stop tracking a checkout but keep its data, use
  `orbit workspace remove <workspace>`. It removes the workspace and checkout
  records and the local task-publication binding. It keeps the checkout's
  `.orbit/` directory and task-store partition, including task bundles; the
  publication Git repository is not changed. This command has no `--confirm`
  or `--dry-run` option. Require the user's explicit authorization before
  running it.
- To delete Orbit data for a checkout, use
  `orbit workspace teardown <workspace> --confirm`. It deletes the task-store
  partitions bound to that checkout, Orbit-owned skill links under
  `.agents/skills/` and `.claude/skills/`, and its `.orbit/` directory, then
  deregisters the workspace. It leaves the source checkout and global Orbit
  data directory itself in place. Without `--confirm`, it prints the resolved
  deletion plan and
  refuses; there is no `--dry-run` option.

  Require the user's explicit
  authorization and a verified backup or task export before teardown.
- If the repository moved, use
  `orbit workspace source-remote rebind --remote <URL>`. First inspect and
  save the current identity with `orbit workspace source-remote show --json`,
  then validate the proposed change with
  `orbit workspace source-remote rebind --remote <URL> --dry-run --json`.
  After reviewing that result, apply the same command without `--dry-run` and
  update the checkout's Git `origin` separately.

  The new URL must be portable;
  credentials, local paths and remote aliases are refused. Rebinding changes
  the registered source remote and repository identity and reconciles the
  task-registry source pairing. Equivalent SSH and HTTPS URLs compare equal.
  Repeat the command with the registered URL to repair a pairing left stale
  by an older Orbit version, even when the source reports `unchanged`.
  Git configuration is updated separately.

  It refuses to rewrite an existing publication binding or its lineage and
  snapshots; record and remove that binding before the move, then use
  `orbit workspace publication rebind` with a new publication ID and an empty
  publication branch. Verify with `orbit workspace publication show --json`.
  See [Publication](publication-bind.md) and [Multi-host](multi-host.md) before
  moving state between hosts.
- If the checkout directory moved on disk and the repository did not change,
  use `orbit workspace rebind --path <DIR>` to re-point this host at it. This
  is not `remove` or `teardown`, and it does not change the source remote. See
  [Rebind a moved checkout](#rebind-a-moved-checkout).

Before `remove` or `teardown`, manually confirm there are no live drains, runs,
or claims, and identify any unpublished tasks. The commands do not enforce
these checks. Inspect `orbit task publication status --json`; a `current`
status does not prove that every recent task change is published. Preserve the
task data you need with a verified publication or export, and back up required
host configuration separately, following [Publication](publication-bind.md),
before teardown.

Capture `orbit workspace show --json` and
`orbit workspace source-remote show --json` before a change. After removal or
teardown, use `orbit workspace list --all --json` to confirm the workspace is no
longer registered on this host. `--all` includes replica checkouts, which the
default list hides. After a rebind, use `orbit workspace list --all --json`,
`orbit workspace show --json` and
`orbit workspace source-remote show --json` to verify the registration and
new source identity.

### Rebind a moved checkout

Use `orbit workspace rebind --path <DIR>` after the operator has moved a
registered Git checkout directory to a new path. It re-points this host's
registration at the directory that was already moved. It works on owner and
replica checkouts and keeps their role and owner.

It does not move files, change the source remote, or change another host. Run
it on each host whose checkout moved. If the repository's origin also changed,
use the source-remote bullet under "Choose by intent" for that change; it is a
separate operation. `remove` and `teardown` are not moves: `remove` stops
tracking the checkout and keeps its data, and `teardown` deletes the checkout's
Orbit data.

Preconditions, all before the directory is moved:

- Stop the workspace's runs, settle pulled claims, and release workspace claims
  and path reservations.
- Remove the retained Orbit-managed worktrees first.
  [Worktree garbage collection](maintenance-worktree-gc.md#worktree-garbage-collection) describes how
  worktrees are reclaimed.
- Move the entire repository, including `.git` and the checkout-local `.orbit`
  directory. External Orbit data roots are refused.
- Keep the host store outside the moved directory. The store, `orbit.db` under
  `~/.orbit` (or under the selected `--root`), must already exist. On a newly
  initialized checkout, run `orbit tool run orbit.task.list --input '{}'` from
  the registered checkout before moving it. Without the store, both apply and
  `--dry-run` refuse with `workspace_rebind_host_store_required` and write
  nothing.
- The new path must be the checkout's primary Git root, with the registered Git
  origin and the existing `.orbit` task-store identity. A target claimed by
  another workspace is refused.

If the directory has already moved and the store is missing, move it back to
its registered path, run the runtime command there, then move it again and
rebind.

Preview first. `--dry-run` validates and reports the new binding. It leaves the
checkout's binding in the workspace registry and task store, the pending rebind
record and the audit log unchanged. Normal generation admission and registry
lock bookkeeping can still write, so a preview does not promise that the host
root is untouched. A missing host store is refused earlier, before generation
admission, as the precondition above describes.

Review the output, then repeat
the command without `--dry-run`:

```bash
orbit --workspace <registered-name-or-id> workspace rebind --path /new/checkout --dry-run --json
orbit --workspace <registered-name-or-id> workspace rebind --path /new/checkout --json
```

If the command is interrupted, runtime commands refuse to open the workspace
until the same rebind finishes its recovery. Repeat the same command with the
same `--path`, and finish that recovery before starting another rebind.

Verify from the new directory:

```bash
cd /new/checkout
orbit workspace list --all --json
orbit workspace show --json
orbit tool run orbit.task.list --input '{}'
```

`workspace list --all --json` should list the workspace on this host. `--all`
is required to include a replica checkout, which the default list hides. The
record's `repo_root` should be the new path, and its `role` (`owner` or
`replica`) is unchanged by the rebind. The default listing,
`orbit workspace list --json`, still shows an owner checkout.
`workspace show --json` should resolve it from the new directory. The runtime
command confirms that the store opens against the moved checkout.

A successful
rebind also appears in `orbit audit list --json`.
