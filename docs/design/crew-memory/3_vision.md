---
title: Crew Memory — Vision
owner: claude
last_updated: 2026-11-10
last_validated: 2026-11-08
status: Accepted
feature: crew-memory
doc_role: vision
type: design
summary: "Open questions for crew memory after the first experiment: cross-workspace memory, reviewer and pilot experiments, automatic promotion, and what prior work on agent reflection suggests."
tags: [crew-memory]
paths: ["crates/orbit-core/src/application/crew_memory/**"]
related_features: [experiments, agent-scorecards]
related_artifacts: [ORB-23105]
---

# Crew Memory — Vision

This doc looks forward. Everything here is speculation until the first experiment concludes (see [2_design.md §7](./2_design.md#7-the-experiment-and-adoption)). If that readout doesn't favour memory, the feature is removed and none of this is built.

## 1. Open Questions

1. **Cross-workspace memory.** Spaces are per workspace, because the evidence and the experiments are. A lesson such as "run the full test target before landing a migration" may hold in every Rust repository. Should the global memory have a host-wide layer, and who reviews it?
2. **Reviewer and pilot experiments.** The first experiment covers only the implement role (§7). A reviewer's outcomes are missed regressions, which take 30 days to settle, and the experiment's metrics don't include reviewer catches or misses yet. A pilot runs before delivery admission, so it can't take a delivery-assigned arm. Each needs its own assignment point and metrics before its memory can be measured.
3. **Automatic promotion.** When two or more models independently write the same lesson with disjoint evidence, can that agreement count as the review, or does every global note need an operator?
4. **Spaces of different sizes.** A model with ten times the volume of another gets the same 6 KiB. Should the cap scale with support, or does a fixed cap force better compression?
5. **Forgetting.** Memory is rewritten whole each week, so a model can drop a note that was still useful. Should a note that a later readout links to better outcomes be pinned?
6. **Carrying memory to a new model.** When a model is upgraded, is its predecessor's space a useful prior or a source of stale habits? A carried-over space could itself be an experiment arm.
7. **Memory as a crew-performance signal.** Could revision history tell us anything, for example a model that rewrites the same lesson every week without improving on it?

## 2. Prior Work

### Reflection over trial outcomes

Reflexion (Shinn et al., 2023) has an agent write a verbal reflection after a failed trial and keep it in an episodic buffer for the next trial. Within a task it improves results on coding and reasoning benchmarks. Crew memory applies the same loop across tasks and on a weekly cycle. The feedback comes from the downstream outcomes Orbit records, not from a test run inside the trial.

### Experiential learning across tasks

ExpeL (Zhao et al., 2023) gathers experiences across training tasks, extracts insights from them, and recalls those insights at inference. Its insight list is edited with add, edit, upvote and downvote operations, which is close to the weekly rewrite proposed here. The difference is that ExpeL's insights are judged by benchmark success, while crew memory is judged by a randomised production experiment.

### Memory and reflection in agent architectures

Generative Agents (Park et al., 2023) periodically condense observations into higher-level reflections, which is the "dreaming" step. MemGPT (Packer et al., 2023) treats context as a managed memory hierarchy with a fixed budget, which is the idea behind the size caps. Voyager (Wang et al., 2023) keeps a growing library of verified skills. Crew memory deliberately does not grow: caps and weekly rewrites keep it small.

### Provider memory files

Coding agents already read persistent instruction files such as `CLAUDE.md` and `AGENTS.md`, and some keep automatic memory. Those files are global to a repository or a user and aren't measured. Crew memory is scoped to one model, cites its evidence, and is gated by an experiment.

## 3. What May Be Distinctive

- **Score-blind reflection over attributed outcomes.** The evidence pack is cut from scorecards in which each problem counts against exactly one role, so a model reflects only on problems it caused, and it never sees the score.
- **Memory judged by a randomised production experiment,** with intention-to-treat analysis and guardrails, not by a benchmark or an operator's impression.
- **Revisions bound to deliveries by digest,** so any run can be traced to the exact memory it received.

Nothing here is proven yet.

## 4. References

**Orbit-internal**

- [Crew Memory — Design](./2_design.md)
- [Experiments scope](../experiments/1_scope.md)
- [Agent Scorecards — Design](../agent-scorecards/2_design.md)
- [Project Learnings — Decisions](../project-learnings/4_decisions.md)

**External**

- Shinn, N. et al. *Reflexion: Language Agents with Verbal Reinforcement Learning.* NeurIPS 2023. arXiv:2303.11366.
- Zhao, A. et al. *ExpeL: LLM Agents Are Experiential Learners.* AAAI 2024. arXiv:2308.10144.
- Park, J. S. et al. *Generative Agents: Interactive Simulacra of Human Behavior.* UIST 2023. arXiv:2304.03442.
- Packer, C. et al. *MemGPT: Towards LLMs as Operating Systems.* arXiv:2310.08560, 2023.
- Wang, G. et al. *Voyager: An Open-Ended Embodied Agent with Large Language Models.* arXiv:2305.16291, 2023.

## Task References

- [ORB-23105] — designs crew memory (this folder)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
