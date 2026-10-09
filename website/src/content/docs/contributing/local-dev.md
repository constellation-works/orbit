---
title: Local Development
description: "Commands and expectations for working on Orbit locally."
sidebar:
  order: 2
---

## Setup

Prerequisites and the first build are in the repository's
[`CONTRIBUTING.md`](https://github.com/constellation-works/orbit/blob/main/CONTRIBUTING.md).

Run targeted checks while you iterate. Before you hand off a task, run the full
pre-review set. `make ci-fast` alone is not enough, because it runs no Rust
tests:

```bash
make ci-fast
make ci-test-affected
make ci-lint
make goldens
```

The full `make ci` is the merge gate and runs in CI on every pull request. For
what each gate covers, see [PR Workflow](../pr-workflow/#checks).

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
