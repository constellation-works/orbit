---
title: "Scope: docs layout and principle pages"
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Draft
stage: designing
next: "Operator review of this scope; build tasks are filed after approval [ORB-29306]"
feature: doc-layout
doc_role: scope
type: design
summary: "Scope: give every page under docs/ a folder that answers one question, add docs/principles/ for the stances Orbit holds across features, and add one page on how the repository is worked"
tags: [doc-layout, docs, principles, index]
---

# Scope: docs layout and principle pages

Status: design only. Nothing is moved or written until the operator approves this scope.
Bearing: the feature board is leaving the generated index ([ORB-29261]), `docs/dev/` is being
folded into `docs/agents/records/` and `docs/development/examples/` ([ORB-29290]), and the two
large references are being split into `docs/development/` ([ORB-29291]) and `docs/config/`
([ORB-29292]). Design docs already follow [the design conventions](../CONVENTIONS.md), and the
vendored standards already carry their own [index](../../standards/README.md). Sizes and counts
below were measured at agent-main `7c6fa22806`.

## Problem

Three pages sit at the root of `docs/` because no folder fits them:

| Page | Bytes | Linked from |
|---|---|---|
| `docs/OPTIMISTIC_MERGING.md` | 5,982 | `README.md`, the generated index |
| `docs/POSITIONING.md` | 9,962 | `README.md`, the index, the optimistic-merging page, three design docs |
| `docs/DATA_HANDLING.md` | 15,302 | the index only |

Once the four chained tasks land, these three and two maps of old headings are all that is left
at the root, and the next page with no obvious home will join them.

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

## Goal

Every page under `docs/` sits in a folder that answers one question, and the root holds only
the generated index and the two maps. Each stance Orbit holds across features has one page that
states the rule, the reason and the cost, and every other statement of it becomes a link. One
page explains how the repository is worked and links to the document that owns each part. No
existing page is reworded when it moves, and the four chained tasks keep their scope.

## Shape

### Layout

```
docs/
├── INDEX.md              generated entry point
├── CONFIG.md             map of old headings to docs/config/        [ORB-29292]
├── DEVELOPMENT.md        map of old headings to docs/development/   [ORB-29291]
├── principles/           new
│   ├── CONVENTIONS.md        admission test and page shape
│   ├── positioning.md        moved from POSITIONING.md
│   ├── optimistic-merging.md moved from OPTIMISTIC_MERGING.md
│   ├── data-handling.md      moved from DATA_HANDLING.md
│   ├── declared-footprints.md    new
│   ├── operator-admission.md     new
│   ├── pilot-before-promote.md   new
│   └── refuse-not-degrade.md     new
├── development/          [ORB-29291]
│   ├── README.md
│   ├── operating-model.md    new
│   └── …
├── config/               [ORB-29292]
├── agents/               rule files, corpus, records/               [ORB-29290]
└── architecture/  design/  design-patterns/  lessons/  runbooks/  standards/  assets/
```

Each folder answers one question, and a page goes where its question is answered:

| Folder | Question |
|---|---|
| `principles/` | What does Orbit hold to, and why? |
| `design/` | How is a feature meant to work, and how far is it built? |
| `architecture/` | Where is it in the code? |
| `development/` | How do I work in this repository? |
| `config/` | What does a setting do? |
| `runbooks/` | How do I operate it? |
| `agents/` | What must an agent follow, and what did agents record? |
| `lessons/` | What went wrong, and what did it teach? |
| `standards/` | Which shared engineering rules bind this repository? |

Rules:

1. The root of `docs/` holds `INDEX.md`, `CONFIG.md` and `DEVELOPMENT.md` and nothing else.
   `scripts/generate-doc-indexes.sh` fails on any other Markdown file there, so a page with no
   home is raised as a layout question instead of being parked.
2. Files inside a folder use lowercase, hyphenated names, as `runbooks/` and `architecture/` do.
3. The index gains a Principles table built from `docs/principles/*.md`, and its Guides table
   lists the two maps.
4. The three pages move with `git mv` and no rewording. Their inbound links are updated in the
   same change: `README.md`, three design docs and the pages' links to each other.

`data-handling.md` is an inventory of what is stored and what leaves the machine, which makes
it longer than the others. It belongs with the principles because it is the rule a contributor
must hold when a feature would send something new off the machine, and its first section states
that rule.

### What counts as a principle

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
4. **One statement.** Once the page exists, designs, runbooks and skills keep their mechanism
   and link to the page for the argument.

Rule 2 is the test the design conventions already apply to a decision entry. It is repeated
here because the retired decision store grew to 210 records, 24% of them cited from nowhere,
before an admission test existed.

### Page shape

A principle page is readable in one sitting, about 8,000 bytes at most, with this order:

| Part | Content |
|---|---|
| Lead paragraph | The rule, in one or two sentences. |
| `## Why` | The pressure that produced it, with the measured or observed cost of the alternative. |
| `## In practice` | Where the rule shows: the mechanisms, statuses and refusals, each linked to its design, runbook or config page. No setting is restated. |
| `## What it costs` | What the rule gives up, and who pays. |
| `## Applying it` | The question to ask of a new case, and one boundary where the rule does not apply. |

Frontmatter is `type: context`, `summary`, `last_validated`, `tags` including `principle`, and
`related_features`. The optimistic-merging page already has this shape under other headings and
is not rewritten to match.

### Pages to add

| Page | Rule | Stated today in |
|---|---|---|
| `declared-footprints.md` | A task declares the files it will touch. Orbit runs tasks in parallel only when their declared footprints do not overlap, and holds the reservation until the change lands. | The optimistic-merging page, the path-leases, conflict-budget and distributed-drain designs, the scheduling concept page, the bundled skills |
| `operator-admission.md` | Agents propose and the operator admits. A filed task starts in `proposed`. Promotion, forced transitions, review settings, tool grants and crew pools are the operator's, and an agent cannot grant them to itself. | The positioning page, the auto-tasks and granted-tools designs, the agent rule files, the bundled skills |
| `pilot-before-promote.md` | A task is checked against the current code before it enters the backlog. The pilot settles its footprint, complexity and crew, and holds it when the brief contradicts the tree. Skipping the pilot is the operator's choice. | The automation-triggers, routines and resident-orchestrator designs, the bundled skills, the website |
| `refuse-not-degrade.md` | When Orbit cannot verify something a step depends on (the sandbox, a credential, ownership, delivery evidence, the provider a crew names), it refuses with a remediation hint. It does not substitute a weaker path. The code rule is STD-02 §R31, which the page cites. | STD-02, the policy-sandbox and activity-job designs, the runbooks |

Candidates that do not get a page:

| Candidate | Why not | Where it goes |
|---|---|---|
| Already-landed checks | No real alternative: nobody would choose to deliver the same change twice. It is a mechanism. | The activity-job design keeps it; the optimistic-merging page links to it. |
| No silent provider fallback | Three files state it, and it is one case of refusing instead of degrading. | A paragraph in `refuse-not-degrade.md`. |
| Review layers are the operator's | One case of operator admission. | A paragraph in `operator-admission.md`. |
| Auditability | The positioning page states it once, with its five commitments, and it has a design folder. | Unchanged. |

When the pages land, the positioning page's "Delivery philosophy" section shrinks to its
one-sentence trade and a link to the optimistic-merging page, and its admission bullet becomes
a link to `operator-admission.md`. Its `## Non-negotiables` and `## What Orbit is NOT for`
headings keep their names, because the standards cite them.

### How the repository is worked

`operating-model.md`, in `docs/development/`, is one page, about 6,000 bytes at most. Each section is
one short paragraph that states the role of a part and links to the document that owns it. The
page holds no procedure.

| Section | Says | Links to |
|---|---|---|
| Tasks and delivery | Every change starts as a task, the task id is in the commit, and delivery follows the principles. | `docs/principles/`, the task concept page |
| Standards | A set of shared engineering standards is vendored read-only under `docs/standards/` and is normative. A deviation is a decision that cites the rule and its version. Copies are updated by a sync, never by hand. | [the standards index](../../standards/README.md), the managed block in `AGENTS.md` |
| Designs | One folder per feature, two tracks, a delivery stage separate from the document's status, and a generated board. | [the design conventions](../CONVENTIONS.md), [the feature board](../FEATURES.md) |
| Agent rules and records | Rule files and the decision corpus are guidance. Records are dated evidence and describe no current behaviour. | `docs/agents/` |
| Companion tools | Orbit is developed with two companion tools, each in its own public repository: one staffs and coordinates agent sessions, the other records preferences and session history so a session can resume. A private workspace repository holds knowledge that spans repositories and the coordinating sessions' own task records. | The two public repositories |

The README of `docs/development/` stays the index [ORB-29291] makes it and gains one line linking
this page first.

The private workspace keeps the full account of how it, the session tool and the memory tool
are used. This repository states their roles only, because it is public and those procedures
change faster than Orbit does.

### Surfaces

- `docs/INDEX.md`: a Principles table; the Guides table lists the two maps.
- `scripts/generate-doc-indexes.sh`: renders the Principles table and enforces the root rule in
  both write and `--check` modes.
- `README.md`: its two links point at the moved pages.

## Non-goals

- **Rewording the three moved pages.** They move as they are. The only edit is the positioning
  page's duplicated section, in the phase that adds the pages it would then repeat.
- **Changing the four chained tasks.** [ORB-29261], [ORB-29290], [ORB-29291] and [ORB-29292]
  keep their scope and order.
- **Website pages.** The concept pages on the website explain how scheduling and tasks work. A
  principle page says why, and links to them.
- **A project-docs standard for this repository.** Whether Orbit adopts the shared project-docs
  standard is a separate decision. This layout does not depend on it.

## Concerns and honest limitations

- **Old links break.** A link from outside the repository to `docs/POSITIONING.md` or
  `docs/OPTIMISTIC_MERGING.md` stops resolving, because a renamed file is not redirected. Open
  question 2 covers leaving maps at the old paths.
- **The standards cite two of the pages by path.** The vendored STD-05 exemplars name
  `docs/POSITIONING.md` and `docs/DATA_HANDLING.md` for §R15 and §R20. The vendored copy cannot
  be edited here, so it names the old paths until the shared canon is corrected and synced.
- **Phrase counts measure scatter, not importance.** They show that a rule is restated in many
  places. The admission test, not the count, decides whether it gets a page.
- **A principle page can go stale.** `last_validated` shows its age, and rule 4 of the admission
  test keeps the copies few, but nothing checks a page against the behaviour it describes.
- **The two maps stay at the root.** They exist so old anchors still lead somewhere. Removing
  them is a later decision, once the links that need them are gone.

## Phasing

1. **Folder, moves and index.** Create `docs/principles/` with `CONVENTIONS.md`, move the three
   pages, update inbound links, add the Principles table and the root rule to the generator.
   Runs after [ORB-29290], the last chained task that edits the generator.
2. **Four principle pages.** One task per page, each reading the sources named above, replacing
   restated arguments in those sources with links where the page now carries them. The task for
   `operator-admission.md` also trims the positioning page. Runs after phase 1.
3. **Operating-model page.** Add `operating-model.md` to `docs/development/`, and the README line. Runs
   after [ORB-29291], which creates the folder's README.
4. **Standards citation.** Correct the STD-05 exemplar paths in the shared canon and sync the
   vendored copy. The operator approves that change to the canon. Runs after phase 1.

## Open questions

1. **Does the companion-tools paragraph belong in this public repository?** Recommended: yes,
   as one paragraph of roles with links to the two public repositories. A reader of this
   repository meets task ids, an integration branch and vendored standards with nothing that
   says how the project is run. The alternative is a single line saying the project is developed
   with companion tools, with no description.
2. **Should maps stay at the three old paths?** Recommended: no. Inside the repository only six
   files link to them and all are updated in phase 1. Maps would keep outside links alive, and
   would leave five files at the root instead of three.
3. **Does `data-handling.md` move, or stay at the root as a user reference?** Recommended: move.
   Only the index links it today, and the website has its own privacy page for users.

## Acceptance (feature level)

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

## Task References

- [ORB-29306] — writes this scope.
- [ORB-29261] — moves the feature board out of the generated index.
- [ORB-29290] — moves `docs/dev/` records and examples.
- [ORB-29291] — splits `docs/DEVELOPMENT.md` into `docs/development/`.
- [ORB-29292] — splits `docs/CONFIG.md` into `docs/config/`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
