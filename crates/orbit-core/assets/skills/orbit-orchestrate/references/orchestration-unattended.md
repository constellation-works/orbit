# Supervise shared and unattended drains

## Multi-operator workspaces

When two operators act on the same authoritative workspace store, one can hold
an exclusive claim and another must present its token. Claims do not coordinate
independent stores on different machines. These examples present an existing
token; they do not acquire a claim:

```bash
orbit run ship --claim-token <token>
ORBIT_WORKSPACE_CLAIM_TOKEN=<token> orbit run auto --for 1h
```

For splitting work across machines so their task IDs and schedules don't collide
in the first place, see [multi-host.md](../../orbit-setup/references/multi-host.md).

## Unattended shipping

An `orbit run auto` drain is the way to ship without a human watching: it
admits the ready backlog for its window, one workspace per drain, and never
grants `--complete` unless the operator passed it. No routine ships work; the
retired `ship-sweep` routine and command are gone, and a workspace that still
holds the routine loads it, warns, and never runs it. Before leaving a drain
unattended:

- Confirm the registered workspace base branch (else `workflow.base_branch`) points where PRs should actually land.
- Enable worktree GC first — unattended shipping is the fastest way to fill a
  disk with abandoned worktrees. → [maintenance-worktree-gc.md](../../orbit-setup/references/maintenance-worktree-gc.md)
- Check `orbit run readiness` across a realistic backlog first.
