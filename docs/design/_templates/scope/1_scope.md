---
title: "Scope: <feature in a few words>"
owner: <agent family: codex | claude | grok | gemini>
last_updated: YYYY-MM-DD
last_validated: YYYY-MM-DD
status: Draft
feature: <feature-slug>
doc_role: scope
type: design
summary: "Scope: <one line — what changes and why, for agent retrieval>"
tags: [<feature-slug>]
---

# Scope: <feature in a few words>

Status: proposed.
Bearing: <!-- The existing mechanisms this builds on, with links to their docs, CONFIG.md
     sections and the tasks that built them. One short paragraph: what is already true. -->

<!-- Keep the whole doc readable in one sitting. If a section needs its own
     sub-sub-sections, specs or a glossary, the feature belongs on the full track
     (CONVENTIONS.md §1). Delete these comments in real docs. -->

## Problem

<!-- A concrete situation, with real numbers where they exist, in which Orbit does
     the wrong thing today. Then the fact about the current code that shapes the fix. -->

## Goal

<!-- One paragraph: the outcome, and what stays unchanged. -->

## Shape

<!-- The mechanism. Formulas in code blocks, a worked example or table where it
     helps, then the rules as a short numbered or bulleted list. Typical
     subsections: the core rule, configuration (key | default | meaning),
     surfaces (CLI, run show, readiness, dashboard). -->

### <Mechanism>

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `<section>.<key>` | `<default>` | <meaning> |

### Surfaces

## Non-goals

<!-- Bold lead-in per item: what this deliberately does not do, and where that work lives instead. -->

- **<Non-goal>.** <Why, or where it belongs.>

## Concerns and honest limitations

<!-- What is weak, lagging, approximate or surprising, stated plainly. -->

- **<Concern>.** <Consequence and what bounds it.>

## Phasing

<!-- Numbered phases, each one deliverable change. The first usually only shows
     the numbers before anything steers. Update the Status line as phases land. -->

1. **<Phase>.** <What lands.>

## Open questions

1. **<Question>?** <The tradeoff.>

## Acceptance (feature level)

<!-- Observable outcomes for the whole feature, concrete enough to become task criteria. -->

- <Given ..., Orbit ...>

## Task References

- [ORB-NNNNN] — <verb phrase: what that task did>

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
