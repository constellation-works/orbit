---
title: Doc Layout — Overview
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Implemented
stage: shipped
next: "Correct STD-05 exemplar paths in the shared canon and sync docs/standards/"
feature: doc-layout
doc_role: overview
type: design
summary: "Give every page under docs/ a folder that answers one question, add docs/principles/ for the stances Orbit holds across features, and add one page on how the repository is worked"
tags: [doc-layout, docs, principles, index]
---

# Doc Layout — Overview

Doc layout puts every page under `docs/` in a folder that answers one question, gives each stance Orbit holds across features one principle page, and adds one page that explains how the repository is worked.

Status: the operator approved this design ([ORB-29306]) on 2026-11-27, with the three recommendations under
[Settled questions](./2_design.md#settled-questions). Phases 1–3 ([ORB-29310], [ORB-29312]–[ORB-29315],
[ORB-29316], plus [ORB-29729]) and all four chained tasks ([ORB-29261], [ORB-29290], [ORB-29291],
[ORB-29292]) are done and merged on `agent-main`; only phase 4 (canon STD-05 exemplar sync, tracked in
the canon workspace) remains in [2_design.md](./2_design.md#phasing).

## 1. Motivation

Bearing: the feature board left the generated index ([ORB-29261]), `docs/dev/` folded into
`docs/agents/records/` and `docs/development/examples/` ([ORB-29290]), and the two large
references split into `docs/development/` ([ORB-29291]) and `docs/config/` ([ORB-29292]).
Design docs already follow [the design conventions](../CONVENTIONS.md), and the vendored
standards already carry their own [index](../../standards/README.md). Sizes and counts below
were measured at agent-main `7c6fa22806`.

### The gap

Three pages sit at the root of `docs/` because no folder fits them:

| Page | Bytes | Linked from |
|---|---|---|
| the optimistic-merging page, formerly at the `docs/` root | 5,982 | `README.md`, the generated index |
| the positioning page, formerly at the `docs/` root | 9,962 | `README.md`, the index, the optimistic-merging page, three design docs |
| the data-handling page, formerly at the `docs/` root | 15,302 | the index only |

With the four chained tasks landed, these three and two maps of old headings would be all that
remained at the root, and the next page with no obvious home would join them.

The pages also overlap. The positioning page's "Delivery philosophy" section and the
optimistic-merging page both list the required checks and the review modes, so a change to
either has two places to update.

Other stances that decide design questions have no page at all. They are restated wherever
they apply, across designs, runbooks, agent rules, bundled skills and the website:

| Phrase | Files that use it |
|---|---|
| "fail closed" | 91 |
| "footprint" | 80 |
| "operator-only", "operator-reserved" | 79 |
| pilot admission terms | 111 |

A reader who wants the rule and its reason has to assemble it from those files. A contributor
who wants to know how the repository is worked (why standards are vendored, where a design
goes, what the companion tools are) finds the pieces in `AGENTS.md`, the standards index and the
design conventions, with nothing that connects them.

### Intended outcome

Every page under `docs/` sits in a folder that answers one question, and the root holds only
the generated index and the two maps. Each stance Orbit holds across features has one page that
states the rule, the reason and the cost, and every other statement of it becomes a link. One
page explains how the repository is worked and links to the document that owns each part. No
existing page is reworded when it moves, and the four chained tasks keep their scope.

### Out of scope

- **Rewording the three moved pages.** They move as they are. The only edit is the positioning
  page's duplicated section, in the phase that adds the pages it would then repeat.
- **Changing the four chained tasks.** [ORB-29261], [ORB-29290], [ORB-29291] and [ORB-29292]
  keep their scope and order.
- **Website pages.** The concept pages on the website explain how scheduling and tasks work. A
  principle page says why, and links to them.
- **A project-docs standard for this repository.** Whether Orbit adopts the shared project-docs
  standard is a separate decision. This layout does not depend on it.

### Acceptance (feature level)

- Given the finished layout, `ls docs/*.md` prints `CONFIG.md`, `DEVELOPMENT.md` and `INDEX.md`,
  and `scripts/generate-doc-indexes.sh --check` fails when any other Markdown file is added there.
- Given the three moved pages, `git log --follow` shows each as a rename with no content change
  in the commit that moves it, and no link in the repository outside `docs/standards/` names an
  old path.
- Given each new principle page, it has the five parts in order, names at least two feature
  folders it reaches, states a cost, and is listed in the index's Principles table.
- Given the operating-model page, each section links to the document that owns its
  subject, the page names no person, and it is the first link in the folder's README.
- Given the positioning page after phase 2, the required checks are listed in the
  optimistic-merging page only.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Root pages.** The pages that sit at the root of `docs/` because no folder fits them; the finished layout keeps only the generated index and the two maps there.
- **Principle page.** A page that states one cross-feature stance in five parts, with its rule, reason and cost, and names at least two feature folders it reaches; every other statement of the stance links to it.
- **Operating-model page.** The one page that explains how the repository is worked, linking each section to the document that owns its subject.
- **Chained tasks.** The four tasks that move the feature board, `docs/dev/` and the two large references; this feature keeps their scope and order.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Target layout and the three moved pages | [2_design.md](./2_design.md#layout) | [ORB-29306], [ORB-29310] (done) |
| What counts as a principle and the page shape | [2_design.md](./2_design.md#what-counts-as-a-principle) | [ORB-29306], [ORB-29310] (done) |
| Principle pages to add | [2_design.md](./2_design.md#pages-to-add) | [ORB-29312], [ORB-29313], [ORB-29314], [ORB-29315] (done) |
| How the repository is worked | [2_design.md](./2_design.md#how-the-repository-is-worked) | [ORB-29316], [ORB-29729] (done) |
| Feature board out of the generated index | [2_design.md](./2_design.md#phasing) | [ORB-29261] (done) |
| `docs/dev/` records and examples | [2_design.md](./2_design.md#phasing) | [ORB-29290] (done) |
| `docs/DEVELOPMENT.md` and `docs/CONFIG.md` splits | [2_design.md](./2_design.md#phasing) | [ORB-29291], [ORB-29292] (done) |
| Settled questions | [2_design.md](./2_design.md#settled-questions) | [ORB-29306] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-29261] — moved the feature board out of the generated index (done).
- [ORB-29290] — moved `docs/dev/` records and examples (done).
- [ORB-29291] — split `docs/DEVELOPMENT.md` into `docs/development/` (done).
- [ORB-29292] — split `docs/CONFIG.md` into `docs/config/` (done).
- [ORB-29306] — wrote the original scope, now this overview and its design.
- [ORB-29310] — created `docs/principles/`, moved the three root pages, and added the Principles index and root rule (done).
- [ORB-29312] — wrote `docs/principles/declared-footprints.md` (done).
- [ORB-29313] — wrote `docs/principles/operator-admission.md` and trimmed `docs/principles/positioning.md` (done).
- [ORB-29314] — wrote `docs/principles/pilot-before-promote.md` (done).
- [ORB-29315] — wrote `docs/principles/refuse-not-degrade.md` (done).
- [ORB-29316] — added `docs/development/operating-model.md` (done).
- [ORB-29729] — linked the Tasks concept from `docs/development/operating-model.md` (done).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
