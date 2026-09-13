---
summary: "Activity / Job — Vision"
type: design
title: "Activity / Job — Vision"
owner: codex
last_updated: 2026-07-20
last_validated: 2026-09-13
status: Draft
feature: activity-job
doc_role: vision
tags: ["activity-job"]
---

# Activity / Job — Vision

This document pressure-tests where the Activity / Job substrate may go next. It starts from the v2 runtime shipped across [T20260418-2010], [T20260418-2018], [T20260418-2143], [T20260418-2210], [T20260419-0104], and [T20260419-2156]. [2_design.md](./2_design.md) is the current contract; this file asks what should harden, collapse, or disappear.

---

## 1. Open Questions

### 1.1 How much of Activity / Job should remain public?

README frames tasks, jobs, and activities as substrate. Should Orbit keep this layer mostly internal, with goals/sessions/locks as the API, or does a durable local system still need human-editable workflows?

### 1.2 Should `target:` grow beyond `activity:<name>`?

`TargetRef` is namespace-prefixed, but only `activity:<name>` exists after [T20260418-2019]. Do nested job refs, subroutine refs, or cross-workspace refs help, or would they turn this layer into a generic workflow engine?

### 1.3 Is the provider model too closed-set?

The schema names more providers than Orbit can execute — `openai_compat` has no CLI runtime. Do we keep the closed enum and wire each runtime deliberately, or move toward a more explicit "provider capability" registry?

### 1.4 Should Orbit enforce tool allowlists itself?

Agent dispatch advises rather than enforces: the declared `tools:` list is delegated to the provider harness. Does the CLI agent path need a wrapper so `tools:` means the same thing everywhere?

### 1.5 Which limits should stay structural literals?

`task_auto_pipeline` relies on literal `max_workers` and `max_iterations`. Should those stay static, or do we need templated numerics?

### 1.6 What is the right audit landing zone?

The v2 envelope from [T20260419-0002] makes runs traversable, but the full agent transcript still lives in the sibling loop sink. Should review converge on one query surface even if files remain separate?

### 1.7 How much more special-casing belongs in ActivityV2?

Groundhog once had a dedicated kind ([T20260420-0510-2]) but was removed as unused in [ORB-10332], leaving only `agent_loop` and `deterministic`. If more modes arrive, do we add variants again, or is churn like that a sign the abstraction should stay minimal?

### 1.8 What should become the canonical reference corpus?

Seeded assets added in [T20260419-2347] and extended in [T20260419-0622-3] and [T20260419-0623] already act as executable docs. Should they become the main reference set, or do we need smaller spec-only exemplars?

---

## 2. Prior Work

Activity / Job productizes familiar ideas. The interesting question is whether Orbit's mix is local, inspectable, and agent-friendly enough to matter.

### 2.1 Durable workflow engines

- **Temporal** treats replay-safe orchestration and activity boundaries as first-class runtime concepts.
- **Argo Workflows** and **GitHub Actions** show that human-authored YAML orchestration is still valuable even when higher-level product abstractions exist.
- **LangGraph** has made durable, stateful control flow part of the mainstream agent-tooling vocabulary.

Activity / Job borrows the idea that orchestration must survive outside one model turn, but keeps the runtime local-first and repo-scoped.

### 2.2 Agent runtimes and tool loops

- **Claude Code**, **Codex**, **Gemini CLI**, and similar tools normalize the "agent loop plus tools" model.
- **OpenAI-compatible** and provider-native HTTP APIs normalize direct transport-level loops.

Orbit's difference is that a tool loop is one typed activity inside a durable runtime with local audit, policy, and job control-flow.

### 2.3 Policy-aware execution substrates

- Sandbox and policy systems in CI/CD platforms establish the norm that execution policy is part of the runtime contract, not just ambient environment.
- Agent harnesses frequently expose allowlists, but many treat them as soft guidance rather than a load-bearing runtime surface.

Orbit's `fsProfile` attachment makes this concern explicit, even if the current behavior is still uneven.

### 2.4 Executable reference assets

- Mature workflow systems almost always ship example pipelines, starter templates, or scaffolded defaults.

Orbit's seeded activities and jobs from [T20260419-2347] are already more than examples; they are the closest thing to an executable spec corpus.

---

## 3. What May Be Distinctive

Soft claims only:

- **Load-time normalization as a public contract.** Target-ref resolution and retired-declaration rejection are part of what a job *is*, not just hidden parser details.
- **One execution path, stated plainly.** Orbit retired agent backend selection rather than keeping a selector whose alternatives could not run ([ORB-10801]); a provider that cannot be executed fails structurally instead of silently recovering.
- **A two-layer audit tree tied to repo provenance.** The v2 envelope from [T20260419-0002] gives runs, steps, activities, and workspace origin a stable skeleton while still preserving the underlying loop transcript and blobs.
- **Seeded workflows as load-bearing contracts.** The shipped jobs from [T20260419-0622-3] and [T20260419-0623] are not toy examples; they are how the control-flow substrate proves itself against real Orbit work.

None of these are research contributions. Activity / Job earns its keep only if it stays understandable, local, and inspectable while the product evolves.

---

## 4. References

### Orbit-internal

- [1_overview.md](./1_overview.md) — feature purpose and core concepts
- [2_design.md](./2_design.md) — current implementation
- [4_decisions.md](./4_decisions.md) — ADR log
- [specs/backend-resolution.md](./specs/backend-resolution.md) — retired agent backend selection and its migration
- [specs/audit-envelope.md](./specs/audit-envelope.md) — v2 audit event tree and persistence layout
- [../_archive/knowledge-graph/1_overview.md](../_archive/knowledge-graph/1_overview.md) — graph substrate that sits beside this execution substrate

### External

- Temporal — https://temporal.io/
- Argo Workflows — https://argo-workflows.readthedocs.io/
- GitHub Actions — https://docs.github.com/actions
- LangGraph — https://langchain-ai.github.io/langgraph/

---

## Task References

- **[T20260418-2010]** — Add the first v2 activity runtime scaffolding.
- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[ORB-10332]** — Remove the unused Groundhog activity kind and the epic/parallel pipeline layer.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
