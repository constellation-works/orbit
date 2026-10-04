---
title: Set Up a Distributed Drain
description: "Run a second machine as a replica that pulls work from one owner. Installation is not rollout; the owner keeps landing authority; recovery is manual."
sidebar:
  order: 4
---

Use this guide when you want a second machine to execute the same Orbit
workspace. One host owns the task store. Other hosts register as replicas,
reach that owner over SSH, and run `orbit run auto --pull`: the owner orders
the work and admits one claim at a time, and each claim runs on the replica
until it hands a pull request back. Matching binaries and a working preflight
are **installation**; starting the pull drain is the rollout.

Automatic reclamation, automatic review, and follower merges are not
available. By default the owner approves each handoff before it lands; an owner
can instead opt in to landing validated handoffs itself (see
[What this is not](#what-this-is-not)).

The deterministic owner/follower RPC is separate from ordinary agent MCP.
Probe, receipt lookup, admission, bind and settle have no public tool schemas;
public calls refuse their canonical and formerly advertised names. Orbit's
runtime selects the internal endpoint through the SSH launch, and client names
or initialize metadata cannot enable it. Both machines must support that
internal protocol revision; there is no public fallback. The owner-side CLI
diagnostics below remain available with their existing authority checks.

## 1. Keep one owner

A repository checkout, a logical workspace, and a machine's live task store
are different things. Sharing Git history does not synchronize Orbit.

On the host that will stay authoritative, confirm that the checkout is
registered with role `owner`:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
```

If the checkout is not registered yet, register it. Re-running this on a
registered checkout is refused unless you pass `--force`:

```bash
orbit workspace init --role owner --base-branch <integration-branch>
```

On a second machine, pick a **different** task prefix at `orbit init` (`ORB` and
`ADR` are reserved), then register the checkout as a replica of the owner's
machine id:

```bash
orbit init --non-interactive --machine-name <name> --task-prefix <PREFIX>
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

`workspace init` and `workspace role` both print the recorded role and owner (or
the same fields with `--format json`), so you can confirm the replica before
draining. `workspace role` reasserts the role; it is not a takeover, and a
replica role always needs `--owner` (the owner's `machine.id`). A replica
refuses `orbit run auto` and `orbit run ship` and points at `--pull`. If both hosts were
already independent owners of the same repo, stop their drains, finish or
export leftover work, then demote the extra checkout: `workspace init` and
`workspace role` refuse to rebind a registered owner, so run
`ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>` (registry only;
`.orbit` stays) and repeat `workspace init --role replica --owner <owner-machine-id>`.
Move tasks with `orbit task export` /
`orbit task import` — do not copy the database.

## 2. Match binaries, crews, and review policy

Every participant must run the same Orbit version and toolchains, and use
review policy `none`. Crews may differ between hosts. A replica only receives
tasks whose crew it can run, so name shared crews the same on every host:

```bash
orbit --version
orbit doctor
orbit config get operation.review_policy
```

v1 refuses `before-pr` and `after-landing` for distributed admission rather
than silently downgrading them. Empty required-validation configuration is
fail-closed for a claimed handoff. Compiler caches and build slots stay
per host; they are not a shared fleet.

## 3. SSH login is owner access

There is no destination callers file and no per-caller identity registry.
Whoever can `ssh` to the owner already owns that machine. Start federation
from the follower with the authority you intend:

```bash
orbit mcp init --federated --client <client>
orbit mcp serve --mode federated --operator
```

Without `--operator` on the calling side, remote sessions hold `agent`.
A managed agent session cannot promote itself. To deny a caller, remove its
key from the owner's `~/.ssh/authorized_keys`. Leftover
`~/.orbit/mcp-callers.toml` files are ignored; `orbit doctor` names them so
you can delete them.

See [Connect Your Agent](../mcp-integration/) for client registration. Machine labels
forwarded over SSH are audit attribution, not credentials.

## 4. Probe the owner

Run the diagnostic CLI on the **owner**, locally or through an operator SSH
shell. Both drain tools require an
identified caller, `agent` or `operator`; a plain shell has neither, so set
`ORBIT_OPERATOR=1` for a deliberate operator run (it is recorded in the audit
trail). A replica checkout refuses these diagnostics and names its owner:
run the CLI there. The follower runtime uses the internal owner route.

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 1,
  "caller_review_policy": "none"
}'
```

The probe reports the owner, binary and protocol versions, this session's
capabilities, ship configuration, and review policy. If you declare your
version and policy, it also reports the first refusal a real admission would
raise. It creates no task, reservation, or claim. Treat it as a preflight,
not a health check — and never call pull itself as one, since a pull is a real
admission.

Reconcile a past request the same way — still read-only:

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.receipt.lookup --input '{"request_id":"<request-id>"}'
```

`not_found` does not mean you may mint a replacement request id.

## 5. Start the pull drain

On the replica, with the owner in `~/.orbit/mcp-destinations.toml` and the
same `workflow.required_validation_commands` the owner declares:

```bash
orbit run auto --pull <owner-machine>/<ws_id> --for 8h --concurrency 3
```

The selector is the owner's `selector` from federated discovery; if this host
has no destination for the owner the command says to add it to
`~/.orbit/mcp-destinations.toml`. The command
refuses before submitting anything unless this checkout is a replica of that
owner and workspace and the owner's probe admits this executor. Each claim
runs as a local leaf that implements, validates, pushes and opens a pull
request, then hands off; the owner reads the pull request itself and moves the
task to `review`. Approve it on the owner's dashboard to land it.

The drain checks which crews this host can run when it starts: each crew must
be enabled, and its provider's CLI must be installed where a leaf would
launch it. Every pull request declares the result, and the owner skips tasks
on any other crew. Those tasks stay in the backlog for the owner or another
replica. The check doesn't sign in to providers. A provider that fails to
authenticate is caught by the first leaf that uses it: that leaf's task goes
back to `backlog` rather than `blocked`, and the drain stops offering that crew
until it ends. `orbit run show <drain-run>` lists the runnable crews and each
excluded crew with its reason on `Crews:` lines (`crew_window` in `--json`,
and on the dashboard's run detail). After you install or sign in a provider,
start a new drain.

Any other leaf that fails before its handoff moves its task to `blocked` on
the owner, with a summary naming the leaf run, the failed step and its error. The full
diagnostic stays on the replica: `orbit run show <leaf-run>`, whose `Claim:`
line names the owner task and claim the leaf works for and whether its outcome
has reached the owner (`pull_claim` in `--json`). After three
consecutive claims settle as failures the drain stops requesting work
(`circuit_open` in the iteration output) and only keeps settling. Fix the
cause, move the blocked tasks back to `backlog` deliberately, and start a new
drain.

## 6. Stop, cancel, and settle

The drain keeps settling its claims after the window closes. Stop it early
with `orbit run auto --stop`: it ends new admissions, running leaves finish and
still hand off, and it delivers any settlement still waiting for the owner.

`orbit run cancel <drain-run> --confirm` kills the coordinator instead. Claims
it had not launched yet end as failures, and their tasks go to `blocked`;
leaves already running deliver their own handoff or failure when they end. The
command prints one line per admission under `Pull settlements`; each line that
leaves something to do says what (for example, rerun `orbit run auto --stop`
once the owner answers, or recover a `launch_uncertain` claim on the owner's
dashboard). Prefer `--stop` when you only want no new work.

If the owner was unreachable when a leaf ended, its settlement stays recorded
on the replica as `settling`. Run `orbit run auto --stop` in the replica
checkout once the owner answers; it is safe to repeat and needs no active
drain. Nothing retries on a timer, so run `orbit doctor` on the replica to see
whether any are waiting: its `pull-settlements` warning gives the count, the age
of the oldest, and this same command.

## 7. Inspect claims; recover by hand

List claims on the owner (operator shell):

```bash
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
```

The owner's dashboard shows the same claim state inside the task it belongs to,
and only for a task the workspace holds a claim for. Start it with
`orbit web serve --operator`; see [Use the Dashboard](../dashboard/).

Age, an expired reservation, and a missing local run are diagnostics. Nothing
in that listing reclaims work. `orbit job resume` refuses a claimed leaf;
deliberate recovery inspects the recorded run, reconciles any uncertain
merge, and only then revokes the old attempt. On the owner's dashboard, use
**Recover claim → blocked** to diagnose or **Recover claim → backlog** to
retry; both ask for a reason. There is no CLI verb or tool for recovery.
Followers never merge. Review status is not an automated review, and no
heartbeat reassigns a dead worker.

## 8. Leave schedules as they are

Seeded ship-sweep routines, `workspace_ship_pipeline`, and
`orbit run ship-sweep` stay at whatever enablement you already chose.
Distributed-drain setup does not opt them in.

```bash
orbit routine list
orbit run ship-sweep --dry-run
```

Enable a routine only by editing its definition under `.orbit/routines/`. Replica hosts
refuse owner-only sweeps.

## What this is not

- **Not a fleet.** There is no host list to enable.
- **Not auto-recovery.** A dead replica can hold a footprint until you
  inspect and revoke it.
- **Not follower merge.** Landing stays on the owner after explicit
  completion authority: a per-task **Approve handoff**, or the owner's
  `workflow.distributed_completion = "done"`, which lands every validated
  follower delivery the way `--complete` lands the owner's own tasks.
- **Not automatic.** A clean probe means the hosts are installed. A replica
  pulls only while an operator-started `orbit run auto --pull` is running.

For ordinary single-host drains, stay on
[Run Continuous Delivery](../continuous-delivery/).
