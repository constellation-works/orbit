---
id: STD-06-design-docs-and-feature-tracking
title: Design docs and feature tracking — one folder per feature, a delivery stage separate from doc approval, a generated feature board, admitted decisions, and tasks tagged with their feature
summary: Normative rules for how a project keeps feature designs and knows where each feature stands — one docs/design/<feature>/ folder on a scope or full track, required frontmatter, a `stage` and `next` on the entry doc kept apart from the doc's approval `status`, a generated and CI-checked feature board, decision entries admitted only when they explain surprising code or govern future choices, and every implementation task tagged with its feature slug so live progress is read from the tracker; follow it in any project with design docs.
status: draft
tags: [standard, docs, design, tracking, agents]
version: 1
created: 2026-10-25
updated: 2026-10-25
last_validated: 2026-10-25
related: [STD-04-testing-and-verification]
---

# Design docs and feature tracking

These rules cover where a project writes down its feature designs, how it records how far
each feature is built, and how that record stays tied to the work tracker. They exist so
that one read of a generated board answers "what are we building, and where does each
thing stand", and one tag query answers "which tasks belong to it".

Every rule is portable. A project with three features and a plain issue tracker can comply.
The minimum is:
- one folder per designed feature with a frontmatter entry doc;
- `stage` and `next` on that doc;
- a script that renders the board and fails CI when a stage is missing;
- the feature slug as a tag on each task that implements the feature.

Orbit is the worked example, not the audience. Orbit paths are relative to
`codebases/orbit/`. "Tracker" means the project's task system (Orbit, for constellation
repos).

Other standards own the neighbouring rules, and this one does not repeat them:
- **Docs claims as contracts** (a documented behaviour is tested or marked) are
  [STD-04](../STD-04-testing-and-verification/STD-04.md) §R13 and §R14. This standard covers
  the design record, not user documentation.
- **Regenerated artifacts checked in CI** are STD-04 §R11. §R9 below applies it to the
  feature board.

The keywords MUST, MUST NOT and SHOULD are normative. A rule marked *review-only* in
[`checks.md`](checks.md) has no automated gate, so a reviewer enforces it.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Where designs live

- **R1.** Each designed feature MUST have exactly one folder, `docs/design/<feature>/`. The
  slug is lowercase, hyphenated and singular, and it MUST NOT be renamed while tasks carry it
  (§R12); a rename retags every open task in the same change.
- **R2.** A folder MUST follow one of two tracks:
  - *scope*: `1_scope.md`, plus `4_decisions.md` once an entry is admitted (§R10). For one
    change to an existing mechanism, readable in one sitting.
  - *full*: `1_overview.md`, `2_design.md`, `3_vision.md`, `4_decisions.md`, with `specs/`
    and `references/` as needed. For a subsystem with its own vocabulary or several
    mechanisms.

  A scope doc that outgrows one sitting graduates to the full track in the change that makes
  it outgrow. `4_decisions.md` keeps its name on both tracks.
- **R3.** A feature folder MUST NOT hold a `README.md`, roadmap, changelog or tutorial, or
  any top-level file outside its track's numbered files. Roadmaps live in the tracker,
  changelogs in git, tutorials in user docs.

### Frontmatter and stage

- **R4.** Every numbered doc MUST start with YAML frontmatter carrying `title`, `owner`
  (the accountable person or agent family), `last_updated`, `status`, `feature` (the slug),
  `doc_role` and a one-line `summary`. `status` is `Draft` until the feature lead approves the
  doc, then `Accepted`, and `Deprecated` when archived (§R14). It records the doc's approval
  and nothing else.
- **R5.** The entry doc (`1_overview.md` or `1_scope.md`) MUST carry `stage`, one of:
  - `idea`: problem stated, no agreed design, nothing built;
  - `designing`: design written or under review, nothing implemented and nothing approved to
    build;
  - `building`: implementation landed or approved, but the design's committed scope (not its
    vision) is incomplete;
  - `shipped`: the committed scope is implemented and in use; what remains is vision work;
  - `parked`: deliberately on hold or superseded.

  Delivery state MUST NOT be expressed through `status`.
- **R6.** The entry doc MUST carry `next`: one line, at most about 100 characters, naming
  what has to happen next and the task that does it when one exists. A `shipped` feature with
  nothing pending uses `—`; a `parked` one says why.
- **R7.** `stage` and `next` MUST change in the same change that moves the feature: the
  change that approves the first implementation task, lands the last committed slice, or
  parks the work. Design approval alone does not move a feature to `building`.
- **R8.** Body text MUST NOT contradict `stage`. A new doc SHOULD NOT carry a prose status
  banner at all; an existing banner is corrected or removed when the stage changes.

### The board

- **R9.** The project MUST generate its feature board from the entry docs' frontmatter,
  grouped by `stage` and showing each feature's `next` and owner, and MUST check it in CI
  with the generator's check mode. The generator MUST fail on a missing or unknown `stage`.
  The board is never hand-edited.

### Decisions

- **R10.** A `4_decisions.md` entry MUST be admitted through one of two doors:
  - it explains code a future reader would rightly think is wrong, and names that code
    (`**Code anchors:**` or `**Paths:**`, as `path::symbol` where one exists); or
  - it governs future decisions: a standing preference written so it applies to a case
    nobody has seen yet.

  Either way it MUST name a real alternative and carry at least one `Cost:` line. Other
  choices go in design prose.
- **R11.** A decision entry is addressed by its unique `##` title. Supersession MUST be a
  `**Superseded by:**` title link, and the superseded entry stays in place.

### Tasks and progress

- **R12.** Every task that implements, repairs or retires part of a designed feature MUST
  carry the feature slug as a tag, set when the task is filed or, at the latest, when it is
  promoted for work. A task that changes several features carries each slug. Whoever files or
  promotes the task owns the tag; an implementer does not add it after the fact.
- **R13.** Live progress (open, in-flight, blocked and done counts) MUST be read from the
  tracker by the feature tag, never copied into design docs. Docs cite task ids as plain text
  with a verb phrase saying what the task did, and each numbered doc ends with a **Task
  References** list of the ids it cites.

### Retirement

- **R14.** A feature that is gone for good MUST either be deleted with its inbound links
  fixed, or archived: `status: Deprecated`, a one-line pointer to the replacement or reason
  at the top of the entry doc, and the folder moved to `docs/design/_archived/<feature>/`. A
  feature only on hold stays in place as `stage: parked`.

## Applies to

- `universal`: R1 to R14. Any project that keeps design docs, of any size or language, and
  any author or operator, human or agent.
- R12 and R13 bind on whoever files and promotes tasks: in constellation repos that is the
  orchestrator or operator, not the implementing worker.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-06@<version> §Rn` and the reason, for example `STD-06@1 §R2`. It never edits its
vendored copy of this standard.

Some deviations are expected, and the decision still needs to be recorded:
- A small repo whose only designs are a handful of scope docs may render the board into its
  README instead of a separate index, under `STD-06@1 §R9`, as long as CI still checks it.
- A folder whose own acceptance shape pairs a scope doc with a `2_design.md` cites
  `STD-06@1 §R2` in that scope doc.

Known deviations in the exemplar itself, so adopters do not copy them:
- At the time of v1, several Orbit entry docs still carry prose `Status:` banners written
  before `stage` existed, and some contradict it (against R8).
- `docs/design/executors/` and `docs/design/project-learnings/` have no `1_` entry doc; the
  generator falls back to `4_decisions.md` (against R2).
- `docs/design/plugins/1_scope.md` has no `owner`, `status`, `feature` or `doc_role` (against
  R4).
- Orbit's `docs/design/CONVENTIONS.md` §11 calls its conventions recommendations; adoption
  makes them binding where this standard states them.
- Feature tags on Orbit tasks are used by some features and not others (against R12).
