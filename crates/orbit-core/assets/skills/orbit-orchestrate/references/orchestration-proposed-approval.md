# Authorize a drain to approve proposed work

## Approval admission and pilot decisions

`orbit run auto --approve-proposed` (MCP `orbit_workflow` action `auto` `start` with
`approve_proposed: true`) is the explicit authorization for one drain to approve
`proposed` work, independent of `--complete`. Default-off and never enabled by
configuration.

- When none of its pilots is running, a pass selects up to ten qualifying
  proposed tasks, including ones filed mid-window. A task qualifies with the
  `no-diff-expected` tag, or with non-empty `context_files` and an assessed
  complexity. A fresh assessment from an explicit request or a routine pilot
  is approved on that pass without another pilot.

  - A stale assessment whose certified description/criteria or other material drifted is selected again for a fresh pilot like a new proposal when no live execution claim holds it; a holding warning keeps the task proposed and is not selected again. A task another pilot is assessing is held as
    `pilot_running` until it finishes. A new explicit request still starts one
    pilot; the drain's targeted task ids are not that request.
- The drain pilots them through a detached `task_pilot_pipeline` run, and the
  pilot's apply step approves a task only under the existing promotion rules:
  no duplicate, already-landed, blocked-by, conflict or warning finding, and
  selectors that resolve at the pinned revision. The authority is verified
  against the drain that dispatched the pilot.
- Admission never waits for the pilot: eligible backlog tasks keep starting
  and freed slots keep refilling while it runs. Idle passes wait on the pilot,
  at most one poll interval, instead of sleeping. A task the running pilot
  carries is held as `pilot_running` and is not piloted again.
- An approved task gets the ordinary approve transition with a history note
  naming the drain run, and is admitted by the pass that sees the pilot
  finish. A pilot still running when the window closes holds the drain open
  until it finishes; what it approves then waits in the backlog for the next
  drain.
- A task tagged `no-auto-approve` is never approved by any automatic
  promotion authority (this drain or the CI sweep). A proposed task tagged
  `no-drain` also opts out of this drain's approval: it is not piloted and
  stays `proposed`, held with reason `no-drain`. Use `no-auto-approve` when
  every automatic approval authority must stop; use `no-drain` to keep work
  out of drains.
- An untagged task whose current pilot assessment is `verified_no_diff` is
  held as `pilot_verified_no_diff` with the pilot's evidence and cited
  commits. When Orbit automation filed it (`ci-failure-sweep`,
  `delivery-code-review` or `auto-task:*`), the pass archives it with a system
  comment only after every cited commit exists and at least one qualifying
  covering commit is on the base branch.

  - Human- and orchestrator-filed tasks
    are never closed this way; archive or re-scope them yourself. A task already
    tagged `no-diff-expected` does not regain this finding hold from a later
    `verified_no_diff` assessment.
- An eligible no-diff deliverable (such as a report or audit that files tasks)
  is tagged `no-diff-expected` atomically with its `verified_no_diff`
  assessment. The drain judges it with that tag, so this pass can approve it
  when no other promotion hold applies.

  - The tag is added only when the
    assessment cites no commit, has none of the `already_landed`, `duplicate_of`,
    `release_action_required` or `finding_invalid` findings, and the task is
    untagged and is not a review, QA, CI-sweep or host-computed comment-hygiene
    task. A comment-hygiene batch with no eligible candidates means no work, not
    a no-diff deliverable.

  Adding
  `no-auto-approve` stops automatic approval while keeping the
  `no-diff-expected` tag; removing that tag opts out of the no-diff disposition.
- Ordinary apply rejects a `selectors` assessment with empty
  `context_files_after` before writing anything. A host-computed held
  comment-hygiene batch is the separate exception: it requires empty selectors,
  the `selectors` disposition, then the host attaches `evidence_gaps` and a
  holding `utility_warnings` finding that keeps it held. It is not an eligible
  no-diff report.
- Everything else stays `proposed`. `orbit run show` and `orbit run readiness`
  report approved, closed and held counts with each hold reason, and
  `awaiting_reuse` lists fresh assessments a later pass approves. A task this
  drain held is not piloted again until it changes, except a stale assessment the drain repilots automatically as above.

  - A fresh explicit or
    routine assessment is approved without another pilot, and a stale one held for a live claim, a failed repilot, or a warning stays
    proposed until the task changes or an operator acts.
- `--approve-proposed` with `--pull` is refused before anything is submitted:
  a follower does not approve another host's tasks.
