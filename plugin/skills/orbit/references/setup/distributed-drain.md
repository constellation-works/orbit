# Distributed drain setup and recovery

One owner checkout, optional replica checkouts, and a read-only owner preflight.
Use this when the user wants a second machine to execute the same logical
workspace later, or when a claimed attempt needs inspection. Installing matching
binaries is not a rollout.

Public pull, binding, settlement, handoff approval, and `orbit run auto --pull`
are **not registered**. Do not invent those tools, write a callers file, or try
to enable the gated mutation surface. Owner-local claimed leaves already exist
on the owner; a follower destination still fails that gate.

Owner/replica registration lives in
[multi-host.md](../../../orbit-setup/references/multi-host.md). SSH federation
and session authority live in
[remote-access.md](../../../orbit-setup/references/remote-access.md). Tool
routing is in [tool-surface.md](../tool-surface.md).

## What v1 does not do

- No heartbeat, automatic reclamation, fleet registry, or follower merge.
- No automatic review. v1 admits only `review_policy = none`. Status `review`
  means a delivery handoff is waiting, not that a reviewer ran.
- Age, reservation TTL, and a missing local run are diagnostics, not proof of
  death.
- Seeded `ship_sweep`, `workspace_ship_pipeline`, and `orbit run ship-sweep`
  stay at their current enablement. This setup does not turn them on.
- Remote machine labels (`--remote-caller-machine-id`) are attribution, not
  credentials. SSH login is owner access. There is no destination callers
  file, forced-command acceptance, KeyBound proof, or replacement identity
  registry.

## Prerequisites

On every participating host:

```bash
orbit --version
orbit host show
orbit workspace show
orbit doctor
orbit config get operation.review_policy
```

Require one owner per repository, matching binaries, matching distributed-drain
protocol schema `1`, equivalent crew and toolchain resolution, and
`operation.review_policy = none`. Empty
`workflow.required_validation_commands` is fail-closed for a claimed handoff.
A leftover `~/.orbit/mcp-callers.toml` or `~/.orbit/mcp-ssh-acceptance/` is
ignored: `orbit doctor` warns; delete the files. Deny a caller by removing its
key from `~/.ssh/authorized_keys`.

Per-host compiler capacity is independent of drain slots. Keep the shared
build-budget defaults (two heavy slots, four Cargo jobs) unless the operator
raises them.

Managed agents never propagate `--operator` or `ORBIT_OPERATOR`. Claim,
machine, bound run, and phase checks fence attempt ownership from trusted
runtime invocation context, not from a payload label.

## Collapse two owners before any replica work

If both hosts initialized the same repo as owners, stop competing drains on the
host that will become a replica, reconcile in-flight runs and reservations,
export tasks that must move, then re-register:

```bash
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica
```

`workspace role` reasserts; it is not a takeover. Copying Git history does not
move the control plane.

```bash
orbit task export --ids <task-id> -o <archive>
orbit task import <archive> --on-conflict=renumber
```

`--on-conflict=owner-wins` is for a repeatable mirror of IDs the owner already
minted. Do not rsync the store. Restore pruned `context_files` only from
history:

```bash
orbit task lint <task-id>
orbit task lint <task-id> --restore-pruned
```

Missing files are valid declarations. `--restore-pruned` never guesses.
Reservation TTL on a pulled claim is 14,400 seconds and does not revoke the
claim or shrink the frozen footprint.

## Read-only owner surface

Both probe and receipt lookup are owner-served `control_plane` tools. A replica
destination refuses them. They need an identified caller (`agent` or
`operator`); a non-interactive shell uses an agent envelope or
`ORBIT_OPERATOR=1`. They create no claim.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
```

Declaring version, schema, or review policy reports the first refusal
admission would raise. The probe is never a health check for admission.

```bash
orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

Returns `found`, `expired` (permanent tombstone), or `not_found`. `not_found`
does not license a replacement request ID. Naming another machine's namespace
needs operator capability. Idle receipts compact immediately; unsettled claims
keep full receipts. Do not delete tombstones by age.

Claim listing is operator-only and off MCP:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

It inspects. It does not reclaim.

## Recovery

Generic resume of a claimed leaf is refused:

```bash
orbit job resume <run-id>
```

Inspect the claim, task, locks, and the run on the execution host named by
`job_run_host`. Reconcile an uncertain merge intent before any reassignment.
Public approve/revoke/pull tools are not registered; do not invent them, do not
ship the same task again, and do not treat `--complete` on a follower as
landing. Followers never merge. A returning worker after revocation receives
`stale_claim`.

## Leftover epic and review state

There is no epic execution path. Gather evidence with ordinary commands:

```bash
orbit run history -j epic_pipeline --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_host
orbit task locks list
orbit doctor
```

Do not treat the workspace as migrated while an old epic/child run, reservation,
or uncertain landing is live, even if the root is already `review`. Inherited-only
epic roots (no own context, descendants that have some) need operator-supplied
own context; nothing inherits the old union.

Failed-run triage is gone. Re-backlog is a deliberate status write after
someone inspects `blocked` and `job_run_host`.

## Verify, then stop

A clean probe, matching versions, `review_policy = none`, and a replica role
mean the hosts are **installed**. They do not mean pull is enabled. Confirm
`orbit run auto --help` has no `--pull`, and leave schedules untouched.
