---
title: Workspace agent corpus
type: design
owner: Orbit
last_updated: 2026-11-07
status: Draft
stage: shipped
next: "—"
feature: workspace-agent-corpus
doc_role: scope
summary: "Pinned activity-scoped corpus guidance and deterministic append proposals"
---

# Workspace agent corpus

Status: shipped. Corpus injection, append proposals and decision-rule consolidation are
implemented. The precedent auto-answer repairs [ORB-20822] and [ORB-20823] are done;
the document remains Draft pending recorded design approval.

Bearing: activity dispatch already pins repository guidance to a run's base revision;
task history supplies the durable append outbox. This scope joins those existing seams.

## Problem

Agent activities otherwise start without the workspace's reviewed issue mechanisms,
decision precedent and standing guidelines. Raw task history alone cannot serve as
trusted guidance or authorize a new decision.

## Goal

Deliver reviewed, activity-specific corpus guidance at a pinned revision and preserve
structured observations for later reviewed publication, with human curation of rules.

## Shape

A workspace keeps reviewed operational knowledge in `docs/agents/corpus/`:
`INDEX.md`, `issues/<mechanism>.md`, `decisions/<topic>.md` and
`guidelines/<topic>.md`. One issue describes one mechanism; its occurrences are
entries on that document, not separate incident documents. Nothing is enabled
by default. This is a repository corpus plus deterministic append outbox, not a
revival of the retired native project-learnings inference subsystem.
[ORB-20637] implements this corpus: the documents, pinned injection and the
append outbox.

### Schema and generation

Every document has YAML frontmatter with `type` (`issue`, `decision`,
`guideline`), `applies_to` (nonempty list of exact activity names), `status`
(`active`, `draft`, `resolved`, `superseded`) and `links` (list of source links,
possibly empty). The first heading names the document. An issue may declare
`match_infra_class: lock_busy` or `store_locked`, the engine's existing typed
classification. This optional field is the structured mechanism-match contract;
it does not accept an agent's diagnosis or message as a match.

Issues have Signature, Mechanism, Resolution, Fix task and Occurrences sections.
An occurrence records date, task, run, matched signature and outcome. Decisions
have Scope and Entries sections; entries record question, options, chosen option,
human note, task, run and request binding. Guidelines have Rule, Rationale and
Escalation sections. New mechanisms and guidelines are reviewed PR changes.
A landed cause fix changes its issue to `status: resolved`; history stays in Git.

The generator requires Python 3 and PyYAML, the same tooling dependency used
by auto-task body-history generation. `scripts/agent-corpus.py` validates frontmatter and sections and generates the
index table and versioned JSON row comments from the same rows. `make docs-index`
runs it, and `scripts/generate-doc-indexes.sh --check` (also in `make ci-guards`)
fails on stale rows. Never edit the index by hand. Other workspace repositories
can use `--repo-root` and `--corpus` with the same generator.

### Pinned injection

`workflow.agent_corpus` maps exact activity names to a repository-relative
`INDEX.md` path. A workspace has one corpus, shared by all mapped activities.
The host reads that file at the run's recorded worktree base commit (HEAD only
when a run has no recorded base). Task fields and worktree edits never supply
injected text. An owner captures selected rows and refusals at claim admission;
a claimed worker uses that capture, never its own corpus mapping. An older
owner's missing capture retains the existing tag-capture compatibility fallback.

Dispatch appends only rows whose `applies_to` includes the exact activity;
resolved issue rows are excluded. A decision document contributes one line per
current rule and nothing else. A decision document with no current rule is
omitted. Current means `status: current`, `review_by` has not passed, and
every `depends_on` digest still matches. Implementing (`agent_implement`) and recovery
(`final_recovery`) use the same dispatch boundary. `triage_escalation`
is the current triage activity name; `pilot_warning_triage` is only an example.
An unmapped activity, an unset key, or no matching rows adds no section or audit
event. Missing, malformed or oversized configured indexes refuse dispatch.

The configured path is at most 512 bytes; the index read limit is 1 MiB. Injected text is capped at 8 KiB, independently
of tag instructions. Whole rows are admitted in deterministic path order; a
truncation note replaces omitted rows. No UTF-8 character is cut. The audit event
`activity.agent_corpus_applied` records activity, step, index path, base revision,
delivered document paths, SHA-256, byte count and truncation. The run audit trail
is the receipt; it does not assert the agent actually opened every document.

### Deterministic writes and reviewed batches

The write path is a **task-history sidecar outbox, then one reviewed batch PR**.
There are no automatic commits, merges or new merge gates on `agent-main`.
This reuses the existing task-history store and lock; it adds no store or crate
edge. The index remains a repository artifact, never a view of unreviewed data.

When the human-answer apply step records `decision_answered`, the same task
record update appends a system `agent_corpus_append` history entry. It targets
`decisions/orbit-requests.md`. The entry derives from the accepted request and
human resolution, including all offered options and the note, not an agent's
recommendation. Rejected, expired or automated answers write no corpus entry. A precedent
answer is not a human answer and writes no corpus entry.
Decision source history remains authoritative if later batch publication fails.
With the config unset, no outbox entry is written. Request identity makes
materialization idempotent. The seed topic must exist before materialization.

Final recovery already receives the engine's `TransientInfraBlock`, never from
agent output. The deterministic applier matches its typed class against the
pinned issue rows, and records a system append beside the applied recovery
record in the same update. It carries date, task, run, step, matched class and
actual outcome (`escalated`). Replay writes no duplicate. Unknown classes and
resolved or unmatched issues produce no append. This contract owns the match
producer; it does not rely on the related decision-request task to create one.
Other structured producers can use the same outbox contract in future changes;
free-text recovery output cannot create an automatic occurrence.

An operator or a corpus batch task exports relevant owner task histories with
`orbit.task.show` (`fields: ["history"]`) into JSON under `.orbit/tmp/`. For
example, after saving a task-show response to `.orbit/tmp/corpus-history.json`:

```sh
python3 scripts/agent-corpus.py --records .orbit/tmp/corpus-history.json
```

Repeat `--records` for a batch of task exports, or supply a JSON array. The
materializer accepts only system outbox entries, validates destination paths and
sections, appends structured JSON entries under Occurrences or Entries, and
regenerates the index. Stable hashed identity markers make retries harmless.
It never deletes source history or acknowledges a pending append before a PR
lands. Keep the outbox indefinitely; later exports and retries are safe. Before
a reviewed batch lands the append is durable in the sidecar, not yet injected.
Materialization requires an existing reviewed topic; new issues must be authored
and reviewed first. Validate generated indexes before ordinary PR delivery.

When the effective `workflow.agent_corpus` mapping names a tracked corpus index and
the workspace provides `scripts/agent-corpus.py`, the pilot-block-patterns pass proposes
**every observed group** in that mapped corpus, independently
of the five-fix-task cap or recurrence threshold, in its own corpus PR. New
mechanisms get one issue document; known mechanisms receive evidence-backed
occurrences. Agent-authored synthesis is a reviewed proposal, not an automatic
append. It also materializes pending deterministic appends when exported source
histories are available. Counts and source links must agree with its report;
its report artifact alone is not the corpus. Historical blocked-task backfill
is separate and does not happen implicitly during injection. [ORB-20650]
backfilled the past month of blocked and escalated tasks. [ORB-22229] made the corpus
step conditional on those prerequisites: when either is missing, the pass records
which prerequisite is absent, still attaches its report and cursor, completes its
other rules and leaves the worktree clean; it does not create a corpus.

### Curated decision rules

Raw decision history, including Entries, is not a rule and is not authority.
A rule lives in an optional `## Rules` section of `decisions/<topic>.md`, one
fenced YAML document per rule. The corpus generator does not require that
section, so a history document with only Scope and Entries stays valid. A
rule has:

- `id`: a slug unique inside the topic file
- `question`: the question pattern this rule answers
- `answer`: the option id the rule chooses
- `why`: why that option is the rule
- `citations`: each human answer it was built from (`task`, `date` as
  `YYYY-MM-DD`, `option`)
- `depends_on`: `path` (a repo-relative file) or `config` (a dotted key),
  each with the SHA-256 digest last confirmed
- `status`: `current`, `stale`, or `superseded`
- `review_by`: `YYYY-MM-DD`
- `auto`: defaults to false. Consolidation never sets it
- `auto_set_by`: `operator` only when the operator set `auto`

A weekly workspace-local auto-task proposes rule edits [ORB-20747]. It is not a shipped
default. The template is `decision-precedent.auto-task.yaml` beside this
document. An operator copies it into `.orbit/auto_tasks/` and enables it
there. The run calls `orbit decision consolidate`, which prints proposed file
text and writes nothing. The pull request is the only place a rule changes,
and a human merge is the curation step. [ORB-20822] repaired precedent auto-answer
to read decision documents and path dependency digests at the workspace's pinned
integration revision, never from working-tree edits. The revision resolver prefers
the fetched origin base branch, then the local base branch, then HEAD
(`application/task/base_revision.rs::workspace_base_revision`). Config dependency
digests still read current workspace configuration. Consolidation and `rule-auto`
read the working tree as proposal/curation surfaces; their edits must land before
auto-answer can use them.

Each run re-checks every rule. A later human answer that chooses a different
option marks the old rule `superseded` (its id is renamed and `auto` is
cleared) and writes a new `current` rule. A rule whose `depends_on` digest no
longer matches, or whose `review_by` has passed, becomes `stale`. An already
superseded rule stays superseded. Replaying an answer a rule already cites
(the default history run does) changes nothing: [ORB-21053] repaired replay
idempotence and routing to an existing unique bare rule id. Only a new human answer
later than the recorded citation refreshes a rule's digests and review date,
and a stale rule is otherwise
re-confirmed by an edit in that pull request. A bare topic that names a rule
id held by exactly one decision file updates that file's rule; otherwise it
keeps the `topic/topic` default. Only a `current` rule whose review date has not passed and
whose digests still match is injected or cited.

An older replay cannot replace a choice with a later human citation already
recorded for that rule id, including a citation kept in its superseded
history; [ORB-21064] repaired that partial-history chronology case.
Consolidation still emits proposal text only; a human merge is the
curation step.

`auto: true` is operator-only (`orbit decision rule-auto`). An agent cannot
set it, and neither can the precedent actor. When `orbit.decision` action `raise`
names a `file/rule` topic, or a rule id that appears in exactly one decision
file, and that rule is one such current rule whose answer is one of the
request's options, Orbit answers immediately with that option. Matching a topic
equal to a topic file's stem is not this contract [ORB-20823]. The history
actor is `precedent`, not a human. The record carries
the rule id, the rule's citations, and that the operator approved the rule. A
comment tells the operator how to reverse it. A non-auto, stale, superseded,
ambiguous, or non-option match leaves the request open. Reversal is
`orbit decision reverse`, operator-only. It re-opens the request and restores
the task status from before the precedent answer.

### Authority and Library boundary

This corpus holds Orbit issue mechanisms, Orbit decision-request history and
standing workspace judgement rules. Daniel's product direction and general
office knowledge remain in the orbit-office Library. No Library read or write
is introduced. Only a current rule may be cited. Raw entries never answer a
new request. A current rule the operator marked auto may answer a request
that names it exactly; every other corpus match leaves the request open.
Separately, [ORB-21097] shipped default-off, operator-enabled category assistance
through the `model` actor
([precedent-decision scope](../_archived/decision-assistance/specs/precedent-decisions.md));
it was removed with decision assistance in [ORB-22808].
Task tags, comments and result messages cannot write trusted corpus
text or select an arbitrary revision for injection.

## Non-goals

- Reintroducing the retired project-learnings inference store.
- Treating raw decision history or agent narrative as trusted rules or automatic appends.
- Reading or writing the orbit-office Library.

## Concerns and honest limitations

- An append is durable in task history before publication, but is not injected until
  its reviewed corpus change lands. Materialization needs an existing reviewed topic.
- Injection records what guidance was delivered, not whether an agent read or followed it.
- Missing, stale, ambiguous or inapplicable precedent leaves a new request open for a human.

## Phasing

1. **Corpus, pinned injection and append outbox:** done in [ORB-20637].
2. **Historical corpus backfill:** done in [ORB-20650].
3. **Curated decision rules and opt-in auto-answer:** done in [ORB-20747]. The
   merged-source and exact-topic repairs are done in [ORB-20822] and [ORB-20823];
   replay/routing and chronology repairs are done in [ORB-21053] and [ORB-21064].
4. **Portable corpus proposals:** done in [ORB-22229], retaining every-group proposals
   where the workspace supplies the mapped corpus and script.

## Open questions

No implementation slice remains pending in this committed scope. Additional structured
append producers require separately scoped work.

## Acceptance (feature level)

- Dispatch uses pinned, exact-activity corpus rows and respects size bounds and refusals.
- Only trusted structured producers create idempotent task-history append proposals;
  reviewed publication precedes injection.
- Only current, matching, operator-auto precedent can answer a new request; raw history
  and unmerged corpus edits cannot supply that authority.
- Consolidation produces proposal text without refreshing authority from replayed history
  or replacing a newer curated choice with an older answer.

## Task References

- [ORB-20637] — implements the activity-scoped corpus, pinned injection and the deterministic append outbox.
- [ORB-20747] — adds weekly decision-rule consolidation, staleness checks and opt-in auto-answer.
- [ORB-20650] — backfills the corpus from the past month of blocked and escalated tasks.
- [ORB-20822] — pins precedent auto-answer rules and path digests to the workspace integration revision.
- [ORB-20823] — removes the topic-file-stem match from precedent auto-answer; a bare rule id must appear in exactly one decision file.
- [ORB-21053] — prevents replay from refreshing rule authority and routes unique bare ids to existing rules.
- [ORB-21064] — preserves newer curated choices during partial-history replay.
- [ORB-21097] — adds separately enabled category assistance through the model-answer route.
- [ORB-22229] — conditions pilot-block-patterns corpus proposals on the mapped corpus and script.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
