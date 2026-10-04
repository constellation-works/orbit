---
title: Publish and Restore Tasks
description: "Back up a workspace's task records to a dedicated Git repository and restore them: ask your agent, or run three commands."
sidebar:
  order: 5
---

Task publication pushes a snapshot of one workspace's task records to a Git
repository you control, so the backlog survives losing the machine. You choose
when to publish; nothing publishes on its own.

It covers task records only. Audit events, run history, and configuration
need the
[state and backup runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/state-and-backup.md).

:::tip[Let your agent do it]
Ask your agent to **back up this workspace's tasks to `<repository URL>`**. The
`orbit-setup` skill binds the repository, publishes a first snapshot, and
checks that it landed. Ask the same way to publish again or to restore.
:::

## What you need

- An empty Git repository used only for this workspace's tasks, reachable over
  SSH. HTTPS works too, but needs the credential bridge in the
  [runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/task-publication.md),
  because publication ignores your global Git credential helpers.
- The workspace's owner checkout, not a replica.

Orbit refuses the source repository itself and any URL with credentials in it.
Keeping the repository private is up to you: task text and attachments can
hold sensitive detail.

## Publish

From the workspace's checkout:

```bash
orbit workspace publication bind --remote git@example.com:backups/my-tasks.git \
  --publication-id pub_my_tasks
orbit task publication publish
orbit task publication status
```

`bind` is a one-time, machine-local record and contacts nothing. Each
`publish` adds a new snapshot to the same history and never force-pushes over
another writer. `status` should report `current`, with matching local and
remote generations and commits.

Attachments are opt-in. By default, `publish` refuses if any task has one and
lists them. Pass `--attachments omit` to publish the task records without
them, or `--attachments include` only after you have reviewed their content
(it also needs `--allow-unscanned-attachments`, since Orbit has no scanner).

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
`orbit task import` instead.

## When something is wrong

| Symptom | What to do |
|---|---|
| Git asks for a username or password. | Use SSH, or pass a `GIT_ASKPASS` bridge (see the runbook). |
| The first publish finds unrelated history. | Stop. Orbit won't adopt a repository it didn't create; use an empty one. |
| Status reports a moved branch or an authority conflict. | Stop publishing and find the other writer or the stale binding. |

The [task publication runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/task-publication.md)
has the full procedure, including rebinding and attachment limits.
