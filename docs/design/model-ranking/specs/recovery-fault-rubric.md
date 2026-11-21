---
title: Final-recovery fault rubric proposal
owner: codex
last_updated: 2026-11-16
last_validated: 2026-11-16
status: Draft
type: design
summary: Proposed common fault ordering, evidence requirements and blind reassessment of the suspended recovery-fault measure.
tags: [model-ranking, final-recovery]
---

# Final-recovery fault rubric proposal

## Scope and evidence

ORB-25350 asks for a proposal following ORB-25349's blind audit. This draft changes
no running prompt, fault enum, recorded decision, audit count, ranking weight or
automation setting. Adoption would require a separate implementation change to
both prompt assets and an explicit decision about measurement eligibility.

Sources inspected at `31bd63f3e7e4cb10fc51216907bc33231e09950e`:

- [Final recovery, Fault section](../../../../crates/orbit-core/assets/activities/final_recovery.yaml)
  says first supported fault wins, in order `implementer`, `specification`,
  `review`, `infrastructure`, `orbit`, `external`, `unknown`.
- [Audit, Re-judge and Rate by season sections](../../../../crates/orbit-core/assets/auto_tasks/recovery-fault-audit.yaml)
  instead orders `specification`, `review`, `infrastructure`, `orbit`, `external`,
  `implementer`, `unknown`. Thus the audit does not actually mirror recovery's
  precedence. No cited case establishes that ordering alone caused a disagreement.
- [Ranking design §5.2](../2_design.md#52-fault-on-final-recoveries) suspends a
  season above 20% disagreement until the rubric is fixed. The current
  [ranking measure definition](../../../../crates/orbit-core/src/application/agents/rank/measures.rs)
  leaves final recovery uncomputed; the suspension is an audit finding, not
  evidence that a runtime ranking switch has been applied.
- `recovery-fault-audit.json` on ORB-25349, audited 2026-11-16, carries prior
  ORB-24503 counts of 1/10, adds 5/10, and reports 6/20 = 30% for season-0.
  There were 31 eligible decisions after a blindness exclusion, a seeded sample
  of 10 (seed 20261116), and no short sample. Four new disagreements were
  `unknown`, one was `implementer`; these are not five proven alternative causes.

The case review below uses that artifact and only the five tasks' projected
`id`, `title`, `description`, `acceptance_criteria` and `context_files`. No
sampled-task history, run logs, crew, provider or model was sought. The original
labels and independent judgments are already disclosed to this author, so this
is an analysis of the rubric, **not another blind independent audit**. Current
task fields cannot establish what a run saw at an earlier time.

## Proposed shared wording

The following is replacement wording for recovery's **Fault** section and
audit's **Re-judge** section. Their different evidence permissions stay in force:
recovery may inspect its admitted evidence; an auditor uses only the permitted
blind task fields, recorded evidence and outcome. A pointer to unavailable or
forbidden material does not authorize an auditor to fetch it.

> Attribute the specific failure that stopped the named run at the named step.
> Separate the observed symptom, the governing requirement at that time, and the
> evidence linking the symptom to a cause. A recovery action is not a cause:
> `decide` does not imply specification, `friction_reported` does not prove an
> Orbit defect, and `requeue` does not imply an external failure.
>
> A known fault needs a concrete observation and a causal comparison: expected
> versus actual behavior, incompatible authoritative requirements, a candidate
> failure against a passing base, or an explicit environmental failure. Cite the
> run/step, check or file, the relevant requirement, and the observation that
> distinguishes that fault from plausible alternatives. Keep `fault_evidence`
> one line; put essential causal facts in that line rather than only a pointer.
>
> Missing evidence is not evidence of a missing action, an impossible task, a
> stale lock, or a broken gate. A timeout, deterministic refusal, accepted
> candidate, asserted need for a decision, or no-compete instruction alone does
> not establish fault. If the applicable requirement changed and its timing is
> unknown, do not treat the current wording as the historical contract.
>
> For the same causal failure, apply the following order in both recovery and
> audit. Select the first category whose entire condition is supported; an
> earlier plausible category does not outrank a later demonstrated one. The
> order resolves overlapping supported categories, not competing unproved
> explanations. If the evidence cannot distinguish those explanations, use
> `unknown`.
>
> 1. `specification`: an authoritative requirement applicable to the run was
>    contradictory, impossible within its authorization, materially incomplete,
>    obsolete or already satisfied. Name the conflicting requirements, the
>    missing product/authority choice and why normal implementation discretion
>    cannot settle it, or the covering landed work. Multiple valid implementation
>    choices do not by themselves make a task underspecified.
> 2. `review`: the rejection depended on a concrete demand unsupported by the
>    applicable task and workspace guide. Show the demand and why compliant work
>    satisfies the actual contract. A rejection or disagreement with a reviewer
>    alone does not suffice; review can enforce guarantees without prescribing
>    the implementation mechanism.
> 3. `infrastructure`: an observed host, sandbox, network, provider, credential
>    or CI-runner failure prevented the required execution or judgment. Identify
>    the failed capability or response. Ordinary lock contention in Orbit is
>    not itself a host outage.
> 4. `orbit`: evidence shows an Orbit gate, pipeline or applier violated its
>    contract. State the invariant, its satisfied preconditions and the observed
>    violation. A correct refusal of absent or ambiguous state is not a defect;
>    reproducibility of the refusal alone does not establish one.
> 5. `external`: an evidenced change outside Orbit and the task, such as a
>    concurrent base change, upstream service change or dependency release,
>    caused the failure. Name that change and its causal effect. Another task's
>    legitimate lock ownership or an operator scheduling instruction alone does
>    not establish an external fault. Class explicit service outages under
>    infrastructure; class a changed upstream contract under external.
> 6. `implementer`: the task was clear and achievable under the contract then
>    applicable, and the candidate demonstrably violated it, omitted required
>    work or validation, or broke a check that the base passes. Show the mismatch
>    or an evidenced failure to use an available authorized route. Do not infer
>    an omission merely because a short evidence line does not mention an attempt.
> 7. `unknown`: the permitted evidence does not establish a known fault, or
>    leaves a material causal or historical-contract ambiguity. Name the missing
>    fact. This reports uncertainty, not absence of a fault or a proved alternative
>    cause. Prefer it to a guess, even when a recovery action is clear.
>
> Judge without the implementer's identity. For an audit, fix the independent
> label and reason before comparing with the recorded label. An independent
> `unknown` against any known recorded fault is a disagreement; never discard it
> from the denominator or convert it into agreement.

This proposes the audit's existing precedence, with specification and review
before charging the implementer, once their stronger evidence conditions are
met. Retaining recovery's implementer-first order is a viable alternative, but
requires a separate rule to settle cases where both a contract defect and a
candidate defect are proved. Choosing one common order removes the current
inconsistency. Cost: the stricter evidence bar will initially increase `unknown`
and require better recovery evidence; it does not guarantee improved agreement.

## Five disagreement cases

These are applications to the **available packet**, not replacements for the
recorded judgments. Additional evidence below is conditional, not a claim that
such evidence exists or permission for an auditor to read forbidden sources.

| Task / run | Recorded → independent | Ambiguity under shipped wording | Application of proposed wording and distinguishing evidence |
| --- | --- | --- | --- |
| ORB-25178 / `jrun-20261116-0125-c5` | specification → unknown | AC1 requires all-or-none multi-part status CAS. The recovery line asserts a lock-scope decision is necessary because two writers take no task lock. It does not demonstrate that ordinary implementation discretion cannot satisfy AC1, or that F1 demands more than AC1. Five other fixes passing says nothing decisive about F1. | `unknown`. Specification needs incompatible requirements or a material authority choice that no compliant mechanism resolves. Review needs F1's actual demand compared with AC1 and the guide. Implementer needs evidence that the candidate still permits a partial CAS application. The proposed order cannot manufacture any of these facts. |
| ORB-24499 / `jrun-20261114-1024-t2` | external → unknown | `footprint_unavailable` after 3600s while another operator task owns registry locks establishes contention. A null job binding and an old update time do not establish an abandoned lock or broken expiry. Recovery's broad “outside Orbit and the task” could sweep in normal scheduling; the audit's narrower upstream wording does not. | `unknown`. A valid owner can hold a lock legitimately. Orbit requires a demonstrated lease/release invariant violation; infrastructure requires an actual host/service failure; external requires a causally relevant outside change. Timeout duration alone proves none. |
| ORB-25064 / `jrun-20261116-0019-c5` | orbit → unknown | `candidate_resume` deterministically refuses a missing unique requeue binding, while review accepted candidate `cd49b96b`. Review acceptance and resumability certify different preconditions. The action `friction_reported` can bias attribution toward Orbit. | `unknown`. Orbit needs evidence that the unique admitted binding existed and applied to this publishing run, or that an Orbit writer was required to persist it and failed. Otherwise refusal may correctly protect candidate ownership. Candidate acceptance alone does not decide. |
| ORB-24812 / `jrun-20261114-2131-c3` | specification → implementer | Current AC5 expressly permits seeded or simulated readiness; the recovery line describes an unmet live-window requirement. The audit flags unknown clarification timing. No attempted simulation is described, but silence does not prove none was attempted. | `unknown` for the historical run on this packet. Implementer would require evidence that simulation was authorized and available then and required QA was omitted or an invalid dependency on a real drain caused the stop. Specification would require the historical criterion to prohibit every authorized simulation route and require prohibited dispatch. Preserve the audit's original implementer judgment and its timing caveat. |
| ORB-23550 / `jrun-20261109-0232-c3` | specification → unknown | An operator says not to compete with an open repair PR. A clean worktree and another task in backlog do not show the repair landed, that this task was obsolete, or what caused the original CI failure. A sensible stop action is easy to confuse with fault attribution. | `unknown`. Archive/coordination can be appropriate without settling fault. Specification needs explicit supersession/withdrawal of this mandate or covering landed work, with the affected requirement identified. Attribution of the CI failure separately needs evidence for that failure, not the no-compete instruction. |

All five packets leave a material gap under the proposal. That is a diagnostic
result, not proof that their original known faults were false or that the
independent auditor was correct in every case. In particular, changing the fourth
case's diagnostic label does not subtract its disagreement from the original audit.

## Boundary examples for adoption review

These hypothetical comparisons exercise the proposed rule; they are not new
findings about the five tasks.

| Boundary | Insufficient evidence → unknown | Sufficient evidence → known fault |
| --- | --- | --- |
| Implementation choice / specification | “Need a lock-scope decision” with an atomicity criterion. | Two applicable requirements demand both retaining partial writes on a CAS miss and leaving no writes on that miss → specification. |
| Review / implementer | “Review rejected F1” without its demand or observed candidate behavior. | Reviewer requires a particular lock primitive although an admitted public-boundary check demonstrates the required atomic guarantee and no other rule requires that primitive → review. Candidate leaves a document edit on CAS failure while the contract demands none → implementer. |
| Contention / Orbit | Review waits one hour on a legitimate task lock. | Packet establishes lease expiry, no valid renewal, a required expiry-release rule and continued exclusion after expiry → orbit. An explicit sandbox namespace permission error preventing the required review check → infrastructure. |
| Refusal / Orbit | Accepted candidate lacks a unique requeue binding. | Packet identifies the one admitted binding for the exact task/run/candidate and the gate rejects it as absent despite its contract requiring acceptance → orbit. |
| Current / historical criteria | Current AC permits simulation, but no timestamp establishes when that permission applied. | An identity-safe run-time criterion snapshot permits simulation and the run's evidence explicitly says it refused to use that available authorized route and stopped solely for lack of live data → implementer. |
| Coordination / specification | Operator asks two repairs not to compete; the other PR is open. | An authoritative instruction explicitly withdraws this task as superseded by the named repair → specification for the now-obsolete mandate, independent of the original CI defect's cause. |
| External / infrastructure | A base SHA changed, with no connection to the failed check. | Passing candidate plus a named concurrent base API removal that causes its integration failure → external. Provider quota-exceeded response before execution → infrastructure. |

For adoption, copy the same wording and examples into both prompt surfaces,
including their precedence, without changing allowed reads or the seven fault
tokens. Keep examples in contributor/agent material. No new decision fields or
runtime schema are proposed. A prompt-only change needs human review of these
case applications; tests that merely pin wording would not prove attribution.

The causal line is bounded. Each of the ten decision shapes in
`final_recovery.yaml` caps `fault_evidence` at `maxLength: 500`, and
`FaultAttribution::new` (`crates/orbit-types/src/workflow/final_recovery.rs`)
cuts a longer line at `MAX_FAULT_EVIDENCE_CHARS` with an ellipsis, which can
drop the distinguishing observation. The wording above asks for the requirement,
the observation and the alternative it rules out, so adoption must put the
decisive fact first and check that each example fits. Raising the cap is a
separate schema decision that this proposal does not make.

## Reassessing the suspension without rewriting history

1. **Freeze the existing record.** Retain the two original audit artifacts,
   their seeds, decision IDs, independent labels, reasons, prior-artifact links
   and cumulative 6/20. Season-0 remains suspended and excluded as a ranking
   input. Proposal approval or a wording edit alone is not evidence to lift it.
   Do not edit historical task faults, recovery decisions, routing or ranks.
2. **Version the proposal on adoption.** Pin the adopted rubric by source SHA
   and record its common order and evidence permissions. Call it a new rubric
   version in a separate reassessment artifact; retain the original as the
   legacy version with its known ordering conflict. Do not pool judgments from
   different versions into one agreement rate. An attribution-rule change also
   starts a new calibrated ranking season under design §4.2: `season.rs` bumps
   `DEFINITION_VERSION` for a change to a measure's definition, weight or
   attribution rule, and calibrated seasons are already recorded in
   `rank-seasons.json`. Decisions carry no season label today, so the audit's
   `season-0`, which every decision receives by default, is not the calibrated
   season 0 in that file. Retain
   `season-0` as historical provenance and distinguish reassessments by rubric
   SHA and cohort dates explicitly; do not infer a ranking season from them.
3. **Separate diagnosis from confirmation.** The five disagreements are useful
   calibration examples but were selected for disagreement and their labels
   have been seen. Reapplying the draft to them cannot estimate reliability.
   A separately labelled diagnostic replay may cover all 20 original decisions
   with the revised rule, preserving old labels alongside new ones. Any
   supplemented evidence must be copied into an identity-safe, immutable packet
   and distinguished from an unchanged-packet replay; never silently improve an
   old packet and call its result the original audit.
4. **Confirm prospectively and blind.** After adoption, use the existing seeded,
   cross-family audit procedure on fresh eligible decisions judged under the
   same adopted rubric, excluding previously judged IDs. Sample up to 10,
   without padding, and retain eligibility counts and every exclusion. The
   auditor may see only the existing permitted fields, evidence and outcome;
   `fault_judged_by` is used for the family exclusion, never to learn the
   implementer. A preparer masks the recorded fault until the independent label
   and reason are fixed, then compares. Identity leakage excludes that packet
   and draws a replacement; missing historical facts yield `unknown`, not an
   invented explanation. Report originally unknown faults separately as coverage;
   do not add them to the audit's known-fault sample to inflate agreement.
5. **Append a reassessment artifact.** Record parent audit task/artifact
   references and digests; rubric SHA; original season and prospective cohort;
   packet revision/time and available historical-contract evidence; auditor
   family and eligibility/exclusion reasons; seed; sampled task/run IDs; fixed
   independent labels and reasons; recorded labels; agreements; and cumulative
   counts **within that rubric/cohort**. Report the known-to-unknown disagreement
   count and evidence gaps separately. Keep diagnostic replay rates separate
   from prospective rates and retain 6/20 in the legacy series.
6. **Make eligibility explicit.** Proposed confirmation requires at least 10
   fresh judged decisions and a cumulative disagreement rate no greater than
   20% under the adopted common rubric. Unknown-vs-known stays a disagreement;
   a short sample is reported but cannot confirm eligibility. This is a minimum
   confirmation rule, not a confidence guarantee. Daniel must explicitly adopt
   any restored measure and its applicable cohort/season after reviewing the
   rubric repair and confirmation evidence. If confirmation fails or is short,
   keep it suspended and continue sampling without resetting counts. A new
   cohort's success does not retroactively unsuspend or score season-0. Using
   revised historical attributions would need separate explicit authorization
   and a versioned derived dataset; original records still remain intact.

Arithmetic dry-run: the actual legacy series is `(1 + 5) / (10 + 10) = 30%`,
which is above 20%. Even appending 10 perfect same-version judgments would
produce `6/30 = 20%`; a threshold crossing alone neither fixes the rubric nor
authorizes ranking. A **separate hypothetical** new-version cohort with 2/10
disagreements meets the proposed minimum rate; 3/10 fails, 0/9 is short, and
2/10 followed by 3/10 is cumulatively 5/20 = 25% and fails. No such confirmation
sample has been run here, and none of these numbers replaces the actual 6/20.

## Limits and adoption work

This proposal deliberately leaves the shipped ordering conflict in place until
adoption; the required deliverable is proposed wording, not a live policy change.
Historical criterion snapshots are not supplied by the present audit projection.
A future packet improvement must preserve identity masking and authorize only
the necessary immutable facts, rather than opening history or run logs to
auditors. More `unknown` labels can improve honesty while reducing measurable
coverage. A ten-decision sample remains small and exclusions can bias it.

Adoption work would update both assets together, validate their shared causal
conditions and precedence, and specify the reassessment artifact and operator
eligibility record. The disabled audit schedule and currently uncomputed ranking
term remain separate implementation concerns. This proposal does not claim that
an artifact alone enforces suspension in ranking code.

## Task References

- ORB-25350 — requests this rubric proposal and suspended-season reassessment plan
- ORB-25349 — records five new disagreements and suspends season-0 at 6/20
- ORB-24503 — supplies the prior audit counts of 1/10 carried by ORB-25349
- ORB-25178 — supplies the atomic CAS versus lock-scope disagreement
- ORB-24499 — supplies the review-footprint contention disagreement
- ORB-25064 — supplies the accepted-candidate versus resume-binding disagreement
- ORB-24812 — supplies the simulation permission and historical-timing disagreement
- ORB-23550 — supplies the no-competing-repair instruction disagreement

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
