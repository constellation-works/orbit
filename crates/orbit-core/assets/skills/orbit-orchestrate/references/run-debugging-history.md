# Follow parent and child runs in history

## Check Parent And Child Runs

Parent gate/auto runs can fail because a child failed, and children can keep working after a parent reports a gate failure:

```bash
orbit run history --json
orbit run show <run_id> --json
orbit run events <run_id> --json | rg '<run_id>|<task_id>'
```

Look for `input.task_ids` overlap between candidate runs, parent events that invoke/wait on another `jrun-*`, child run ids named in gate/auto/`invoke_and_wait` step output, and parent runs still `pending`/`running` after a child failed. Report the run owning the first real failure as primary, then name parent/child fallout separately.

A gate or auto run that failed only because its child did records that leaf in its `--json` `root_cause` (`leaf_run_id`, `task_id`, `step`, `code`; null on a run that failed on its own), and `orbit run history --state failed --incidents` shows one row per leaf with the folded parents in `cascaded_run_ids`.

`orbit run history --failures` shows one row per task and root cause instead: the root-cause run, its step and crew, the cause's first line in full, and `orbit run show` for that run (`--json`: `failures`). It cannot be combined with `--incidents`. On a drain, `orbit run show` names that same root-cause run in the failed-leaf warning, states each task's current status, and omits the `orbit job resume` line once the task has left `blocked`.

A red base or a before-landing review awaiting a decision ends the run `held`, and a cancelled child ends its waiting parents `cancelled`; neither is a failure. A claimed leaf stopped by its provider's usage limit also ends `held`: its step code is `provider_limit`, its message names the provider and the reset, and the claim returns to the owner. A CI sweep whose only gaps are run-scoped retryable collection errors succeeds with outcome `deferred` and a `deferred_count`.

Neither lists under `--state failed`; Incidents lists both under the `deferred` class.

A run an Orbit upgrade stopped at a step boundary ends `interrupted` with step code `upgrade_quiesce`, and the clock resumes it once the generation settles.

That is a designed pause, not a failure: `--failures` and `--incidents` leave it and any parent echoing it out of their rows and count them in a top-level `upgrade_paused`, present only when nonzero (one per root run, also printed as the `Upgrade-paused, not failures:` line), a plain history row carries `upgrade_paused: true`, and the reliability counts report it as `upgrade_paused` outside the failure rate.

A claimed leaf's settlement reports `accounting.outcome: upgrade_paused` with reason `upgrade_quiesce`.

A run stopped any other way, such as `process_not_found` for a lost or killed worker, stays a failure, except a recorded host-manager stop: when the supervisor's recorded worker exit shows `systemd` sent the terminating signal with no memory-limit (`oom_kills`) or task-limit (`tasks_refused`) breach, reconciliation settles the orphaned run as `held` with step code `operator_stopped`, and `--failures`, `--incidents`, and the reliability counts leave it out of failures.

An unknown or unreadable sender, any other sender (including `orbit`), an exit no signal ended, and a `systemd` signal after a memory- or task-limit breach stay `interrupted` failures, and `operator_stopped` never counts in a failure query even when an earlier writer left the run in a failure state.

Classification reads only the exit row already recorded: reconciliation that wins before the supervisor records it still writes `interrupted` / `worker_terminated`, and the later exit note cannot change the terminal state. A run that already held an error when the upgrade stopped it keeps that error as its cause and still counts as failed.

`orbit run history --task <id>` lists runs whose `input.task_ids` array
contains the task or whose top-level `input.task_id` matches it.
`orbit run history --json` prints `{"runs":[<job-run>]}`; `--failures` prints
`{"failures":[{task_id, run_id, step, crew, cause, show}]}` instead; both it and `--incidents` add `upgrade_paused` when an upgrade paused any listed run.

ROLE says
how a run was submitted: top-level directly, child by a parent run; run ids
minted before role markers existed read as unmarked. `--incidents` folds every
listed run that echoes the same leaf into one row, the leaf's own when it is
listed: CASCADED counts the folded runs and `.cascaded_run_ids` names them.

`--failures` folds the same runs into one row per task and root cause: RUN_ID,
STEP and CREW are the root-cause run's, and CAUSE is that run's error on one
line without the plain ERROR column's 60-character cap. `--no-reconcile`
reports stored records as-is; without it, stale-run reconciliation finalizes
an orphaned pending or running run as interrupted, releases its task
reservations, and settles with their owner the pull claims an ended drain or
leaf left.

## Settlement hydration

Hydrate settled claims that have no leaf-run evidence. In `orbit run settlements --no-reconcile --json`, that is a claim whose `run_evidence.provenance` is null (no evidence row exists, so its `build_state` and `root_cause_state` are both `unrecorded` and say nothing about the run). Default hydration selects claims with missing evidence after applying the supplied filters; selection and pagination do not depend on a successful follower read. A selected claim whose run is absent or whose follower is unreachable still consumes a page slot and returns `absent` or `unreachable`. `--recheck` also selects claims that already hold evidence.

A claim whose `provenance` is `settlement` or `hydrated` already holds its evidence. There, a `build_state` or `root_cause_state` of `unrecorded` means the leaf's run record held no build or failing step. That state is final and is not a default hydration target: hydration never fills it. With `--recheck`, held evidence remains immutable: a claim with no leaf run can return `no_leaf_run`, follower lookup can return `absent` or `unreachable`, and claim/run validation can return `mismatch`. Only after a valid observation passes those checks can an identical report return `replayed` or a difference return `conflict`. Settled claims can be listed without operator capability: `orbit run settlements --since 7d --no-reconcile --json`.

Authority: an operator on the owning host runs `orbit run settlements-hydrate`. It is operator-only and bounded; an agent session, a managed scan or a claimed worker is refused. It writes only immutable `hydrated` evidence and changes no claim, task or run.

```bash
orbit run settlements-hydrate --follower-workspace <workspace> --task <task-id> --json
```

`orbit run settlements-hydrate --help` lists every filter (`--claim`, `--task`, `--machine`, `--since`), the paging options and `--recheck`. The outcomes, claim matching and JSON shape are in [the owner surface reference](../../orbit/references/setup/distributed-drain-owner-surface.md#read-only-owner-surface).

Verify the result:

- Read each row's `outcome`. `hydrated` means the evidence is stored now. `replayed` means the same evidence was already held (shown with `--recheck`). `conflict` means different evidence is held and stands, so do not overwrite it. `mismatch` means the follower's run is not the claim's bound terminal claimed-leaf job, or the leaf is still live; hydrate after it ends. `absent` means the follower holds no such run, and legacy evidence stays `unrecorded`. `unreachable` means ask again later. `no_leaf_run` means the claim has no leaf run to read.
- Page through the claims. While `remaining` is nonzero, repeat the command with `--after <next_after_claim_id>` and the same filters.
- Reread `orbit run settlements --no-reconcile --json`. A hydrated claim's `run_evidence.provenance` reads `hydrated`. A field that stays `unrecorded` is final: do not fill it from the owner's record, a current build or the settlement text.

Behind the command, hydration reads follower runs through the operator-only `orbit.run` action `observe` route, which reads stored records without reconciliation or mutation. The follower opens that route without stale-run reconciliation, including discovery that precedes the read. A nonterminal leaf is a mismatch. Ordinary run inspection is an operator surface: `orbit.run` action `show` requires operator capability and refuses agent sessions.

A shell session on the owning host can use `orbit run show <RUN_ID>`; the MCP route requires operator capability. Both reconcile on open. Followers must support the observe route; older followers cannot be hydrated safely.
