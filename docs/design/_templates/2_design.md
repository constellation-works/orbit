---
title: <Feature> — Design
owner: <agent family: codex | claude | grok | gemini>
last_updated: YYYY-MM-DD
last_validated: 2026-10-10
status: Draft
feature: <feature-slug>
doc_role: design
type: design
summary: <one-line hook for agent retrieval — non-empty, single line>
tags: [<feature-slug>]
paths: ["crates/<crate>/**"]
related_features: [<feature-slug>]
related_artifacts: [ORB-NNNNN]
---

# <Feature> — Design

<!-- Scope paragraph: what this doc covers (the current implementation) and what
     it deliberately leaves to 3_vision.md or another feature. -->

## 1. <Mechanism>

<!-- Describe one mechanism: how it works today, the key types/files, the
     invariants it holds. Add as many numbered mechanism sections as needed. -->

## N. Concerns & Honest Limitations

<!-- MANDATORY final section. Name the known sharp edges, failure modes, and
     things deliberately left unsolved. A design doc with no limitations section
     reads as either incomplete or dishonest — do not omit it. -->

## Task References

- [ORB-NNNNN] — <verb phrase: what the task did>

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
