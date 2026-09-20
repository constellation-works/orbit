---
type: runbook
summary: Set up, migrate, and recover a single-owner distributed drain without enabling gated pull or follower merges.
tags: [operations, distributed-drain, multi-host, recovery]
paths:
  - "crates/orbit-tools/src/builtin/orbit/drain/**"
  - "crates/orbit-core/src/application/distributed.rs"
  - "crates/orbit-cli/src/command/task/lint.rs"
  - "crates/orbit-web/src/api/distributed.rs"
related_features: [distributed-drain, federated-mcp, host-registry, remote-access]
related_artifacts: [ORB-12516, ORB-12515, ORB-12500, ORB-12495, ORB-12564, ORB-12491, ORB-12490]
last_validated: 2026-09-20
---

# Set Up and Recover a Single-Owner Distributed Drain

Use this runbook to collapse two independent owners, match follower
prerequisites, inspect the live read-only drain surface, migrate leftover
epic/child/review state, and recover a claimed attempt. Installing matching
binaries is not a rollout. Public pull, binding, settlement, routed handoff
acceptance, and `orbit run auto --pull` are not registered; do not invent them
or try to turn the gated mutation surface on. The owner's own dashboard does
carry approve, revoke and recover for the claims this checkout holds — that is
an operator surface on the owner, not a routed entry point, and it does not
enable anything for a follower.

## Prerequisites and safety

- **One control plane per repository.** Two independently initialized owners of
  the same repo mint overlapping work. Collapse to one owner before any replica
  pull is even a future option.
- **No live host migration in this procedure.** Copy no private hostnames,
  credentials, or inventory. Use placeholders.
- **No automatic reclamation, automatic review, fleet registry, or follower
  merge.** Age, reservation TTL, and a missing local run are diagnostics, not
  death. `review` means a delivery handoff is waiting; it does not mean a
  reviewer ran.
- **Managed agents never become operators.** A client inside a managed run, or
  with an agent envelope, does not propagate `--operator` or `ORBIT_OPERATOR`.
- **Seeded schedules stay as they are.** This procedure does not enable
  `ship_sweep`, `workspace_ship_pipeline`, or the independent
  `orbit run ship-sweep` CLI.

Placeholders:

| Placeholder | Meaning |
|---|---|
| `<owner-machine-id>` | Owner `machine_id` from `orbit host show` on the owner |
| `<workspace-id>` | Logical `ws_*` id from `orbit workspace show` |
| `<selector>` | Host-qualified selector from federated `orbit_workspace_list` |
| `<ssh-alias>` | An SSH config alias already able to log in as the owner |
| `<request-id>` | Durable ID of one intended admission request |
| `<archive>` | Path to a `tar.zst` from `orbit task export` |

## Inspect before mutating

On **every** participating host:

```bash
orbit --version
orbit host show
orbit workspace show
orbit doctor
```

Confirm:

- binary versions match;
- each host has a distinct task prefix;
- exactly one checkout of this repository reports role `owner`;
- `orbit doctor` `mcp-callers` is `ok`, or a leftover
  `~/.orbit/mcp-callers.toml` / `~/.orbit/mcp-ssh-acceptance/` warning that
  those files **grant nothing** (delete them; deny access by removing the
  caller's key from `~/.ssh/authorized_keys`);
- `[operation] review_policy` is `none` on the owner.

```bash
orbit config get operation.review_policy
```

v1 admits only `none`. `before-pr` and `after-landing` are refusals, not silent
downgrades. A workspace that still ships those policies through its **legacy**
leaf is not ready for distributed pull.

Match crews and toolchains the same way you would for a second owner-local
executor: every participant must resolve the workspace default crew, explicit
task crews, and required validation commands. Heterogeneous eligibility is not
supported. Empty `workflow.required_validation_commands` is fail-closed for a
claimed handoff.

Per-host compiler capacity is independent. Keep the shared build-budget
defaults (two heavy slots, four Cargo jobs) unless you deliberately raise them;
see [build-budget.md](./build-budget.md).

## Procedure: owner and follower setup

### 1. Choose the owner and stop competing drains

On the host that will remain owner, keep its checkout as `owner`. On every
other host that currently owns a copy of the same logical workspace:

1. Stop owner-side admission: cancel or wait out `orbit run auto` /
   `orbit run ship-sweep` windows on that host. Stopping a drain stops new
   admissions; it does not cancel live children.
2. Reconcile in-flight runs, pull requests, and reservations (migration
   section below) **before** changing the role.
3. Move tasks that must keep their IDs, or drain them on the demoted host
   first.

Do not delete `~/.orbit/host.toml`, `workspaces.json`, or the store to force
the switch.

### 2. Re-register the demoted checkout as a replica

From the follower checkout, using the **owner's** machine id:

```bash
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica
```

`workspace role` validates or reasserts; it is not a takeover. A replica must
not originate owner-only task mutations or publish/restore the owner's live
task set. Route those operations to the owner. See
[multi-host setup](../../crates/orbit-core/assets/skills/orbit-setup/references/multi-host.md).

### 3. Transfer remaining tasks from evidence, not by copying the store

On the demoted host, after drains are quiet:

```bash
orbit task export --ids <task-id>,<task-id> -o <archive>
# or, when the whole remaining set should move:
orbit task export --all -o <archive>
```

On the owner:

```bash
orbit task import <archive> --on-conflict=renumber
```

Use `--on-conflict=owner-wins` only for a repeatable mirror of IDs the owner
already minted. Do not rsync `~/.orbit/orbit.db` or task bundles between
machines as a substitute. Publication restore is same-authority recovery, not
cross-host ownership transfer; see [state-and-backup.md](./state-and-backup.md)
and [task-publication.md](./task-publication.md).

### 4. Restore pruned selectors only from recorded history

Filesystem-missing selectors are valid declarations. Do not delete them, and
do not guess replacements. Inspect first:

```bash
orbit task lint <task-id>
orbit task lint
```

Re-declare selectors an **older prune recorded in that task's history**:

```bash
orbit task lint <task-id> --restore-pruned
orbit task lint --restore-pruned
```

`--restore-pruned` never invents scope. Unrestorable entries stay unrestorable;
supply own `context_files` yourself. Empty lock surfaces stay ineligible for
distributed pull and for operator task-scope reservation until an operator
declares context.

Reservation TTL on a pulled claim is 14,400 seconds (four hours). Expiry does
**not** revoke the claim, admit another worker, or shrink the frozen
non-pruned footprint. Status-derived locks on `in-progress` and `review` tasks
keep the declared selectors, including files that do not exist yet.

### 5. Establish SSH owner access (no destination callers file)

SSH login to the owner **is** owner access. There is no
`~/.orbit/mcp-callers.toml`, forced-command acceptance, KeyBound proof, or
replacement destination identity registry. `--remote-caller-machine-id` is an
attribution label, not a credential.

On the follower, put the owner in `~/.orbit/mcp-destinations.toml` and serve
federation from the caller:

```bash
orbit mcp init --federated --client <client>
orbit mcp serve --mode federated --operator
```

Without `--operator` on the **calling** side, remote sessions hold `agent`.
Managed agent sessions cannot acquire operator by changing tool input or
launching a privileged child. To deny a caller, remove its key from the
owner's `~/.ssh/authorized_keys`. Details:
[remote-access.md](../../crates/orbit-core/assets/skills/orbit-setup/references/remote-access.md).

### 6. Read-only probe (never a health check for admission)

From a session that can reach the **owner** (federated selector or owner-local
tool run). Both `orbit.drain.probe` and `orbit.drain.receipt.lookup` require an
identified caller (`agent` or `operator`). A non-interactive shell uses an
agent envelope or `ORBIT_OPERATOR=1`.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
```

The probe reports owner machine, binary version, distributed-drain protocol
schema `1`, this session's capabilities, diagnostic caller machine,
owner-resolved ship configuration, and review policy. Declaring version,
schema, or review policy also reports the **first refusal admission would
raise**, in admission order. It creates no receipt, reservation, claim, or
task. A replica destination refuses the tool instead of answering about
itself. Do not call pull — it is not a registered tool — as a health check.

Expected refusals you may see (and must not work around):

| Error | Meaning |
|---|---|
| `capability_refused` | Destination is a replica, or the session lacks agent/operator identity |
| `version_mismatch` | Caller binary or protocol schema differs from the owner |
| `ship_mode_unsupported` | A remote caller targeted a local-only ship workspace |
| `review_policy_unsupported` | Owner or executor review policy is not `none` |

### 7. Receipt lookup after uncertainty

A receipt is historical evidence, not current execution authority.
`not_found` is not proof an earlier request cannot still arrive, and it never
licenses a replacement request under a new ID.

```bash
orbit tool run orbit.drain.receipt.lookup --input '{
  "request_id": "<request-id>"
}'
```

Returns `found` (original receipt plus current claim phase), `expired`
(non-reusable tombstone), or `not_found`. Naming another machine's namespace
requires operator capability:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.receipt.lookup --input '{
  "request_id": "<request-id>",
  "machine_id": "<other-machine-id>"
}'
```

Idle receipts compact to permanent tombstones in v1. Unsettled claims keep
full receipts. Do not delete request rows by age. At a 30-second idle poll,
one drain leaves 2,880 request identities per day even after compaction. Stop
a refill pass after the first idle response; the next poll uses a **new**
request ID.

### 8. Explicit pull enablement — not available in this slice

Matching binaries, a replica role, a working probe, and `review_policy = none`
are **installation**. They do not enable pull.

The following are **not** registered and must stay unavailable until the
lifecycle integration slice lands:

- `orbit.task.pull`
- run binding, settlement, accept-handoff, approve-handoff, revoke-handoff
- `orbit run auto --pull <selector>`

The owner's dashboard approve/revoke/recover actions are **not** on that list:
they are owner-local operator mutations against this checkout's own claims, not
routed entry points a follower can reach. They change nothing about the gate.

`DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED` cannot be turned on by
configuration. Do not register a local tool, write a callers file, or start a
second owner store to simulate pull. Owner-local claimed leaves already exist
on the owner; followers still fail the public mutation gate.

When that slice lands, rollout is a separate operator action: enable pull
only after this runbook's inspect/migrate/probe steps are clean, keep
ship-sweep enablement unchanged unless you deliberately edit the routine, and
never treat a successful probe as permission to merge.

## Claim inspection and manual recovery

List claims on the owner. This tool is off the MCP surface and requires
operator capability:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

It reports phase, age, reservation expiry, execution machine, bound run, last
event, unresolved merge intent, and landing invalidation. **Nothing in this
listing reclaims, rebinds, or repairs a claim.**

The owner's dashboard shows the same state, plus the accepted handoff, inside
the task detail it belongs to — there is no distributed tab, and the panel
appears only for a task this workspace holds a claim for:

```bash
orbit web serve --operator
```

`GET /api/distributed/claims` is the read; a replica answers that the owner
machine holds claim state rather than showing an empty list. Read-only
inspection needs no operator authority; the three actions below do, and the
server re-resolves that for every call regardless of what the page rendered.

Also inspect the task, lock surface, and recorded run:

```bash
orbit task show <task-id>
orbit task locks list
orbit run show <run-id>
orbit run events <run-id>
```

`job_run_host` on the task names where the bound run lives; a host pointer is
not remote reachability. Inspect that run on the execution host.

### Interrupted claimed-run resume refusal

v1 refuses generic resume of a claimed leaf. `orbit job resume` creates a new
run and cannot inherit the immutable claim/run binding:

```bash
orbit job resume <run-id>
```

Expect a validation error naming deliberate recovery, not a new attempt.
In-run step retries of the **same** bound run are different: they keep the
claim. Crash recovery before launch may recover the same queued run; it must
not restart a run whose execution became uncertain.

Deliberate recovery is operator-driven:

1. Inspect the claim, run, branch, and any PR or local candidate.
2. Reconcile an uncertain merge intent **before** reassignment. Database
   revocation cannot cancel a request already sent to GitHub.
3. Revoke the old claim, invalidate pending landing authority, release only
   that reservation, and choose the task transition (`blocked` to diagnose or
   `backlog` to retry) in one authorized recovery. On the owner's dashboard
   that is **Recover claim** on the task's distributed panel, which requires a
   reason and the phase you were shown, and refuses if the claim moved on. There
   is still no CLI verb and no registered tool for it; do not invent one, and do
   not admit a second attempt by shipping the task again while the old claim is
   unfenced.
4. A sleeping worker that returns receives `stale_claim`. Its local compute
   and an in-flight GitHub write cannot be undone; its old candidate cannot
   become authoritative.

Owner-local landing of an already-authorized handoff does not need a drain or
ship-sweep. Followers never merge. Review-only work stays in `review` until
explicit completion authority is recorded.

Handoff **approval** and **revocation** are owner-operator mutations (agent
capability cannot approve). They are owner-domain seams reached from the
owner's dashboard — `POST /api/distributed/handoffs/<handoff-id>/approve` and
`.../revoke`, governed as `handoff.approve` and `handoff.revoke` — and they are
still not `orbit tool run` entry points. Never substitute `--complete` on a
follower or `orbit job resume`:

- Approval records one immutable candidate-scoped authorization and one
  landing-start request, and it does not merge. It carries the exact candidate
  and base commits you were shown, so a stale page is refused with
  `stale_claim` rather than approving whatever the owner now holds. Retries of
  the same request ID replay that decision instead of creating a second grant.
- Revocation invalidates pending landing permission and leaves the task in
  `review`; it requires a reason. Unresolved merge intent still has to be
  reconciled first — the dashboard refuses with `uncertain_merge_intent`, and
  so does the store.
- Pull eligibility and `agent` access never grant merge rights. A replica
  refuses all three actions with `replica_checkout`; route them to the owner.

## Migration recipe: leftover epic, child, review, and reservations

Epic execution is retired. Hierarchy remains for reading; admission ignores
the `epic` tag except for roots that declared **no** own `context_files` while
a descendant did. Those inherited-only roots are withheld until an operator
supplies own context or retires them. Do not guess the union.

There is no public `epic retirement` command. Gather the same evidence the
read-only readiness check uses:

```bash
orbit run history -j epic_pipeline --limit 50
orbit run history --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_host
orbit task locks list
orbit doctor
```

Refuse to treat the workspace as migrated while any of these remain
unreconciled, **regardless of the root's status** (a root in `review` can
still have a live completion step):

| Leftover | What to do |
|---|---|
| Non-terminal `epic_pipeline` run | Wait, cancel with evidence, or finish it; do not delete the run row |
| Active child/family run | Inspect on its host; stop or complete it; do not ship the root as a leaf beside it |
| Uncertain landing (failed/interrupted family run, or a missing recorded run) | Reconcile the branch/PR/local ref before reassignment |
| Active reservation naming an epic-family task | Release only when the owning run is terminal and no newer claim holds it: `orbit task locks list`, then operator `orbit task locks release <reservation-id> --confirm` |
| Inherited-only epic root | Copy descendant selectors you can prove from `orbit task show`, or restore pruned declarations from history; never invent files |
| Historical epic worktrees | Let GC reap them; keep decoding until `orbit doctor` / worktree GC shows they are gone |

Failed-run triage is also retired. A failed claimed or family run parks the
task in `blocked` with `job_run_host`. Re-backlog is a deliberate
`orbit task update --status backlog`, made by whoever inspected the evidence.

## Retained ship-sweep (do not enable as part of setup)

Keep the seeded `ship_sweep` routine, `workspace_ship_pipeline`, and
`orbit run ship-sweep`. Existing `enabled` flags, cadences, and
`workflow.auto_ship` stay as the operator left them.

```bash
orbit routine list
orbit run ship-sweep --dry-run
```

A replica sweep reports destination-authority refusal before it reads a
backlog. Scheduled invocation still confers no completion authority. Do not
enable a dark routine to "turn on" distributed drain.

## Verification

On the owner:

```bash
orbit --version
orbit workspace show
orbit doctor
orbit config get operation.review_policy
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<owner-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
curl -s -H 'Host: localhost:7878' http://localhost:7878/api/distributed/claims?workspace=<workspace-id>
```

From a follower session aimed at the owner selector, repeat the probe with
that follower's `orbit --version`. Confirm:

- versions and schema match;
- review policy is `none` on both sides;
- the probe created no task, reservation, or claim (`orbit task locks list`
  unchanged);
- `orbit job resume` of a known claimed leaf still refuses;
- `orbit run auto --pull` is not a flag (`orbit run auto --help`);
- no leftover callers file is treated as an ACL.

## Rollback

Leave the replica registered but idle. Do not enable pull. Restore the
demoted host as an owner only by a deliberate, documented re-init after
quiescing the current owner — this runbook does not perform that reversal.
Preserve schedule files; do not bulk-disable routines as cleanup.

## Related references

- [Runbook conventions](./CONVENTIONS.md)
- [Bound concurrent builds](./build-budget.md)
- [Inventory and protect Orbit state](./state-and-backup.md)
- [Recover stuck job runs](./stuck-job-runs.md)
- [Distributed drain design](../design/distributed-drain/2_design.md) and
  [task-pull spec](../design/distributed-drain/specs/task-pull.md)
- Embedded skill: [distributed-drain setup](../../crates/orbit-core/assets/skills/orbit/references/setup/distributed-drain.md)
