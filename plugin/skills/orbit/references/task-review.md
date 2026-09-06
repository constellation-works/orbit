# Reviewing a task

Review someone else's work and surface issues in your review summary. Read-only: **never** transition the reviewed task's lifecycle, and never call `orbit.task.approve` — approval requires the authorized operator or human decision, not the reviewing agent.

## Load context

`orbit.task.show` for `description`, `acceptance_criteria`, `plan`, and `execution_summary`. Inspect the diff and the changed files. Run the target repo's build and its relevant test commands, taken from that repo's own instructions and configuration. Optionally `orbit.search` with `semantic: "<task-id>"` for prior similar decisions.

## Two stages, in order

**Stage 1 — spec compliance.** Does the change satisfy every acceptance criterion? Is anything missing, or added beyond scope? Are there interpretation gaps? If stage 1 fails, report those findings and stop; stage 2 is wasted effort on a change that has to come back anyway.

**Stage 2 — quality**, only once stage 1 passes: maintainability, patterns, performance, test-coverage gaps, risks, edge cases, security.

## Record findings

One finding per distinct issue, citing `path:line` when location-specific:

```text
**[Spec compliance | Code quality | Nit] — short headline.**
Why this matters / what's wrong.
Suggested fix.
```

Skip stylistic nits when a blocking issue already stops the change.

Summarize in chat: finding count, which are blockers, and the verdict (approve / request changes).

## Meta-review

After recording findings, check whether they reveal a gap in an Orbit-authored instruction asset — an activity definition or a `SKILL.md`. It fires when two or more findings map to the same instruction gap, or when one finding is clearly a recurring class. When it fires, file a friction ([friction.md](friction.md)) *in addition to* the individual findings, never as a replacement. Skip it for a single nit, a style preference, or a one-off mistake with no link to instruction text.

Exit: all findings recorded with a clear verdict; no status transitions on the reviewed task; chat summary names blocker count and verdict.
