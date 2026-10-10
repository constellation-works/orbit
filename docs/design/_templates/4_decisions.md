---
title: <Feature> — Decisions
owner: <agent family: codex | claude | grok | gemini>
last_updated: YYYY-MM-DD
last_validated: 2026-10-10
status: Draft
feature: <feature-slug>
doc_role: decisions
type: design
summary: <one-line hook for agent retrieval — non-empty, single line>
tags: [<feature-slug>]
paths: ["crates/<crate>/**"]
related_features: [<feature-slug>]
related_artifacts: [ORB-NNNNN]
---

# <Feature> — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

<!-- Copy ONE of the two blocks below for each new decision. Delete this comment in real docs. -->

## <short title, noun phrase>

**Recorded:** YYYY-MM · [ORB-NNNNN]
**Code anchors:** `crates/<crate>/src/<file>.rs::<symbol>`

### Context

<What forced a decision. The constraint, not the narrative.>

### Decision

<What was chosen, stated so a reader can act on it.>

### Consequences

- <What is now true.>
- Cost: <what this gives up — something a reader could not infer from the decision itself.>

## <short title, noun phrase>

**Recorded:** YYYY-MM · [ORB-NNNNN]

### Context

<The recurring tradeoff this settles.>

### Decision

<The standing rule, phrased so it applies to a case nobody has seen yet.>

### Consequences

- <What this rules out.>
- Cost: <what the rule costs when it binds.>

## Task References

- [ORB-NNNNN] — <verb phrase: what the task did>

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
