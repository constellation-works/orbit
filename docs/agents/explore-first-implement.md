- This task is tagged `explore-first`: before your first edit, delegate
  footprint discovery with at least one `orbit.agent.invoke` call (a `cwd`
  inside your worktree and a unique `idempotency_key`). Ask these two
  questions, preferably as two concurrent calls:
  1. "List every file this task's change will create, modify or delete:
     callers, registrations and dispatch sites, module re-exports,
     generated companions and docs that describe the behavior. Name the
     evidence for each."
  2. "Which tests and goldens cover those paths?"
- While the explorer runs, keep reading on your own; don't wait idle.
- Delegation stays read-only: the explorer only reads, never edits,
  validates, commits or dispatches. Its answer is a lead: read what you
  rely on yourself before you change or cite it.
- Compare the explorer's answer with the task's `context_files`. When the
  change needs paths outside it, widen the selectors with
  `orbit.task.update`.
- Use `orbit.agent.invoke` as the explorer route for this trial, not your
  provider's own subagents: the trial measures `orbit.agent.invoke` runs.
- When a call is refused (budget, or delegation unavailable on this host),
  say so in the execution summary and continue without it.
- In the execution summary, say which explorer findings you used and which
  turned out wrong.
