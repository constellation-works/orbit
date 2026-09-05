---
title: Local Development
description: "Commands and expectations for working on Orbit locally."
sidebar:
  order: 2
---

## Setup

Run targeted checks while iterating. Before handing off a task, run the
repository's fast guardrail gate:

```bash
make ci-fast
```

The full `make ci` workflow is the canonical PR merge gate and runs in CI.

## Website

The website is independent of the Rust workspace.

```bash
cd website
npm install
npm run dev
npm run check
npm run build
```

Every page is authored by hand under `website/src/content/docs/`; nothing is
generated at build time. When you change CLI behavior, verify the affected page
against `orbit <command> --help` from a current build and update it in the same
pull request.

## Orbit State

Review `.orbit/` changes carefully before committing. Tracked asset changes are product changes. Mutable runtime artifacts are operational data unless the change is intentional.
