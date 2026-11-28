---
type: context
summary: "Principle page conventions: what counts as a principle, the admission test, the page parts, frontmatter and size bound"
last_validated: 2026-11-27
tags: [principles, conventions, docs]
related_features: [doc-layout]
---

# Principle Page Conventions

Rules for writing and admitting a page under `docs/principles/`. A principle page states a
stance Orbit holds across features, with the reason and the cost. Pages in the folder are
listed in the [documentation index](../INDEX.md); this file is not.

## What counts as a principle

Four kinds of document already carry rules. A principle page is admitted only where none of
the others fits:

| Kind | Home | Reach |
|---|---|---|
| Standard rule | `docs/standards/` (vendored) | Code in every adopting repository. A principle cites a standard and never restates it. |
| Decision | a feature's `4_decisions.md` | A site in the code, or a standing preference within one feature. |
| Lesson | `docs/lessons/` | One incident and what it taught. |
| Principle | `docs/principles/` | A stance that decides cases across features and that someone using Orbit can observe. |

A page is admitted when all four hold:

1. **Reach.** It decides cases in at least two feature folders, and the page names them.
2. **Alternative and cost.** A different choice was available and would have produced a
   different product, and the page says what the rule gives up.
3. **Observable.** Someone using Orbit can see it: a refusal, a hold, a status, a default. A
   rule only a compiler or a reviewer sees is a standard or a design pattern.
4. **One statement.** Once the page exists, designs, runbooks and agent rule files keep their
   mechanism and link to the page for the argument. Bundled skills and website pages are left
   as they are: they must stand alone and cannot link to repository docs.

Rule 2 is the test the [design conventions](../design/CONVENTIONS.md) already apply to a
decision entry.

## Page shape

A principle page is readable in one sitting, about 8,000 bytes at most, with this order:

| Part | Content |
|---|---|
| Lead paragraph | The rule, in one or two sentences. |
| `## Why` | The pressure that produced it, with the measured or observed cost of the alternative. |
| `## In practice` | Where the rule shows: the mechanisms, statuses and refusals, each linked to its design, runbook or config page. No setting is restated. |
| `## What it costs` | What the rule gives up, and who pays. |
| `## Applying it` | The question to ask of a new case, and one boundary where the rule does not apply. |

Frontmatter is `type: context`, `summary`, `last_validated`, `tags` including `principle`, and
`related_features`. The index generator reads `summary` for the Principles table. Files use
lowercase, hyphenated names; this file is the one exception and is left out of the table.

Pages that predate these conventions (`positioning.md`, `optimistic-merging.md` and
`data-handling.md`) keep their own headings and are not rewritten to match.
