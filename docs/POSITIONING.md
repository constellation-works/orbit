---
type: context
summary: "Orbit Positioning"
last_validated: 2026-09-05
---

# Orbit Positioning

This document names what Orbit is for, who it's for, and what it deliberately isn't. Use it as a decision lens — when a design debate feels like it's really about *what Orbit is for*, reference this doc instead of re-litigating case by case.

## What Orbit is for

**Applying engineering rigor to AI-assisted coding.**

Coding agents are fast enough that the disciplines that keep code maintainable — planning before edits, decision records for load-bearing choices, audit trails, conflict-aware parallel execution — become tempting to skip. Orbit makes those disciplines cheap and enforces them by default: every change starts as a task, every load-bearing decision is written into the design docs for the feature it governs, every tool call lands in a structured audit log, and parallel runs are dispatched into worktrees with file-level locks.

The audience is the individual engineer driving multiple coding agents against real code and unwilling to trade engineering rigor for raw throughput. Agent vendors solve in-session execution; Orbit is the layer above that turns individual agent sessions into a coherent, traceable body of work.

**Orbit is a free, self-hosted, permissively licensed OSS project. There is no paid tier, no hosted offering, no commercial roadmap. Whatever ships, ships in the OSS repo.**

## Who Orbit is for

The AI-native engineer running multiple coding agents (Claude Code, Codex CLI, Gemini CLI, plus any OpenAI-compatible or Ollama-served model) heavily, who has outgrown the in-session model and wants engineering discipline around their AI-assisted work — tasks, decision records, a searchable docs corpus, audit, sandboxing, parallel dispatch.

Staff and principal engineers, tech leads, and founding engineers fit the same profile. If they bring Orbit into their team's workflow, that's a natural extension — but the project is not positioned for team conversion. Orbit optimizes for the individual engineer who refuses to vibe-code their way through agent-driven development.

## What Orbit is NOT for

- **Generic workflow orchestration.** n8n, Airflow, LangGraph, Temporal — Orbit is a coding-agent framework, not a workflow engine.
- **A framework for building agents.** LangChain, AutoGen, CrewAI, Mastra cover that. Orbit is the layer that governs what agents do *for you*, not the runtime they execute inside.
- **Hidden cloud dependencies.** Orbit must never phone home.
- **Vendor lock-in to one LLM provider.** Cross-provider is table stakes.
- **Black-box agent decisions.** Every agent decision should be inspectable.
- **Onboarding designed for non-technical users.** Patronizing and misaligned with the audience.
- **Per-account subscription arbitrage.** The wedge drives multiple agents in parallel against real code; that breaks the single-personal-CLI-account assumption.
- **Team-scale features.** Cross-engineer aggregation, SSO/SAML, RBAC, multi-tenant hosting are explicitly out of scope. If you need them, fork — don't expect them upstream.

## Primary focus: auditability

Auditability is a product feature, not a cross-cutting concern. When something goes wrong, the operator answers *what / why / who* without asking anyone for help.

- **Complete coverage** — every operation that touches code, state, or external services emits an audit event. Silent paths are bugs.
- **Structured, queryable events** — typed records with stable schemas, exportable to your own observability stack.
- **Faithful reproducibility** — prompts and responses stored verbatim (configurable redaction). Summaries are derived, not replacements.
- **Tamper-evident retention** — append-only, verifiable.
- **Agent-identity attribution** — every write carries the identity of the agent (and model) that produced it.

When auditability conflicts with performance, ergonomics, or feature surface, auditability wins.

## Non-negotiables

- **Self-hostable under permissive license.** Single binary, no mandatory cloud dependency. MIT.
- **Bring-your-own-credentials.** API keys belong to the operator; Orbit is pass-through.
- **HTTP/SDK-first provider communication.** CLI shell-out is an escape hatch, not the backbone.
- **Audit trail for everything that touches code.** See above.
- **Intent attribution at the codebase level.** `task_id` in commit messages, queryable, durable across rewrites.
- **Reproducibility where possible, recorded non-determinism where not.**
- **Source-native inspection.** Agents use direct file reads and ubiquitous
  search tools such as `rg`, keeping repository inspection transparent and
  independent of a derived index.
- **Sandboxed-by-default execution** on supported platforms. Disciplines that can be machine-enforced should be.
- **Cost-visible.** Operator knows what each run costs in tokens and wall-clock.
- **Git- and GitHub-native.** No custom VCS abstractions.
- **Configurable, not rigidly opinionated.** Job DAGs, activities, skills, role profiles are YAML data, not code.

`task_id` is locally meaningful by design — a personal search key for the task author, recorded in local audit. Not resolvable on another engineer's machine; for cross-engineer references, use `external_refs` to link tasks to your team's tracker (Jira, Linear, GitHub Issues, etc.).

## The decision lens

When a design decision is contested, the tiebreaker:

> **Would this hold up for an engineer who insists on engineering rigor while driving multiple agents against real code?**

If the honest answer is no, it doesn't ship.

Secondary lenses, in order:

1. **Does this make a discipline cheaper to apply, or does it just add ceremony?** Friction-free disciplines get adopted; ceremonial ones get worked around.
2. **Would this still hold up at 10× the current agent fleet size on a single host?**
3. **Can the operator debug this without asking the maintainers?** If not, the feature is under-instrumented.
4. **Does this survive losing confidence in a single provider?**

## Boundaries (when to reconsider)

This positioning is not permanent. Reconsider if:

- The "engineering rigor" framing fails to resonate after a serious distribution effort — no substantive replies, no downloads-with-retention, no community.
- Trust in coding agents matures dramatically faster than expected and the disciplines Orbit enforces start to feel like dead weight rather than safety net.
- A coherent community fork appears wanting an alternative direction. That's a signal to talk, not to ignore.
- Auditability stops being differentiating — e.g., agent vendors ship native per-session audit at parity with Orbit's structured event model.

Until one of those happens, the framing above is the lens.
