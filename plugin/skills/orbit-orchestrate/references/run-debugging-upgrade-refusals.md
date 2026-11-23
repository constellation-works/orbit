# Diagnose agent steps refused after an upgrade

## Contents

- [Upgrade-refused agent steps](#upgrade-refused-agent-steps)
  - [Placement](#placement)

## Upgrade-refused agent steps

An Orbit upgrade can refuse an agent's own `orbit` commands while a step runs. A
command inside an Orbit-managed activity never starts or waits on a generation
switch, so while an upgrade holds admission the command fails at once with
`[upgrade_pending]`, and the agent is expected to report a blocker.

A blocker
whose kind names that refusal is classified as an upgrade refusal: its words
include `upgrade` with `admission`, `pending` or `quiesce`, or `generation` with
`switch` (for example `upgrade_pending`, `upgrade_admission_refused` or
`orbit.generation_switch_pending`). This applies to any agent step, reviewers
included, and step recovery can declare the same blocker. A malformed blocker (no
well-formed kind, or empty evidence) is an ordinary result, not a stop.

Recognise the outcome in `orbit run show <run_id> --json`:

- On an owner run, `run.state` is `failed`. It is not `held`, which is the
  `task_blocked_by_agent` stop, and not `interrupted`, which is the
  `upgrade_quiesce` pause.
- Match the refusal on its failure text, not on an error code. The text starts
  `[upgrade_pending] kind=<kind>` on its first line, and the agent's evidence
  follows on the lines after it. `run.error_message` carries that text, and so does
  the `error_message` of the failing `steps` entry.
- `steps_source` says where the top-level `steps` came from. A run whose record
  holds only the job-level failure, the usual v2 case, reads `audit` when its audit
  trail has step entries: those are rebuilt from the audit trail, so their
  `error_code` is always null. Find the failing step's marker in `error_message`
  instead. When the audit trail has no step entries, the record's own job-level
  entries are shown and `steps_source` reads `record`.

  - A `record` source keeps each
    entry's stored `error_code`, which may also be null.
- On an audit-sourced step, `error_code` and `agent_response_json` are always null.
  `exit_code` is filled only when that step's checkpoint in the run's pipeline state
  is a command result, so an earlier command step in the same run can carry its exit
  code while the refused agent step keeps it null. `duration_ms` is computed from
  `started_at` and `finished_at` when the audit recorded both.

  - For an unfinished step
    of a terminal run, `state` takes the run's terminal state and `outcome` reads
    `interrupted`; a finished step with no recorded outcome reads `finished`. `outcome`
    is null only for an unfinished step of a run that is not terminal, whose `state`
    then reads `running`. `started_at`, `finished_at` and `lock_wait_ms` are null when
    the audit recorded no value. `error_diagnostic` is the audit's structured value when
    it recorded one.

  - When it did not, and `error_message` carries a legacy
    worktree-integrity message (the text `worktree integrity violation`, a backticked
    failure code, then a JSON object), `error_diagnostic` is that parsed object,
    redacted. Otherwise it is null. A legacy message also replaces `error_message` with
    a readable message for its code and reason, even when the structured value is
    present.
- Fields that may be null: `run.error_code` is `upgrade_pending` only when the run
  record kept that code, and is otherwise null. `root_cause` is null because the
  owner run failed on its own.
- The run is not reported as `upgrade_paused`, so `--failures` and the reliability
  counts treat it as a failure.

What happens to the step and the task:

- The step is not retried. Neither step recovery nor final recovery runs, and the
  candidate is kept.
- A claimed leaf releases its claim as `transient`, with the candidate on the
  release. A claimed PR leaf that already published its head reuses it, and an
  unchanged published head needs no extra ref; a tip past it, or an unpublished
  tip, is first pushed to a durable ref. An ordinary `upgrade_pending` refusal
  carries neither `provider_limit` nor `owner_busy`, so the owner counts it
  against the task's **2-in-24h release budget**:
  - Within budget (fewer than 2 prior budgeted releases in 24h): the claim ends in
    phase `revoked` (`last_event` `claim_released`), the task returns to the
    owner's `backlog`, and its next claim resumes the kept candidate under the
    [placement rule](#placement). The leaf's crew sits out the rest of that drain's
    window.
  - Budget exhausted (3rd+ budgeted release within 24h): the owner fails the
    claim and moves the task to `blocked` instead of `backlog`, still keeping any
    committed candidate in `claim_candidate_kept`.

    | Owner evidence |
    |---|
    | Identify this state on the owner via `orbit task show <task-id>` (`orbit.task.show`) and `orbit run settlements --json`: `status` is `blocked`; task history has a `claim_release_budget_exhausted` event whose `note` starts `release budget exhausted (2 in 24h): …`; a system comment starting `Blocked: follower drains released this claim … times within 24h for failures that were not the candidate's, and it failed again.` lists every counted release's timestamp, failure class and reason; and on the settlement/claim, `phase` is `failed` and `last_event` is `claim_release_budget_exhausted` (`release.budget_exhausted: true` on the stored claim record). |

    No next claim
    can start until an operator returns the task to `backlog`.
- An owner `task_pr_pipeline` run spends no claim release budget and never sets
  the task to `blocked`: it commits what the agent left, pushes it to its durable
  ref, `refs/orbit/candidates/<task-id>/<run-id>`, on `origin`, and opens no pull
  request. Run finalization returns the task to the backlog with the history event
  `upgrade_pending_requeued`.
- That push can be refused. The hold still succeeds and the candidate stays on
  the holding host. The evidence depends on who held it:
  - Owner run: the task's `candidate_held` history entry. A host-local candidate
    has no `durable_ref` in its `decision`, `carry_failure` holds the push
    diagnostic, and the note reads `host-local: …`. The `## Orbit upgrade`
    comment says the same in words, and names the holding machine when the host
    has an identity. A pushed candidate instead reads `durable at …` and carries
    `durable_ref`.
  - Claimed leaf: the owner's `claim_candidate_kept` history entry, written when
    the release settles. It has no `candidate_held` entry and no `## Orbit
    upgrade` comment, so their absence does not mean the candidate is missing.
    Its note reads `durable at <ref>` when pushed; names the pull request with
    neither `durable at` nor `only on` when the leaf's published head was reused;
    and reads `only on <machine> (pushing it failed: <diagnostic>)` when the
    carry was rejected.

    - A claimed PR leaf stopped before its normal commit step (the
      pipeline commits before it prepares or synchronizes the branch) has one of three
      outcomes. The leaf's `claim_candidate_carry` failure-hook output tells them apart;
      owner history does not, because only the first writes an entry:
    - Preservable edits (uncommitted changes in its worktree): `carry: durable` (or
      `failed` when the push was rejected) with `partial: true`. The head is a
      `Partial implementation candidate [<task-id>]` commit, carrying an
      `Orbit-Partial-Candidate-Run: <run-id>` trailer; it moves no branch, index or
      file. The note reads `durable at` or `only on` (the placement), then
      `stopped at <step>`.
    - Refused edits (a protected or non-regular path, or unmerged changes): no
      partial commit is made, so the edits are not preserved. The hook reports
      `carry: failed` with a `reason` and no branch or head. Three refusals apply,
      and the `reason` names which one stopped it. Unmerged edits give `task
      worktree '<path>' has unresolved merge conflicts`, and that check runs before
      the path checks.

      - An untracked protected path (`??` in `git status`) is refused
        first, whatever its file type, while the candidate paths are listed: the reason
        contains `task delivery refused protected untracked paths: […]`. Staging that
        path removes it from that listing, so the next refusal applies instead. Other
        protected or non-regular paths (staged or modified, or irregular) give
        `partial candidate refused protected or non-regular paths: […]`.

      - All three
        refusals report no branch or head and write no `claim_candidate_kept` entry.
        The reason also reaches the owner in the claim's release failure.
    - No edits: `carry: none` with the reason `the leaf left no uncommitted
      candidate`, and no branch or head.
    - Refused and empty snapshots give the release no head to keep, and no
      synchronized or prepared head exists yet, so neither writes a
      `claim_candidate_kept` entry. Read the carry output, not the absence of
      history. A kept entry from a pre-commit stop always names a `Partial
      implementation candidate` head.
    - A claimed-local leaf (`task_claimed_local_pipeline`) has no pre-commit hook
      and keeps a candidate only from its own commit, so a stop before that commit
      records no entry.

### Placement

- Placement decides where the next run can resume it:
  - Durable: a run or claim on any host resumes the candidate from `origin`,
    once the upgrade settles.
  - Host-local: only a run on the holding host resumes it, and only while that
    host's repository still has the commit. A claim on any other host is offered
    no candidate (`not_durable`) and implements the task afresh. Pushing the
    commit to `origin` by hand does not change this, because placement reads the
    durable ref recorded in the history entry.
- Owner requeues (`upgrade_pending_requeued`) and in-budget claimed releases
  (`claim_released`) never set the task to `blocked`, so an operator does not move
  them back as they must after a `task_blocked_by_agent` stop — check that the
  upgrade has settled and that the next run or claim picks the candidate up.

  - Only
    a claimed release that exhausts the 2-in-24h release budget sets the task to
    `blocked` (`claim_release_budget_exhausted`, claim `phase: failed`);
    after confirming the upgrade has settled, an operator must move that task back
    to `backlog` before a next claim can start. For a host-local candidate, expect a
    resume only on the holding host; anywhere else the next run starts afresh.

Three outcomes can look alike. The state and the code tell them apart:

- `task_blocked_by_agent` ([recovery evidence](run-debugging-recovery-evidence.md#inspect-recovery-evidence-before-falling-back-to-audit-files)):
  the run is `held`, and the task is blocked until an operator moves it back.
- `upgrade_quiesce` ([Check Parent And Child Runs](run-debugging-history.md#check-parent-and-child-runs)):
  a worker yields at a step boundary, its checkpoint is kept, and the run is
  `interrupted`. The clock resumes it once the generation settles.
- `upgrade_pending` (this section): the step stops mid-run, the run is `failed`,
  and the task returns to the backlog without an operator action on owner runs
  (`upgrade_pending_requeued`) and in-budget claimed releases (`claim_released`),
  or moves to `blocked` (`claim_release_budget_exhausted`) once a claimed release
  exhausts its 2-in-24h budget.

To avoid stopping a step at all, let long agent steps finish before the installed
`orbit` is replaced, or install while no drain is live. A running step keeps going
after the binary is replaced, and while the upgrade holds admission its `orbit`
commands are refused.
