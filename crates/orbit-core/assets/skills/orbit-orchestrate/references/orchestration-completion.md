# Carry an authorized delivery through completion

## Handoff discipline

Task state, run state, and the durable stores are the handoff — never agent
prose. An orchestrator that reads a summary paragraph instead of
`orbit.task.show` or `orbit run show` is guessing.

## Delivery and completion

By default the task pipelines end in `review`. PR mode prepares a source branch
and opens a PR, then stops with that PR unmerged unless `--complete` was
authorized. Local mode implements in an isolated worktree and fast-forwards the
configured local base branch before the task reaches `review`; the leaf job's
`auto_push` input controls its optional push for a run that ends in `review`
(a run under `--complete` always pushes). Review is not a pre-merge stop in
local mode.

Inspect the effective wrapper and child job inputs rather than
assuming local mode means the current checkout was edited or a remote branch was
updated.

Record validation, commit, branch/PR, and run evidence, then follow the user's
approval policy for completion. Task snapshot publication is independent of
source delivery and lifecycle.

`orbit run ship --complete` and `orbit run auto --complete` are the operator's
explicit authorization for one submitted run to finish delivery and take the
tasks it ships from `review` to `done`. Default-off, and never enabled by
workspace configuration, an environment variable, or a routine.

- Local mode completes only after the bundle merged *and* pushed; a failed
  publication leaves the task in `review`. The push merges commits other hosts
  published to the base since the run started (landed commits keep their shas and
  stay reachable from the pushed base); a remote that cannot be merged cleanly
  leaves the task in `review` with the merge failure as the reason.
- PR mode completes only after the PR is verified merged. Branch protections and
  required checks are respected and never bypassed; pending checks may use
  GitHub auto-merge, but enabling auto-merge is not success. A closed or blocked
  PR, a refused auto-merge, or an expired wait leaves the task in `review`.
- Completed `no-diff-expected` work completes without a PR. In owner or claimed
  mode, it runs only commands its own brief requires, with no workspace gates,
  required validation commands, or `no-diff.json` and workspace validation logs.
  No-op implementations without the tag still need validated no-diff evidence.
- `run auto --complete` is blanket authorization for every task the drain admits
  during its whole window, including work filed after it starts. Do not use it
  where the user authorized only the currently visible backlog.
- It authorizes delivery completion and `review -> done` only. It never approves
  `proposed` work into the backlog and is not an independent review verdict; the
  transition is recorded against the authorizing run and operator.
