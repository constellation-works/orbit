---
title: Publish and Restore Tasks
description: "Bind a workspace to a dedicated Git repository, publish a task snapshot, verify it, and restore it under the same authority."
sidebar:
  order: 5
---

Task publication is an explicit durability channel for your **task records**. It
pushes a validated snapshot of one owned workspace's tasks to a dedicated Git
repository you control, so the backlog survives losing the machine.

Two things it deliberately is not:

- **It is not automatic.** No task mutation publishes anything. Orbit ships no
  publication routine. You publish when you decide to.
- **It is not a full backup.** Audit events, run history, claims, reservations,
  configuration, host identity, and runtime caches are all out of scope. For
  those, back up the global root and database — see the [state and backup
  runbook](https://github.com/danieljhkim/orbit/blob/main/docs/runbooks/state-and-backup.md).

## Before you bind

You need:

- a registered **owner** checkout of the workspace, not a replica;
- one empty, dedicated Git repository, used for exactly this one workspace;
- working SSH or HTTPS credentials for it; and
- the workspace's logical `ws_*` selector.

```bash
orbit workspace list --all --format json
```

Never reuse the source repository as the publication destination, and never put
credentials in the remote URL — Orbit refuses both. Orbit also cannot prove that
your repository is private or erase history from it, so provider-side privacy,
collaborators, retention, and branch protection stay your responsibility.

## Bind the repository

The global `--workspace` selector must come *before* `workspace publication` or
`task publication`:

```bash
orbit --workspace ws_example workspace publication bind \
  --remote git@example.com:backups/example-tasks.git \
  --publication-id pub_example_primary \
  --branch refs/heads/main

orbit --workspace ws_example workspace publication show --json
```

The binding is machine-local. It records the logical workspace, portable source
identity, publication remote and branch, the lineage ID, and the authority
machine — and never credentials or checkout paths. Neither `bind` nor `show`
contacts the remote.

To change lineage later, use `workspace publication rebind`. To drop the local
binding and last-success record without touching the repository, use
`workspace publication remove --confirm`.

### Authentication

SSH is the simplest durable choice. Verify reachability before binding; an empty
result with exit status 0 is normal for a new empty repository:

```bash
git ls-remote git@example.com:backups/example-tasks.git
```

For HTTPS, note that publication Git runs non-interactively with system and
global Git configuration disabled — so a credential helper installed only in
your global config (including one from `gh auth setup-git`) is **not** read.
Pass a short-lived `GIT_ASKPASS` bridge to each command that touches the remote
instead. The runbook linked at the bottom has a ready-made one.

## Publish a snapshot

Attachments are an explicit exposure decision, and the default is fail-closed:

| `--attachments` | Behavior | Use it when |
|---|---|---|
| `fail` (default) | Refuses the whole publication if any attachment exists. | First attempt, and workspaces that should have none. |
| `omit` | Publishes core task records plus an omission ledger. | Attachments exist but are not approved for Git. |
| `include` | Applies size and deny-pattern limits, then includes admitted bytes. | Only after deliberate content review. |

Start with the default so Orbit inventories anything blocking before you choose
a policy:

```bash
orbit --workspace ws_example task publication publish --attachments fail --json
```

If the named attachments are not approved for Git storage, publish the core
records only:

```bash
orbit --workspace ws_example task publication publish --attachments omit --json
```

There is no sensitivity-scanner integration today, so `include` still refuses
attached bytes unless you add `--allow-unscanned-attachments`. A private
repository is not a substitute for reviewing those bytes: if a secret reaches
Git history, rotate the credential and do provider-side history remediation —
deleting it from the latest commit is not erasure. `--max-file-bytes`,
`--max-total-bytes`, and repeatable `--deny-pattern` bound what `include`
admits.

Publishing uses an Orbit-owned cache. It never checks out, switches, stages, or
changes your source worktree's branch.

## Verify it

After every publish, compare the owner-local success record against the
validated remote tip:

```bash
orbit --workspace ws_example task publication status --json
```

A good result has all of:

- `state` is `current`;
- local and remote generation numbers match;
- local and remote commit IDs match; and
- `incomplete_attachments` matches the policy you chose (`true` after `omit`).

The first publish creates generation 1 and the branch in the empty repository.
Later publishes advance the same linear lineage with compare-and-swap semantics,
so Orbit will not merge or force-push over an unexpected writer.

## Inspect a snapshot without adopting it

`inspect` is read-only and needs no local binding. You supply the pairing facts
rather than trusting the repository to declare its own identity:

```bash
orbit --workspace ws_local task publication inspect \
  --workspace-id ws_example \
  --source-remote git@example.com:team/example.git \
  --publication-id pub_example_primary \
  --authority-machine-id hm_alpha \
  --remote git@example.com:backups/example-tasks.git \
  --json
```

The result labels every record with publication time, generation, workspace,
source identity, authority, publication ID, commit, freshness, and completeness,
and marks it `render_authority: snapshot` — it is a snapshot, not live owner
state. A pairing mismatch, unsupported schema, corrupt data, changed bytes, or
invalid Git lineage returns no trusted task projection at all.

Add `--commit <sha>` to inspect an exact commit instead of the branch tip.

## Restore

Restore is deliberately narrow: it reinstates a **same-authority** snapshot into
an empty destination.

```bash
orbit --workspace ws_example task publication restore \
  --workspace-id ws_example \
  --source-remote git@example.com:team/example.git \
  --publication-id pub_example_primary \
  --authority-machine-id hm_alpha \
  --remote git@example.com:backups/example-tasks.git \
  --confirm
```

What to expect:

- `--confirm` is mandatory; it is a deliberate mutation of the canonical task
  store.
- There is no authority-transfer command. The destination must be an owner
  checkout whose machine, logical workspace ID, and source remote match the
  publication. Restore global configuration plus `host.toml` and
  `workspaces.json` first.
- The default refuses any non-empty destination. For an interrupted or repeated
  recovery, `--allow-identical-retry` admits only byte-identical task-ID
  collisions; a single non-identical collision aborts the whole restore, with no
  renumbering and no partial replacement.

To move tasks between *unrelated* authorities, use `orbit task export` and
`orbit task import` with renumbering instead. That is migration, not recovery.

## When something is wrong

| Symptom | What it means |
|---|---|
| Git cannot read a username, password, or terminal prompt. | Global credential helpers are intentionally isolated. Use SSH, or pass `GIT_ASKPASS` explicitly. |
| The binding seems missing or belongs to another checkout. | Re-check with `orbit workspace list --all --format json`, then `workspace publication show` under the intended logical `--workspace`. |
| The remote is rejected at bind time. | Credentials in the URL, or the same repository as the source. Use a dedicated one. |
| The first publish finds unrelated history. | Stop. Orbit will not adopt or overwrite a repository it did not create. Provision an empty one. |
| Status reports a moved branch or authority conflict. | Stop publishing and find the unexpected writer or stale binding. |

The full operational procedure — credential bridges, identity diagnosis, and
escalation — is in the [task publication
runbook](https://github.com/danieljhkim/orbit/blob/main/docs/runbooks/task-publication.md).
