# Setting Orbit up: verify and hand off

## Step 5 — Verify

```bash
orbit --version
orbit workspace show
orbit tool run orbit.task.list --input '{"workspace":"<workspace-id>"}'
orbit doctor
```

Run the `orbit.task.list` call before `orbit doctor`: registering a checkout does not create the
host's task and workspace databases, and a write-free `orbit doctor` on a host without them
prints only a `runtime` error row that asks for `orbit task list`.

`orbit doctor` is the real check — it inspects config, database, disk, indexes,
locks, and runs. Report its output. Distinguish failures affecting the requested workflow from optional capabilities
not configured. Resolve relevant failures or report the specific blocker; a
passing `--version` alone is not sufficient.

## Step 6 — Hand off

- **First real task** — route through [task-authoring-contract.md](../../orbit/references/task-authoring-contract.md).
  Create it when requested; setup alone does not authorize unrelated work.
- **Feature tour** — read the README's feature section and summarize against the
  user's stated goal, not generically.

## What to set up next

Offer only additions relevant to the user's goal; none is required merely because
initial setup succeeded:

1. **Crews and base branch** — register the workspace with `--base-branch`
   at the branch task PRs should target (`workflow.base_branch` is only the
   config fallback), and set a default crew. → [configuration-settings-workflow.md](configuration-settings-workflow.md)
2. **The scheduler** — host clock, then routines, in the documented order.
   → [automation-scheduler.md](automation-scheduler.md)
3. **Recurring chores** — QA sweeps, friction curation, anything periodic.
   → [auto-tasks-authoring.md](auto-tasks-authoring.md)
4. **Task publication** — bind a dedicated snapshot repository and verify one
   publish/inspect cycle before relying on recovery. → [publication-bind.md](publication-bind.md)
5. **Upgrade convergence** — use `orbit workspace sync --check`, then
   `orbit workspace sync` to refresh managed defaults. → [maintenance-upgrades.md](maintenance-upgrades.md)
6. **A replica execution host** — collapse to one owner, match binaries and
   protocol, make sure the follower can run the owner's captured reviewer from
   `workflow.purpose_crews.pre_landing_review` when non-empty, otherwise
   `workflow.purpose_crews.review`. Check this when the owner's
   `review.timing` enables before-PR or before-landing review, then probe.
   Installation is not pull enablement.
   → [distributed-drain-setup.md](../../orbit/references/setup/distributed-drain-setup.md)

## Anti-patterns

- Check the installed release and effective resource catalog when commands
  differ from these examples; do not rebuild Orbit just to access a task.
- Don't pick a task prefix casually on a second machine. It cannot be changed.
- Run `orbit init` before creating any task. Tasks created first (for example after
  `orbit workspace init` alone) mint `ORB-` ids, which fixes the prefix: a later
  `orbit init --task-prefix <PREFIX>` is refused before it writes anything.
- Don't enable every seeded routine at once. Enable worktree GC before, not
  after, scheduling ship traffic.
- Task search uses the local SQLite index; no model download is needed.
