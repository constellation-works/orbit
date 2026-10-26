---
title: Publish and restore tasks
description: "Back up a workspace's task records to a dedicated Git repository and restore them: ask your agent, or run three commands."
sidebar:
  order: 5
---

Task publication pushes a snapshot of one workspace's task records to a Git
repository you control, so the backlog survives losing the machine. You choose
when to publish; nothing publishes on its own.

It covers task records only. Audit events, run history, and configuration
need a full state backup: see [Back up and move Orbit state](../state-and-backup/).

:::tip[Let your agent do it]
Ask your agent to **back up this workspace's tasks to `<repository URL>`**. The
`orbit-setup` skill binds the repository, publishes a first snapshot, and
checks that it landed. Ask the same way to publish again or to restore.
:::

## What you need

- A Git repository used only for task publications, reachable over SSH or HTTPS. It
  may be shared by several workspaces, each on its own branch (below); what
  must be unused is the branch for this workspace, not the whole repository.
  For HTTPS, configure the explicit credential bridge below;
  publication ignores global Git credential helpers.
- The workspace's owner checkout, not a replica, with a Git `origin` remote.
  Orbit identifies the source repository by that remote and refuses to bind
  without one.

Orbit refuses the source repository itself and any URL with credentials in it.
Keeping the repository private is up to you: task text and attachments can
hold sensitive detail.

## Publish

From the workspace's checkout:

```bash
orbit workspace publication bind \
  --remote git@example.com:backups/my-tasks.git \
  --publication-id pub_my_tasks
orbit task publication publish
orbit task publication status
```

`bind` is a one-time, machine-local record and contacts nothing. Each
`publish` writes snapshots to the same history, reusing an unchanged snapshot,
and never force-pushes over another writer. `status` should report `current`, with matching local and
remote generations and commits.

## Share one repository across workspaces

One repository can hold many workspaces' snapshots. Bind each workspace to
the same repository with its own branch and its own publication ID:

```bash
# From each workspace's owner checkout, with that workspace's values:
orbit workspace publication bind \
  --remote git@example.com:backups/all-tasks.git \
  --publication-id pub_my_workspace \
  --branch ws/my-workspace
orbit task publication publish
```

A workspace whose branch does not exist yet starts at generation 1 on that
branch, even when the repository already holds other workspaces' branches.
Publishing never reads, writes, or validates another workspace's branch, and
the first push only succeeds while its own branch is still absent. If the
branch already exists but is not this workspace's publication — foreign
commits, or another publication ID, workspace, or owner — publishing refuses
and changes nothing. Orbit does not check the rest of the repository (a
default branch with ordinary commits, say), so keep the repository private
and dedicated to publications.

Attachments are opt-in. By default, `publish` refuses if any task has one and
lists them. Pass `--attachments omit` to publish the task records without
them, or `--attachments include` only after you have reviewed their content
(it also needs `--allow-unscanned-attachments`, since Orbit has no scanner).

## Authenticate unattended HTTPS publication

Publication deliberately ignores system and global Git configuration, including
helpers installed by `gh auth setup-git`. The supported HTTPS path is an explicit
`GIT_ASKPASS` executable. Orbit passes through `GIT_ASKPASS`, `PATH`, `HOME`,
`XDG_CONFIG_HOME`, `GH_CONFIG_DIR`, `GH_TOKEN` and `GITHUB_TOKEN` when set, so the
bridge can use the service account's existing GitHub CLI authentication. SSH
publication can instead use `SSH_AUTH_SOCK`, `GIT_SSH` or `GIT_SSH_COMMAND` and
that account's SSH configuration. Orbit sets `GIT_TERMINAL_PROMPT=0` and
`GCM_INTERACTIVE=never`; neither path may require a TTY or an unlock prompt.

For GitHub HTTPS, authenticate `gh` once as the account that runs Orbit, with
write access to the dedicated publication repository. Replace `OWNER/WORKSPACE-tasks`
with your empty repository. If already bound, verify the remote with
`orbit workspace publication show` and skip `bind`:

```sh
orbit workspace publication bind \
  --remote https://github.com/OWNER/WORKSPACE-tasks.git \
  --publication-id pub_my_tasks
gh auth login --hostname github.com
gh auth status --hostname github.com
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

The bridge contains no token and sends the token only through Git's credential
pipe. Keep credentials out of URLs, command arguments and diagnostic output;
do not enable shell tracing in the bridge. It only answers prompts for
`github.com`; use a separate host-scoped bridge for another provider.
Publication disables Git tracing and withholds transport stderr because helpers
and servers can echo credentials there. A transport failure should be diagnosed
by checking `gh auth status`, repository access and the service environment.

For a clock-tick/systemd run, keep this bridge at its persistent absolute path
and set `GIT_ASKPASS` and `LC_ALL=C` in the **Orbit service's** environment, not
just an interactive shell. For example, a user-service drop-in can contain:

```ini
[Service]
Environment=GIT_ASKPASS=%h/.local/libexec/orbit-publication-askpass
Environment=LC_ALL=C
```

Reload and restart the installed service after applying its drop-in. Ensure its
`PATH` includes `gh` (or use its absolute executable path in the bridge), and its
`HOME`/`GH_CONFIG_DIR` and credential store are accessible without an interactive
keyring unlock. If using a token environment variable, provision it through the
service's private credential mechanism rather than putting its value in a unit
file or command line. Verify `publish` and `status` from that same service
account/environment with stdin closed before relying on a scheduled run.

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

It refuses a destination that already has tasks, and it never renumbers or
partly replaces anything. `orbit task publication inspect` takes the same
flags without `--confirm` and shows a snapshot without adopting it. To move
tasks to a different machine identity, use `orbit task export` and
`orbit task import` instead; see
[Restore tasks, or move them](../state-and-backup/#restore-tasks-or-move-them).

## When something is wrong

| Symptom | What to do |
|---|---|
| Git asks for a username or password. | Set the persistent `GIT_ASKPASS` bridge in the service environment as shown above, or configure unattended SSH. |
| The first publish finds unrelated history on its own branch. | Stop. Orbit won't adopt a branch it didn't create for this workspace; bind an unused branch (other workspaces' branches in the same repository are fine). |
| Status reports a moved branch or an authority conflict. | Stop publishing and find the other writer or the stale binding. |

The [task publication runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/task-publication.md)
has the full procedure, including rebinding and attachment limits.
