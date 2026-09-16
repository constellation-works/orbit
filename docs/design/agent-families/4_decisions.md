---
summary: "Agent Families — Decisions"
type: design
title: "Agent Families — Decisions"
owner: grok
last_updated: 2026-08-11
last_validated: 2026-08-29
status: Draft
feature: agent-families
doc_role: decisions
tags: ["agent-families"]
---

# Agent Families — Decisions

This document preserves the feature's non-obvious decisions and their reasoning.

---

## Add Grok (xAI) as a fourth peer agent family

**Recorded:** 2026-05-16 19:07:25.023260Z · [ORB-00042], [ORB-00043], [ORB-00044], [ORB-00045], [ORB-00046], [ORB-00049], [ORB-00050], [ORB-00052]

### Context

Orbit's agent modeling (introduced across activity-job, auditability, and policy-sandbox work) treated Claude, Codex, and Gemini as the complete set of first-class CLI agent families. `all_agent_families()` was deliberately a fixed-size array of 3, `agent_from_model` / `infer_agent_family_from_model` only recognized those three prefixes, executor YAMLs and macOS sandbox profiles existed only for those three, and `orbit mcp init` only knew how to configure those three clients.

Grok Build (and the xAI API surface) is now a real, actively used client in the Orbit development workflow. Treating it as an unknown/foreign agent produces invisible attribution, broken duels/scoreboards, unsafe sandbox execution, and inconsistent onboarding.

Real alternatives considered: (1) treat Grok as a variant of Codex/OpenAI-compat, (2) keep it as an unmodeled third-party agent forever, (3) add it as a true peer family with the same rights and obligations as the original three.

### Decision

We add "grok" as a fourth peer agent family alongside claude/codex/gemini.

This means:
- Extending `agent_from_model`, `infer_agent_family_from_model`, `all_agent_families()`, `resolve_agent_model_pair`, and `provider_from_model` to recognize Grok model strings and map them to a stable family identifier ("grok") and provider ("xai").
- Adding a `grok.yaml` executor definition and the corresponding CLI runner + sandbox support.
- Adding a Grok provider to the `orbit mcp init` machinery so it can generate `.grok/config.toml` entries.
- Updating all documentation, tests, duels, scoreboards, releasing processes, and repo-root configuration directories to treat Grok as a first-class peer.

### Consequences

- Grok-authored tasks, reviews, and commits will be correctly attributed and will participate in planning duels and analytics.
- `backend: cli` execution against Grok (via xAI-compatible wrapper or future official CLI) will be sandbox-safe on macOS.
- `orbit mcp init` will support Grok Build users with the same one-command experience as the other three agents.
- The fixed-size array contract in `all_agent_families()` will now be 4; every call site that assumed "exactly three" must be audited.
- Cost: We accept a permanent increase in the number of agent families we must maintain (executors, sandbox rules, MCP providers, model-pair defaults, docs). Future families will be cheaper to add, but each still carries non-trivial integration cost in sandboxing and client configuration.

## Replace `[agent.<role>]` tables with named `[crews.*]` registry

**Superseded by:** [Flatten crews to one provider-model assignment](#flatten-crews-to-one-provider-model-assignment)
**Recorded:** 2026-07-11 · [ORB-00058] · legacy_id: `agent-families/[Replace `[agent.<role>]` tables with named `[crews.*]` registry](#replace-agentrole-tables-with-named-crews-registry)`

**Context.** Workspace config previously selected planner, implementer, and reviewer models with three top-level `[agent.<role>]` tables, while task execution had no durable way to request a different lineup. Layering a new registry beside the old role tables would have forced Orbit to validate and explain two schemas for the same decision.

**Decision.** Replace the role-keyed config shape wholesale with named `[crews.<name>]` entries and `[workflow].default_crew`. A task may store `crew`, and a run may override it with CLI/tool input; precedence is CLI override, then task field, then workspace default.

**Consequences.**
- "Crew" was chosen over "profile" because profiles sound user-scoped, and over "pair" because the lineup contains planner, implementer, and reviewer.
- Run records persist the resolved crew plus the three role model strings so audit trails survive later config edits.
- The v2 `agent_loop` dispatch path reads role models from the crew registry (`crates/orbit-core/src/runtime/engine/environment_host.rs`). Scoreboard and friction projections use family identity after [Collapse agent identity to family and move model strings to configuration](#collapse-agent-identity-to-family-and-move-model-strings-to-configuration); exact model strings remain visible through resolved crew/run configuration.
- Deferred: duel-plan participant configuration, per-role task overrides, and planner-vs-executor workflow split.
- Cost: old workspaces with only `[agent.planner]`, `[agent.implementer]`, and `[agent.reviewer]` must migrate before config load succeeds.

## Scope duel-plan candidate and model overrides to `[duel]`

**Recorded:** 2026-05-17 05:48:49.830825Z · [ORB-00072]

### Context

Duel-plan previously walked the full `all_agent_families()` registry and used the same model-pair resolution chain as non-duel callers. That made local CLI availability load-bearing for every supported family and made reproducible planning-duel scoreboards depend on executor YAML state.

### Decision

Add a workspace `[duel]` section with `candidates` as a normalized subset of `all_agent_families()` and `[duel.models]` as flat orchestrator-only per-family overrides. Duel role selection reads those values through `DeterministicActionHost`; non-duel callers continue to use executor overrides and builtin model pairs.

### Consequences

- Duel permutations remain dynamic but require at least three distinct configured families.
- `[duel.models]` wins only for duel role-model lookup; helper models and non-duel model identity are unchanged.
- The crew registry remains separate from duel participant selection. Reusing `[crews.*]` for duels was rejected because duels need a family pool, not a fixed planner/implementer/reviewer lineup.
- Cost: duel-plan reproducibility now depends on a third configuration surface (`[duel]`) in addition to crew registry and executor overrides. Operators triaging a duel run must consult all three to explain a given family/model selection.

## Collapse agent identity to family and move model strings to configuration

**Recorded:** 2026-05-17 05:48:49.885727Z · [ORB-00080]

### Context

Planning-duel artifacts and scoreboards compared model strings even though model names drift across aliases, CLI shorthand, and self-reported tool payloads. A Gemini planner configured as `pro` could produce an artifact stamped `gemini-3.1-pro`; both values describe the same family but failed equality checks. Alias tables (`resolve_agent_model_pair*`, `matches_model_alias`, `canonical_model_for_agent`) treated the symptom and grew with every provider change.

### Decision

Family is identity, model is configuration, and slot is role. Orbit identity surfaces use exactly `codex`, `claude`, `gemini`, or `grok`. Planning-duel assignments persist `family`; `planner_a`, `planner_b`, and `arbiter` are explicit slots used in artifact paths and signatures. Exact model strings stay in crew config, `[duel.models]`, CLI invocation translation, and resolved-crew run records.

### Consequences

- New planning-duel artifacts are written as `planning-duel/{slot}.md` and signed `*authored by: {family} / {slot}*`; historical model-path artifacts remain a legacy read concern.
- Runtime tool boundaries treat envelope identity as authoritative. Agent-supplied `model` fields are overwritten with the canonical family before persistence/comparison so self-report drift cannot affect validation.
- Scoreboard and friction projections are family-keyed (`by_family`) when they answer "who actually ran?". Resolved-crew projections remain the source for "who was selected?" because they describe configured routing.
- The legacy resolver and alias-canonicalization surfaces are deleted from production code. `infer_agent_family_from_model` remains for legacy artifact recovery and CLI invocation translation.
- ORB-00079 and ORB-00071 are superseded by this structural identity change.
- Cost: model granularity is lost from identity comparisons. Two different Gemini model versions (e.g. `pro` vs `flash`) collapse to the same `gemini` identity in scoreboards; distinguishing them requires drilling into resolved-crew run records or `[duel.models]` configuration.

## Favor claude (opus) for planner role on planning duels and design-shaped plans

**Recorded:** 2026-05-18 · cites AO-002 · (acceptance pending a related task — see [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions))

**Context.** AO-002 ("Instruction surface shapes plan output, not tool selection") closed on 2026-05-18 after four experiments spanning four Gemini-as-planner implementation/audit duels and one 4-model cross-read on an identical UX-design task. Three observations recurred across the thread:

- Gemini ranked last on plan depth on every task shape tested (implementation, audit, refactor, UX-taste), losing every duel it played as planner. Arbiter rationales consistently cited graph-discovery gaps, hallucinated symbols, generic findings, and thin ADR content.
- Claude won every duel it entered as planner in the window, including the UX-redesign duel (ORB-00154), where claude's metric-major layout call was the differentiating taste signal. Codex placed in the middle — thorough plans without bold design calls, ranked above grok and gemini but below claude on plan depth in the 4-way UX comparison.
- Instruction-surface levers (memory file or per-run prompt) shifted Gemini's output structure (verification commands, severity-tagged findings, section format) but not its tool selection. Three rounds of prompt strengthening did not change the duel outcome.

AO-002 scope: planning-duel plan quality on the Orbit codebase, single window in May 2026, ad-hoc task selection, model versions not held constant (gemini-2.5-pro on two runs, gemini-3.1-pro-preview on three). The thread is explicitly framed as decision-grade-for-us, not an objective ranking.

**Decision.** Until new evidence warrants otherwise, default the planner role on planning duels and design-shaped plans to **claude** (currently the `opus` crew/model alias in the workspace defaults). This applies to both implementation-shaped planning and UX / design-shaped planning. The choice is provisional and bound to AO-002's evidence window; AO-002's open questions (post-rubric run on `gemini-3.1-pro-preview` against an implementation task, plan-depth response to a volume-rubric clause, new model-family releases) are the natural triggers for revisiting.

**Consequences.**

- Crew defaults that select a planner family should favor `claude` when no per-task override is set. `[duel.candidates]` continues to include all four families so duels remain genuine plan-vs-plan comparisons; this ADR governs single-planner selection and tie-breaking guidance, not duel participation.
- Arbiter selection is unchanged. AO-002 finding D observed that Gemini-as-arbiter performed adequately even where Gemini-as-planner did not; reading completed work is a different cognitive task than producing it.
- Implementer selection is unchanged. AO-002's scope is planning-duel plan quality only; implementer rankings live in a separate observation thread when there's data.
- Re-evaluation triggers: (1) a new Gemini-family release with a stable model alias; (2) the missing AO-002 experimental cell (post-rubric Gemini run on `gemini-3.1-pro-preview` against an implementation-shaped task) producing a counter-finding; (3) a non-Orbit codebase producing a different ranking; (4) a same-task within-model-version repeat that flips the outcome. Any of these reopens AO-002 or spawns a follow-up observation that this ADR must be reconciled against.
- Cost: surrendering planning diversity. Defaulting to one family forfeits the safety net of cross-family disagreement, concentrates dependency on a single provider, and risks anchoring on claude's distinctive design patterns (e.g. the metric-major preference observed in ORB-00154) as if they were universally correct. The duel mechanism partially mitigates this when explicitly invoked: a duel still gathers multiple plans before selecting one.

## Default Claude to opus/sonnet CLI aliases; centralize model defaults in orbit-common::model_defaults

**Recorded:** 2026-08-01 19:17:27.707459Z · [ORB-10051], [ORB-10479]

**Context.** Default model names were hardcoded as version-pinned string literals scattered across ~7 production sites, and the pins had drifted out of sync: the default Claude model appeared as `claude-opus-4-7` (`agent_detect`, seeded crews, `claude.yaml` strong), `claude-sonnet-4-6` (`claude.yaml` weak), and `claude-sonnet-4-5` (`exec_ctx::DEFAULT_MODEL_FOR_SESSION`, `agent_loop_driver::DEFAULT_ANTHROPIC_MODEL`) depending on the code path. The Claude CLI accepts the unversioned `opus`/`sonnet` aliases, which never drift.

**Decision.** Introduce `orbit-common::model_defaults` as the single source of truth for production default model names; every production default now references a constant there (`agent_detect::default_model_for` delegates to `default_model_for_provider`; seeded crews, the Anthropic HTTP session/loop defaults, and the dashboard ADR/friction tool models reference the constants). The default Claude CLI model becomes the unversioned `opus` (strong) / `sonnet` (weak) aliases — planner+reviewer=opus, implementer=sonnet — applied to `assets/executors/claude.yaml` and the Rust crew/duel seeds. codex/gemini/grok keep their existing values (no unversioned aliases invented for CLIs that may not accept them). The Anthropic **HTTP Messages API** default stays version-pinned (`claude-sonnet-4-5`, `ANTHROPIC_HTTP_DEFAULT_MODEL`) because the Messages API rejects bare aliases. Tests keep referencing model strings via frozen `orbit-common::test_fixtures` constants rather than being deleted.

**Consequences.**
- One edit updates every production default; the opus-4-7 / sonnet-4-6 / sonnet-4-5 drift can no longer recur.
- Fresh workspaces seed `opus`/`sonnet` for the claude crew and duel default; existing workspaces are unchanged until `orbit init --refresh-defaults` (config.toml is never overwritten; executor defs re-seed only on refresh).
- Asset ↔ const seam: YAML/TOML assets cannot reference a Rust const, so `claude.yaml` uses the alias directly while `model_defaults` stays authoritative for Rust paths; an executor-asset guard test pins the `claude.yaml` pair to `{CLAUDE_DEFAULT_STRONG, CLAUDE_DEFAULT_WEAK}`.
- Scoreboard attribution matches model strings exactly, so historical review/duel artifacts recorded as `claude-opus-4-7` stop matching the new `opus` pair; only new runs match. A family-equality fallback was considered and left as a possible follow-up.
- Cost: default model names now live in two layers (Rust `model_defaults` const for code paths, literal alias duplicated into the executor/config assets); a future model bump must touch both the const and the YAML asset, and the asset↔const guard test is what keeps them honest.

## Flatten crews to one provider-model assignment

**Recorded:** 2026-07-11 19:53:22.638085Z · [ORB-10130]
**Supersedes:** [Replace \[agent.<role>\] tables with named \[crews.*\] registry](#replace-agentrole-tables-with-named-crews-registry)
**Paths:** `crates/orbit-types/src/identity/agent_pair.rs`, `crates/orbit-config/src/**`, `crates/orbit-core/src/runtime/**`, `crates/orbit-store/src/**`, `crates/orbit-web/src/**`, `docs/CONFIG.md`

### Context
Named crews currently carry separate planner, implementer, and reviewer assignments, but production crews are homogeneous and only the implementer is on the primary ship path. Role labels still matter for prompts and telemetry, while model selection through three independent slots adds configuration and persistence complexity without selecting distinct behavior.

### Decision
A crew is one provider-model-backend assignment. Every activity role resolves to that assignment; role labels remain descriptive, duel participant selection stays independent, and legacy three-role config is accepted by choosing implementer while warning when the discarded roles diverge.

### Consequences
- Per-task and default crew selection now directly choose one provider-model binding.
- Run records and projections expose one crew model; legacy SQLite role columns remain nullable for compatibility.
- Existing homogeneous legacy crew configuration continues to load without behavior changes.
- Cost: Deliberately heterogeneous legacy crews collapse to their implementer assignment and require a warning-guided config rewrite; cross-provider review must use duel machinery or a future explicit mechanism.

## Freeze Agent Detection at Init Seeding

**Recorded:** 2026-05-30 20:17:29.805438Z · [ORB-00347]
**Paths:** `crates/orbit-config/src/**`, `crates/orbit-core/src/command/init.rs`, `crates/orbit-cli/src/command/init.rs`, `crates/orbit-config/assets/default-config.toml`

### Context
`orbit init` needs a config that reflects installed agent surfaces, but live detection reads ambient PATH and API-key environment. Re-running detection during `RuntimeConfig::load_layered` would make crew and duel resolution vary between invocations without a config diff.

### Decision
Agent availability is detected once during init using `DetectedAgents`, rendered into `config.toml`, and then treated as static configuration. Runtime config loading continues to use file contents and built-in fallbacks only; it never probes PATH or environment.

### Consequences
- Fresh configs pick a sensible `default_crew` and duel candidate set for the host that created them.
- Runtime behavior is deterministic for a given config file, including hot config loads.
- Cost: A user who installs or removes agent CLIs after init must edit or regenerate config instead of getting automatic runtime drift.

## Retire crew role slots and role-based model resolution

**Recorded:** 2026-08-09 06:33:12.872319Z · [ORB-10620], [ORB-10621], [ORB-10622]
**Paths:** `crates/orbit-types/src/identity/agent_pair.rs`, `crates/orbit-types/src/workflow/activity_job/activity_v2.rs`, `crates/orbit-types/src/workflow/activity_job/job_v2.rs`, `crates/orbit-config/src/**`, `crates/orbit-core/src/runtime/**`, `crates/orbit-engine/src/activity_job/**`, `crates/orbit-core/assets/activities/**`, `docs/CONFIG.md`

### Context

[Flatten crews to one provider-model assignment](#flatten-crews-to-one-provider-model-assignment) flattened a crew to one provider-model-backend assignment but kept two compatibility surfaces: legacy `planner`/`implementer`/`reviewer` sub-tables accepted at config load, and an `AgentRole` label carried on activities and job steps. Both are now inert. A legacy crew must supply all three sub-tables, and load keeps the implementer assignment while discarding the other two behind a warn-level log when they diverge; role-based resolution returns the run's single crew assignment for every role, so an activity declaring `role: reviewer` runs on the run's resolved crew. Three routing mechanisms are documented; only an explicit `crew` input actually selects a different model, and a declared role currently pre-empts it. The alternative was to keep the shims indefinitely, at the cost of a schema that advertises selection it does not perform.

This amends two clauses of [Flatten crews to one provider-model assignment](#flatten-crews-to-one-provider-model-assignment): that legacy three-role config is accepted by choosing implementer with a warning, and that role labels remain as descriptive resolution inputs. [Flatten crews to one provider-model assignment](#flatten-crews-to-one-provider-model-assignment)'s core decision — one assignment per crew — stands and is completed here.

### Decision

Crew configuration accepts flat `provider`/`model` only; the retired `backend` key is accepted only as inert `cli` or rejected at load. Legacy role sub-tables are rejected at load with rewrite guidance rather than collapsed. `AgentRole` and the role-to-assignment resolution path are removed from config, runtime, and asset schemas. Routing becomes: an activity or step with an explicit `crew` input dispatches on that crew, and one without dispatches on the run's resolved crew — replacing today's inline-baseline fallback, so no activity is left without a crew when its role is removed.

### Consequences

- Two routing mechanisms collapse to one, so an activity's crew is either named in its input or is the run's.
- A heterogeneous legacy crew now fails loudly at load instead of losing its planner and reviewer assignments behind a log line.
- System activities that need a model distinct from the run's crew must name a configured crew through an input, making that choice visible in config rather than in engine code.
- Cost: breaking config change. Any workspace carrying the three-role shape fails to load until rewritten, and shipped activity assets carrying `role:` must be reseeded in the same release.
- Cost: the planning-duel override carve-out described by the superseded crew decision was removed with the planning-duel path; current activity assets use explicit crew input or the run's resolved crew.

## Task References

- ORB-00042: Onboard Grok (xAI) as a first-class supported agent family.
- ORB-00058: Introduce per-task crew override for agent model selection.
- ORB-00072: Make duel-plan agent pool and per-family model configurable via `[duel]`.
- ORB-00080: Collapse agent identity to family; isolate model strings to invocation surface.
- ORB-10130: Flatten each crew to one provider-model assignment.
- ORB-10620: Reject legacy crew role sub-tables at config load.
- ORB-10622: Retire activity roles and role-based crew resolution.
- ORB-10627: Remove the planning duel and its parallel model-selection path.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
