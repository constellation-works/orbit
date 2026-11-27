---
title: Pilot Notes — Overview
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Draft
stage: designing
next: "Operator approval of the draft [ORB-29567]"
feature: pilot-notes
doc_role: overview
type: design
summary: "A workspace names a question for the task pilot to answer per task; the pilot's answer is a short labelled note that Orbit keeps with the assessment and shows on the task, without acting on it"
tags: [pilot-notes, task-pilot]
related_features: [task-pilot, plugins, workspace-agent-corpus]
related_artifacts: [ORB-29567]
---

# Pilot Notes — Overview

A pilot note is a short text the task pilot writes about one task, in answer to a question the
workspace asked. The workspace maps a label to an instruction file. The pilot reads the
instruction with its own, and returns a note under that label for each task it assesses. Orbit
keeps the note with the assessment and shows it on the task. Orbit never reads a note's
meaning and never acts on it: a note is for whoever asked the question, usually a plugin. This
is a draft for the operator to approve ([ORB-29567]). Nothing in it is built.

## 1. Motivation

| Observed | Cause |
|---|---|
| An adopter workspace wants the pilot to pick, for each task, the few items of a long checklist that the change can break. The checklist is the workspace's own and is served by a plugin tool | The pilot is the one run that reads every task and its files before admission. It reports through a fixed assessment, and nothing in that assessment is the workspace's to define |
| The ways to get such an answer today are a local copy of the pilot activity, or a second model run after the pilot | The shipped activity is 1,424 lines and changes with releases, so a copy needs a re-merge at each one and a stale copy pilots by old instructions. A second run costs a model call per task and can finish after the task has started |
| `[workflow.tag_instructions]` cannot carry the question | It has no role for the pilot, and the pilot runs before a task's tags are settled |
| `[workflow.agent_corpus]` reaches the pilot and is the wrong carrier | Its rows are workspace knowledge under one shared 8 KiB bound, dropped when they do not fit. An instruction that asks for output cannot be the thing that is dropped |

**Goal.** A workspace gets a per-task answer from the pilot run that already exists, by
committing an instruction file and one config table. A plugin or a person reads the answer
from the task. Orbit learns nothing about what the question means.

**Not in it:**

- **A note that changes what Orbit does.** A note never holds admission, never changes
  selectors, crew or complexity, and is never a finding. Something that should hold a task is
  a pilot finding, which has its own typed contract.
- **Note text in a prompt.** Orbit never injects a note into an implementer's, a reviewer's or
  a later pilot's prompt. A note is model output about a task whose text Orbit does not trust.
- **A new tool grant for the pilot.** The pilot already may call any registered tool its deny
  list does not name, which includes a plugin's read-only tools
  ([design](./2_design.md#3-what-the-pilot-may-call)).
- **Instructions shipped inside a plugin.** The text the pilot reads comes from a file
  committed to the workspace's repository, as tag instructions do. A plugin that wants a note
  gives the workspace a file to commit.

## 2. Core Concepts

| Term | Meaning |
|---|---|
| Note label | A short name the workspace chooses, such as `review_focus`. It keys the config table and the note |
| Note instruction | The committed file that tells the pilot what to write under a label |
| Pilot note | The pilot's text for one task under one label, at most 1 KiB |
| Assessment | What a pilot returns for a task, stored today as the task artifact `pilot/assessment-<operation-id>.json` ([task pilot](../task-pilot/1_scope.md)) |

## 3. At a Glance

| Concern | Design section | Phase |
|---|---|---|
| Naming a note and its instruction | [Configuration](./2_design.md#1-configuration) | 1 |
| What the pilot is told | [The pilot's prompt](./2_design.md#2-the-pilots-prompt) | 1 |
| What the pilot may call | [What the pilot may call](./2_design.md#3-what-the-pilot-may-call) | — |
| What the pilot returns, and what apply does with it | [Notes in the assessment](./2_design.md#4-notes-in-the-assessment) | 1 |
| Where a note is read | [Reading a note](./2_design.md#5-reading-a-note) | 1 |
| Pilots on another host | [Claimed pilots](./2_design.md#6-claimed-pilots) | 2 |
| The dashboard | [Reading a note](./2_design.md#5-reading-a-note) | 2 |

## 4. Open decisions

Two choices are the operator's. Each has a recommendation, and the design is written to it.

1. **Where a note lives.** *Recommended:* inside the assessment artifact, shown through the
   task's pilot readout. The alternative is one task comment per label. A comment is visible
   in the comment list with no new readout, but it spends the task's comment budget on every
   pilot, needs its own replace-in-place rule, and makes a second copy of what the assessment
   already holds ([design](./2_design.md#4-notes-in-the-assessment)).
2. **What a broken note instruction does.** *Recommended:* the pilot runs without that note
   and the gap is reported. The alternative is to refuse the pilot, as a broken tag
   instruction refuses its activity. A tag instruction is refused for the tasks that carry
   its tag; a note instruction applies to every task, so refusing would stop every admission
   in the workspace over an advisory text ([design](./2_design.md#2-the-pilots-prompt)).

## 5. Build

Build tasks are filed once the design is approved.

## Task References

- [ORB-29567] — this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
