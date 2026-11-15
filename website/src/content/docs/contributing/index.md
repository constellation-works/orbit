---
title: Contributing
description: "Where to find Orbit's contributor guides, and the few rules every pull request follows."
---

Orbit is developed at
[constellation-works/orbit](https://github.com/constellation-works/orbit). The
contributor guides live in the repository, next to the code they describe:

| Guide | Covers |
|---|---|
| [`CONTRIBUTING.md`](https://github.com/constellation-works/orbit/blob/main/CONTRIBUTING.md) | Prerequisites, the first build, and how to propose a change. |
| [`docs/DEVELOPMENT.md`](https://github.com/constellation-works/orbit/blob/main/docs/DEVELOPMENT.md) | Local commands, validation and CI, and which checks a change needs. |
| [`ARCHITECTURE.md`](https://github.com/constellation-works/orbit/blob/main/ARCHITECTURE.md) | What each crate owns and which way dependencies point. |
| [`website/README.md`](https://github.com/constellation-works/orbit/blob/main/website/README.md) | Building and editing this site. |

## Pull requests

- Target `agent-main`, the development branch. `main` is for releases.
- Keep each change focused, and update the affected docs in the same pull
  request.
- Start the commit message with a type: `feat:`, `fix:`, `docs:`, `refactor:`
  or `chore:`.
- When you change CLI behaviour, check the affected page on this site against
  `orbit <command> --help` from your build.

## Report a problem

Open an issue on
[GitHub](https://github.com/constellation-works/orbit/issues). To report a
vulnerability, follow
[`SECURITY.md`](https://github.com/constellation-works/orbit/blob/main/SECURITY.md).
