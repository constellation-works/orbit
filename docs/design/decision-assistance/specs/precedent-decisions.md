---
title: "Proposal: precedent-answered human decisions"
owner: claude
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
feature: decision-assistance
doc_role: spec
type: design
summary: "Proposed consumer: predict a pending human decision from recorded human precedent, measure it in shadow per category, and self-answer only enabled low-risk categories with veto."
tags: [decision-assistance, human-decision-channel]
---

# Proposal: precedent-answered human decisions

Status: **design only; nothing implemented, configured or activated** ([ORB-21089]).
The [scope](../1_scope.md) keeps the older consumers parked. This consumer is a
separate evaluation candidate. Its labels are real, structured human answers to
[human decision channel](../../human-decision-channel/2_design.md) requests, not
synthetic or agent-authored labels. Every mode defaults to `off`. Self-answering
needs a measured pass per category and an explicit operator enablement. The
implementation slices are filed as proposed tasks (see Phasing).

## Purpose and limits

Agents and recovery mechanisms raise decision requests that wait on Daniel. Many
repeat an earlier choice. A decision model can return one of a request's own
options with a probability, given the most relevant recorded past decisions.
The consumer predicts that answer. When a category has passed its measured gate,
the operator has enabled it, and the request's risk tier allows it, the consumer
answers on its own and lists the answer for veto. Otherwise the request goes to
the human exactly as it does today.

It does not:

- add an agent-side call, change who may raise or answer a request, or widen any
  authority (agents keep filing requests as today; §Agent surface);
- answer an irreversible, authority-widening, spending or unknown-tier request,
  whatever the confidence (§Risk tiers);
- turn its own answers into precedent without human confirmation (§Feedback);
- replace the existing curated-rule auto-answer
  ([workspace agent corpus](../../workspace-agent-corpus/1_scope.md#curated-decision-rules)),
  which keeps running first;
- show its prediction to the human in a category under measurement, because a
  visible prediction would bias the label it is scored against.

## Question

One assessment per open decision request, raised after the request is written
and after curated-rule auto-answer has left it open. The question set
`precedent-decision/1` has three parts, all checked by the host against the
packet:

| Question | Type | Allowed answers |
|---|---|---|
| `decision_choice` | choice | Exactly the request's `answers[].id` values, plus `insufficient_precedent` |
| `precedent_supports_<i>` for `i` = 1..k | predicate | Whether packet precedent `i` supports the chosen answer |
| `precedents_conflict` | predicate | Whether the supplied precedents disagree about this kind of request |

`insufficient_precedent` is the abstention. The rubric tells the model to choose
it when no precedent addresses the request, when precedents disagree, or when
the request's consequence differs from every precedent's.

The answer the consumer records carries the chosen answer id, its probability,
the probability distribution, the cited precedent ids, the conflict probability,
and the model and contract versions.

### Checks: no invented options or precedents

The model selects; the host decides what a selection means.

- **Options.** The allowed values of `decision_choice` are copied from the
  request's own `answers[]` at packet build. The service already refuses any value
  outside the allowed set and any distribution that does not cover exactly that
  set ([service contract](service.md#application-and-evaluation-contracts)). The
  host re-checks the chosen id against the live request under its write lock
  before applying it; an answer id the request no longer offers is `stale`.
- **Precedents.** Citations are packet indexes, never ids the model writes.
  `precedent_supports_<i>` exists only for `i` in `1..=k`, where `k` is the
  number of precedents actually in the packet; the host maps each index with
  probability ≥ 0.5 to the precedent's stored id and source citation. The
  expanded-template rules (template name plus a positive numeric index, identical
  rubric and type) already refuse other names.
- **Grounding.** A non-abstaining choice with no supporting precedent is recorded
  as `uncertain`, never as a confident answer. The supporting precedents must
  have chosen an option whose meaning the request also offers; the model judges
  that correspondence, and the readout measures it.

One service extension is needed: today an expanded template's choice questions
must share identical allowed values. `decision_choice` needs a **packet-bound
choice**, whose allowed values are copied from one host-built packet field (at
most 8 ids, each at most 64 bytes) plus the template's fixed abstention value. The
template digest binds the field name, bounds and abstention value, not the
per-request ids. That is slice 4 below; the rest of the service contract is
unchanged.

## Packet

The host builds one immutable packet per request; the request text is untrusted
evidence, never instructions.

- **Request:** request id, workspace id, gate, category (§Categories), risk tier,
  the question text (the agent request's question, the final-recovery summary, or
  the gate's fixed wording), each answer's id, label and consequence, the
  recommended answer when the raiser marked one, `topic` when present, and
  `raised_at`.
- **Task context:** title, and the description and acceptance criteria truncated
  to fixed bounds, with truncation recorded.
- **Precedents:** at most `k` (default 8) records from §Precedent provider,
  ordered by the drift weighting in §Drift, each with its age in days.
- **Provenance:** packet builder version, provider ids and contract versions, the
  provider omissions and truncation, and `as_of` (= `raised_at`).

A truncated or omitted precedent set cannot produce a `suggested` result, as for
every consumer of the service. The packet goes outside the host only under the
service's existing data boundary: a host grant naming the destination and the
granted fields, with value-based redaction. Precedent notes are Daniel's words;
the operator's enablement must name them as sent evidence.

## Precedent provider

A generic, pluggable contract `orbit.precedent-provider/1`. Orbit core holds the
contract, the validator and one built-in provider. Any other source is a plugin
tool the operator names in host configuration. Core has no Office-specific code,
field or vocabulary.

### Request

```json
{
  "contract": "orbit.precedent-provider/1",
  "workspace_id": "ws_…",
  "request": {"id": "dr-…", "gate": "agent_request", "category": "…",
              "question": "…", "options": [{"id": "1", "label": "…", "consequence": "…"}],
              "topic": "…", "raised_at": "2026-11-01T09:00:00Z"},
  "as_of": "2026-11-01T09:00:00Z",
  "k": 8
}
```

### Response

```json
{
  "contract": "orbit.precedent-provider/1",
  "precedents": [{
    "id": "provider-stable id",
    "question": "…",
    "options": [{"id": "…", "label": "…"}],
    "chosen_option": "…",
    "context": "bounded text",
    "scope": {"workspace_id": "ws_…", "category": "…", "topic": "…"},
    "decided_at": "2026-10-20T10:12:00Z",
    "decided_by": "daniel",
    "decided_by_kind": "human",
    "reversed": false,
    "citation": {"source": "provider name", "ref": "stable reference", "digest": "sha256:…"}
  }],
  "omissions": [],
  "truncated": false
}
```

### Host validation

The host refuses the whole response (and records a provider gap) when any rule
fails. It never repairs a record.

- `contract` matches; at most `k` records; ids are unique within the response;
  every string and the whole response stay within fixed byte bounds.
- `decided_at` is strictly before `as_of`. A provider cannot leak a decision made
  after the request was raised, so offline replay and shadow measurement see the
  same precedent set the live request would have seen.
- `chosen_option` is one of that precedent's own `options[].id`.
- `decided_by_kind` is `human` or `human_confirmed` (a model answer a human later
  confirmed, §Feedback). `model`, `precedent` or any automation kind is refused,
  so neither model answers nor curated-rule auto-answers enter as precedents.
  The built-in provider also checks `decided_by` with `is_automation_actor`.
- `reversed: true` records are kept as contradiction evidence (§Drift) and never
  count as support.
- `citation` names its source and a stable reference; the host stores the citation,
  never fetches it.

### Sources

1. **Orbit decision history (built in).** Answered requests in the workspace's
   task histories whose `decision_answered` resolution was written by a human
   label, plus confirmed model answers. It reuses the bounded listing scans of the
   [decision channel](../../human-decision-channel/2_design.md#23-storage).
   Relevance ranks by same category, then same topic, then lexical overlap of the
   question with the request, then recency. A later reversal or veto marks a
   record `reversed`.
2. **Plugin providers.** A configured plugin tool that implements the contract,
   called through the existing plugin callback route under its grant. The
   orbit-office decision ledger is designed as one such provider in its own
   repository. Core treats every plugin provider alike.

Several sources are merged deterministically: validated records are
deduplicated by `citation.digest`, sorted by the §Drift weight, then by source
order in configuration, and cut to `k`.

## Categories

A category is `<workspace_id>/<kind>`, where kind is:

- the gate name for core gates (`block_triage`, `pilot_admission`,
  `proposed_approval`, `review_approval`); and
- `agent_request/<topic file>` for agent-raised requests, the topic's file
  component (`agent_request/untopiced` when there is none).

The category is derived from the request at raise and stored on it, so a later
change to the derivation cannot move a recorded decision between categories.
Measurement, targets, enablement, holdback and demotion are all per category.

## Risk tiers

Every answer carries a `risk_tier`, and the request's tier is the most severe tier
among its answers. A model may answer on its own only when the request's tier is
`routine` or `reversible`. These tiers always go to the human, whatever the
confidence, category status or enablement:

| Tier | Meaning | Examples |
|---|---|---|
| `irreversible` | Effects the normal path cannot undo | Merge or landing past protection, releases, deletes, terminal reject |
| `authority` | Widens what someone or something may do | Grants, consents, secrets, tool grants, overriding a pilot safety hold, admitting a task for agent execution |
| `spend` | Commits money or paid quota beyond the existing budget | Purchases, paid API activation, budget or allocation increases |
| `unknown` | Tier not declared, unparseable, or from a record older than the field | Every existing request |

Self-answerable tiers:

| Tier | Meaning |
|---|---|
| `reversible` | Effects that the normal decision path undoes (re-queue, re-pilot, archive, dismiss) |
| `routine` | No lasting effect beyond recording the choice |

An unknown, missing or unrecognised tier means **ask the human**. Every request
raised before the field exists reads as `unknown`, so turning the consumer on
changes nothing for them.

Tiers for core gates are fixed by the raiser, not by the model or a workspace:

| Gate | Tier |
|---|---|
| `review_approval` | `irreversible` (`approve` lands work) |
| `proposed_approval` | `authority` (`approve` admits agent execution) |
| `pilot_admission` | `authority` (every verdict overrides or resolves a safety hold) |
| `block_triage` | Per option: `requeue`, `re_pilot`, `change_crew`, `split` and `archive` are `reversible`; `reject` is `irreversible`; `dismiss` is `routine`; a field rewrite is `reversible` |
| `agent_request` | Declared by the raising agent per answer; missing means `unknown` |

So in practice only final-recovery decisions without a terminal option, and agent
requests with declared low tiers, can ever be self-answered. An agent's declared
tier is a claim. The tier and the category's enablement both bound it, and the
veto digest shows it (see Concerns).

## Shadow measurement

`shadow` is the first and default non-off mode. On every request in a category
the consumer is granted for, it predicts, seals the prediction, and changes
nothing visible:

1. On raise, the host enqueues an assessment keyed by request id and binding.
   Notification to the human is not delayed.
2. The completed assessment is sealed in the assistance ledger with its
   completion time. It is not shown on the dashboard, in mail, in
   `orbit.decision.show` or in the digest for that category.
3. When a human answers, the readout joins the sealed prediction to the human's
   answer. A prediction completed at or after the human's `answered_at` is
   excluded as leaked. Superseded and expired requests are counted, not scored.
4. Requests answered by curated-rule auto-answer are excluded: they are not human
   labels.

### Per-category report

The readout (`precedent-decision-readout/1`) reports, for each category and for
the registered operating threshold `t`:

- requests raised, assessed, scored, and the reasons for any gap (unavailable,
  refused, invalid, stale, leaked, provider gap);
- **coverage** at `t`: the share of scored requests with a non-abstaining choice
  whose probability is ≥ `t` and which has at least one supporting precedent;
- **accuracy** at `t`: the share of covered requests whose choice equals the
  human's answer, with a Wilson 95% interval;
- **calibration:** expected calibration error over 10 equal-width probability
  bins and the Brier score of `decision_choice`, plus the reliability table;
- abstention rate, conflict-flag rate, and accuracy when `precedents_conflict` was
  set;
- citation agreement: the share of covered correct answers whose cited precedents
  chose an option with the same consequence (sampled and human-checked);
- latency, recorded usage and estimated cost, with `unknown` kept unknown.

### Evaluator contract `decision-assistance-eval/4`

The existing offline evaluator scores this consumer under a new additive contract
version. `/1`–`/3` keep their consumers and meaning.

- **Consumer** `precedent_decision`, question set `precedent-decision/1`.
- **Label provenance** `recorded_human_decision`: the answer a human writer
  recorded on the request through the dashboard, CLI or a registered channel,
  with its `decided_by`, `answered_at` and channel provenance. It is not
  relabelled after the fact. It is holdout-eligible only for this consumer, and
  only when the recorded writer is not an automation actor and not the
  `precedent` or `model` actor.
- **Splits.** Family is the category plus topic. Holdouts are **prospective
  time windows**: the registration fixes a future window start. Only requests
  raised inside the window, with predictions sealed before their human answers,
  are scored. Precedents are retrieved as of each request's `raised_at`, so a
  holdout decision can be a precedent for a later holdout request only through
  that request's own as-of retrieval.
- **Registered targets, per category, before the window opens:** `threshold`
  (the operating `t`, chosen on development data), `min_samples` (scored requests
  in the window), `min_accuracy` (lower Wilson bound at `t`), `min_coverage` and
  `max_ece`. Suggested defaults, which a registration may tighten but not loosen
  without a fresh registration: 30 samples, 0.95 accuracy lower bound, 0.30
  coverage and 0.05 ECE.
- **No tuning after the readout.** Prompt, rubric, threshold, `k`, provider
  ranking and packet bounds are part of the registered policy digest. Any change
  after the window opens needs a new registration and a new window.

The existing `check-targets` keys (`min_precision`, `min_recall`) do not express
coverage or calibration, so the evaluator slice adds the per-category keys above
for `/4` only.

## Measured gate and enablement

A category may answer on its own only when all of these hold:

1. Its `/4` readout met every registered target on an unopened prospective window.
2. The operator enables it from the interactive CLI (refused from agent envelopes
   and plugin callbacks, like channel registration):
   `orbit decision assist enable <category> --readout <task>/<artifact>`.
   The command records the category, the policy version and digest, the threshold,
   the readout reference and the operator, in a host-only store beside the
   decision channels. It refuses a readout whose policy digest differs from the
   live host grant.
3. The live host grant still has the same policy digest. A grant change suspends
   every enablement made under the old digest.

`orbit decision assist disable <category>` and `orbit decision assist status`
complete the surface. Disabling is immediate; open requests then wait for the
human.

### Self-answer path

For a request in an enabled category:

1. Curated-rule auto-answer runs first, unchanged.
2. If the request's tier is not self-answerable, the request goes to the human,
   and the prediction is still recorded in shadow form for the rolling measure.
3. A **holdback** share (default 10%, chosen by a keyed hash of the request id, not
   by the raiser) goes to the human anyway and is scored like shadow. This keeps
   an unbiased live accuracy sample.
4. Otherwise the channel waits for the assessment up to `assist_wait_seconds`
   (default 120) before notifying. If the assessment is `suggested` at or above
   the threshold, has a supporting precedent and no conflict flag, core applies the
   answer. A timeout, any other disposition or a provider gap means notify the
   human as today.
5. The apply step is the one curated-rule auto-answer uses: a fresh read under the
   task write lock, binding, state, expiry and answer re-checked. The writer is the
   `model` actor (§Recording), never a human label.

## Recording, veto and feedback

### `answered_by: model`

`DecisionResolution` gains a `model` actor value beside `precedent`, and a
`model` citation recorded only with it:

- assessment id and logical id, category, policy version and digest, question-set
  and contract versions;
- adapter, requested and returned model identity, model revision epoch;
- chosen answer, probability, threshold, conflict probability;
- cited precedents: provider, precedent id, `decided_at` and citation;
- the enablement record it ran under (operator, readout reference);
- `previous_status`, as for precedent answers, for reversal.

`model` is an automation actor: `is_automation_actor` is true for it, so no
guard that requires a human resolution (pilot `operator_resolution` and the like)
accepts a model answer. That is also why the excluded tiers include every
gate whose effect needs a human resolution.

### Veto digest and path

Every model answer raises, in the same write, a review request on the same task:
gate `model_answer_review`, answers `confirm` (tier `routine`) and `veto` (tier
`reversible`), bound to the original request id and its resolution time, with the
normal expiry. The model never answers this gate. It rides the existing channel:

- `orbit.decision.digest` returns model answers since the last digest in a
  separate `model_answered` section, each with the request, chosen answer,
  probability and cited precedents. The daily digest mail lists them, and replies
  use the existing grammar (`veto 2`, `confirm 1,3`).
- The dashboard and `orbit decision answer` answer it like any request.

`veto` reverses through the existing reversal step generalised from precedent
answers (`orbit decision reverse`): it re-opens the original request, restores
`previous_status`, comments the veto, and notifies the human, who then answers the
original request as usual. A veto changes only what the decision record and task
status own. Downstream work that already started from the model's answer is not
undone automatically; that is why only `routine` and `reversible` tiers qualify.

`confirm` records the human's confirmation on the model answer. An expired review
leaves the model answer standing but unconfirmed.

### Feedback rules

- A model answer becomes a precedent only after a human `confirm`. It is then
  provided as `decided_by_kind: human_confirmed` with the confirming human as
  `decided_by`. Unconfirmed, expired-review and vetoed model answers are never
  precedents. This prevents a self-reinforcing loop.
- A human answer to a re-opened (vetoed) request is an ordinary human precedent.
  The vetoed model answer is marked `reversed` and counts as contradiction.
- A human answer to a holdback or excluded-tier request is an ordinary precedent.
- The corpus outbox stays human-only: a model answer writes no
  `agent_corpus_append`. A confirmed one appends then, with the confirming human
  as the answerer.

## Drift

- **Recency.** The packet orders precedents by a weight that halves every
  `precedent_half_life_days` (default 90) and states each precedent's age. The
  rubric tells the model that newer precedents win a disagreement. The half-life is
  part of the registered policy.
- **Contradiction.** The model's `precedents_conflict` flag at or above 0.5
  downgrades the result to `uncertain`, so the request goes to the human.
  Deterministic contradictions do the same, whatever the model says: a `reversed`
  precedent in the same category and topic within the half-life, or two human
  answers to the same topic in the half-life choosing different options.
- **Demotion.** Each enabled category keeps a rolling window of its last
  `demotion_window` (default 30) scored live outcomes: holdback and excluded-tier
  predictions against the human's answer, confirmed model answers as agreement and
  vetoes as disagreement. When the window's Wilson lower bound of accuracy falls
  below the enabled `min_accuracy`, or vetoes reach `max_vetoes` (default 2) in
  the window, the category is demoted to shadow automatically. The demotion
  records its evidence and notifies the operator through the digest. Demotion is
  automatic; promotion never is: re-enabling needs a new readout and an operator
  enablement.
- **Model revision.** A changed model revision epoch or policy digest suspends
  every enablement under the old one (§Measured gate).

## Agent surface

No change. Agents keep raising requests with `orbit.decision.raise`. The only
addition is an optional per-answer `risk_tier` field the raiser may declare; an
undeclared tier is `unknown`. Agents cannot read sealed shadow predictions,
enable categories, confirm or veto. `orbit.decision.show` shows a self-answered
request as answered by `model`, with its citation, like a precedent answer. The
channel consults the consumer between raise and notification; nothing else in an
agent's flow changes.

## Configuration

Proposed, not yet in [CONFIG.md](../../../CONFIG.md):

- An `[assistance]` policy with `use_case = "precedent_decision"`, `mode` `off`
  (default) or `shadow`, the question-set contract digest, and evidence grants
  covering the packet fields above. `advisory` mode is not used for this consumer.
- Host-only consumer settings: precedent provider list (built-in, plus plugin tool
  names in order), `k`, `precedent_half_life_days`, `assist_wait_seconds`,
  holdback share, `demotion_window`, `max_vetoes`.
- Category enablements live in the host-only enablement store written by
  `orbit decision assist`, never in workspace configuration.

With the consumer off, no assessment is enqueued, no provider is called, and the
decision channel behaves exactly as today.

## Concerns and honest limitations

- **Labels reflect habit, not correctness.** Matching Daniel's past answers
  measures predictability, not whether the answer was right. The veto digest and
  demotion rules bound the damage; they do not establish quality.
- **Few decisions per category.** Most categories may never reach 30 scored
  requests in a reasonable window. They stay in shadow, which is the intended
  failure mode.
- **Practical scope is narrow.** Every core approval and pilot gate is excluded by
  tier. The early value is in final-recovery dismissals and requeues and in
  agent requests, and it is unmeasured.
- **Agent-declared tiers can be wrong.** An agent might declare `routine` for a
  consequential option. Bounds: enablement per category after measurement, the
  holdback, the veto digest that shows the declared tier, and demotion. A
  deterministic tier lint (an option whose action or consequence names a
  never-auto effect) is a later hardening slice, not a guarantee.
- **Veto is not undo.** A vetoed answer restores the decision record and task
  status only.
- **Sensitive content leaves the host.** Precedent notes and contexts are
  Daniel's own words and go to the external decision endpoint when enabled. The
  host grant must name them, and redaction is value-based, not semantic.
- **Shadow requires hiding.** Showing a shadow prediction to the human, by
  any surface, contaminates that category's labels. The category's readout is
  then invalid and needs a fresh window.
- **Vendor capabilities.** The packet-bound choice and per-precedent predicates
  depend on the Decisions API's choice and predicate support with per-question
  probabilities, which activation must recheck, as for every consumer.

## Phasing

Each slice is filed as a proposed task in `ws_orbit`. None is approved to build
by this design.

1. **Request risk tier and category** ([ORB-21091]). `risk_tier` per answer and
   per request, fixed core-gate tiers, optional raiser declaration, `unknown`
   default, a stored category, shown by list and show surfaces.
2. **Model-answer provenance, review gate and veto** ([ORB-21092]). The `model`
   actor and citation, the `model_answer_review` gate, the generalised reversal,
   the digest `model_answered` section, confirmation-only precedent and corpus
   rules. Depends on 1.
3. **Precedent-provider contract and built-in provider** ([ORB-21093]). The
   `orbit.precedent-provider/1` contract, host validation, the Orbit-history
   provider, the plugin-provider route and deterministic merge.
4. **Packet-bound choice questions in the assistance service** ([ORB-21094]).
   Allowed values copied from a packet field, bound by the template digest.
5. **Shadow consumer** ([ORB-21095]). Packet builder, enqueue on raise, sealed
   predictions hidden from every surface, the join to human answers and the
   readout export. Depends on 1, 3, 4.
6. **Evaluator contract `decision-assistance-eval/4`** ([ORB-21096]).
   `recorded_human_decision` provenance, prospective windows, per-category
   targets, coverage, accuracy and calibration scoring.
7. **Category enablement, self-answer and drift** ([ORB-21097]). The
   `orbit decision assist` surface and store, holdback, notification wait,
   self-answer apply, contradiction downgrade and automatic demotion. Depends on
   2, 5, 6.

The orbit-office decision ledger provider is designed in the paired orbit-office
task and depends only on slice 3's contract.

## Task References

- [ORB-21097] — proposes category enablement, self-answer, holdback and demotion.
- [ORB-21096] — proposes the `decision-assistance-eval/4` evaluator contract.
- [ORB-21095] — proposes the shadow precedent-decision consumer.
- [ORB-21094] — proposes packet-bound choice questions in the assistance service.
- [ORB-21093] — proposes the precedent-provider contract and built-in provider.
- [ORB-21092] — proposes model-answer provenance, the review gate and veto.
- [ORB-21091] — proposes risk tier and category on decision requests.
- [ORB-21089] — authors this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
