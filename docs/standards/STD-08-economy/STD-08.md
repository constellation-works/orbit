---
id: STD-08-economy
title: Economy — bounded agent context, delta-only records, one channel per ask, bookkeeping off the page, one-line errors, complete stored records, size stated at design time
summary: Normative rules for designing anything an agent or operator reads repeatedly (prompts, briefs, tool results, task comments, checkpoints, errors, logs, reports) with an economical mindset — a fixed size that does not grow with history, each fact once and referenced by id, delta-only records, machine bookkeeping kept out of the readable body, asks in their own channel, outcome first, one-line errors with the diagnostic behind a detail view and repeats collapsed, and the steady-state and repeated size stated before it ships — while errors, logs and audit records stay complete and structured when stored; follow it in any project where agents produce or consume text.
tags: [standard, agents, tokens, design]
version: 3
created_date: 2026-11-15
updated_date: 2026-11-25
last_validated: 2026-11-15
related: [STD-01-cli-surface, STD-07-ui-ux]
---

# Economy

These rules make a design pick the smallest form that serves its reader. Every token an agent reads is paid for on every read, and every line a person reads costs attention. A design that is thorough by default grows with each cycle, run and comment until its readers skim or pay for what they skip.

Economy applies to what is put in front of a reader. It does not cut engineering rigour, tests, evidence or scope: detail is moved somewhere reachable, never deleted.

Other standards own the neighbouring rules:
- **Screens** (minimal words, answer first, show each thing once) are [STD-07](../STD-07-ui-ux/STD-07.md) §R13, §R16 and §R17.
- **CLI output and `--json` shapes** are [STD-01](../STD-01-cli-surface/STD-01.md).

This standard is a directory. This file holds the binding text. Beside it, [`why.md`](why.md) gives the reason for each rule, [`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Design

- **R1.** A new prompt, brief, record, comment, report or tool output MUST state, in its task or design doc before it ships: who reads it, what each reader reads it for, and its size at steady state and after realistic repetition (for example 24 cycles of an 8-hour mandate). A design that cannot state these is not ready.
- **R2.** Where thoroughness and economy conflict, a design MUST choose the smaller form and keep the detail reachable by id, link, artifact, `raw` view or "show full". It never inlines detail "in case".

### Context agents read

- **R3.** Context an agent rereads (a prompt, a wake brief, a digest, a tool result) MUST have a fixed upper size that does not grow with the age or history of the thing it describes. History past the bound is summarised in a bounded form or left for the agent to fetch by id.
- **R4.** A context MUST state each fact once. It refers to a document, record or schema the agent can open by path or id instead of restating it, and it does not repeat what an earlier part of the same context already says.
- **R5.** A read tool MUST return a compact default and offer a projection (fields, ids, a limit) for more. Full records are opt-in.
- **R12.** An agent that finds stale, incomplete, unclear or suboptimal wording or delivery in guidance it reads (a skill, an agent instruction file such as AGENTS.md, a domain doc, a runbook or a design doc) MUST NOT leave it as it found it. Where the fix is certain, small and within its task and authority, it MUST make the fix in the same change. Otherwise (the fix needs a decision, belongs to another owner, or its authority does not reach the file) it MUST file a fix task or a friction naming the file and the defect before its session ends. Working around the defect without either is not allowed.

### Records agents write

- **R6.** A record written on every cycle or run (a checkpoint, status, progress note) MUST carry only what changed since the previous one and what comes next. Standing state lives in structured fields carried forward, never retold in prose. Free-text fields in such a record have a small cap.
- **R7.** Machine bookkeeping (schema ids, digests, hashes, ledger keys, authority snapshots, cost rows) MUST NOT be part of a record's readable body. It lives in structured fields a renderer hides, and a record meant for people renders as a summary with its raw form one action away.
- **R8.** An ask for a person MUST go through the channel built for it (a decision request, a notice, a request field) once, and be closed there when it is answered or moot. It is not repeated in prose on every record.
- **R9.** Text written for a reader MUST lead with the outcome. It does not restate the request, narrate the steps taken, or close with a recap of what it already said.

### Errors, logs and audit

- **R10.** An error or failure shown to a reader MUST say in one plain line what failed, why, and the next step. Its structured diagnostic (codes, paths, ids, serialized payloads) is kept in a field or behind a detail view, never interpolated into the message. Repeated identical failures collapse into one entry with a count and the time range. [STD-02](../STD-02-rust-architecture-and-errors/STD-02.md) owns which cause an error names; this rule owns how much of it is shown.
- **R11.** Errors, logs and audit records MUST be complete when stored. Each keeps its full structured diagnostic, and each logged event or audited action records who acted, what was done, when, its inputs (or a digest of them) and its outcome, as structured fields rather than prose. Nothing is trimmed from a stored record to save space; only a declared retention policy removes records. Economy applies to how a record is shown and reread (R5, R7, R10), never to what is recorded.

## Applies to

`universal`: any project where agents produce or consume text, including prompts, tool results, task comments, run records, reports and logs.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing `STD-08@<version> §Rn` and the reason. It never edits its vendored copy of this standard.
