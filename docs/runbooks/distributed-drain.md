---
type: runbook
summary: Set up, migrate, run, and recover a single-owner distributed drain — follower pull with owner-only landing, no follower merges.
tags: [operations, distributed-drain, multi-host, recovery]
paths:
  - "crates/orbit-tools/src/builtin/orbit/drain/**"
  - "crates/orbit-core/src/application/distributed/**"
  - "crates/orbit-cli/src/command/task/lint.rs"
  - "crates/orbit-web/src/api/distributed.rs"
related_features: [distributed-drain, federated-mcp, host-registry, remote-access]
related_artifacts: [ORB-13941, ORB-13663, ORB-13642, ORB-13625, ORB-12968, ORB-12516, ORB-12515, ORB-12500, ORB-12495, ORB-12564, ORB-12491, ORB-12490]
last_validated: 2026-10-04
---

# Set Up and Recover a Single-Owner Distributed Drain

Use this runbook to collapse two independent owners, match follower
prerequisites, inspect the owner's drain surface, start a follower's pull
drain, migrate leftover epic/child/review state, and recover a claimed
attempt. Installing matching binaries is not a rollout: starting a follower's
drain is a separate, explicit operator action (step 8).

The owner serves probe, receipt reconciliation, task admission, bind and
settlement over a deterministic internal RPC selected by Orbit's runtime SSH
launch. A replica runs `orbit run auto --pull <selector>` to use it. The five
operations are absent from public MCP discovery and refused by public
`tools/call` under both canonical and formerly advertised names. Client names
and initialize metadata cannot enable internal access. Both endpoints must
support the internal transport revision; preflight fails closed with no public
fallback. CLI diagnostics below retain their authority requirements, and
completion approval, revocation and recovery remain owner-dashboard actions.
The follower never merges.

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
| `<owner-machine-id>` | Owner `machine.id` from `orbit config get machine.id` on the owner |
| `<workspace-id>` | Logical `ws_*` id from `orbit workspace show` |
| `<selector>` | Host-qualified selector from federated `orbit_workspace_list` |
| `<ssh-alias>` | An SSH config alias already able to log in as the owner |
| `<request-id>` | Durable ID of one intended admission request |
| `<archive>` | Path to a `tar.zst` from `orbit task export` |

## Inspect before mutating

On **every** participating host:

```bash
orbit --version
orbit config get machine.id
orbit workspace show
orbit doctor
```

Confirm:

- binary versions and distributed-drain protocol revisions match;
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

Match toolchains and required validation commands the same way you would for
a second owner-local executor. Crews may differ: a follower only receives
tasks whose crew it can run. A follower runs a pulled task on the crew the
owner's task names, and uses its own `workflow.default_crew` only when the
task names no crew. Each drain declares the crews its host can run (see step
8), and the owner skips a task whose crew is not among them, leaving it in
the backlog for the owner or another follower. Use the same crew names on
both sides: crews are matched by name. Empty
`workflow.required_validation_commands` is fail-closed for a claimed handoff.

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

Do not delete the `[machine]` table in `~/.orbit/config.toml`, `workspaces.json`, or the store to force
the switch.

### 2. Re-register the demoted checkout as a replica

From the follower checkout, using the **owner's** machine id:

```bash
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

A checkout that is registered as an owner cannot be rebound in place: both
commands refuse with "refusing to rebind". Once its drains are quiet, drop the
registration (registry only; `.orbit` and its tasks stay) and register it again:

```bash
ORBIT_OPERATOR=1 orbit workspace remove <workspace-id>
orbit workspace init --role replica --owner <owner-machine-id>
```

`workspace init` and `workspace role` print the recorded role and owner
(`--format json` carries `role` and `owner_machine_id`). A replica role always
needs `--owner`; the refusals name the flag and say the id is the owner's
`machine.id`. `workspace role` validates or reasserts; it is not a takeover. A replica must
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

Run the diagnostic CLI on the **owner**, locally or through an operator SSH
shell. Both `orbit.drain.probe` and `orbit.drain.receipt.lookup` require an
identified caller (`agent` or `operator`). A non-interactive shell uses an
agent envelope or `ORBIT_OPERATOR=1`.

```bash
orbit tool run orbit.drain.probe --input '{
  "caller_version": "<this-binary-version>",
  "caller_schema": 2,
  "caller_review_policy": "none"
}'
```

The probe reports owner machine, binary version, distributed-drain protocol
schema `2`, this session's capabilities, diagnostic caller machine,
owner-resolved ship configuration, and review policy. Declaring version,
schema, or review policy also reports the **first refusal admission would
raise**, in admission order. It creates no receipt, reservation, claim, or
task. A replica destination refuses the tool instead of answering about
itself, naming its owner. Run the diagnostic CLI there; the follower runtime
uses its internal owner selector. Do not call pull as a health check: a pull is an admission, and an
admitted claim is real work the owner holds until it settles.

Expected refusals you may see (and must not work around):

| Error | Meaning |
|---|---|
| `capability_refused` | Destination is a replica, or the session lacks agent/operator identity |
| `version_mismatch` | Caller binary version differs from the owner |
| `protocol_mismatch` | Caller and owner protocol revisions differ; diagnostics name both |
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
full receipts. Do not delete request rows by age. At the shipped 60-second
idle poll (`idle_sleep_seconds`), one drain leaves 1,440 request identities per
day even after compaction. Stop a refill pass after the first idle response; the
next poll uses a **new** request ID. The follower's own copy is bounded: when it
allocates a new request it prunes its idle and refused rows to the newest
10,000 (about a week of one drain's polls). Settled claims are never pruned.

### 8. Start the follower's pull drain

Matching binaries, a replica role, a working probe, and `review_policy = none`
are **installation**. Starting a drain is the rollout, and it is explicit. On
the follower, from the replica checkout:

```bash
orbit run auto --pull <selector> --for 8h --concurrency 3
```

`<selector>` is the owner's host-qualified selector from federated discovery
(`orbit_workspace_list`, e.g. `hm_owner/ws_orbit`); an owner with no entry in
`~/.orbit/mcp-destinations.toml` is refused as an unknown selector, and the
message says so. Before anything is
submitted, the command refuses unless:

- this checkout is a **replica**, and the selector names **its** owner machine
  and **its** logical workspace;
- the owner answers the probe **as that machine** and would admit this
  executor now (binary, protocol schema, review policy, ship mode);
- this host declares `workflow.required_validation_commands` — the same list
  the owner uses, since the owner re-checks the evidence against its own.

The drain is an ordinary durable run of `workspace_pull_pipeline`:

- On its first iteration the drain runs a provider preflight over every crew
  this host configures. A crew is runnable when it is enabled, its provider's
  executor resolves, and that executor's CLI is found where a leaf would
  launch it. The preflight starts no provider and checks no login; a provider
  with no signed-in user is caught by its first claimed leaf (below). The
  result is kept for the drain's window. Every pull request declares the
  runnable crews, and the owner admits only tasks this host can run. If no
  crew is runnable, the drain requests nothing and reports
  `no_runnable_crew`. After you install a CLI or sign a provider in, start a
  new drain to pick it up.
- Each iteration first carries earlier admissions forward — retries an
  unanswered request under the **same** ID, binds, launches, and delivers a
  finished leaf's settlement — then, while the window is open, tops free slots
  up with new pull requests, each persisted before it is sent.
- Each claim runs as one local `task_claimed_pr_pipeline` leaf: implement,
  validate on the exact candidate, push, open the PR, hand off. The owner
  observes the PR itself and moves the task to `review`. **Nothing lands until
  the owner approves the handoff** on its dashboard.
- The implement step runs in **claimed mode**. The agent sandbox denies
  `~/.ssh`, so a sandboxed agent on a follower has no route to the owner; it
  does not need one. It works from the injected task envelope, is not granted
  `orbit.task.show` or `orbit.task.update` (nor is any recovery agent the leaf
  launches, such as `step_failure_recovery`), and returns its execution summary
  in the step output. `claim_handoff` carries that summary in the typed
  handoff, and the owner writes it as the task's `execution_summary` when it
  accepts. The leaf's delivery gate judges that same summary, so a retry is
  not refused on the `Outcome: failed` a previous attempt left on the owner;
  do not clear it by hand. Do not loosen the sandbox or add SSH credentials to it to "fix" a
  leaf; an agent that reports an unreachable owner store is a prompt or
  binary mismatch, not a transport problem (check the follower's binary is
  current).
- An owner that refuses a request is checked against its receipt first: a
  committed claim is carried forward, and only a request the owner holds no
  receipt for is closed (`Refused`) and its slot returned.
- An unreachable owner is reported in the iteration output and retried; the
  drain never fails over to its own store.
- Each leaf settles itself when it ends ([ORB-13663]): its worker records
  the handoff (success) or a failure, then delivers it to the owner, retrying
  for a few minutes if the owner is unreachable and no live drain carries the
  claim. Every drain pass, a cancel or `orbit run auto --stop` delivers
  anything the leaf could not, and a drain pass also reconciles a launched
  leaf whose worker died so its settlement is recorded and delivered. A leaf
  that was cancelled before it launched releases its claim instead: the task
  goes back to `backlog` on the owner with a comment naming the drain. A leaf
  that fails before its handoff moves its task to `blocked` on
  the owner with a summary naming the leaf run, its failed step and that
  step's error. The exception is a leaf whose provider could not be used, such
  as a CLI that failed authentication. Its claim is released instead: the
  task goes back to `backlog` on the owner with a comment naming the crew and
  the provider's error. The failure breaker does not count it, and the drain
  stops offering that crew for the rest of its window, so the task is not
  pulled straight back. The full diagnostic stays in the follower's run
  (`orbit run show <leaf-run>`, and `.orbit/state/logs/<leaf-run>.worker.log`
  on the follower). That run page carries a `Claim:` line (`pull_claim` in
  `--json`): the owner task, claim, owner selector and admitting drain, and
  whether the leaf's outcome has reached the owner.
- `orbit run show <drain-run>` lists the crew window as `Crews:` lines
  (`crew_window` in `--json`; the dashboard's run detail shows the same
  panel). The first line names the runnable crews. Each excluded crew is
  listed with its source and the reason: `preflight` (disabled, executor
  unresolved, or CLI not found) or `provider_unavailable` (a claimed leaf's
  provider failed, with the task and error). Each iteration's output carries
  the same window as `crews`. To use an excluded crew again, fix the provider
  on this host (for example, sign the CLI in), then start a new drain.
- After three consecutive claims settle as failures, the drain stops
  requesting work (`circuit_open` in the iteration output) and only keeps
  settling. Inspect the blocked tasks and their leaf logs, fix the cause,
  re-backlog them deliberately, and start a new drain.
- The run outlives its window until every admission has settled, so a leaf
  that finishes late still hands off, and a later drain for the same owner
  carries anything an earlier drain left behind.
- Each iteration reclaims the `target/` build output of every settled leaf
  whose worker has exited (`reclaimed_build_bytes` in the iteration output)
  and keeps its checkout. A leaf's `target/` runs to gigabytes, so follower
  disk does not wait on a GC schedule. `orbit gc worktrees --confirm` on the
  follower can then remove a leaf's checkout on the strength of a locally
  settled, accepted handoff alone. Released claims and other settlements,
  including handoffs the owner refused as obsolete, still require the task's
  status, read from the owner over the claim's route. A release returns the
  task to backlog, so its checkout is retained. A worktree kept
  as `skipped:owner_unreachable` carries the transport error in `detail`;
  `skipped:no_owner_route` means the follower has no route to ask (owner
  missing from `~/.orbit/mcp-destinations.toml`, or an unregistered
  checkout), not that the owner is down. A status lookup failure without a
  transport error is `skipped:owner_lookup_failed`, with the reason in
  `detail`; it does not establish that the owner is down.

Operate it with the ordinary run commands: `orbit run show <run-id>`,
`orbit run concurrency <run-id> --set N` (MCP: `orbit_workflow_auto` with
`action: "resize"`), and `orbit run auto --stop` (closes the window; live
leaves keep running and still settle). The drain's `--concurrency`, as last
retuned, is the only ceiling on its leaves: the claimed leaf jobs declare no
active-run limit of their own. `--for` is at most 24 hours. Ship-sweep
enablement is unchanged by any of this.

To close the feature entirely, set `DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED`
to `false` in `orbit-core` and rebuild: it is a source constant, not
configuration, so no key or environment variable can open or close it.

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

`job_run_machine` on the task names where the bound run lives; a host pointer is
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
   that reservation, and choose the task transition in one authorized recovery.
   On the owner's dashboard, use **Recover claim → blocked** to diagnose or
   **Recover claim → backlog** to retry. Both controls require a reason and the
   phase you were shown, and refuse if the claim moved on. There is still no CLI
   verb and no registered tool for it; do not invent one, and do not admit a
   second attempt by shipping the task again while the old claim is unfenced.
4. A sleeping worker that returns receives `stale_claim`. Its local compute
   and an in-flight GitHub write cannot be undone; its old candidate cannot
   become authoritative.

Owner-local landing of an already-authorized handoff does not need a drain or
ship-sweep. Followers never merge. Review-only work stays in `review` until
explicit completion authority is recorded.

### Let the owner land follower deliveries

To land follower deliveries the way `orbit run auto --complete` lands the
owner's own tasks, set this on the **owner** checkout, then restart its
long-lived processes so they read it:

```toml
[workflow]
distributed_completion = "done"
```

Followers pick it up on their next probe. A request built from the old
contract is refused with `ship_contract_mismatch` and retried with the new
one. From then on:

- Each accepted handoff is authorized in the same transaction that moves the
  task to `review`, and `task_landing_pipeline` starts at once. The task
  reaches `done` when the landing job has verified the merge.
- Claims admitted before the change keep their `review` contract and still
  need **Approve handoff**.
- Setting the key back to `review` (or removing it) stops every handoff that
  has not landed. The landing job refuses with `owner completion policy
  withdrawn`. Restore the key, or revoke the handoff and recover the claim.

Do not merge follower pull requests on the provider by hand. That skips the
owner's validation gate, and the owner still has to settle the claim.

On the dashboard, **approve** on a review task that has a handed-off claim
sends **Approve handoff** for the exact candidate. A plain status write would
be refused with `active execution claim requires a claim-scoped mutation`.

**Stopping or cancelling a follower drain** ([ORB-13663], [ORB-13892]). All
three are safe; none strands a claim, and none fails a task that never ran.

- `orbit run auto --stop` (or the dashboard's auto **Stop**) closes the
  window. The drain keeps running until every admission settles, and the stop
  also runs a settle-only pass of its own. Its output lists the claimed
  leaves still running (`remaining_children` in `--json`).
- `orbit run cancel <drain-run> --confirm` (or the dashboard's **cancel** on
  the run, answering **Cancel** when asked whether to stop the leaves) is
  graceful. It returns at once with `cancelling: waiting for N leaves` and
  lists them (`waiting_leaves` in `--json`). The drain stops requesting work;
  its next pass releases every claim it had not launched back to the owner's
  `backlog`, with a comment naming the drain and your `--reason`. Launched
  leaves keep running and deliver their own handoff or failure; once every
  settlement has reached the owner, the drain ends `cancelled`. Meanwhile
  `orbit run show <drain-run>` prints a `Cancelling:` line and the
  `Claimed leaves:` it waits for (`.run.drain_cancel` and `.claimed_leaves`
  in `--json`), and the dashboard's run page shows the same.
- `orbit run cancel <drain-run> --confirm --force` (the dashboard's **OK** to
  stopping the leaves) does not wait. It stops the drain, records each running
  leaf's claim as released, stops the leaf's process group, and every claim —
  running or not — goes back to the owner's `backlog` with a comment naming
  the drain and the reason. The stopped leaves are listed as `forced_runs`.
  A leaf that had already recorded its handoff is left to finish and deliver
  it. `--force` on a drain that already ended still stops the leaves it left
  running. It touches only what that drain carries: the leaves it admitted
  and, while it was live, those an ended drain for the same owner left
  behind. Never a leaf another live drain admitted.
- If the pull drain worker cannot be confirmed stopped, `--force` fails
  naming the drain and the signal outcome before finalizing the drain,
  stopping leaves, or releasing carried claims. This includes workers in
  another PID namespace or with an unverifiable identity. Stop the drain
  on its host and retry once its exit can be confirmed. A worker confirmed
  already exited permits forced leaf cancellation and claim release.
- A leaf `--force` cannot stop and see gone keeps its claim on the owner.
  Its worker might run in another PID namespace, or have an identity that
  cannot be verified. The cancel lists it under `unstopped_leaves` (the
  dashboard flags it) and exits 1. When the leaf is refused before the stop,
  nothing is released and the leaf delivers its own outcome when it ends. A
  stop that was signalled but not confirmed holds the recorded release
  (`release_held` in `pull_settlements`): it reaches the owner only once the
  leaf is seen to stop, so a second executor never starts the task beside a
  first that is still running. Stop that leaf by hand on its host.
- The MCP stop control takes the same option: `orbit.workflow.auto` with
  `action: "stop"` and `force: true` stops admissions, then force-cancels each
  live drain as above (a local drain's task runs included). It fails, naming
  the leaves, when any leaf could not be confirmed stopped.
- A drain that is still queued, or whose worker is gone, is cancelled at once;
  its unlaunched claims go back to `backlog` and its live leaves settle
  themselves.
- Cancel prints a `Pull settlements` section, one line per admission, and
  `--json` carries the same list as `pull_settlements`.
- Prefer `--stop` when you only want no new work: it ends nothing. The
  dashboard's cancel prompt for a drain run says this too.

A graceful cancel waits as long as a leaf runs and its settlement is owed. If
the owner stays unreachable the drain keeps showing `cancelling` (each pass
reports the transport error); `--force` ends it, and anything undelivered stays
recorded for the retry below.

For the owner's local drain (`orbit run auto`), cancel still detaches the task
runs it started, which finish on their own; `--force` cancels them too. Stops
are confirmed before each child is finalized. An unconfirmed stop is reported
under `unstopped_children` with the child run ID and reason; the CLI exits 1
and the dashboard flags the incomplete cancellation. The parent and children
that were successfully stopped remain reported as cancelled and `forced_runs`
respectively. The MCP forced-stop control also fails if a detached child
cannot be confirmed stopped.

The dashboard reports the same `pull_settlements` list after **Stop** and
after **cancel**: a one-line summary counting each outcome, with any
settlement that has not reached its owner called out (`owner_unreachable` and
`pending_delivery` say to run Stop again once the owner is reachable;
`launch_uncertain` says it needs manual recovery, below). With no live window
the auto card's button reads **Settle pending** and runs the same settle-only
pass; it stays available because the pass needs no active drain.

Leaf delivery can still fail — the owner was unreachable when the leaf ended,
or when `--force` released it. The settlement stays recorded on the follower
as `settling` and is retried without a new drain. A live drain retries on
each pass, and a leaf's worker retries briefly (15s, 60s, 240s). After both
have ended, the OS clock sweep (`orbit clock tick`, every minute once
`orbit clock install` has run) retries it: each tick opens the host's replica
checkouts too, only to deliver what their drains recorded. A tick never ends
unlaunched work, and it delivers a leaf's failure once the leaf's dead worker
is reconciled. On a host without the clock, flush it with
`orbit run auto --stop` in the replica checkout once the owner is reachable
(safe to repeat, and it needs no active drain), or by starting the next
drain. `orbit run cancel <drain-run> --confirm` on a drain that already ended
does the same flush. `orbit doctor` reports a
`pull-settlements` warning while any recorded settlement is undelivered: how many
wait, how long the oldest has, and `orbit run auto --stop` as the fix. The row
is `ok` once they are delivered, and on a workspace that never pulled. Every line
that leaves work for the operator says what to do next, and `launch_uncertain`
still needs the deliberate recovery below.

**Settlements an older binary stranded.** A binary before [ORB-13663] left a
cancelled drain's finished leaves `settling` and its failed leaves
`launched` with no settlement, and their owner claims `running`. After
upgrading the follower, run `orbit run auto --stop` in its replica checkout.
Delivered handoffs move their tasks to `review` on the owner, and the
failures move theirs to `blocked`. Inspect first: `orbit doctor` counts the
recorded-but-undelivered (`settling`) settlements. It does not see a failed leaf
that recorded no settlement (`launched`), so for a complete read-only list of
everything still holding a slot, query the database:

```bash
sqlite3 -readonly ~/.orbit/orbit.db "SELECT leaf_run_id,
  json_extract(record_json,'$.phase'),
  json_extract(record_json,'$.receipt.claim.task_id')
  FROM local_pull_admissions
  WHERE json_extract(record_json,'$.phase') NOT IN ('settled','idle','refused')"
```

If the owner revokes those claims in the meantime, the next pass cannot
deliver their settlements. The owner refuses each one as `stale_claim`. The
pass then looks up the claim on the owner, sees it has ended, and closes the
record locally (`closed_obsolete`). Its `refusal` records why. Those tasks still need an owner-side
status decision: set a task to `done` if its pull request merged, otherwise
move it back to `backlog`. A refused settlement for a claim the owner still
holds stays pending and blocks new admissions until it is delivered.

The same applies to a handoff the owner will deterministically never accept:
for example, the owner refuses the leaf's bind or its settlement while it still
holds the claim as `claimed` or `running`. Every pass reports the refusal, the
record stays `settling` and keeps its slot, and `orbit run auto --stop` does not
clear it. The escape is on the owner: revoke or recover the claim from the
owner's dashboard (**Recover claim → blocked** or **→ backlog**). The next pass
on the follower, whether a drain iteration or `orbit run auto --stop`, looks the
claim up, sees it has ended, and closes the record `closed_obsolete`, which
frees the slot. A claim whose bind the owner refuses before the leaf ever
launched closes the same way: the leaf is failed and never starts.

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
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_machine
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
task in `blocked` with `job_run_machine`. Re-backlog is a deliberate
`orbit task update --status backlog`, made by whoever inspected the evidence.

## Retained ship-sweep (do not enable as part of setup)

Keep the seeded `ship_sweep` routine, `workspace_ship_pipeline`, and
`orbit run ship-sweep`. Existing `enabled` flags, cadences, and
`workflow.auto_ship` stay as the operator left them.

```bash
orbit routine list
orbit run ship-sweep --dry-run
```

The CLI sweep reads the registry selected by `--root <dir>`, then `ORBIT_ROOT`,
then `~/.orbit`. Use `orbit --root <dir> run ship-sweep --dry-run --json` to
inspect an alternate registry before allowing dispatch. It does not create a
workspace in the directory from which the scheduler invokes it.

A replica sweep reports destination-authority refusal before it reads a
backlog. Scheduled invocation still confers no completion authority. Do not
enable a dark routine to "turn on" distributed drain.

## Scheduled host shutdown or reboot

When the host has a shutdown or reboot pending (for example
`unattended-upgrades` running `shutdown -r 04:00`), Orbit starts no new
unattended work until the schedule clears. Routines and auto-tasks do not
fire, drain waves admit no leaves, and `orbit run ship-sweep` skips with
`host_shutdown_scheduled`. Runs already in flight are not cancelled or
signalled.

```bash
orbit doctor                 # host-shutdown: warning naming mode and time
orbit run readiness          # "Admissions held: host reboot scheduled for …"
cat /run/systemd/shutdown/scheduled   # USEC= / MODE= written by logind
```

- To resume admissions before the restart, cancel the schedule as root with
  `shutdown -c`. The next tick or drain iteration admits again.
- Otherwise, do nothing. `/run` is cleared on reboot, and admissions resume
  on their own when the host comes back.
- `orbit run ship` and `orbit run auto` still run while a hold is active and
  log a warning. A single explicit ship may be killed by the restart. A drain
  started during the hold admits nothing until the hold clears.
- Reboot policy (apt `Automatic-Reboot*` settings) is root territory, and
  Orbit does not change it.

## Host resource pressure

While CPU, memory or a checkout/worktree/global-root filesystem stays at or
above its high mark for ten seconds (`[workflow.resource_throttle]` in
[CONFIG.md](../CONFIG.md#workflowresource_throttle--host-pressure)), Orbit
starts no new task work until that resource falls back below its resume mark.
Local drain waves admit no leaves and poll for recovery, pull drains request
no new claims but keep settling and reconciling, and `orbit run ship` without
task ids (and `orbit run ship-sweep`) is refused with `resource_throttled`.
Running work is never cancelled, paused or killed.

```bash
orbit run readiness          # "Admissions throttled: memory 93% ≥ 90% since …"
orbit run show <drain-run>   # Throttled: line from the drain's last pass
```

- Sampling monitors share recent pressure history in
  `<global-root>/cache/host-resource-pressure.json`, independently of drain
  records and workspace. A fresh `orbit run ship` or `ship-sweep` evaluates
  its sample against that history using its configured thresholds. Unknown
  readings, observation gaps over fifteen seconds and changed thresholds
  reset the affected resource's history; a first high sample alone does not
  prove sustained pressure. If history is unavailable, evaluation uses local
  observations. Drains sample every five seconds and also record the throttle
  on each pass; readiness and the dashboard Drain card can read that record.
  MCP `orbit.workflow.auto` status carries
  `capacity.resource_throttle`; `orbit.workflow.run.show` carries the drain's
  `drain_last_pass`.
- `orbit run auto`, MCP `orbit.workflow.auto` start, and `orbit run ship` with
  named tasks proceed and print a warning. The drain admits nothing until the
  pressure clears; a named ship starts at once.
- Readings that cannot be taken (unavailable, invalid or stale) never
  throttle. They are listed as `resource_telemetry_unknown` and logged once.
- Each throttle and recovery is logged once under `orbit.core.host_resource`.
- To restore the previous behaviour, set
  `workflow.resource_throttle.enabled = false`; no pressure is then sampled
  for admission.

Followers must match the owner's distributed-drain protocol revision, independently of
`orbit --version`. Deploy matching revisions on both hosts and restart long-lived processes.
The read-only probe reports `protocol_schema`; a mismatch is `protocol_mismatch` with both
revisions, including when an older owner calls its refusal `version_mismatch`.

`orbit run show <drain-run>` exposes a pull drain's latest pass error and consecutive failure
count. JSON carries `last_pass_error`, `consecutive_pass_failures`, and `degraded` under
`pipeline_state.drain_last_pass`. Three consecutive failed passes latch a visible degraded
warning and stop new admissions for that drain. A successful pass before the threshold resets
the streak. Degraded drains keep retrying settlements and outlive their window until nothing
is unsettled; successful settlement does not clear the warning. Fix the reported cause, run
`orbit run auto --stop` to close the window, and start a new drain once this one ends. An unreadable or unwritable run-state record fails the activity visibly.

## Verification

On the owner:

```bash
orbit --version
orbit workspace show
orbit doctor
orbit config get operation.review_policy
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<owner-version>",
  "caller_schema": 2,
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
- no leftover callers file is treated as an ACL.

After starting a drain (step 8), confirm the first claim end to end: the
owner's `orbit.drain.claims` shows it `running` on the follower's machine,
the follower's `orbit run show <leaf-run-id>` shows the claimed PR leaf and its `Claim:` line, and
after handoff the owner task is in `review` with the PR attached and nothing
merged.

## Rollback

Stop the follower's drain with `orbit run auto --stop` and let its live
leaves settle, or cancel them (each cancelled leaf settles its claim as a
failure) and recover their claims on the owner's dashboard. Leave the replica registered but idle. Restore the
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
