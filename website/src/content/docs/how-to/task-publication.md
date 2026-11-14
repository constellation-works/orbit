---
title: Publish and restore tasks
description: "Back up a workspace's task records to a Git repository you control, on demand or weekly, and restore them after losing the machine."
---

Task publication pushes a snapshot of one workspace's task records to a Git
repository you control, so the backlog survives losing the machine. It covers
task records only. For audit events, run history and configuration, see
[Back up and move Orbit state](/how-to/state-and-backup/).

:::tip[Let your agent do it]
Ask your agent to **back up this workspace's tasks to `<repository URL>`**. The
`orbit-setup` skill binds the repository, publishes a first snapshot, and
checks that it landed. Ask the same way to publish again or to restore.
:::

## What you need

- A private Git repository used only for task publications, reachable over
  SSH or HTTPS. Task text can hold sensitive detail. Orbit refuses the source
  repository itself and any URL with credentials in it.
- The workspace's owner checkout, not a replica, with a Git `origin` remote.
  Orbit identifies the source repository by that remote.

## Publish

From the workspace's checkout:

```bash
orbit workspace publication bind \
  --remote git@example.com:backups/my-tasks.git \
  --publication-id pub_my_tasks
orbit task publication publish
orbit task publication status
```

`bind` runs once. It records the binding on this machine and contacts
nothing. Each `publish` adds a snapshot to the same history and never
force-pushes over another writer. `status` should report `current`.

By default, `publish` refuses if any task has an attachment, and lists them.

| Flag | Effect |
|---|---|
| `--attachments omit` | Publishes the task records without attachments. |
| `--attachments include --allow-unscanned-attachments` | Publishes attachments. Review their content first: Orbit has no scanner. |

## Scheduled publication

`orbit workspace init` seeds a weekly routine, `task-publication-<workspace>`,
disabled. `orbit workspace sync` adds it, also disabled, to a workspace that
lacks it. Check your binding and unattended credentials first, then enable
it with the name from `orbit routine list`:

```bash
orbit routine enable task-publication-<workspace>
```

Enabling is operator-only and audited; an agent session is refused. Do not
edit `enabled` in the routine's YAML by hand, which skips both.

The routine runs Sundays at 04:47 host-local time, skips missed runs, and
always omits attachments. Replicas never run it. To run the same job now:

```bash
orbit run job task_publication_pipeline --wait
```

| Situation | Result |
|---|---|
| The workspace has no binding | The run succeeds with `skipped: publication_unbound`. |
| Nothing changed | `status: unchanged`, no new commit. |
| An authority conflict or an authentication failure | The run fails with the publisher's error. |

A scheduled run reads credentials from the clock service's environment, not
your shell. Set them up as shown under
[Authenticate unattended HTTPS publication](#authenticate-unattended-https-publication).

## Share one repository across workspaces

Bind each workspace to the same repository with its own branch and its own
publication ID:

```bash
# From each workspace's owner checkout, with that workspace's values:
orbit workspace publication bind \
  --remote git@example.com:backups/all-tasks.git \
  --publication-id pub_my_workspace \
  --branch ws/my-workspace
orbit task publication publish
```

Publishing reads and writes only its own branch. If that branch already
exists and is not this workspace's publication, publishing refuses and
changes nothing. Orbit does not check the rest of the repository, so keep it
dedicated to publications.

## Authenticate unattended HTTPS publication

Publication ignores system and global Git configuration, including helpers
installed by `gh auth setup-git`. It must never need a terminal or an unlock
prompt.

- **SSH:** publication uses `SSH_AUTH_SOCK`, `GIT_SSH` or `GIT_SSH_COMMAND`
  and the account's SSH configuration.
- **HTTPS:** point `GIT_ASKPASS` at an executable that answers Git's prompts.
  Orbit passes `GIT_ASKPASS`, `PATH`, `HOME`, `XDG_CONFIG_HOME`,
  `GH_CONFIG_DIR`, `GH_TOKEN` and `GITHUB_TOKEN` through to Git.

For GitHub over HTTPS, sign `gh` in once as the account that runs Orbit, with
write access to the publication repository. Bind the workspace with the
repository's `https://` URL as in [Publish](#publish). Then create the bridge
and publish:

```bash
gh auth login --hostname github.com
mkdir -p "$HOME/.local/libexec"
cat > "$HOME/.local/libexec/orbit-publication-askpass" <<'ASKPASS'
#!/bin/sh
case "$1" in
  *"Username for 'https://github.com'"*) printf '%s\n' x-access-token ;;
  *"Password for 'https://x-access-token@github.com'"*)
    exec gh auth token --hostname github.com ;;
  *) exit 1 ;;
esac
ASKPASS
chmod 700 "$HOME/.local/libexec/orbit-publication-askpass"
export GIT_ASKPASS="$HOME/.local/libexec/orbit-publication-askpass"
export LC_ALL=C
orbit task publication publish
orbit task publication status
```

The bridge holds no token and answers only for `github.com`. Write a separate
bridge for another provider. Publication withholds Git's transport errors
because they can echo credentials, so diagnose a failure with
`gh auth status`, repository access, and the service environment.

For scheduled runs, set both variables in the Orbit service's environment,
not only in your shell. A systemd user-service drop-in can contain:

```ini
[Service]
Environment=GIT_ASKPASS=%h/.local/libexec/orbit-publication-askpass
Environment=LC_ALL=C
```

Restart the service afterwards. Its `PATH` must include `gh`, and its
credential store must open without an interactive keyring unlock. Do not put
a token value in a unit file or on a command line. Before you rely on the
schedule, run `publish` and `status` from that service account's environment.

## Restore

Restore puts a snapshot back into an empty task store on the same machine
identity. First restore the global `config.toml` and `workspaces.json`, then
pass the facts that identify the snapshot:

```bash
orbit task publication restore \
  --workspace-id <ws_id> \
  --source-remote git@example.com:team/my-repo.git \
  --publication-id pub_my_tasks \
  --authority-machine-id <machine-id> \
  --remote git@example.com:backups/my-tasks.git \
  --confirm
```

Add `--branch` when the workspace published to a branch other than `main`,
and `--commit` to restore an exact snapshot instead of the branch tip.
`orbit task publication inspect` takes the same flags without `--confirm` and
shows a snapshot without adopting it.

Restore refuses a destination that already has tasks. It never renumbers or
partly replaces anything. To move tasks to a different machine or workspace,
use export and import: see
[Restore tasks, or move them](/how-to/state-and-backup/#restore-tasks-or-move-them).

## When something is wrong

| Symptom | What to do |
|---|---|
| Git asks for a username or password. | Set the `GIT_ASKPASS` bridge in the service environment, or configure unattended SSH. |
| The first publish finds unrelated history on its branch. | Stop. Orbit won't adopt a branch it didn't create for this workspace. Bind an unused branch. |
| `status` reports a moved branch or an authority conflict. | Stop publishing and find the other writer or the stale binding. |

The [task publication runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/task-publication.md)
covers rebinding and attachment limits.
