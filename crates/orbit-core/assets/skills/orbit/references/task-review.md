# Reviewing a delivered diff

Review someone else's delivered diff and surface issues in your review summary. Read-only: **never** use `orbit.task.update` to transition the reviewed task's lifecycle — approval requires the authorized operator or human decision, not the reviewing agent.

A review auto-task tagged `no-diff-expected` follows its own brief. In owner or
claimed mode, it runs no workspace gates or required validation commands and
attaches no `no-diff.json` or workspace validation logs. Run only commands the
brief requires, including reproducing a finding when requested; retain the
brief's evidence artifacts and execution summary. The build and test guidance
below applies to reviewing a delivered diff, not to these auto-tasks.

## Load context

`orbit.task.show` for `description`, `acceptance_criteria`, `plan`, and `execution_summary`. Inspect the diff and the changed files. Run the target repo's build and its relevant test commands, taken from that repo's own instructions and configuration. Search relevant title or description terms with `orbit.search` when prior task context is needed.

## Two stages, in order

**Stage 1 — spec compliance.** Does the change satisfy every acceptance criterion? Is anything missing, or added beyond scope? Are there interpretation gaps? If stage 1 fails, report those findings and stop; stage 2 is wasted effort on a change that has to come back anyway.

For a diff that changes something a user sees (dashboard, web pages, docs site, desktop app UI), stage 1 includes the visual evidence. List the task's artifacts with `orbit.task.show` (`field: "artifacts"`), fetch the screenshots with `orbit.task.artifact.get` and look at them against the criteria: each changed view, in every theme it supports, and at 375px when the layout is responsive ([task-execution.md](task-execution.md#screenshots-of-a-ui-change)). Check the PR's `## Screenshots` table as well: it should show the Before/After shots Orbit published. Treat a draft PR whose `## Screenshots` section carries the "Missing required section" note as missing evidence. Missing screenshots without a stated reason, such as no browser kit on the host, are missing evidence: record a spec-compliance finding. This applies whether or not the task is tagged `ui`.

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
