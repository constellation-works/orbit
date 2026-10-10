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
related_artifacts: [ORB-14260, ORB-14194, ORB-13908, ORB-13941, ORB-14149, ORB-13663, ORB-13642, ORB-13625, ORB-12968, ORB-12516, ORB-12515, ORB-12500, ORB-12495, ORB-12564, ORB-12491, ORB-12490]
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

Internal worker reads and typed claim updates use host-owned session
provenance. The local runtime supplies it directly; the remote runtime selects
the hidden `--worker-host` SSH server mode. Managed worker processes cannot
launch that mode, even after removing their activity environment. Neither
initialize metadata nor tool arguments can grant it. Ordinary agent calls
containing `_worker_read` or `_worker_update` are refused before routing and
again at the owner. Allowed worker updates still pass through input redaction
and record any redaction audit against the claimed task.

Ordinary worker MCP calls to task and friction tools route to the bound remote
owner. Omitting `workspace`, sending `null`, or sending an empty or whitespace-only
string uses that owner destination. A non-string selector is refused as invalid
input; a string naming another workspace is denied as a worker binding mismatch.
Explicit selectors may name the bound owner destination, its logical workspace
ID, or the worker's current checkout path.

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
- when before-PR or before-landing review is on at the owner
  (`review.before_pr = true` or `review.before_landing = true`), the owner sets `operation.review_crew`, the workspace ships through the PR route,
  and every follower can run that crew. When `operation.review_crew` is a pool
  (`["sol", "grok"]`), the owner resolves it to one member each time it offers
  a claim, and that crew is what the claim captures, so every follower must
  be able to run each member it may be offered.

```bash
orbit config get review.before_pr
orbit config get review.before_landing
orbit config get operation.review_crew
orbit config show        # the Review lines report both switches and their sources
```

The owner's `review.before_pr` is captured on each claim with its review crew,
minutes, `workflow.required_validation_commands` and `review.baseline_commands`; the follower's own
setting does not define review requirements. With it on, each
claimed PR leaf runs the before-PR review between base synchronization and
push: one reviewer with the captured crew fixes what it finds as the
candidate's second commit and comments a summary on the owner's task. A
`reject` or `incomplete` verdict fails the leaf before anything is pushed, and
the owner blocks the task with the findings already on it. An evidence-only
`incomplete`, such as a macOS reviewer owing the Linux CodeQL run, holds
instead. An owner `[[review.host_evidence]]` rule makes that hold
deterministic: the leaf owes the rule's check whatever its reviewer reports. The leaf pushes the held candidate to `orbit-evidence/<branch>` on
`origin` and ends `held`. Its settlement releases the claim with the hold, and
the owner keeps the task `in-progress` under `review_awaiting_evidence`. A
held leaf is not a failure for the breaker, and the task is not pulled again
while held. A Linux owner then fulfils `codeql`-only holds itself
([owner fulfilment](codeql-local.md#owner-fulfilment)). Receipt of the
evidence queues the task for a fresh review, which a later pull takes. A
passed verdict
travels in the handoff, and the owner checks the certificate against its own
copy before it accepts, including the captured owner check list. The owner also
verifies exact-run and exact-head validation logs using its own required
commands; a command-list mismatch fails closed. A follower that cannot resolve
or run the captured crew stops pulling with `before_pr_reviewer_unavailable`
rather than claiming work it cannot review. A local-only ship workspace with
before-PR review on is refused (`before_pr_unsupported`).

The owner's `review.before_landing` is captured the same way, and the
follower's own value is ignored. A claimed PR leaf then pushes and opens its
PR first and runs the same contract on the published head after `pr_open`,
while hosted CI runs. A reviewer fix is revalidated on the leaf and pushed onto
the PR under a lease (`push_lease_lost` if the branch moved). The handoff
carries before-landing evidence for the settled head, and the owner refuses a
handoff without it, with before-PR evidence instead, or for any other head, so
an unreviewed head never reaches `review` or a merge. A leaf whose review does
not approve fails with its PR open and unmerged; evidence-only holds do not
apply to this timing. The claim's ship contract carries the field, so owner and
followers need matching builds. Readiness, refusals and the reviewer-crew
requirement are the same as for before-PR review, with the reason codes
unchanged.
After-landing review (the `delivery-code-review` auto-task) never affects
admission: the owner reviews landed deliveries whatever host implemented them.
A follower's landed PR reaches the owner's review batch under the claimed
task's id, which the owner reads from the handoff it accepted.

Match toolchains and required validation commands the same way you would for
a second owner-local executor. Crews may differ: a follower only receives
tasks whose crew it can run. A follower runs a pulled task on the crew the
owner's task names, and uses its own `workflow.default_crew` only when the
task names no crew. Each drain declares the crews its host can run (see step
8), and the owner skips a task whose crew is not among them, leaving it in
the backlog for the owner or another follower. Use the same crew names on
both sides: crews are matched by name.

Hosts may run different operating systems. A task tagged `os:linux`,
`os:macos` or `os:windows` runs only on a host of a named OS (several tags mean
any one of them; no tag means any host). Each pull request declares the
follower's OS, and the owner admits a tagged task only to a follower whose OS
it names; the owner's own drain applies the same rule to itself. A task no
current host can run stays in the backlog, never claimed, with the wait named
(`waits for a macos host (os:macos)`) in the owner's readiness, `orbit run
show`, the Drain card, and the follower's idle receipt (`os_unavailable`). So
an `os:macos` repair filed on a Linux owner waits for a macOS follower instead
of blocking the owner's worker. Retag with `orbit.task.update` to reroute a
backlog task; running or claimed work is not moved. Owner and followers must
deploy the same protocol revision (currently 8, as defined by
`DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA`; OS matching arrived in revision 4). Empty
`workflow.required_validation_commands` means no required check, as on an
owner's own delivery: a claimed leaf runs nothing and records that, and the
owner accepts its handoff without validation logs. Every other handoff check
(footprint, protected paths, candidate and base integrity) still applies.
`orbit doctor` reports the empty list, and `orbit run auto` notes it.

Within the same dispatch band and priority, after the frozen-batch expiry
boost, pull admission prefers tasks the follower's OS satisfies but the
owner's OS does not, before age and task ID. A macOS follower of a Linux
owner therefore takes macOS-only work before ordinary work the owner can
run. Critical work still leads. Tasks tagged for both hosts have no affinity
boost, and owner-local admission keeps its existing order.

The latest applied task-pilot disposition `host_operational` identifies work
that requires an operator-side action no managed lane can perform. Human
approval to backlog does not clear that finding: local drain and ship
selection report `host_operational_handoff`, workflow admission refuses it,
and owner pull admission defers it without a claim. The pilot's audit and
`host_operational_held` history event retain the disposition and its evidence.
Admission backfills older receipts once. The hold is scoped to the assessed
material; status, priority and unrelated comments do not clear it. Older
receipts without a snapshot hold conservatively until admission or a trusted
operator decision records one; a generic update event cannot prove a re-scope.

An operator can release the hold through the trusted human task-update path
with `task-pilot-admission: evaluated`, `clear`, or `approve-anyway` as the
comment's first line and evidence below it (`evaluated` must reference a
non-empty attached evaluation artifact). This records `host_operational_resolved`.
A newer assessment supersedes the decision; a `selectors` or `verified_no_diff`
assessment without another hold admits as before. `no-diff-expected` does not
exempt host-operational work, even if an auto-task lane grants relevant tools.
An operator decision releases admission only; task prose never grants tools.

Task-pilot checks acceptance criteria for native host evidence requirements.
It records each one as a typed finding, committed with the applied assessment
as `native_os_hold`: `required_os` (the one-based criterion and the OS) for an
OS requirement, `required_machine` (the criterion and the owner machine) for
evidence only the owner's own store, services or data can produce. A
requirement stated only in warning prose routes nothing. When every
`required_os` entry names one OS and the task carries no `os:` tag, the pilot's
atomic apply adds that tag, so tag routing sends the task to a host of that OS
and a follower of another OS sees it as `host_os_mismatch`. An `os:` tag the
task already carries is never removed or replaced; a conflicting one is
reported in `utility_warnings`. Platform mentions, cross-compilation, mocked
checks, negative admission tests and measurements any host could repeat do not
require a native host. If evidence is required on every named OS, use separate
host-scoped validation tasks: multiple tags allow any one OS, so the pilot adds
none.

A `required_machine` finding has no tag. The owner refuses a pull claim from
every other machine (`deferred_conflicts`, shown as an `owner_hold` whose
reason begins `Machine requirement:` and names the machine), while its own
local drain may start the task. A pilot may name only the owner (by its
registered machine name or id); a requirement on another machine stays a
`utility_warnings` finding for an operator. The same re-scope, newer assessment
or evidenced operator decision clears it.

Admission honours an OS finding while the task's `os:` tags do not name the
required OS. A local drain, ship discovery or `orbit run ship` on a host of
another OS leaves the task in `backlog` as `native_os_required`, and the owner
defers a pull from a follower of another OS (`deferred_conflicts`, shown as an
`owner_hold`). Readiness, `orbit run show` and the Drain card name the criterion
and the tag to add. A host or follower of the required OS may still take the
task, and a task whose own `os:` tags exclude a host keeps `host_os_mismatch`.
The wait clears when the tag is added, the acceptance criteria are re-scoped, a
newer assessment carries no finding, or an operator records an evidenced
`task-pilot-admission: clear` or `approve-anyway` decision through
`orbit.task.update` (history event `native_os_requirement_resolved`).

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

The role change also keeps the host's local friction corpus. List it on the
replica with `orbit friction list --status open`, then close each legacy report
with `orbit friction resolve <ID>` or an audited `orbit.friction.update` with
`status: resolved` and disposition evidence. These operations resolve only
existing records in the replica's local workspace partition; the owner's
records are untouched even when IDs collide. New reports, reopening, and actual
re-homing still require owner authority. No owner connection or store migration
is needed to close local legacy reports. See
[friction lifecycle](../../crates/orbit-core/assets/skills/orbit/references/friction.md#closing-legacy-records-on-a-replica)
for tool inputs and audit behavior.

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
supply own `context_files` yourself. Empty context holds no context lock.
Distributed pull admission, an explicit ship and a single-slot local drain
admit selector-free backlog tasks on the next pass. A local drain or ship with
more than one slot waits for this host's task pilot to prepare one
(`awaiting_footprint`), then runs it only alone (`awaiting_exclusive_slot`). Undeclared edit conflicts are handled at
landing by rebase and conflict repair. Operator task-scope reservation still
requires a declared surface. A live task-pilot preparation (its reservation
from prepare, then its checkpoint) still holds its tasks until that run settles.

Reservation TTL on a pulled claim is 14,400 seconds (four hours). Expiry does
**not** revoke the claim, admit another worker, or shrink the frozen
non-pruned footprint. Status-derived locks on `in-progress` and `review` tasks
keep the declared selectors, including files that do not exist yet.

A claimed implementer may add files anywhere the work requires; the frozen
footprint is a scheduling hint, not a delivery gate. Every added path the
footprint does not cover becomes a widening request, derived from the final Git
candidate rather than the implementer's reported selectors. The owner
independently checks the diff and accepts the widening even when another live
claim, in-progress/review selector or reservation names the path. Acceptance
records exact file selectors, a `context_files_widened` history entry and the
enlarged live claim; the original receipt stays immutable. Only Git or `.orbit`
metadata, environment files, symlinks and malformed paths are refused, with
exact paths. Protected metadata names and environment patterns (including `.envrc`)
ignore ASCII case on every host: `.Orbit/`, `.GIT/`, `.ENV` and `.Env.local`
are refused even on Linux. Both peers require the same protocol revision (currently 8;
widening arrived in 3).


### 5. Establish SSH owner access (no destination callers file)

SSH login to the owner **is** owner access. There is no
`~/.orbit/mcp-callers.toml`, forced-command acceptance, KeyBound proof, or
replacement destination identity registry. `--remote-caller-machine-id` is an
attribution label, not a credential.

On the follower, register the owner and serve federation from the caller:

```bash
orbit host add <owner-ssh-target>
orbit host list            # owner reachable, same binary_version and protocol_fingerprint
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
  "caller_schema": 9,
  "caller_before_pr": false
}'
```

The probe reports owner machine, binary version, distributed-drain protocol
schema `8`, this session's capabilities, diagnostic caller machine,
owner-resolved ship configuration (`ship.before_pr`, `ship.before_landing`),
and `review`: the review switches with their sources — before-PR and
before-landing on/off and minutes, after-landing enabled and its next batch
due. Declaring version, schema, or `caller_before_pr`
also reports the **first refusal admission would raise**, in admission order. It creates no receipt, reservation, claim, or
task. A replica destination refuses the tool instead of answering about
itself, naming its owner. Run the diagnostic CLI there; the follower runtime
uses its internal owner selector. Do not call pull as a health check: a pull is an admission, and an
admitted claim is real work the owner holds until it settles.

Expected refusals you may see (and must not work around):

| Error | Meaning |
|---|---|
| `capability_refused` | Destination is a replica, or the session lacks agent/operator identity |
| `version_mismatch` | Caller binary version differs from the owner |
| `protocol_skew` | Caller and owner request fingerprints differ (or the owner predates fingerprints); refused before pull, with both fingerprints in the diagnosis |
| `protocol_mismatch` | Legacy probe report for differing integer revisions; current followers surface typed `protocol_skew` |
| `ship_mode_unsupported` | A remote caller targeted a local-only ship workspace |
| `before_pr_unsupported` | Owner has `review.before_pr` or `review.before_landing` on and ships local-only, or the executor's leaf does not run the review gate (an older binary) |

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

Matching binaries, a replica role, a working probe, and, when the owner has
`review.before_pr` on, the owner's review crew on this host are **installation**. Starting a drain is the rollout, and it is explicit. On
the follower, from the replica checkout:

```bash
orbit run auto --pull <selector> --for 8h --concurrency 3
```

`<selector>` is the owner's host-qualified selector from federated discovery
(`orbit_workspace_list`, e.g. `hm_owner/ws_orbit`); an owner with no host entry
(`orbit host add`) is refused as an unknown selector, and the message says so. Before anything is
submitted, the command refuses unless:

- this checkout is a **replica**, and the selector names **its** owner machine
  and **its** logical workspace;
- the owner answers the probe **as that machine** and would admit this
  executor now (binary, protocol schema, before-PR review, ship mode);
- this host can resolve the owner's before-PR review crew, when the owner has
  `review.before_pr` on (`before_pr_reviewer_unavailable` otherwise);
- every `--allow-crew` name is a crew this host configures (none blank).

Without `--for` (or with `--for 0s`) the drain makes **one** admission pass:
it requests up to `--concurrency` claims once, admits no replacement as they
settle, and ends when they have. Over an empty backlog it asks once and ends.
A stop or cancel before that pass takes it away; a retried or resumed drain
never makes a second one.

`--allow-crew sol,luna` restricts which crews this drain declares to the owner
for its whole life, resume included: the owner hands it only tasks on those
crews (or on a crew resolving to the same provider and model), and other tasks
stay in its backlog on their own crews. It changes no configuration, task crew
or owner pool. The owner's before-PR review crew is not restricted by it but
must still run here. If nothing allowed is runnable, each pass reports
`no_runnable_crew` and requests nothing; restart without the flag or with a
crew that runs here.

A pull drain never approves `proposed` tasks: `--pull` conflicts with
`--approve-proposed`, and only the owner approves work. The replica's dashboard
Drain card follows the same rule: readiness reports `replica: true` and the
**Proposed tasks** control is disabled with that reason. Start an
approve-proposed window (`orbit run auto --approve-proposed`, or **Approve
qualifying** in the owner's Drain card) on the owner.

This host should declare the same `workflow.required_validation_commands` as
the owner, since the owner re-checks the evidence against its own list. An
empty list is not a refusal: the drain starts with a `Note:` line saying no
required validation runs here.

The drain is an ordinary durable run of `workspace_pull_pipeline`:

- On its first iteration the drain runs a provider preflight over every crew
  this host configures. A crew is runnable when it is enabled, its provider's
  executor resolves, and that executor's CLI is found where a leaf would
  launch it. The preflight starts no provider and checks no login; a provider
  with no signed-in user is caught by its first claimed leaf (below). The
  result is kept for the drain's window. Every pull request declares the
  runnable crews and this host's OS, and the owner admits only tasks this host
  can run. Tagged `no-diff-expected` work is claimable: the follower's leaf
  hands off `NoDiff` instead of opening a PR, needs no `no-diff.json` from a
  review, and refuses a change as `no_diff_expected_changed` (remove the tag
  to ship code). Pin work that must stay on the
  owner with an `os:` tag or a crew. If no
  crew is runnable, the drain requests nothing and reports
  `no_runnable_crew`. After you install a missing CLI, start a new drain to pick it up.
  Auth failures may recover in the same window through a declared probe (below).
- Each iteration first carries earlier admissions forward — retries an
  unanswered request under the **same** ID, binds, launches, and delivers a
  finished leaf's settlement — then, while the window is open, tops free slots
  up with new pull requests, each persisted before it is sent.
- Each claim runs as one local `task_claimed_pr_pipeline` leaf: implement,
  validate on the exact candidate, push, open the PR, hand off. The owner
  observes the PR itself and moves the task to `review`. **Nothing lands until
  the owner approves the handoff** on its dashboard.
  A leaf whose implementation proves no change is needed hands off `NoDiff`
  instead. The implementer writes `no-diff.json` (or `already-landed.json`)
  and its validation logs beneath `.orbit/tmp/`, returning their artifact paths
  and scratch `source_path`s as `no_diff_artifacts`. Commit imports these
  bounded files through the claim and verifies them before the leaf skips PR
  preparation and publication.
  The handoff pins that verifier report; the owner rechecks it against its live
  base and completes without a PR under the same completion authority. A moved
  base or changed report requires fresh validation; a skip flag alone is refused.
- The implement step runs in **claimed mode**. The agent sandbox denies
  `~/.ssh`, so a sandboxed agent on a follower has no route to the owner; it
  does not need one. It works from the injected task envelope, is not granted
  `orbit.task.update` (nor is any recovery agent the leaf launches, such as
  `step_failure_recovery`). Its read-only `orbit.task.show` is scoped to the
  claimed task through the run's coordinator. It returns its execution summary
  in the step output. `claim_handoff` carries that summary in the typed
  handoff, and the owner writes it as the task's `execution_summary` when it
  accepts. The leaf's delivery gate judges that same summary, so a retry is
  not refused on the `Outcome: failed` a previous attempt left on the owner;
  do not clear it by hand. Do not loosen the sandbox or add SSH credentials to it to "fix" a
  leaf; an agent that reports an unreachable owner store is a prompt or
  binary mismatch, not a transport problem (check the follower's binary is
  current).
- An agent in a claimed leaf reaches the owner only through the run's
  coordinator (the step runner's broker, outside the sandbox), for a closed
  list of calls: `orbit.task.show` of the claimed task,
  `orbit.task.add` of a task `spawned_from` the claimed task and related to
  nothing else (a claimed `delivery-code-review` or `code-review` task's
  finding may also name its culprit as `regression_from`, which the owner
  checks), `orbit.friction.add` (during the claimed task, if it names
  one), and `orbit.task.artifact.get`/`put` on the claimed task. Its nested
  `orbit`, from the CLI or MCP, hands those calls over; the coordinator takes
  the task and claim from its own records, applies the activity's tool
  policy, and carries them to the owner over this follower's SSH route, where
  the claim fence refuses them once the claim is no longer active
  (`stale_claim`). Inside the sandbox, any other owner call is refused
  (`claimed_owner_bridge_refused`) and none is tried over SSH. The claimed
  implement step is still not granted `orbit.task.update` (above).
  `orbit.search` is among the refused calls, so a claimed review files its
  findings without a duplicate search, and the owner's triage dedupes them.
  A finding the owner still refuses arrives as the claimed task's
  `unfiled-findings.json` artifact: file each entry on the owner, keeping its
  relations. The implement step refuses `unfiled_findings` that is not an array
  of `{title, description}` objects, before the commit, push or PR-open steps
  run; its retry and recovery are the repair attempt. A candidate already
  published with plain-string entries (or a replay of its failed
  `claim_handoff`) still hands off: the handoff rewrites each non-blank string
  as `{title: <first sentence>, description: <the whole string>,
  normalized_from: "string"}`, sets `normalized_string_entries` in the
  artifact, and says so in the execution summary. Any other malformed entry, a
  blank string or a non-array field, is still refused.
- If the coordinator is missing or gone, the call fails as
  `owner_route_unavailable` and the agent ends its step on that code. The
  run skips step and final recovery, and the leaf releases its claim with the
  `owner_route` class (below): the task goes back to `backlog` on the owner
  and the drain requests no more work for the rest of its window. Check that
  the follower's binary and launch pass `ORBIT_PLUGIN_BROKER` to the agent's
  `orbit` before starting a new drain.
- The before-PR reviewer's `review-*` reads (`review-manifest.json`, its
  prior review evidence and the evidence the owner's hold names) and its
  `review-report.json` write take the same route and are also checked against
  the running review attempt. Refusals and their recovery are in the
  [claimed-review artifacts runbook](./claimed-review-artifacts.md); none is
  fixed by loosening the sandbox.
- An owner that refuses a request is checked against its receipt first: a
  committed claim is carried forward, and only a request the owner holds no
  receipt for is closed (`Refused`) and its slot returned.
- An unreachable owner is reported in the iteration output and retried; the
  drain never fails over to its own store.
- An owner that times out waiting on its task commit lock or its database
  answers `lock_busy`. A leaf's owner reads retry it twice before failing, and
  a step it still fails releases the claim as `transient`. When these recur,
  read the owner's `orbit.jsonl`: the `still waiting for advisory file lock`
  warning names each holder of the lock by pid, section and call site, and
  `advisory file lock held past its threshold` names a section that held it
  for 2 s or more. A section queued behind a waiting admission or recovery
  names it as `queued exclusive waiter`, followed by the holders that waiter
  is waiting on.
- Each leaf settles itself when it ends ([ORB-13663]): its worker records
  the handoff (success) or a failure, then delivers it to the owner, retrying
  for a few minutes if the owner is unreachable and no live drain carries the
  claim. Every drain pass, a cancel or `orbit run auto --stop` delivers
  anything the leaf could not, and a drain pass also reconciles a launched
  leaf whose worker died so its settlement is recorded and delivered. A leaf
  that was cancelled before it launched releases its claim instead: the task
  goes back to `backlog` on the owner with a comment naming the drain. A
  pre-spawn launch failure also cancels the queued leaf and releases its claim
  with class `environment`, suppressing further pulls on that host for the
  window. If a child was spawned before registration or observer handoff
  failed, the supervisor stops and reaps it when possible; launch intent stays
  recorded until the run is reconciled. A pending run alone does not establish
  that the worker never executed, so this case is not released as unlaunched.
  A launched leaf that ends without its handoff settles with a typed failure
  class. Only `candidate` (the work failed) and `task_input` (final recovery
  rejected or archived the task) move the task to `blocked` on the owner, with
  a summary naming the leaf run, its failed step and that step's error. Every
  other class releases the claim: the task goes back to `backlog` with a
  comment naming the class and the reason. These classes are
  `operator_cancel` (`orbit run cancel <leaf-run>` or the dashboard's cancel,
  with its reason; `--block` fails the claim and blocks the task instead), `provider` (the CLI failed authentication or its model was
  at capacity), `environment` (validation lacked a tool), `owner_route` (the
  leaf could not reach the owner), `baseline_red` (required validation fails
  on the base exactly as on the candidate; the owner holds the task until the
  base passes), `transient` (validation could not reach the network after its
  reruns, the forge kept refusing the leaf's push past its retry window
  (`[forge_unavailable]`), an Orbit upgrade refused the leaf's agent mid-step
  (`[upgrade_pending]`, see [in-flight agent steps during a binary swap](upgrades.md#in-flight-agent-steps-during-a-binary-swap)),
  or the leaf's worker died) and `base_conflict` (the committed
  candidate could not be synchronized onto a base that moved). The failure
  breaker does not count a release. When the forge refuses a claimed PR
  leaf's push for a server-side reason (`Internal Server Error`, `Service
  Unavailable` and the like), the leaf keeps its claim and retries the push
  of the same reviewed head, past the usual backoff budget, for up to two
  hours from the first refusal (the pipeline's `forge_retry.window_ms`). The
  claim stays live and the leaf's runtime stays busy, so an upgrade waits
  for it as for a long agent step. Once the forge accepts, the same leaf
  pushes that head and opens the pull request without implementing or
  reviewing again. When the window closes first, the leaf releases the claim
  as `transient` and the release names the held head, its target ref, the
  attempts and the first refusal; that candidate then stays only on the
  follower. A forge release blames neither the crew nor the host: the drain
  keeps offering both, and the owner may hand the task straight back to the
  same drain, whose next claim continues the kept candidate. After
  `operator_cancel` or any other `transient` release, the drain stops
  offering that crew for the rest of its window. After
  `provider`, an authentication failure stops every crew of that provider
  (an `anthropic` crew is the same provider as `claude`); a capacity failure
  stops only the crew the leaf ran. A usage limit stops no crew by itself:
  the reading the leaf recorded on this host excludes the provider's crews as
  `provider_limit` until it lapses.
  After `environment` or `owner_route` — failures of the host itself — it requests
  no more work at all for its window (`host_suppressed:` refusal,
  `crews.host_suppressed`); fix the host and start a new drain. In either
  case (forge releases aside) the owner does not hand the released task back
  to that drain, so it is not pulled straight back; another drain may still
  take it. When the leaf had
  committed a candidate, the release or block names it, and the task's next
  claim continues it rather than starting over, unless the task's spec
  changed or `orbit task update --discard-candidate` discarded it since. A
  claimed PR leaf that fails after its commit and before its push carries
  the candidate to `refs/orbit/candidates/<task>/<run>` on `origin`, and the
  release comment names that ref, so a claim on any host can fetch it. When
  that push fails (no push access, `origin` unreachable), the release says
  why and the candidate stays only on the follower that made it: a claim on
  that follower still continues it, and a claim on another host implements
  fresh. Claimed-local leaves run on the owner and continue its candidates
  from its own repository. Returning a blocked task to the backlog for an
  owner-local run (tagging it `os:linux` after a follower's review failure,
  say) keeps its candidate too: the owner's own `task_pr_pipeline` continues
  the candidate the failed claim kept, and its `resume_candidate` output
  names the follower's run with `source_machine_id`. Every fresh start that
  sets a kept candidate aside is in the owner's task history as a
  `candidate_resume` event whose note begins `fresh:` and names the reason
  (`not_durable`, `spec_changed` or `discarded`; `reason_code` `not_durable`,
  `spec_changed` or `candidate_discarded` on an owner-local run), with the
  claim and the machine that committed it: `orbit task show <task>` shows it. The
  same holds the other way round: an owner-local run held for a red base, a
  missing validation tool or a provider failure pushes its candidate to
  `refs/orbit/candidates/<task>/<run>` too, and the task's next claim on any
  host continues it. When that push fails, the hold comment says the
  candidate is host-local and quotes the push error, and a claim on another
  host implements fresh with a `not_durable` reason. Carried refs are not
  deleted automatically; once a task is done, prune them on `origin` with
  `git push origin --delete refs/orbit/candidates/<task>/<run>`, listing
  them with `git ls-remote origin 'refs/orbit/candidates/*'`. A task
  released twice within 24 hours for a typed failure class is blocked by the
  next such failure, with one comment listing every reason; unblock it once the cause
  is fixed. The full diagnostic stays in the follower's run
  (`orbit run show <leaf-run>`, and `.orbit/state/logs/<leaf-run>.worker.log`
  on the follower). That run page carries a `Claim:` line (`pull_claim` in
  `--json`): the owner task, claim, owner selector and admitting drain,
  whether the leaf's outcome has reached the owner, and its `failure_class`.
- `orbit run show <drain-run>` lists the crew window as `Crews:` lines
  (`crew_window` in `--json`; the dashboard's run detail shows the same
  panel). The first line names the runnable crews. Each excluded crew is
  listed with its source and the reason: `preflight` (disabled, executor
  unresolved, or CLI not found), `provider_unavailable` (a claimed leaf's
  provider could not authenticate, which lists every crew of that provider,
  or reported its selected model at capacity, which lists that crew, with
  the task and error), `leaf_released` (a claimed leaf was
  released for a `transient` failure other than a forge outage, with the
  task, class and reason) or `provider_limit` (this host's latest reading of
  the crew's provider usage window is at or over its threshold, written
  `(provider_limit until <time>)` before the reading; it lifts at `until`
  within the same drain, see
  [provider usage limits](../CONFIG.md#provider-usage-limits)). Each iteration's output carries
  the same window as `crews`. Auth exclusions also list provider, host, failure time, error class,
  re-login hint, credential source and next probe time (`auth_exclusions`).
  Doctor warns about these on live drains; the dashboard shows the same data.
  Follow the hint on the named host. A declared `auth_probe` first runs after
  ten minutes, backs off to twenty then thirty minutes on failure, and
  re-admits the provider's crews when it passes. Recovery is durable across
  restart/resume; a later auth failure starts a new delay. Only active auth
  exclusions are probed while the drain is admitting. Claude ships a minimal
  Haiku probe; other providers currently need a new drain after re-login.
  See [executor authentication recovery probes](../CONFIG.md#executor-authentication-recovery-probes)
  for the declaration and credential-route details. Missing launchers,
  capacity, refusal, crew and host exclusions retain their existing behavior.
- After three consecutive claims settle as failures, the drain stops
  requesting work (`circuit_open` in the iteration output) and only keeps
  settling. Inspect the blocked tasks and their leaf logs, fix the cause,
  re-backlog them deliberately, and start a new drain. Released claims are
  not failures and never open the breaker.
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
  not registered with `orbit host add`, or an unregistered
  checkout), not that the owner is down. A status lookup failure without a
  transport error is `skipped:owner_lookup_failed`, with the reason in
  `detail`; it does not establish that the owner is down.

Operate it with the ordinary run commands: `orbit run show <run-id>`,
`orbit run concurrency <run-id> --set N` (MCP: `orbit_workflow_auto` with
`action: "resize"`), and `orbit run auto --stop` (closes the window; live
leaves keep running and still settle). The drain's `--concurrency`, as last
retuned, is the only ceiling on its leaves: the claimed leaf jobs declare no
active-run limit of their own. A leaf in its before-landing review (the owner's
`review.before_landing`) runs no implementer but keeps its claim until it
settles, so the drain admits a replacement beside it: at most `--concurrency`
reviewing leaves free their slots this way, so one drain carries at most
`--concurrency` implementing leaves plus as many reviewing ones. Build-budget
slots still limit the compilers they run. `orbit run show <run-id>` counts the
`Claimed leaves:` it carries as implementing and reviewing (`.claimed_leaves[].stage`),
and so does the dashboard's run page. `--for` is at most 24 hours. Ship-sweep
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

A claimed leaf's run lives in the follower's store, so the owner's
`orbit run history` never lists it. The owner keeps the failure or release
settlement each leaf sent: its kind, evidence class (`provider_unavailable`,
`baseline_red`, `forge_unavailable`, `evidence_hold`, `final_recovery`,
`failure`, or `summary` for an untyped one), typed failure class, crew, failed
step and bounded reason. Settled claims carry no in-flight attempt, so this
listing needs no operator capability; the `run-failure-patterns` auto-task
reads it:

```bash
orbit run settlements --since 7d --no-reconcile --json
```

Claims settled before the owner kept settlements read `unrecorded` unless
their release kept a typed class; the task's history note carries the reason.

The owner's dashboard shows the same state, plus the accepted handoff, inside
the task detail it belongs to — there is no distributed tab, and the panel
appears only for a task this workspace holds a claim for:

```bash
orbit web serve --operator
```

`GET /api/distributed/claims` is the read. It accepts `task=<task-id>` and
`state=active|settled|all`; state defaults to `active`, and settled claims are
compact summaries unless `detail=true`. The task detail panel sends its task
filter with `state=all&detail=true`, so it fetches only that task's claims and still
shows their settled history. A replica answers that the owner
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
Resuming an older local run after another host has bound the task is the same
refusal: the error names that host's run and machine, and it does not reuse
the old checkpoints. Continue the binding on that machine, or admit a new
attempt only after an authorized rebind.
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

If a follower's pull request was merged by hand anyway ([ORB-14175]), revoke the
handoff and use **Recover claim → blocked** on the owner. Then move the task
through `in-progress` back to `review` and complete it with an operator's
evidence-bound desktop review. The task keeps the follower's `job_run_id` and
`job_run_machine`. Completion reads the recovered claim and the accepted
handoff for that exact host and run, takes the pull request from the handoff,
reads it by number, and requires it to be merged into the landing branch.

If the merged head is not the handed-off candidate (for example, after a base
merge or a fix pushed by hand), the candidate's validation and review do not
carry over, and the review gate cannot run on an already-merged head.
Reconcile that head instead, as an operator on the owner (the owner checkout
must hold the merged head and merge commit; `git fetch origin` there first):

```bash
orbit task reconcile-review inspect <task-id>
orbit task reconcile-review submit <task-id> --request <key>
orbit task reconcile-review status <task-id>
```

`inspect` shows the binding (run, host, claim, handoff, pull request, merged
head and base) and the `contract` a new key would freeze. It refuses with the
next step while a claim is live, the pull request is open or names another
repository or landing branch, there is no command to validate with, or
`operation.review_crew` is unset or does not resolve.

The contract's `required_commands` are the accepted handoff's captured list
(`commands_source: accepted_handoff`). An accepted handoff always captures one,
so an empty list means that acceptance explicitly required no check; the
reconciliation then adopts the owner's `workflow.required_validation_commands`
at submission as its own contract (`commands_source:
owner_configuration_at_submission`, with `accepted_commands: []`) rather than
claiming the delivery was held to it. `review_crew` is `operation.review_crew`
at submission; a pool contributes one member, preferring one that did not
implement the task, with `review_crew_source` naming the pool's layer.

`submit` freezes that contract into the record and admits one run of
`task_review_reconciliation_pipeline`: it runs every contract command at the
merged head (and each failure again at the base), has the contract's reviewer
inspect exactly that head read-only, and settles the reconciliation. Editing
the owner's configuration afterwards changes nothing for that record: every
attempt, including one admitted after a stopped run, uses the frozen contract.
If the frozen crew no longer resolves, the attempt fails and resubmitting the
key refuses until the crew is restored; a new key adopts the current
configuration. Resubmitting the same `--request` key replays a live or settled
reconciliation without running anything again; a new key starts a new
reconciliation. The
record lives in the owner's review store, separate from review-gate
certificates, and never changes the original run's identity or the merged pull
request. Agents cannot submit or dispose one.

The default text output includes this information; `--format json` returns the
full tool document. `status` prints one line per reconciliation with its id,
outcome, current run (when present), and exact next step. `submit` and
`accept-baseline` print that same summary for the resulting record, including
when a request is replayed. Use the printed id with `--reconciliation`.

`status` names the outcome and the exact next step:

- `accepted`: complete the task from review.
- `refused`: the reviewer left open findings (they are filed as one follow-up
  task), a command fails only at the merged head, or the delivery changed while
  the run ran. Fix forward through the follow-up, or submit a new key for the
  current head.
- `awaiting_disposition`: every failing command also fails at the base. Once a
  commit on the landing branch remediates it, record the decision. The command
  reruns that same required check at the named commit in a detached checkout;
  it records the output and refuses the disposition unless the check passes:
  `orbit task reconcile-review accept-baseline <task-id> --reconciliation <id> --command '<command>' --remediation <commit> --reason '<why>'`.
  The outcome becomes `accepted_with_disposition`; validation stays incomplete
  in the record.

  The remediation must contain the delivery as it landed. `submit` binds the
  commit the provider reports the pull request landed as (`binding.pull_request.landed`:
  its merge commit, squash commit or last rebased commit), and the disposition
  refuses, before running anything, a remediation that is not a descendant of
  it. A fix that reached the landing branch before the pull request merged
  passes the check without the delivery's code, so it would hide any failure
  the delivery added to the same command. Land the fix on top of the landed
  commit and name that new commit. Only the landed commit counts, not the pull
  request's head, which a squash landing does not keep. The disposition also
  refuses when the provider's answer (head, landed commit, task, claim or
  handoff) changes before or while the check runs; inspect and submit a new
  request key. A legacy all-pass `accepted` record can still authorize
  completion after the reconciliation consumer confirms its existing checks
  for task meaning, execution, handoff, pull request, merged head and binding
  integrity; schema 3 or earlier does not disqualify that outcome. The legacy
  restriction applies to baseline dispositions: a record written before the
  landed commit was bound (schema 3 or earlier) keeps its original validation
  and failed evidence, but cannot record a disposition or authorize
  `accepted_with_disposition`. Submit a new request key to reconcile the same
  head with an authenticated landed binding before recording a baseline
  disposition. Validation remains incomplete after a disposition, and the
  remediation must still contain the landed delivery and pass the required
  command.

On the dashboard, **approve** on a review task that has a handed-off claim
sends **Approve handoff** for the exact candidate. A plain status write would
be refused with `active execution claim requires a claim-scoped mutation`, which names the task and the claiming run.

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
  The release holds even with `--block`, which only decides how the stopped
  leaves' own local task couplings are left.
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
runs it started, which finish on their own; `--force` cancels them too and
returns their tasks to `backlog`, preserving their candidates and recording
the cancellation reason. Use `orbit run cancel <drain-run> --confirm --force
--block` to keep those tasks blocked instead. The MCP forced-stop control
returns cancelled local tasks to `backlog`. Stops
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
the auto card's button reads **Send pending results** and runs the same settle-only
pass; it stays available because the pass needs no active drain.

Leaf delivery can still fail — the owner was unreachable when the leaf ended,
or when `--force` released it. The settlement stays recorded on the follower
as `settling` and is retried without a new drain. A live drain retries on
each pass, and a leaf's worker retries briefly (15s, 60s, 240s). After both
have ended, the OS clock sweep (`orbit clock tick`, every minute by default
after `orbit routine init --install-clock`; use `orbit clock enable` to resume
an installed paused clock) retries it: each tick opens the host's replica
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
move it back to `backlog`.

**A settlement the owner refuses while it still holds the claim**
([ORB-13979]) — for example, an owner that refuses a footprint widening onto
a path it protects — is an answer,
not a lost delivery, and repeats until an operator changes the owner. The
follower records the refusal on the admission (`settlement_refusal`), logs it
once, and keeps the record `settling` with its outcome unchanged; nothing is
lost. It is not a failed pass, so it does not degrade the drain, but the
drain requests no new claim while one is held (pass output
`refusal: settlement_refused: …` and `settlement_refused: <count>`). Drain
passes, the clock sweep and the leaf's worker deliver it again only after a
backoff that starts at one minute and doubles to at most 15 minutes, instead
of on every pass. `orbit run show <drain-run>` lists each one on a
`Settlement refused:` line (`.refused_settlements` in JSON) with the owner's
reason, the refusal count, the next attempt and the remedy, and a leaf's
`orbit run show` carries it on its `Claim:` line (`.pull_claim.settlement_refusal`).
Fix the condition the reason names on the owner. The next due attempt then
settles the recorded outcome and the drain resumes requesting. To retry at
once, run `orbit run auto --stop` in the replica checkout: an operator's
settle-only pass ignores the backoff, though it also closes the drain's
window.

To give the claim up instead, revoke or recover it from the owner's dashboard
(**Recover claim → blocked** or **→ backlog**). The next delivery on the
follower, whether a drain iteration or `orbit run auto --stop`, looks the
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

## Replica worktree GC

On the owner, delivery removes a task worktree once the run lands, and the
owner's `worktree-gc-<workspace>` routine is the hourly backstop. A follower's
claimed-leaf worktrees live in its replica checkout, which no owner process
touches, so the replica schedules its own GC on its host clock. That routine is
the only one a replica fires. Its ship sweep, task pilot, CI and Dependabot
sweeps and its auto-tasks stay owner work: `orbit routine list` shows them as
`owner-only` with the owner machine named, a toggle or pause is refused, and
the sweep reports them `skipped` with an `owner_only_in_replica:` reason. The
auto-task panel marks toggle and manual mint unavailable and names the owner;
replica task minting remains refused.

Enable the replica's GC on the follower as an operator, with
`orbit_routine_control` (`action: toggle`) or the dashboard's Operations
routines panel. You can also set `enabled: true` in the replica checkout's
`.orbit/routines/worktree_gc.yaml`. Then confirm it is armed:

```bash
orbit routine list --workspace <replica-workspace>
orbit clock status
```

GC reads historical tasks carrying this machine's prefix from the local store.
It asks the owner only for the owner's prefix, through the run's claim route or
the replica's registered workspace. Existing admissions identify the owner's
prefix; before any pull, GC uses a single foreign prefix in this workspace's
stored task ids. Multiple foreign prefixes without admissions are ambiguous.
Other or ambiguous prefixes stay `skipped:task_prefix_unroutable` without an
owner call, and missing tasks stay `skipped:task_unresolved`. GC reclaims a
worktree only when its claim was accepted and settled or every task is settled
in its authoritative store. A worktree
whose owner is unreachable or unrouted stays, reported as
`skipped:owner_unreachable` or `skipped:no_owner_route` in the run's `reap`
output.

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
orbit run readiness          # "Admissions throttled: memory 93% (throttled at ≥ 90% since …"
orbit run show <drain-run>   # Throttled: line from the drain's last pass
```

On the dashboard, the top bar's `load`, `mem` and `disk` chips show the
serving host's readings; the chip of each held resource is outlined, and its
tooltip gives the verdict, reason and sample age.

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
- CPU-light work keeps moving under CPU pressure. When CPU is the only held
  resource, a local drain still starts `no-diff-expected` auto-task leaves
  (after-landing review, friction curation, full review) until
  `workflow.resource_throttle.cpu_light_leaves` (default 2) of them are live
  leaves; every other leaf waits. Memory or disk pressure holds them too, and
  pull drains do not use the budget. Readiness marks these tasks `cpu-light`,
  prints `CPU-light budget: <active> of <reserved> reserved slots in use`,
  reports `cpu_light_budget_full` for a light task waiting on a spent budget,
  and carries the numbers in `capacity.cpu_light_budget`; the drain's pass
  output carries the same object.
- A task minted over a frozen delivery batch within two hours of the batch's
  admission deadline (`retry_until`, or an operator reissue's) sorts ahead of
  same-priority backlog, corrective work included; critical work still leads.
  Local dispatch and pull admission use the same owner-side expiry set and
  ordering. If deadlines cannot be read, both retain the ordinary order.
  Readiness names that deadline as `frozen-batch-deadline` (JSON
  `frozen_batch_deadline`). Raising such a task to critical is no longer
  needed to keep its batch from expiring behind ordinary work.
- Readings that cannot be taken (unavailable, invalid or stale) never
  throttle. They are listed as `resource_telemetry_unknown` and logged once.
- Each throttle and recovery is logged once under `orbit.core.host_resource`.
- To restore the previous behaviour, set
  `workflow.resource_throttle.enabled = false`; no pressure is then sampled
  for admission.

Followers must match the owner's pull request schema, independently of `orbit --version`.
The read-only probe reports `protocol_fingerprint`, a SHA-256 fingerprint of the JSON schema
derived from the running build's `AdmissionRequest` and all its nested types. The follower
first probes with legacy-compatible fields, checks that fingerprint and `protocol_schema`,
then declares `caller_fingerprint` on a second probe. A different or missing fingerprint,
including a legacy owner, refuses with typed `protocol_skew` before any `orbit.task.pull`.
A protocol identity containing `[REDACTED_ENV]` is a corrupted transport reply,
not evidence of schema skew. The follower treats it as typed `OwnerNegotiation`,
records a transient pass error and retries on the next pass. The owner also
checks redacted drain replies, including nested fingerprints, commit/tree IDs
and evidence hashes: read-only calls fail negotiation; mutating calls report
`OutcomeUnknown` so the follower reconciles or replays the same request.
Credentials remain scrubbed even when they overlap an identity. Known
`XDG_SESSION_*`, `DBUS_SESSION_BUS_ADDRESS`, `SESSION_MANAGER` and
`TERM_SESSION_ID` metadata is excluded from session-name matching; credential
words still take precedence, and other session names retain conservative
handling. Purely numeric environment values shorter than 12 digits are
excluded from substring substitution.
The integer revision remains for persisted requests and lifecycle semantics; request field
changes no longer depend on a manual bump. Deploy matching builds on both hosts and restart
long-lived processes.

`orbit run show <drain-run>` exposes a pull drain's latest pass error and consecutive failure
count. JSON carries `last_pass_error_code`, `last_pass_error`, `consecutive_pass_failures`, and `degraded` under
`pipeline_state.drain_last_pass`. Three consecutive failed passes latch a visible degraded
warning and stop new admissions for that drain. A successful pass before the threshold resets
the streak. Protocol skew immediately latches degradation and ends the drain **failed** with `protocol_skew`,
even with an open window. `orbit doctor` reports the latest skewed pull drain, and the dashboard
keeps its pass health and failure code visible after it ends. Its durable admissions and settlement
records remain available to leaf workers, the settle-only pass, and the clock sweep. Other
degraded drains keep retrying settlements and outlive their window until nothing
is unsettled; successful settlement does not clear the warning. Fix the reported cause, run
`orbit run auto --stop` to close the window, and start a new drain once this one ends. An unreadable or unwritable run-state record fails the activity visibly.

A pull drain also records what its owner kept off this host. When a request is answered idle,
the receipt's diagnostics fill `drain_last_pass` as a local drain's classifier does: `queued` is
the receipt's `queue_depth`; `deferred` lists footprint holds (`context_lock_conflict`, the holder
in `blocked_by`) and other owner holds (`owner_hold`); `excluded` lists unmet dependencies
(`dependency_not_done`, the unfinished tasks in `blocked_by`), `os:` waits (`host_os_mismatch`) and
unrunnable crews (`crew_unavailable`), both lists bounded to 20 with `deferred_total` and `excluded_total` the full counts; and
`waiting_by_reason` counts every kept-off task by code. `waiting_recorded_at` dates the owner's
answer. A pass that sends no request (throttled, settlement held, breaker open, window closed,
owner unreachable), or whose requests all claim, keeps the previous diagnostics and their date
rather than recording an empty backlog. `consecutive_idle_passes` counts the idle answers in a row
that found tasks waiting; from three, `orbit run show` and the dashboard add an `idle:` line
saying how many tasks were kept off this host and why. Both print the same `Still waiting` lines
for a pull drain as for a local one.

## Verification

On the owner:

```bash
orbit --version
orbit workspace show
orbit doctor
orbit config get review.before_pr
ORBIT_OPERATOR=1 orbit tool run orbit.drain.probe --input '{
  "caller_version": "<owner-version>",
  "caller_schema": 9,
  "caller_before_pr": false
}'
ORBIT_OPERATOR=1 orbit tool run orbit.drain.claims --input '{}'
curl -s -H 'Host: localhost:7878' http://localhost:7878/api/distributed/claims?workspace=<workspace-id>
```

From a follower session aimed at the owner selector, repeat the probe with
that follower's `orbit --version`. Confirm:

- versions and schema match;
- the probe admits (`admits: true`), and with the owner's `review.before_pr`
  on, the follower's drain lists the owner's review crew as runnable
  (`Crews:` in `orbit run show <drain-run>`);
- the probe created no task, reservation, or claim (`orbit task locks list`
  unchanged);
- `orbit job resume` of a known claimed leaf still refuses;
- no leftover callers file is treated as an ACL.

After starting a drain (step 8), confirm the first claim end to end: the
owner's `orbit.drain.claims` shows it `running` on the follower's machine,
the follower's `orbit run show <leaf-run-id>` shows the claimed PR leaf and its `Claim:` line, and
after handoff the owner task is in `review` with the PR attached and nothing
merged. With before-PR review on, the owner task also carries
`review-gate.json` and the reviewer's verdict comment, and the follower's
audit holds one brokered `orbit.task.artifact.get` and one
`orbit.task.artifact.put` for the leaf (the smoke procedure in the
[claimed-review artifacts runbook](./claimed-review-artifacts.md) reads
them).

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
