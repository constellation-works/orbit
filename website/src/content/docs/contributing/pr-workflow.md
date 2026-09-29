---
title: PR Workflow
description: "How to keep Orbit changes scoped, tested, and reviewable."
sidebar:
  order: 4
---

## Scope

Keep changes intentional. Avoid unrelated refactors. Update tests when behavior changes.

When a change touches an owned feature's implementation, update that feature's design docs in the same pull request. Flip affected ADR statuses, update the last-updated date, and add an ADR for non-obvious decisions.

## Checks

Run before you ask for review:

```bash
make ci-fast
make ci-lint
make goldens
```

After an intentional change to CLI help or the MCP surface, regenerate the
snapshots with `make goldens UPDATE=1` and review the diff. Use targeted checks
while iterating. The full `make ci` workflow runs on every pull request as the
canonical merge gate; you don't need to run it per change.

Task pull requests target `agent-main`, the development integration branch.

## Commits

Use clear commit messages. Agent-authored commits should use the agent commit identity (e.g. `claude`, `codex`) for that commit and should not leave the repository configured with that identity afterward.

When a commit is associated with an Orbit task, include its allocated task ID in
square brackets in the commit message.

When authoring tasks or design docs, identify yourself by agent family (`codex`,
`claude`, `gemini`, or `grok`), not by a full model string.
