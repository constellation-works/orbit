---
type: design
summary: "Proposal (Draft, nothing built): one larger model consolidates every memory space into trigger-and-action notes and keeps or retires each note on a ledger computed against the control arm"
last_validated: 2026-11-19
title: Spec — Outcome-checked memory consolidation
owner: claude
status: Draft
feature: crew-memory
tags: [crew-memory, spec, experiments, review-gate]
related_features: [crew-memory, experiments, agent-scorecards, review-gate]
related_artifacts: [ORB-25838, ORB-23105, ORB-23117]
last_updated: 2026-11-19
---

# Spec: Outcome-checked memory consolidation

**This is a proposal. Nothing here is built, and it changes four accepted decisions (§8), so
the design and every slice in §10 wait for Daniel.** [2_design.md](../2_design.md) stays the
description of what runs today. If this is accepted, its reasoning moves into
[4_decisions.md](../4_decisions.md) and its mechanisms into `2_design.md`.

The contract it proposes: a memory note is one instruction an implementer can act on and a
reviewer can check; one operator-chosen model writes every space; and a note stays only while
the tasks that received it show fewer of the findings it targets than the tasks that did not.

## Why This Exists

The live experiment `exp-5342a4d6` (memory on against memory off, implementers only) reached
its horizon with the verdict "no difference". Read at 2026-11-20T01:35Z, 419 tasks:

| Measure | Control | Memory | Difference [95%] |
|---|---|---|---|
| First-pass acceptance (primary) | 117/175 (66.9%) | 92/156 (59.0%) | −7.7 pt [−17.8, +3.0] |
| Cost per task | $1.56 | $1.97 | +$0.38 [−0.09, +0.85] |
| Bytes read before the first edit | 116 KB | 183 KB | +62 KB [+18, +112] |

A closer read of the same tasks ([ORB-25838] has the method) found three things. They are
post-hoc slices with bootstrap intervals computed for this proposal, not part of the readout.

1. **The reviewer draw moves the primary metric more than the arm does.** The first verdict
   came from one of three reviewer crews, and their acceptance rates differ by up to 70 points:

   | First reviewer | Control accepted | Memory accepted |
   |---|---|---|
   | `muse-spark@xhigh` | 66/73 (90%) | 56/63 (89%) |
   | `luna@xhigh` | 31/44 (70%) | 26/47 (55%) |
   | `haiku@xhigh` | 20/57 (35%) | 9/45 (20%) |

   Stratified by reviewer, the memory arm is −9.5 pt [−17.9, −0.8]. The deficit sits under the
   two stricter reviewers (−15.1 pt [−27.5, −2.2]); under `muse-spark` the arms are level.
2. **The extra misses are small.** Tasks whose worst first-verdict finding was `low` rose from
   16% to 21%. `medium` (10% to 11%) and `nit` (7% to 8%) barely moved. About one task in
   thirteen failed first-pass acceptance on nits alone, in both arms.
3. **The notes describe failures; they do not say what to do.** A typical note reads "changing
   one surface without checking all related facts". `gpt-6-luna` carried notes on exactly that
   class from 2026-11-14. On 2026-11-18 its memory-arm tasks were accepted first time 0 of 5,
   on findings of that class, against 2 of 2 in control. The readout cannot show that a note
   steered work wrong, only that the notes prevented nothing while the arm read more and cost
   more.

Daniel's starting point (2026-11-19): "have a bigger model consolidate the memory", and "the
consolidating agent should look at effectiveness of previous memory and continuously adjust to
improve it."

This proposal keeps both ideas and changes how each is done:

- **A bigger writer, and a different thing to write.** The same evidence and the same brief
  given to a larger model would produce a better-written retrospective. The note's form
  changes with the writer (§1).
- **Effectiveness per note, not per space, and slowly.** A model gets 5 to 20 reviewed tasks a
  week and acceptance swings with the reviewer, so a writer that adjusts every week on
  acceptance would chase noise. Each note is judged on the findings it targets, against the
  control arm, after a minimum exposure (§4, §5).
- **The signal is repaired first.** A loop that optimises first-pass acceptance as measured
  today learns to satisfy one reviewer's nits (§3, §6).

## 1. Notes

A space holds notes, not free Markdown. Each note has:

| Field | Meaning | Delivered to implementers |
|---|---|---|
| `id` | Stable slug, unique in the space | no |
| `when` | A condition the implementer can recognise from the task or the diff: a path, a kind of change, a surface | yes |
| `do` | One action | yes |
| `check` | What in the diff or the handoff shows the action was taken | yes |
| `class` | One sentence defining the findings this note is meant to prevent; the ledger's unit (§4) | no |
| `evidence` | Task ids from the evidence pack | no |
| `state` | `trial` or `kept` (§5) | no |

Example: `when` a change edits a prompt or skill file; `do` search for every copy of the edited
rule and update each in the same change; `check` the handoff lists the copies found.

- A model space holds at most 6 notes and the global memory at most 6, each at most 400 bytes
  of `when`, `do` and `check` together. That is under half of today's 9 KiB ceiling.
- The five write rules of [2_design.md §4](../2_design.md#4-what-a-space-may-hold) still apply,
  to the delivered fields. A write with a note that lacks any field is refused.
- An empty space is a valid result and delivers nothing.

## 2. The Consolidator

- **Who.** A new key `crew_memory.consolidator_crew` names the crew that writes every space. It
  has no default: while it is unset the weekly routine mints nothing and records
  `no_consolidator`. Which crew is the operator's choice.
- **Per-key tasks.** The routine mints one task per qualifying key as today
  ([2_design.md §6.1](../2_design.md#61-minting)), pinned to the consolidator crew in place of
  the model's base crew. Each task classifies the week's findings (§4), reads the ledger, and
  rewrites that key's notes under the rules in §5.
- **One global task.** A further task depends on the per-key tasks. It reads every key's notes
  and ledgers and opens global-memory proposals. It replaces the two proposals each model may
  open today. Operator review of the global memory
  ([2_design.md §8](../2_design.md#8-the-global-memory-and-its-review)) is unchanged.
- **Tools.** `orbit.memory.show`, `orbit.memory.write` and `orbit.memory.propose` as today,
  plus `orbit.memory.classify` and `orbit.memory.ledger` (§4), granted only to these tasks and
  scoped to the task's key. The global task reads every key and writes none.
- **Cost.** Charged to the memory arm as today
  ([2_design.md §6.4](../2_design.md#64-cost)). It will rise, by an amount not yet measured.

## 3. Evidence

The pack is built as today ([2_design.md §6.2](../2_design.md#62-the-evidence-pack)) with four
changes:

1. Each review finding carries its severity and a stable finding id.
2. Findings of severity `nit` are left out. A task whose only findings were nits contributes
   nothing.
3. Findings from both arms are included, as today, and the arm is not shown.
4. The reviewer's identity stays hidden from the consolidator. Orbit uses it only in the ledger
   arithmetic.

## 4. The Note Ledger

The ledger answers one question per note: since this note was written, did tasks that received
it show fewer findings of its class than tasks that did not?

1. **Classify, blind to the arm.** For every finding in the pack the consolidator records, with
   `orbit.memory.classify`, the note of that key (or the global note) whose `class` it falls
   in, or `none`. It cannot see which arm a task was in, so a generous or a strict reading of a
   class moves both arms alike.
2. **Compute, in Orbit.** For each note, over the time since it was first written:
   - memory arm: the key's reviewed tasks whose delivered revision contained the note, and how
     many had a finding of its class;
   - control arm: the key's reviewed tasks in the same period, and the same count;
   - both stratified by first reviewer, with the stratified difference and its interval;
   - `exposure`: the number of reviewed memory-arm tasks that received the note.
3. **Cost of the space.** Per key, the ledger also gives implement tokens and bytes read before
   the first edit, by arm, so the consolidator sees what its notes cost together.
4. **Read.** `orbit.memory.ledger` for the consolidator; `orbit memory ledger <key>` and the
   dashboard's Memory tab for the operator. Classifications and ledger rows are append-only
   and kept with the revision history.

The delivered notes never contain ledger numbers; the score-blind lint on delivered text is
unchanged.

## 5. Adjustment Rules

Orbit enforces the first two at write time and refuses a write that breaks one.

1. **Exposure floor.** A `trial` note keeps its `when`, `do` and `class` until its exposure
   reaches 20 or it is 4 weeks old. Before that, only an operator reset removes it.
2. **Churn cap.** At most 2 notes are added or replaced per key per week.
3. **Verdict at the floor.** The consolidator then marks the note `kept`, replaces it or
   retires it, with a reason that cites the ledger row. The brief's rule: retire a note whose
   memory-arm rate is not below control's, and a note whose class produced no finding in
   either arm for 4 weeks. A replacement is a new note with a new ledger.
4. **Fewer and narrower when the space costs more.** When the key's memory arm reads or spends
   more than control, the brief tells the consolidator to cut notes or narrow their `when`.

## 6. Measurement Changes

These belong to [experiments](../../experiments/1_scope.md) and are worth having whether or
not memory survives.

- **Reviewer strata.** For every review-derived metric the readout reports the difference
  stratified by first reviewer next to the unadjusted one, and the per-reviewer rates.
- **`reviewer_spread` warning.** Raised when first-verdict acceptance differs between reviewers
  by more than 30 points.
- **`first_pass_clean`.** A new metric: the first verdict carries no finding above `nit`.
  Proposed as the primary for the next memory experiment.

Not proposed here: changing which crews review, or how a reviewer is drawn. One reviewer that
accepts 90% of candidates gave 41% of the first verdicts, which is a review-gate question.

## 7. The Next Experiment and the Exit Rule

- `exp-5342a4d6` is concluded with no arm chosen.
- A new experiment `crew-memory-consolidated` runs the same two arms and population, live, with
  primary `first_pass_clean` stratified by reviewer, the same guardrails, `min_sample` and
  6-week cap.
- **One more round, then the original exit.** If that readout does not favour memory, the
  removal task of [2_design.md §1](../2_design.md#1-experimental-gate) is filed with no third
  round.

## 8. Decisions This Would Amend

| Accepted today | Under this proposal | New cost |
|---|---|---|
| A concluded experiment that doesn't choose memory files the removal ([Experimental until a randomised readout favours it](../4_decisions.md#experimental-until-a-randomised-readout-favours-it)) | The first null readout files this redesign and one further experiment; the second files the removal | Six more weeks of the feature and its cost before the exit the design promised |
| Memory is written "by the model it serves" ([1_overview.md §1](../1_overview.md#1-motivation), [2_design.md §6.1](../2_design.md#61-minting)) | One consolidator writes every space | One model's blind spots shape every space; a larger model costs more per week |
| Text one model wrote reaches another only after operator review ([A model reads only its own space and the reviewed global memory](../4_decisions.md#a-model-reads-only-its-own-space-and-the-reviewed-global-memory)) | The consolidator's notes reach every model's space unreviewed. The fixed note shape, the lint, `orbit memory show` and reset are the defences | The injection channel that decision closed is open again, through one writer |
| A memory-writing run never receives the measurement of the work ([Reflection is score-blind](../4_decisions.md#reflection-is-score-blind)) | The consolidator sees the ledger: recurrence per note by arm. It still sees no score, rank, weight or rate from the scorecards, and implementers see no measurement at all | The consolidator can tune notes toward what reviewers flag rather than what is right. The severity floor and the `escaped_regressions` guardrail are the only checks |

## 9. Alternatives

- **Remove the feature now.** This is what the accepted design says a null readout means, and
  the 2026-11-03 backtest already suggested that crew explains little beyond task size. The
  case for one more round is that this readout tested retrospective notes with a metric the
  reviewer draw dominates, so it is a weak test of the idea. That is a bet, not a finding.
- **The sketch as stated.** A larger model that adjusts each week on overall effectiveness. It
  differs from this proposal in the unit (the space, not the note), the pace, and in keeping
  today's note form and metric.
- **Keep per-model writers and change only the brief.** The cheapest change, and §1 alone
  could ship that way. It gives up the cross-model view and leaves classification to the
  smaller models.
- **Fix the reviewers instead.** A reliable first verdict would help every experiment. It is
  outside this folder; §6 is the part of it this proposal needs.

## 10. Phasing

Each slice is one task, filed only after Daniel accepts this proposal. C0 can go first by
itself.

- **C0. Measurement.** Reviewer strata, the `reviewer_spread` warning and `first_pass_clean`
  (§6).
- **C1. Notes.** The note shape, caps, rendering and lint (§1).
- **C2. Consolidator.** The config key, the routine's crew pin, the pack changes, the new brief
  and the global task (§2, §3).
- **C3. Ledger.** `classify`, the ledger computation, the write-time rules and the read
  surfaces (§4, §5).
- **C4. Experiment.** Start `crew-memory-consolidated`; conclude under §7.

## 11. Limits and Open Questions

**Limits**

- **Per-note samples are small.** `gpt-6-luna` had 18 reviewed memory-arm tasks in nine days.
  A class that shows up in one task in five gives about four events per arm. Most notes will
  reach the floor without a clear verdict, so the ledger mostly removes notes that plainly do
  nothing and bounds churn. It is not an optimiser.
- **Classification is a model's judgement.** It is blind to the arm but not otherwise checked.
- **Nothing yet shows that trigger-and-action notes work better than retrospectives.** That is
  what the next experiment tests.
- **The reviewer is still the ground truth.** A note that prevents a real defect no reviewer
  flags earns nothing in the ledger.
- **The treatment still changes weekly.** The readout stratifies by revision week, as today.
- **The evidence above is post-hoc.** The reviewer and severity slices were chosen after
  seeing the readout.

**For Daniel**

1. One more round, or the removal the design already calls for?
2. Which crew consolidates?
3. Does the global memory stay operator-reviewed, or may the global task write it?
4. Should C0 go ahead whatever the answer to 1?

## Task References

- [ORB-25838] — writes this proposal and records the analysis of `exp-5342a4d6` behind it
- [ORB-23105] — designed crew memory
- [ORB-23117] — started the memory-on vs memory-off experiment in shadow mode

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
