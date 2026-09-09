---
title: "Orbit Docs — Overview"
owner: claude
last_updated: 2026-09-09
status: Draft
feature: orbit-docs
doc_role: overview
type: design
summary: "Orbit Docs — the human-authored workspace corpus and how operators and agents retrieve from it."
tags: [orbit-docs]
related_features: [orbit-docs]
related_artifacts: [ORB-00163, ORB-00206, ORB-10319]
last_validated: 2026-09-09
---

# Orbit Docs — Overview

> **Historical comparison:** learning-specific comparisons in this document
> describe the retired native subsystem. [ORB-10736] / [Remove the native project-learning subsystem](../project-learnings/4_decisions.md#remove-the-native-project-learning-subsystem) remove that
> resource and its tool, storage, and delivery contracts; those passages are
> non-normative.

Orbit Docs is the human-authored knowledge corpus for an Orbit workspace. It indexes the Markdown a team writes for itself — design narratives, reusable code patterns, runbooks, glossaries — and exposes CLI/admin verbs under `orbit docs` plus agent retrieval through the unified `orbit.search` MCP tool. It deliberately does not own a tool-managed copy of the corpus: docs remain PR-reviewed files under configurable `[docs].roots` entries.

The system is **pull-first**: agents call `orbit search --kind doc` (or `--kind all`
for federated task, doc, and friction results) or `orbit docs show` when they need
context. Task-time related-doc injection is available through `task show --with-context`;
PreToolUse hook surfaces remain downstream work ([ORB-00166], [ORB-00167]).

Phase 1 ships the corpus, the locked frontmatter schema, the six-verb surface, the `orbit-docs` skill, doc-corpus embeddings via `orbit docs index`, and a one-shot migrator that backfills legacy `docs/design/<feature>/` and `docs/design-patterns/` files. [2_design.md](./2_design.md) specifies the schema, walker, surface, tolerant indexer, and hybrid search path; [3_vision.md](./3_vision.md) names open questions and the remaining roadmap; [4_decisions.md](./4_decisions.md) is the decision log.

---

## 1. Motivation

Three concrete gaps existed before [ORB-00163]:

1. **Learnings cover load-bearing micro-rules — not explanatory context.** Learnings are scope-globbed, supersedable, and CRUD'd through `orbit.learning.*`. They were designed to carry *rules with known failure modes*, not multi-page design narratives. Stretching them to cover designs would distort the data model. See [docs/design/project-learnings/](../project-learnings/) for the learning shape.
2. **Design docs were over-enforced.** The older `orbit-design` skill enforced a strict
   four-file layout and freshness rule. Those constraints are too opinionated for a
   framework-layer tool that should compose with team conventions. Retiring that skill
   was tracked in [ORB-00165].
3. **Other knowledge categories had no indexed surface.** Reusable code patterns (`docs/design-patterns/`), operational runbooks, business/domain context, glossaries — all existed in the repo but had no retrieval primitive. Agents could grep, but they had no way to ask "what's the documented shape for crate-boundary error translation?" without already knowing the file path.

The hard constraint that shaped the design: **the corpus has to be tolerant.** Existing `docs/design/<feature>/*.md` and `docs/design-patterns/*.md` files have no frontmatter, and we will not force a flag-day migration. The indexer infers `type` and `summary` from directory and filename heuristics when frontmatter is absent, so day-one retrieval works without any author effort. The `migrate` verb provides the optional one-shot backfill.

A second constraint: **no enforcement.** Orbit Docs does not require the 4-numbered layout, the `Last updated:` line, or any specific section structure. It indexes whatever is under configured roots with valid frontmatter (strict mode) or any Markdown at all (tolerant mode). Team conventions stay where they belong — in `docs/design/CONVENTIONS.md` if the team writes one — and Orbit Docs neither enforces nor contradicts them.

---

## 2. Core Concepts

### 2.1 Doc

A `.md` file under a configured `[docs].roots` path, with optional locked frontmatter. The body is the Markdown after the frontmatter block. Docs have no Orbit-allocated ID; they are referenced by repo-relative path.

### 2.2 Frontmatter (locked)

Six fields, two required, four optional:

| Field | Required | Shape | Purpose |
|-------|----------|-------|---------|
| `type` | yes | enum: `design \| pattern \| context \| glossary \| runbook` | Coarse classifier for filtering. |
| `summary` | yes | non-empty single line | One-line retrieval hook; what the doc is about. |
| `tags` | no | string list | Free-form labels; used by `orbit docs list --tag`. |
| `paths` | no | glob string list | File-scope patterns this doc applies to (e.g. `crates/orbit-cli/**`). Used by task-context matching and planned hook-time injection. |
| `related_features` | no | string list | Feature slugs this doc covers; join key with normalized task tags used as feature selectors. |
| `related_artifacts` | no | string list | Cross-references to other Orbit artifacts via [ID-prefix dispatch for orbit-docs `related_artifacts`](./4_decisions.md#id-prefix-dispatch-for-orbit-docs-relatedartifacts) ID-prefix dispatch. |

Schema rationale and the closed-by-default choice: [Locked orbit-docs frontmatter schema](./4_decisions.md#locked-orbit-docs-frontmatter-schema). Why ID-prefix dispatch over object-shape references: [ID-prefix dispatch for orbit-docs `related_artifacts`](./4_decisions.md#id-prefix-dispatch-for-orbit-docs-relatedartifacts).

### 2.3 Tolerant indexer

Files without frontmatter, or with malformed frontmatter, are not silently dropped. The walker falls back to:

- `type`: inferred from directory (`docs/design/<feature>/` → `design`, `docs/design-patterns/` → `pattern`, dir containing `runbooks` → `runbook`, filename or dir matching `glossary` → `glossary`, otherwise `context`).
- `summary`: the first non-empty non-frontmatter Markdown line after stripping `#` heading markers; falls back to a titleized filename stem.
- `tags`: feature slug for design docs (e.g. `tags: [activity-job]` for `docs/design/activity-job/...`); empty otherwise.

Strict parsing still applies if you opt in via the `migrate` verb or by writing frontmatter manually. Tolerant fallback exists so the corpus is queryable on day one without any flag-day work.

### 2.4 Six-verb surface

| Verb | Purpose |
|------|---------|
| `orbit docs list` | Walk configured roots; return all records (with optional `--type` and `--tag` filters). |
| `orbit docs show <path>` | Render one doc with parsed frontmatter and body. |
| `orbit search --kind doc <query>` | Ranked matches against `summary`, `tags`, and `type`. Add `--hybrid` to blend lexical doc scoring with doc embeddings from `orbit docs index`. |
| `orbit docs add <path>` | Append `<path>` to `[docs].roots`. Idempotent. Refuses `.orbit/` paths and non-existent paths. |
| `orbit docs index` | Walk configured roots, embed doc fields into `.orbit/state/semantic.db`, and sweep stale doc rows. |
| `orbit docs migrate [--dry-run]` | One-shot frontmatter backfill for legacy `docs/design/<feature>/*.md` and `docs/design-patterns/*.md`. Idempotent. Never touches `.orbit/`. |

The five domain tool definitions (`orbit.docs.list`, `show`, `add`, `index`, and `migrate`)
remain available to CLI/admin runtime dispatch but are intentionally inactive in
`orbit-tools` for MCP advertisement. Agents retrieve docs through `orbit.search` with
`kind: "doc"`. `orbit-mcp` assembles the canonical active definitions, the CLI MCP server
advertises and routes them, and Core owns the runtime implementations and audit boundary.

### 2.5 The `.orbit/` exclusion

The walker explicitly skips any path under `.orbit/`, even if a configured root accidentally points above it. Decision narratives now live in each feature's `4_decisions.md` and are indexed like other human-authored docs; the retired store and its separate query surface no longer sit behind the exclusion. The locating principle behind this boundary remains [`.orbit/` for tool-managed artifacts; `docs/` for human-authored content](./4_decisions.md#orbit-for-tool-managed-artifacts-docs-for-human-authored-content).

### 2.6 Historical learning comparison

Older Orbit versions contrasted docs with a native project-learning subsystem. That
subsystem and its CRUD/injection path are retired. The current corpus is the human-authored
Markdown under configured docs roots; durable guidance should live there or in the owning
repository's ordinary agent instructions.

---

## 3. At a Glance

| Concern | File / surface | Task |
|---------|----------------|------|
| Frontmatter parsing, tolerant fallback, walker | [crates/orbit-core/src/application/docs/](../../../crates/orbit-core/src/application/docs/) | [ORB-00163] |
| CLI verbs (`orbit docs list/show/add/index/migrate`) | [crates/orbit-cli/src/command/docs.rs](../../../crates/orbit-cli/src/command/docs.rs) | [ORB-00163] |
| Generic doc tool schemas + inactive agent policy | [crates/orbit-tools/src/builtin/orbit/docs.rs](../../../crates/orbit-tools/src/builtin/orbit/docs.rs), [crates/orbit-tools/src/builtin/orbit/mod.rs](../../../crates/orbit-tools/src/builtin/orbit/mod.rs) | [ORB-00163], [ORB-10319] |
| Agent MCP exposure and routing (`orbit.search`, `kind: "doc"`) | [crates/orbit-mcp/src/remote/surface.rs](../../../crates/orbit-mcp/src/remote/surface.rs), [crates/orbit-cli/src/command/mcp/server.rs](../../../crates/orbit-cli/src/command/mcp/server.rs) | [ORB-00202], [ORB-10319] |
| Tool host dispatch | [crates/orbit-core/src/adapter/tool_host/docs_tools.rs](../../../crates/orbit-core/src/adapter/tool_host/docs_tools.rs) | [ORB-00163] |
| Skill (agent-facing entry point) | [crates/orbit-core/assets/skills/orbit/SKILL.md](../../../crates/orbit-core/assets/skills/orbit/SKILL.md) | [ORB-00163] |
| Config root | `[docs].roots` in [.orbit/config.toml](../../../.orbit/config.toml) | [ORB-00163] |
| Backfill migrator | `orbit docs migrate` | [ORB-00163] |
| Internal hardening (real diff, robust YAML edit, batched gitignore) | [crates/orbit-core/src/application/docs/](../../../crates/orbit-core/src/application/docs/) | [ORB-00164] |
| Retire `orbit-design` skill | Retired; no current asset | [ORB-00165] |
| Inject into `task show --with-context` | [crates/orbit-cli/src/command/task/](../../../crates/orbit-cli/src/command/task/) | [ORB-00166] (shipped) |
| Extend PreToolUse hook to surface docs | Not implemented; no current code owner | [ORB-00167] |
| Doc semantic embeddings and hybrid ranker | [crates/orbit-core/src/application/semantic.rs](../../../crates/orbit-core/src/application/semantic.rs) | [ORB-00206] (shipped) |

---

## Task References

- [ORB-00163] — Introduce `orbit docs` indexed knowledge base and `orbit-docs` skill (shipped)
- [ORB-00164] — Harden orbit-docs internals: real diff, robust YAML edit, gitignore caching
- [ORB-00165] — Retire `orbit-design` skill in favor of `orbit-docs`
- [ORB-00166] — Wire `orbit docs` retrieval into `task.show --with-context` and `task.start`
- [ORB-00167] — Extend PreToolUse hook to surface relevant docs alongside learnings
- [ORB-00168] — Add semantic embeddings index for orbit-docs corpus (v2)
- [ORB-10319] — Historical MCP-boundary consolidation; current ownership is split across `orbit-tools`, `orbit-mcp`, the CLI server, and Core.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
