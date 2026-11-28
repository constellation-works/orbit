# How the repository is worked

This page names the parts that shape work on Orbit and links each to the document that owns it. It holds no procedure.

## Tasks and delivery

Every change starts as a task, and the task id appears in the commit. Delivery follows the stances in [`docs/principles/`](../principles/CONVENTIONS.md): agents propose and the operator admits ([operator admission](../principles/operator-admission.md)), a task is checked against the current code before it enters the backlog ([pilot before promote](../principles/pilot-before-promote.md)), tasks run in parallel only when their declared files do not overlap ([declared footprints](../principles/declared-footprints.md)), and changes merge on required checks rather than serial approval ([optimistic merging](../principles/optimistic-merging.md)). What Orbit is for, and what it is not, is in [positioning](../principles/positioning.md).

## Standards

A set of shared engineering standards is vendored read-only under `docs/standards/` and is normative. A deviation is a recorded decision that cites the rule and its version. Copies are updated by a sync, never by hand. The [standards index](../standards/README.md) lists them and how integrity is checked, and the managed block in [`AGENTS.md`](../../AGENTS.md) names the standards and versions this repository adopts.

## Designs

Each feature has one folder under `docs/design/`, with two tracks of documents, a delivery stage that is separate from the document's approval status, and a generated board of where each feature stands. [The design conventions](../design/CONVENTIONS.md) set the folder shape and the admission test for a decision entry, and [the feature board](../design/FEATURES.md) shows the current stage of each feature.

## Agent rules and records

The rule files in [`docs/agents/`](../agents/code-rules.md) and the decision corpus under [`docs/agents/corpus/`](../agents/corpus/INDEX.md) are guidance that agents follow. [Records](../agents/records/README.md) are dated evidence from past work and describe no current behaviour.

## Companion tools

Orbit is developed with two companion tools, each in its own public repository. [`constellation-works/formation`](https://github.com/constellation-works/formation) staffs and coordinates agent sessions. [`constellation-works/scribe`](https://github.com/constellation-works/scribe) records preferences and session history so a session can resume. A private workspace repository holds knowledge that spans repositories and the coordinating sessions' own task records. How each tool is used is documented in its own repository, because those procedures change faster than Orbit does.
