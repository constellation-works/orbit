---
title: PR Workflow
description: "How to keep Orbit changes scoped, tested, and reviewable."
sidebar:
  order: 4
---

## Scope

Keep each change focused, with no unrelated refactors. Update tests when
behavior changes.

When a change touches a feature's implementation, update that feature's design
docs under `docs/design/<feature>/` in the same pull request. Refresh
`last_updated`. Add a titled entry to the feature's `4_decisions.md` only for a
decision that explains surprising code or governs future choices, and point a
replaced entry at its successor with a `Superseded by:` link.

## Checks

For a task that delivers a diff, run the checks the change needs: tests of the
behavior you changed, Clippy on touched crates, and goldens for changed golden
surfaces. The repository provides these broader targets when needed:

```bash
make ci-fast            # formatting and repository guardrails; no Rust tests
make ci-test-affected   # full test targets for changed crates and their dependents
make ci-lint            # dependency direction, then clippy and rustdoc with warnings as errors
make goldens            # CLI help, MCP, CI log, and sandbox profile goldens
```

`make ci-fast`, `make ci-lint`, and `make goldens` check formatting, guardrails,
lints, and snapshots, but they don't run the test suite. `make ci-test-affected`
runs the full test targets of every changed crate and its reverse workspace
dependents. These are focused tools, not blanket pre-handoff gates. Required
PR checks gate formatting, guardrails, lint, rustdoc, goldens, and the full
test suite. The repository's
[validation policy](https://github.com/constellation-works/orbit/blob/main/docs/DEVELOPMENT.md#validation-and-ci)
describes the checks required for each kind of change.

After an intentional change to CLI help, the MCP surface, or a sandbox policy,
regenerate the goldens with `make goldens UPDATE=1` and review the diff. Use
targeted checks while you iterate. In a managed delivery, the pipeline runs
the configured `workflow.required_validation_commands` on the final candidate.

Pull requests target `agent-main`, the development integration branch. `main`
is for releases.

## Commits

Write clear commit messages with a type prefix: `feat:`, `fix:`, `docs:`,
`refactor:`, or `chore:`. When a commit belongs to an Orbit task, include the
task's allocated ID in square brackets.

An agent-authored commit uses the agent's commit identity (for example `claude`
or `codex`) for that commit only. Don't leave the repository configured with
that identity afterward.

When you author tasks or design docs, identify yourself by agent family
(`codex`, `claude`, `gemini`, or `grok`), not by a full model string.
