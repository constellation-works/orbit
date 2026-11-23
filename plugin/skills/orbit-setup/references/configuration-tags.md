# Configuration: tag instructions and review-skip tags

## Tag instructions

A task tag can select extra instructions for the activities that work on
it. Map the tag to committed instruction files, one per role:

```toml
[workflow.tag_instructions.migration]
implement = "docs/agents/migration-implement.md"
review = "docs/agents/migration-review.md"

[workflow.tag_instructions.ui]
review = false            # switch off the shipped reviewer default
```

The three roles are `implement` (`agent_implement`, which also runs QA
sweep tasks), `review` (`agent_review_repair` and
`review_reconciliation_review`) and `recovery` (`step_failure_recovery`,
`final_recovery` and `pr_conflict_recovery`). Map only the roles a tag
needs; an unmapped tag and role adds nothing.

Each value is a path to a file relative to the repository root, with `/`
separators, no `..` and no absolute path — or `false` to switch off the
shipped default for that tag and role. Tags match ignoring case.

Orbit ships defaults for three tags. `no-diff-expected` gives
implementers the read-only handoff rule. `skip-validation` gives
implementers the light-delivery rule: run no local validation while the
change stays inside `workflow.light_delivery_paths`. `ui` routes implementers
to a host with a verified browser kit and selects the optional PR screenshots
section; an empty section never blocks opening. Visual task artifact evidence
follows the workspace guide and actual diff, not the tag.

A mapped file
replaces the default for that tag and role.

The global file layers under the workspace file: a workspace entry
replaces the global one for the same tag and role and leaves the other
tags and roles. Edit `config.toml` directly; `orbit config set` does not
write these keys.

Instruction text comes only from the shipped defaults or from the mapped
file as committed at the delivery's base revision. Uncommitted worktree
edits never change the instructions a delivery runs under, and nothing
written on the task (its description, comments or tags) supplies text: a
tag only selects. Tags apply in sorted order, once each, after the
activity's own instructions. They add to those instructions and cannot
override them.

A mapped file the activity cannot use fails the activity by name before
its provider starts, and only for tasks carrying that tag:

- `tag_instruction_unreadable`: the file is missing at the base
  revision, is not UTF-8, or is empty.
- `tag_instruction_too_large`: the file is larger than 8 KiB.
- `tag_instructions_too_large`: one prompt's tag text would exceed
  16 KiB.

Verify what applied with `orbit run show`: each dispatch that received
tag instructions prints a `Tag instructions:` line, and the audit trail
records an `activity.tag_instructions_applied` event naming the step,
activity, task, and each instruction's tag, role, source, SHA-256 and
size.

## Review-skip tags

A workspace can map its own tag to a review skip for low-risk work that
touches paths outside `workflow.light_delivery_paths`. The workspace file
or the global file may carry the mapping, layered per tag like
`[workflow.tag_instructions]`:

```toml
[workflow.review_skip_tags.comment-hygiene]
paths = ["docs/**", "**/*.md"]
guard = "scripts/check-comment-only-diff.sh"
```

- `paths` lists the path globs a candidate carrying the tag must change
  only within (the `workflow.light_delivery_paths` glob syntax, with `!`
  excludes; `["**"]` allows every path). `guard` is an optional
  repository-relative executable, read from the delivery's base revision,
  that must exit 0 for the tag to skip review.
- Edit `config.toml` directly; `orbit config set` does not write these
  keys (they are unknown keys, unlisted by `orbit config keys`). The
  seeded template is not migrated into existing config files, so an
  existing install opts in by adding the tables to its `config.toml`.
- A mapped tag skips every agent review layer and nothing else:
  required validation, workspace gates and hosted checks still run. The
  built-in `skip-review` and `skip-validation` tags cannot be remapped.
- Only filing a task or an operator update may add a mapped tag; an
  agent's update that adds one is refused, while removal is always
  allowed.
- Verify with `orbit config show` (effective mappings under
  `Review skip tags:`, `review_skip_tags` in `--json`) and with
  `orbit task show`: each candidate's decision is recorded once in the
  task's history as `light_delivery_applied` or
  `light_delivery_ignored`, naming the tag and the failed condition.
