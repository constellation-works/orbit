# Configuration: activity-scoped agent corpus

## Contents

- [Activity-scoped agent corpus](#activity-scoped-agent-corpus)
  - [Verify a mapped activity](#verify-a-mapped-activity)
  - [Proposed corpus updates versus reviewed repository changes](#proposed-corpus-updates-versus-reviewed-repository-changes)

## Activity-scoped agent corpus

`[workflow.agent_corpus]` maps exact workflow activity names to a shared,
committed workspace corpus index (`INDEX.md`) so mapped activities receive
bounded, activity-scoped workspace knowledge (issue mechanisms, current
decision rules and workspace guidelines) at dispatch. The table is unset by
default, which maps no activities and injects nothing. Do not enable or
populate it on a live workspace unless the operator asks for it.

```toml
[workflow.agent_corpus]
agent_implement = "docs/agents/corpus/INDEX.md"
final_recovery = "docs/agents/corpus/INDEX.md"
task_pilot = "docs/agents/corpus/INDEX.md"
triage_escalation = "docs/agents/corpus/INDEX.md"
```

- **Authoring and layering:** edit `<workspace>/.orbit/config.toml` (or
  `~/.orbit/config.toml`) directly. Like `[workflow.tag_instructions]` and
  `[workflow.review_skip_tags]`, `[workflow.agent_corpus]` is TOML-only: it is
  not listed by `orbit config keys`, and `orbit config get` and
  `orbit config set` do not read or write it, so a key-only audit will miss
  it.

  Global and workspace files layer per activity key (a workspace entry
  replaces the global entry for that activity), and the merged table is
  validated whenever configuration loads (`orbit config show`, `orbit doctor`).
- **Limits and admission rules:** at most `32` activities may be mapped. Each
  key must be a non-empty exact activity name containing only ASCII letters,
  digits and `_`. Every mapped activity must point to the **same**
  repository-relative path ending in `/INDEX.md` (at most `512` bytes, `/`
  separators, a leading `./` stripped, and no leading `/`, `\`, NUL, empty,
  `.` or `..` segments).

  Mapping different activities to different index paths
  or violating any limit fails configuration loading.
- **Exact activity matching and pinned source:** only an activity whose exact
  name is mapped in `[workflow.agent_corpus]` is checked; unmapped activities
  receive nothing. On an owner or local run, the host reads the configured
  `INDEX.md` (and any referenced decision documents) from the Git commit at
  the run's recorded worktree base revision (`base_sha` from the `worktree`
  step, or `HEAD` only when a run has no recorded base).

  Uncommitted worktree
  edits, task fields, comments, tags and agent output never supply injected
  corpus text.

  The committed `INDEX.md` blob at that revision must be valid
  UTF-8, at most `1 MiB`, and carry `<!-- agent-corpus-schema: 1 -->` with
  generated `<!-- agent-corpus-row {...} -->` JSON row comments (`type` of
  `issue`, `decision` or `guideline` with relative `.md` paths under
  `issues/`, `decisions/` or `guidelines/`, `status`, `applies_to`, `title`,
  `links` and optional `match_infra_class`); a missing, oversized or malformed
  index refuses dispatch for the mapped activity.
- **Claimed runs:** at claim admission, the owner resolves each mapped
  activity's corpus injection at the claim's authoritative base revision
  alongside tag instructions and records the result in the claim receipt. A
  claimed worker uses the owner's captured rows from that receipt rather than
  its own local `[workflow.agent_corpus]` table or checkout (falling back to
  the replica's local configuration with a warning only when an older owner's
  receipt captured none).
- **Filtering and 8 KiB byte bound:** from the index rows at the pinned base
  revision, dispatch selects only rows whose `applies_to` list includes the
  exact activity name, and omits `issue` rows whose `status` is `resolved`.

  A
  `decision` row contributes one line per `current` citable rule in that
  decision document at the pinned base revision (where `status: current`,
  `review_by` has not passed, and every `depends_on` path or config digest
  matches); a decision document with no current rules (or that cannot be read)
  is skipped without failing dispatch, and raw decision history entries are
  never injected as rules.

  Whole selected rows are appended in index order
  under `Workspace agent corpus (<index> at <revision>)`. The injected text is
  capped at `8 KiB` (`8,192` bytes) independently of tag instructions: when
  the next row would exceed `8,192` bytes (including room for the truncation
  notice), remaining rows are omitted, `truncated` is set to `true`, and
  `[Corpus truncated at 8192 bytes; remaining rows omitted.]` is appended.

  If
  no rows match after filtering, nothing is injected and no audit event is
  emitted.

### Verify a mapped activity

1. Confirm configuration loads cleanly:

   ```bash
   orbit config show
   orbit doctor
   ```

2. After a run executes a mapped activity, inspect its `activity.agent_corpus_applied`
   event in the run's v2 audit trail (`orbit run show` does not print a header
   line for corpus injection):

   ```bash
   orbit run events <run_id> --type activity.agent_corpus_applied --json
   orbit run events <run_id> -s <step_id> --type activity.agent_corpus_applied --json
   orbit run trace <run_id>
   ```

   Each `activity.agent_corpus_applied` event (`body_kind: "agent_corpus_applied"`)
   records `step_id`, `activity_name`, `index` (the configured `/INDEX.md`
   path), `revision` (the pinned base commit SHA read by the host), `paths`
   (the relative document paths of the delivered rows), `sha256` and `bytes`
   (the lowercase hex SHA-256 digest and byte length of the injected block;

the prompt text itself stays out of the audit store), and `truncated`
   (`true` when rows were omitted at the 8,192-byte bound, `false` otherwise).
   An unmapped activity or a mapped activity with zero matching active rows
   writes no `activity.agent_corpus_applied` event.

### Proposed corpus updates versus reviewed repository changes

While `[workflow.agent_corpus]` is configured, two deterministic host paths
record structured `agent_corpus_append` entries in task history (`orbit task show <task-id> --json`):

- **Human decision answers:** when a human resolves a decision request
  (`decision_answered`, excluding automated or precedent answers, model-answer
  reviews and resident-authority grants), the same task update records an
  `agent_corpus_append` proposal targeting `decisions/orbit-requests.md`
  (`Entries` section).
- **Typed recovery matches:** when `final_recovery` is mapped and the engine's
  typed `TransientInfraBlock` classification matches an unresolved `issue`
  row's `match_infra_class` for `final_recovery`, the task update records an
  `agent_corpus_append` occurrence proposal targeting that issue document
  (`Occurrences` section).

These `agent_corpus_append` history records are sidecar proposals, **not**
automatic commits, merges or live corpus mutations.

Because dispatch reads
only committed blobs at the run's pinned base revision, proposed appends — and
any unmerged corpus document or decision-rule edits — do not change what
activities receive until they are materialized into the repository's corpus
files, `INDEX.md` is regenerated by the repository's own documentation
tooling, and the change lands through ordinary code review. Orbit does not ship
an installed subcommand or script that generates `INDEX.md`.
