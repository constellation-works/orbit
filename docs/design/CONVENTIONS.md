---
title: Design Doc Conventions
owner: daniel
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Accepted
---

# Design Doc Conventions

Recommended conventions that feature leads follow when writing and maintaining design docs under `docs/design/<feature>/`. The goal is a set of feature folders that read as one coherent documentation system regardless of which agent authored them. These are recommendations (not hard rules enforced by tooling; `orbit-design` has been retired in favor of the more tolerant docs surface — see the `orbit-search` skill).

This doc is itself the source of truth for the conventions. When a convention changes, update this doc and then update existing feature folders to match — do not silently diverge.

---

## 1. Folder Layout (per feature, recommended)

```
docs/design/<feature>/
├── 1_overview.md       recommended — what and why
├── 2_design.md         recommended — current implementation
├── 3_vision.md         recommended — forward-looking
├── 4_decisions.md      recommended — titled decisions and their reasoning
├── specs/              recommended folder; may be empty initially
│   └── <mechanism>.md  one mechanism per file
└── references/         recommended folder; may be empty initially
    └── glossary.md     recommended; other lookup-style docs allowed
```

- Folder name: lowercase, hyphenated, singular (`knowledge-graph`, `host-registry`).
- No `README.md`, `roadmap.md`, `changelog.md`, `tutorial.md` at this level.
- No top-level narrative files outside the numbered four (`1_`–`4_`). Existing folders may vary; new work should prefer the layout for coherence.

**Starting a new feature?** Copy the ready-made scaffold, then fill the placeholders:

```sh
cp -r docs/design/_templates docs/design/<feature>
mv docs/design/<feature>/specs/_mechanism.md docs/design/<feature>/specs/<mechanism>.md
```

The [`_templates/`](./_templates/) files carry the required frontmatter and section skeletons. They are the canonical copy source — the sections below describe only the *rules*, and point at the template they instantiate instead of repeating the boilerplate.

---

## 2. Required Frontmatter (all numbered docs)

Every numbered design doc starts with the YAML frontmatter carried by the [`_templates/`](./_templates/) `N_*.md` files — copy it and fill the placeholders. The fields:

- `title` mirrors the H1 verbatim.
- `owner` is the accountable agent family, not a committer list or full model string.
- `last_updated` is the calendar date of the last meaningful content change. Trivial reformat commits should not reset it.
- `status` is `Draft` until the doc is approved by the feature lead, then `Accepted`. It moves back to `Draft` if a structural rewrite is in flight.
- `feature` is the folder slug (e.g. `host-registry`, `knowledge-graph`). Lets tooling group docs by feature without parsing paths.
- `doc_role` is one of `overview`, `design`, `vision`, `decisions` — corresponds 1:1 with the filename prefix `1_`/`2_`/`3_`/`4_`.

The template frontmatter also carries the orbit-docs retrieval fields (`type`, `summary`, `tags`, `paths`, `related_features`, `related_artifacts`) so the doc is indexable on day one. `type` and `summary` are required by the strict parser. `summary` must be a non-empty single line. `related_artifacts` accepts task, learning, and friction references (`ORB-NNNNN`, `L-NNNN`, and `FYYYY-MM-NNN`). Decisions are addressed by their titles and links, not artifact IDs, so they do not belong in `related_artifacts`. The tolerant indexer infers these fields for legacy design docs and pattern docs, but new docs should write them explicitly.

---

## 3. Required Sections per Numbered Doc

| File | Required sections (in order) |
|------|------------------------------|
| **1_overview.md** | Elevator paragraph · §1 Motivation · §2 Core Concepts · §3 At a Glance (table: concern → file → task) · Task References |
| **2_design.md** | Scope paragraph · mechanism sections (variable count, numbered) · §N Concerns & Honest Limitations (mandatory last section) · Task References |
| **3_vision.md** | Scope paragraph · §1 Open Questions (numbered) · §2 Prior Work (subsections by category) · §3 What May Be Distinctive · §4 References (Orbit-internal + External) · Task References |
| **4_decisions.md** | Scope explainer · titled entries, each with Recorded provenance · Context · Decision · Consequences (incl. `Cost:`) |

Every numbered doc ends with a **Task References** section listing only the task IDs cited in that doc, plus the line:

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

---

## 4. Decisions

**`4_decisions.md` keeps the reasoning.** A decision is a titled section in its feature's `4_decisions.md`, reviewed in the same change as the code it describes and indexed with the ordinary docs corpus. Orbit tasks carry identity, lifecycle, ownership, and delivery provenance; the decision section carries only the reasoning that should outlive the task.

### 4a. What earns a decision entry

A decision entry is admitted through exactly one of two doors. Most implementation choices go through neither.

**Door 1 — it explains surprising code.** A future reader will hit a specific site, think *this looks wrong*, and be right to think so until they know the decision. The test is concrete: name the file, ideally the function.

**Door 2 — it governs future decisions.** A standing preference or constraint that decides tradeoffs the project has not yet encountered — "prefer coverage over precision when the two conflict," "fail closed rather than degrade silently." These cannot be anchored to a site because they apply everywhere, and they are load-bearing precisely because they are invoked repeatedly across unrelated work.

Door 2 has one discipline, and the convention collapses without it: **the entry must be applicable to a case nobody has seen yet.** Write it as a rule a reader could apply to tomorrow's decision. A retrospective account of a choice already made — "we chose a native primitive over a flat markdown directory" — reads like Door 2 and is not: it settles one past question and governs nothing. That belongs in `2_design.md` prose.

Whichever door, the entry must still name a **real alternative** (a different choice was on the table and would have produced a materially different design) and a **non-trivial cost** (something a reader could not infer from the decision itself). No cost line means the decision wasn't real.

Everything else — organizational choices, crate boundaries, the obvious next instance of an existing pattern — is design prose. Put it in `2_design.md`, or cite the task on the existing decision's `Recorded` line.

### 4b. Why IDs and lifecycle records were retired

Measured over the 210 accepted-and-superseded records the store held: 39% were cited from code, 55% only from other design docs, and 24% from nowhere at all. The split was qualitatively clean — code-cited entries described runtime behaviour contracts; doc-only entries described how the tree was arranged. Against that, the store cost ten CLI subcommands, an MCP write surface whose supersede path silently half-worked, a `proposed/` partition that the workspace-init gitignore template hid inside run worktrees — requiring a host-side staging handoff just to ship a draft — a dashboard API, a search index redundant with the docs corpus, and a dormant hub-global sequence allocator. Two systems of record for one decision, and the expensive one was not the one being read.

Migration was mechanical rather than a rescue: the tracked bodies moved verbatim into their feature's `4_decisions.md` before the duplicate store and tool surface were retired by [ORB-10726]. Orbit tasks already supply allocation-safe IDs, lifecycle state, ownership, and review handoff, so assigning a second identity to the reasoning added no durable capability.

Dropping decision IDs keeps every title and narrative while removing the remaining allocation and cross-link bureaucracy. Door 2 was added after the first draft of this rule made code-citation the sole test, which would have discarded the project's standing preferences along with the noise.

### 4c. Format and links

Copy the entry skeleton from [`_templates/4_decisions.md`](./_templates/4_decisions.md). Each entry is a unique `## <title>` heading followed by **Recorded**, **Context**, **Decision**, and **Consequences**, the last carrying at least one `Cost:` line.

- **Titles are the address.** Make the title specific and unique within the file. Link directly to its generated Markdown anchor: `[Decision title](./4_decisions.md#decision-title)`.
- **Cross-repository references use prose.** Name the repository and decision title; a relative anchor cannot cross repository boundaries honestly.
- **Door 1 entries carry `**Code anchors:**` or `**Paths:**`.** Prefer `path::symbol` when a stable symbol exists. The code site may link back to the decisions file in ordinary prose, but it does not carry a second decision identifier.
- **Door 2 entries state their reach in the Decision prose.** Do not add lifecycle or scope metadata solely to classify the entry.
- **Supersession is a title link:** `**Superseded by:** [New title](#new-title)`. The old entry and body stay in place so the reason the earlier architecture existed is not rewritten after the fact.
- **Provenance is task-backed.** `**Recorded:** <date> · [ORB-NNNNN]` preserves when and why the reasoning entered the project. It is historical context, not a second lifecycle.

### 4d. Tasks carry the tracking job

An executing agent adds or updates a decision entry only when the task and the admission test above require it. The task remains the searchable allocation key and lifecycle record. If a choice does not clear the admission test, capture it in the design prose or execution summary instead of creating a durable decision section.

### 4e. Rollup decisions

When a cluster of entries all instantiate the same underlying decision (e.g. "added language X to the tree-sitter extractor set"), fold the cluster into a single rollup entry:

- Expand the parent body with a per-instance table, or add a newly titled rollup that lists the cluster.
- Each folded entry keeps its heading and gets a title-based `**Superseded by:**` link.
- The rollup must preserve every Cost line from the folded entries that doesn't duplicate a cost already named.
- Compaction is normal maintenance, not emergency cleanup. Fold when the third instance lands, not the tenth.

---

## 5. Glossary Format

Copy [`_templates/references/glossary.md`](./_templates/references/glossary.md): an intro paragraph (scope and deliberate exclusions) followed by an alphabetized `Term | Meaning` table.

Rules:

- Alphabetized.
- Orbit-specific vocabulary only. Standard industry terms (hunk, blob, content-addressed, TTL) are excluded by default unless the feature gives them a specific meaning.
- Every entry references the doc where the term is used, so definitions don't drift from implementation.

---

## 6. Spec Format (`specs/<mechanism>.md`)

Copy [`_templates/specs/_mechanism.md`](./_templates/specs/_mechanism.md): a one-paragraph contract statement, a **Why This Exists** section, mechanism-specific sections, and an optional **Agent Signature**.

A spec is **prescriptive**. It names invariants ("writes do not fall back"), failure modes ("detached HEAD errors"), and migration paths. It is *not* a design-rationale doc — rationale lives in feature `4_decisions.md` entries.

---

## 7. Status Lifecycle (per doc)

- **Draft** — pre-first-review. Owner is still shaping it.
- **Accepted** — reviewed, approved, load-bearing.

There is no `Deprecated` status at the doc level. If the feature is retired, archive the entire folder under `docs/design/_archive/<feature>/` and annotate the first line of `1_overview.md`.

---

## 8. Cross-link Conventions

- Relative paths only, always with `./` or `../` prefix: `[foo](./foo.md)`, `[bar](../other/bar.md)`.
- Never link a task ID — `[ORB-00042]` stays as plain bracketed text. It's searchable via `git log --grep=` regardless of where tasks are stored.
- Section references use full paths: `[2_design.md §6.3]`, not a bare `§6.3` from a sibling doc.

---

## 9. Task ID Citation Format

- Inline: plain bracketed text `[ORB-00042]`.
- In decisions: on the `Recorded` line after the date.
- Never cite a task without naming what that task did — `([ORB-00042])` alone is opaque; always give a verb phrase.

---

## 10. What NOT to Create

| Anti-pattern | Why |
|--------------|-----|
| `README.md` at the feature folder | Duplicates `1_overview.md` |
| `roadmap.md` | Belongs in Orbit task system |
| `changelog.md` | Covered by git history + task IDs |
| `tutorial.md` | Belongs at top-level project README |
| Task-artifact mirrors in `references/` | Decision entries or design prose should absorb the "why"; rot risk otherwise |
| Top-level doc outside the numbered four | If it's important, it belongs inside one of them |

---

## 11. Enforcement

These are recommendations, not mechanically enforced by `orbit-design` (retired) or the docs indexer. The tolerant indexer (see the `orbit-search` skill) accepts both strict numbered design folders and free-form docs.

Five mechanical checks worth adding later (as optional lints, never blocking):

1. Lint: every numbered doc has required frontmatter + Task References section.
2. Lint: every decision has a Cost line.
3. Lint: every decision is admitted through exactly one door and carries concrete code/path anchors when it uses Door 1.
4. Lint: every path in a `Code anchors` or `Paths` line exists.
5. Lint: every title-based decision link resolves, including supersession links.

Check 4 also gives a maintenance signal worth acting on: a Door 1 entry whose paths no longer exist has stopped explaining live code and is a demotion candidate.

Until those exist: cross-review and author judgment are the quality mechanism. When one agent reviews the other's docs, the reviewer treats this doc as a checklist and gives feedback on deviations; the author decides whether the deviation is justified for that folder.

---

## 12. Ownership

The `Lead` value mirrors each feature folder's frontmatter `owner:` field and
uses the canonical agent family (`codex`, `claude`, `gemini`, or `grok`).
Retired features stay listed with their `_archive/` path as a historical record.

| Feature | Folder | Lead |
|---------|--------|------|
| Activity / Job | [docs/design/activity-job/](./activity-job/) | codex |
| Agent Families | [docs/design/agent-families/](./agent-families/) | grok |
| Automation Triggers | [docs/design/automation-triggers/](./automation-triggers/) | codex |
| Auditability | [docs/design/auditability/](./auditability/) | codex |
| Executors | [docs/design/executors/](./executors/) | claude |
| Federated MCP | [docs/design/federated-mcp/](./federated-mcp/) | grok |
| Global Store Consolidation | [docs/design/_archive/global-store-consolidation/](./_archive/global-store-consolidation/) | codex |
| Host Registry | [docs/design/host-registry/](./host-registry/) | claude |
| Knowledge graph | [docs/design/_archive/knowledge-graph/](./_archive/knowledge-graph/) | claude |
| MCP Bridge | [docs/design/mcp-bridge/](./mcp-bridge/) | claude |
| MCP Session Context | [docs/design/mcp-session-context/](./mcp-session-context/) | codex |
| Operation Mode | [docs/design/operation-mode/](./operation-mode/) | codex |
| Orbit Core | [docs/design/orbit-core/](./orbit-core/) | claude |
| Orbit Docs | [docs/design/orbit-docs/](./orbit-docs/) | claude |
| Orbit Graph | [docs/design/_archive/orbit-graph/](./_archive/orbit-graph/) | claude |
| Orbit Search | [docs/design/orbit-search/](./orbit-search/) | claude |
| Policy & Sandboxing | [docs/design/policy-sandbox/](./policy-sandbox/) | claude |
| Project Learnings | [docs/design/project-learnings/](./project-learnings/) | claude |
| Remote Access | [docs/design/remote-access/](./remote-access/) | claude |
| Resident Orchestrator | [docs/design/resident-orchestrator/](./resident-orchestrator/) | codex |
| Routines | [docs/design/routines/](./routines/) | claude |
| Task Artifacts | [docs/design/task-artifacts/](./task-artifacts/) | codex |
| Task Publication | [docs/design/task-publication/](./task-publication/) | codex |
| Task Sync (archived) | [docs/design/_archive/task-sync/](./_archive/task-sync/) | claude |
| Terminal Interface | [docs/design/terminal-interface/](./terminal-interface/) | claude |
| User Interface | [docs/design/user-interface/](./user-interface/) | gemini |
| Worktree Artifacts | [docs/design/worktree-artifacts/](./worktree-artifacts/) | codex |

Ownership means: the lead is accountable for keeping the folder's docs in sync with implementation, for recording task provenance when decisions change, and for responding to cross-review comments. Ownership does not preclude other agents from editing — it names who's on the hook when things drift.
