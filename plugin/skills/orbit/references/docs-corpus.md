# Docs corpus

Agents retrieve through the authoritative `orbit_search` tool. The CLI admin
commands below run on the workspace owner; they are not all exposed over MCP.

Registration-light: Orbit walks the configured `[docs].roots` on demand and indexes every file with valid frontmatter. Legacy design and pattern docs get a tolerant fallback.

## Frontmatter

`type` and `summary` are required; `summary` must be a non-empty single line.

```yaml
---
type: design | pattern | context | glossary | runbook
summary: One-line hook for agent retrieval
tags: [hooks, audit]
paths: ["src/**"]
related_features: [hook-rewrite]
related_artifacts: ["<task-id>"]
---
```

Recommended layout, not enforced: `docs/design/<feature>/`, `docs/design-patterns/`, `docs/context/`, `docs/glossary.md`, `docs/runbooks/`.

## Admin commands

Agents retrieve through `orbit.search`; these are human and admin workflows.

| Verb | Form |
| --- | --- |
| List | `orbit docs list --json [--type <type>] [--tag <tag>]` |
| Show | `orbit docs show <path> --json` |
| Add root | `orbit docs add <path>` — existing, non-`.orbit/` roots only |
| Index | `orbit docs index --json` — after substantial edits or moves; idempotent via content hashes |
| Migrate | `orbit docs migrate` to preview, then `orbit docs migrate --confirm` to backfill locked frontmatter for legacy `docs/design/<feature>/*.md` and `docs/design-patterns/*.md`. Never touches `.orbit/`. |

## Semantic companion

`orbit semantic` manages the local embedding companion.

| Purpose | Form |
|---------|------|
| Install / remove | `orbit semantic install [--model M] [--force]` / `orbit semantic uninstall [--model M] [--all]` |
| Status | `orbit semantic stats` |
| Rebuild embeddings | `orbit semantic index --kind tasks\|docs\|all [--model M] [--force]` |

Supported on macOS arm64 and Linux x86_64/aarch64 with glibc ≥ 2.38. There is no x86_64-apple-darwin asset. Don't install without operator consent.
