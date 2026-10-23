---
title: Local Development
description: "Commands and expectations for working on Orbit locally."
sidebar:
  order: 2
---

## Setup

Prerequisites and the first build are in the repository's
[`CONTRIBUTING.md`](https://github.com/constellation-works/orbit/blob/main/CONTRIBUTING.md).

Run checks for the behavior you changed while you iterate: focused tests,
Clippy on the crates you touched, and goldens when you change a golden surface.
These targets are available for broader checks when needed:

```bash
make ci-fast
make ci-test-affected
make ci-lint
make goldens
```

There is no blanket local pre-handoff gate for a task that delivers a diff.
Required PR checks gate formatting, guardrails, lint, goldens, and the full
test suite. `make ci-fast` runs no Rust tests. For the current validation
policy and what each target covers, see [PR Workflow](../pr-workflow/#checks).

## Website

The website is separate from the Rust workspace.

```bash
cd website
npm install
npm run dev
npm run check
npm run build
```

Docs pages under `website/src/content/docs/` are written by hand; none is
generated from CLI help. When you change CLI behavior, check the affected page
against `orbit <command> --help` from a current build and update it in the same
pull request.

## Orbit state

`.orbit/` is per-user workspace state. `orbit workspace init` adds it to
`.gitignore`, so it is never committed. Orbit seeds the shipped defaults
(activities, jobs, executors, policies, routines, auto-tasks, and skills) from
`crates/orbit-core/assets/`; change a default there, not in `.orbit/`.
