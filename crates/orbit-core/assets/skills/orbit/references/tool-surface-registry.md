# Tools: registry tool details and provenance

Contents:

- [Registry tool details](#registry-tool-details)
  - [GitHub run logs](#github-run-logs)
  - [GitHub auth status](#github-auth-status)
  - [Resident tools](#resident-tools)
  - [Distributed drain tools](#distributed-drain-tools)
  - [Experiment tools](#experiment-tools)
  - [Artifact and recovery tools](#artifact-and-recovery-tools)
  - [Workflow and crew memory tools](#workflow-and-crew-memory-tools)
- [Model provenance](#model-provenance)
  - [Historical agent invocations](#historical-agent-invocations)
- [Missing operations](#missing-operations)

## Registry tool details

Detail for tools whose registry description (`orbit tool show <tool>`,
`orbit tool list`) is kept to a sentence or two. The notice tools are under
[Fleet notices](tool-surface-notices.md#fleet-notices), `orbit.task.block_triage` under
[Pilot-warning triage](tool-surface-pilot.md#pilot-warning-triage) and `orbit.agents.scorecard`
under [Agent scorecards](tool-surface-scorecards.md#agent-scorecards).

### GitHub run logs

`github.run.logs` reads a bounded excerpt of one GitHub Actions run's logs:
failed steps by default, or the full log, plus runner checkout evidence.

- **Narrowing.** Scope the excerpt to a named step (`step`, usually with `job`),
  a byte window (`byte_start`/`byte_end`, 0-based, end-exclusive) or a line
  window (`line_start`/`line_end`, 1-based, end-exclusive). A window combined
  with `step` applies within the step section. An absent step or window fails
  without returning other log bytes. The response reports the selection and its
  `selected_bytes`/`selected_lines`.
- **Step mapping in a raw job log.** Steps are named only by their
  `Run <command>` display group, which closes before the command runs, so a
  step matched there includes its post-group runtime output until the next
  `Run` step. A workflow `name:` that differs from its `Run <command>` cannot be
  reliably mapped in a raw log and fails with an explicit unsupported-mapping
  error instead of a not-found error. Omit `step` and read by `job` with `scope`
  or a byte/line window to recover.
- **Limits.** The source stream is read incrementally with an 8 MiB stdout limit
  and a process timeout. A separate `diagnostic_unit` retains a unique complete
  failing runner command up to 256 KiB; display truncation does not imply
  missing command evidence. Source-limit exhaustion is retryable.
- **Recovery.** When the run-scoped read succeeds with no output, GitHub says
  the parent run is still in progress, or GitHub serves the run's log archive as
  an invalid zip, the excerpt can be recovered from the log API of a verified
  completed job belonging to that run; `failed` scope is restricted to
  unsuccessful jobs. `source` and `source_jobs` identify the recovery. Other
  errors remain errors.
- **Cache.** A completed job's or run's log is kept under `state/github-run-logs/`
  in the Orbit data root (`ORBIT_ROOT` when set, else `~/.orbit`), so paging one
  log in windows downloads it once. The directory is `0700` and each entry a
  `0600` file, written through a staging file and renamed into place. Agent
  sandboxes mask the directory, so an agent reads a cached log only through this
  tool's bounded, redacted response.
- **Redaction.** Before any byte is stored, each span the response redactor would
  rewrite is overwritten with `*` bytes of the same length, so byte and line
  coordinates stay those of the log GitHub served. Every read of a cached log, the
  read that fills the cache included, renders from that masked copy, so a cold and
  a warm read return the same output.
- **Read rules.** Every read first asks GitHub for the job's or run's status under
  the caller's current credential and checkout. The cache serves a read only when
  that request succeeds and reports the job or run completed. Keys use the host and
  repository GitHub reports, not the `repo` text the request names:
  - a job-scoped read (`job`): run, job and scope;
  - a run-scoped read: run, attempt and scope, so a rerun never serves an earlier
    attempt's log;
  - the job-log fallback: run and job, not scope, kept apart from the run-scoped
    entry for that job.

  A refused or failed status request is a plain read, never a cached log.
- **Never cached.** A job or run that is not completed, an empty log, a log whose
  secret the redactor finds only across lines, a read whose status request failed,
  and a status answer that names no host and repository. A read past the 8 MiB
  source limit fails and stores nothing.
- **Bounds.** At most 256 MiB in total and 24 hours per entry; there are no
  settings. Every store removes entries past either bound (oldest first for size,
  and any staging file a crashed write left behind), and a lookup removes an
  expired entry it finds.
- **Earlier releases.** Earlier releases kept unredacted logs in
  `cache/github-run-logs/`. Orbit does not read those files; it removes them when
  the cache is next opened or an agent sandbox is prepared.

### GitHub auth status

`github.auth.status` preflights the GitHub CLI surface: whether a GitHub CLI is
present on this execution lane (`available`) and whether it holds usable
credentials there (`authenticated`). Neither absence nor a missing credential
is an error; both are reported so a caller can record a capability-unavailable
outcome instead of mistaking it for a clean result.

`quota` is the host's GitHub
API quota record for this account: per resource (`core`, `graphql`) the limit,
remaining count and reset time (`state: unknown` when GitHub's rate-limit
answer could not be read), whether agent, background or delivery reads are
currently refused, and the calls each caller class made in the current window.

### Resident tools

`orbit.resident.grant` widens or narrows a resident mandate's authority: its
rights (`dispatch`, `promote`, `merge`, `plugin_upgrade`, `refresh`, `recover`,
`promote_held`) and covered workspaces.

- **Widening** (`add`, `workspaces`) is the human's. From an operator it applies
  at once. From any other caller it changes nothing and raises one
  `resident_authority` decision request that quotes `quote` verbatim and names
  your session; the authority changes only if a human answers `grant`. An open
  request for the same addition is returned, not duplicated; any authority
  change supersedes it.
- **Narrowing** (`remove`, `drop_workspaces`, a shorter deadline) applies at once
  for any caller that can write the assignment and is recorded with that caller.
- A call may not widen and narrow at once. Output is
  `{outcome, authority, request?}` with `outcome` `granted`, `requested` or
  `narrowed`.

`orbit.resident.metrics` reports the resident widening measures over a window,
derived read-only from task history, decision requests, checkpoints and audit
rows:

- manual interventions (per mandate and per active mandate-day);
- escalation load (per mandate-day and per human-day, median answer latency,
  share answered with the recommended option, expired-unanswered);
- reversal rate (resident decisions and actions a human undid within 7 days,
  each counted once);
- crashed-cycle recovery cost (cycles without a checkpoint, time to the next
  good checkpoint, recovery slot minutes, repeated actions, human interventions
  needed).

With `baseline`, the same measures are reported for an interactive assignment
task. A measure that cannot be derived reports `not_derivable`, never zero.

### Distributed drain tools

`orbit.drain.probe` is the read-only preflight for the distributed drain, served
by the workspace owner. It reports the owner machine, binary version,
distributed-drain protocol schema, the capabilities this session holds, the
diagnostic caller machine, the owner-resolved ship configuration, and both
review switches (`review.timing` and the after-landing auto-task).

Declaring
`caller_version`, `caller_schema` or `caller_before_pr` also reports the first
refusal an admission would raise, in the order admission applies it. It creates
no request receipt, reservation, claim or task transition, and it is never a
health check for admission itself.

`orbit.task.pull` is distributed-drain admission, served by the workspace owner.
It selects the first ready, valid, conflict-free backlog task in the owner's
order and, in one transaction, reserves its footprint, records an execution
claim for the calling machine, moves it to `in-progress` and stores the request
receipt. It answers `idle` (a receipt with no claim) when nothing is
admissible. The calling machine is the session's, never a field of the input. A
claim grants execution of that one attempt only; completion still needs owner
approval.

`orbit.drain.receipt.lookup` performs read-only reconciliation of one
distributed-drain admission request on the workspace owner. It returns the
original receipt and the current claim phase (`found`), a non-reusable tombstone
(`expired`), or `not_found`. It creates no receipt, binds no run, and grants no
execution authority: a receipt is historical evidence, and `not_found` is not
proof that an earlier request cannot still arrive, so it never licenses a
replacement request under a new ID.

`orbit.drain.claim.bind` binds an executor's local leaf run to its execution
claim on the owner, moving the claim from `claimed` to `running`. It is
idempotent: repeating the same run returns the recorded outcome, and a different
run is refused, so one claim never has two leaves. The owner fences the claim
against the session's own machine; a claim admitted to another machine, or
already settled or revoked, is refused as stale.

`orbit.drain.claim.settle` settles an execution claim on the owner. A
pull-request handoff is accepted only after the owner reads the pull request
from the provider and resolves its candidate and base in the owner's own
checkout, and only with digest-pinned validation evidence for each command the
owner requires; when the owner's required command list is empty, no validation
evidence is needed, and the handoff is accepted with no validation logs. A NoDiff
handoff reads no pull request and publishes no branch: the owner verifies the
sha-pinned NoDiff verifier checkpoint (this task/run's report, its base, and a
`verified_no_diff`, `verified_already_landed`, `verified_already_resolved`, or
`skipped_no_diff_expected` decision) against the owner's own checkout — the
candidate must equal its synchronized base with no footprint widening, that
base must still be contained in the owner's live base, and the clean-tree
verifier rechecks it at that revision — and only with digest-pinned validation
evidence for each command the owner requires. Two cases need none: a
`no-diff-expected` skip checkpoint, which carries no validation (the owner's
copy of the task must still carry the tag), and an empty required command list,
which accepts a verified checkpoint with no validation logs except that
`verified_already_resolved` still requires a passing replay log for each selector —
as does a report-backed `verified_no_diff` or `verified_already_landed`
checkpoint that carries `already_resolved_evidence.selectors`. A
`no-diff-expected` skip checkpoint carrying `already_resolved_evidence.selectors`
is refused. An accepted
handoff moves the task to `review`, which awaits owner
approval — it does not merge. A failure records its evidence and blocks the
task. A release (a cancelled drain giving back work it will not finish) returns
the task to the backlog with its reason as a comment. Each settlement releases
only this claim's reservation, and retries replay the recorded outcome.

`orbit.drain.claims` lists the workspace's execution claims for inspection and
deliberate recovery: phase, age, reservation expiry, execution machine, bound
run, last recorded event, unresolved merge intent, and landing invalidation.
Age, an expired reservation, and an absent local run are diagnostics, not proof
that a worker died — nothing here reclaims, rebinds, or repairs a claim.

`orbit.drain.claim.effort` reports, after the fact, the reasoning effort and
implementing model a claimed leaf ran at when its settlement carried none, so
the owner's agent scorecards stop counting the attempt as effort unknown or
model unknown.

- It is accepted only from the machine that ran the claim, for the leaf run
  bound to it, once the claim is settled. A claim that settled before the owner
  kept settlement records is accepted only when the task's history names it as
  pulled.
- It is attribution only: it changes no claim, task, run or settlement outcome.
- A value the claim already holds is never overwritten: the same value replays,
  and a different one is refused as `effort_conflict` or `model_conflict`,
  recording neither.

### Experiment tools

Mutating experiment verbs (`create`, `update`, `clone`, `start`, `pause`,
`resume`, `reweight`, `assign`, `conclude`, `abandon`, and CLI-only `delete`)
require operator capability; agent sessions are refused. `show`, `list` and
`readout` are read-only.

- `orbit.experiment.create` defines a new draft experiment: its arms (one
  control plus treatments, each with a weight and an overlay), the tasks it
  enrolls (`population`), its primary metric, guardrails and sample rules. Arms
  and overlays are validated on write and each overlay's digest is recorded; a
  draft assigns nothing.
- `orbit.experiment.update` edits a draft experiment's name, hypothesis, arms
  (the given list replaces every arm), population, metrics and sample rules.
  Only a draft can be edited; a started experiment is refused and changes only
  through `pause`, `resume`, `reweight`, `conclude` or `abandon`, or is cloned
  into a new draft.
- `orbit.experiment.clone` copies an experiment's definition (hypothesis, arms
  with current weights and overlays, population, metrics, sample rules, mode)
  into a new draft under a new name and a fresh assignment salt; retired arms
  are not copied and the source is unchanged.
- `orbit.experiment.start` starts a draft experiment, opening segment 1 and
  freezing its definition. It is refused unless `experiments.enabled` is true,
  while 3 experiments are already running, and unless the definition is
  complete: at least two arms with unique names, exactly one control, positive
  weights, a primary metric, and config overlays only for keys on
  `experiments.config_keys`.
- `orbit.experiment.reweight` changes a running or paused experiment's arm
  weights, retires arms, or both; each call opens a new segment recorded in the
  event log with weights before and after so tasks stay distinguishable.
- `orbit.experiment.assign` forces a nonterminal task with no arm assignment, or
  with an `explicit_crew` exclusion for this experiment, into an active arm of a
  running experiment (shadow or live), recording `forced=true`. Existing arm
  assignments, other exclusions and overlapping experiment surfaces are refused.
- `orbit.experiment.conclude` concludes a running or paused experiment,
  recording the chosen arm (or none), the reason, and the readout JSON at that
  moment on the conclusion event (which `orbit experiment show` returns). A
  chosen arm files one proposed task naming the experiment, arm, readout
  verdict and overlay; no arm files nothing. Assignment stops; fresh admissions
  apply no overlay while already-admitted runs and claims keep their captured
  treatment. It changes no configuration or repository file.
- `orbit.experiment.delete` (operator CLI only) deletes a draft, concluded or
  abandoned experiment with its arms, events and assignments. It is refused for
  running or paused experiments, any that ran live and assigned tasks to arms
  (their crews and runs cite it), and any experiment concluded with an arm (its
  promotion task names it), including shadow experiments. Shadow experiments
  concluded without an arm applied nothing and can be deleted.
- `orbit.experiment.readout` compares experiment arms read-only (outcomes,
  intervals, differences, verdict) from its assignments and Orbit records;
  `--metric` narrows the report while the verdict stays on the primary metric
  and guardrails. Estimator, exclusion and verdict details:
  `https://orbit-cli.com/how-to/experiments/#read-the-readout`.

### Artifact and recovery tools

- `orbit.task.artifact` action `put` (`orbit.task.artifact.put`) stores up to
  1 MiB from a source file confined inside the workspace after symlink
  resolution (write evidence under `.orbit/tmp/`). Paths beginning with
  `review-` in any ASCII case are reserved for the review gate's system writer;
  the only agent-writable exception is the schema-checked `review-report.json`.
  Use a nonreserved name such as `examination-checks.json` for ordinary
  supporting evidence.
- `orbit.task.artifact` action `get` (`orbit.task.artifact.get`) reads one
  stored artifact by owning task ID and path (list first with `orbit.task.show`
  `field: "artifacts"`), bounded to 1,048,576 bytes. It returns `media_type`,
  `size`, and the bytes as UTF-8 `content` or base64 `content_base64`. Supported
  raster images (PNG, JPEG, GIF, WebP) matching their declared type are also
  returned as viewable image content; SVG, HTML and mismatched payloads return
  opaque base64 only.
- `orbit.task.recover` action `repilot` (`orbit.task.repilot`) explicitly
  requests one new pilot assessment for a `proposed` or `backlog` task
  (operator authority on the owner, or a trusted live resident cycle holding
  `dispatch` for its mandate's own workspace via the broker; foreign
  workspaces are refused with `resident_scope_refused`, cycles without
  `dispatch` with `resident_right_refused`, stale cycles with
  `resident_cycle_stale`, and other managed runs or unbrokered callers stay
  operator-only and are refused; refuses pending/running pilots, live
  delivery, human crews and replica checkouts; never promotes or
  dispatches). When the latest assessment holds promotion, the pilot
  re-checks holding findings and keeps fields untouched by the task's
  changes, falling back to a full assessment if the repository moved at its
  footprint, scope changed beyond a bounded edit, or the prior assessment is
  too old or was a re-check; `full` forces a full assessment.
- `orbit.task.recover` action `resume_delivery` (`orbit.task.resume_delivery`)
  starts a recovery leaf for a blocked task's preserved delivery
  through an authenticated cycle with the `recover` right, continuing on the
  current base; the leaf may edit the preserved candidate and submits it to
  the normal PR pipeline with review as the completion target. Fresh
  validation and review run where their gates apply, subject to normal
  light-delivery exemptions (limit: one attempt per cycle, one per task per
  24 hours; landing requires separate `merge` authority; handoff remains at
  review).
- `orbit.task.recover` action `retry_handoff` (`orbit.task.retry_handoff`)
  retries a stopped or uncertain authorized handoff on its explicit owner
  workspace (operator only; managed runs refused). It compares the exact
  accepted candidate/base and latest attempt/state, requires a reason and
  durable `request_key` (replaying identical input returns the original landing
  run), and runs the normal landing job, which reconciles external intent and
  rechecks all merge gates. See
  [distributed-drain-recovery.md](setup/distributed-drain-recovery.md) and
  [tool-surface-capability-map.md](tool-surface-capability-map.md) for
  `reconcile_review`, `review_reset`, `requeue`, `recover_claim` and
  `revoke_handoff`.

### Workflow and crew memory tools

- `orbit.run` action `show` (`orbit.workflow.run.show`) fetches one durable
  workflow run by ID (operator only). The full response puts agent invocation
  outcome, failure reason and parsed answer (summary, findings, next steps and
  extra result fields; stdout blob references retain full provider output) under
  top-level `agent_invocation`, or at `run.agent_invocation` with
  `view: bounded`.
- `orbit.memory.show`, `orbit.memory.write` and `orbit.memory.propose` are
  dormant, run-bound crew-memory operations (never listed by `orbit tool list`
  and refused for operators and ordinary sessions). `show` reads a space's
  newest or chosen `revision` (digest, size, cited evidence, writer and reason;
  empty when uninitialized) from a crew-memory dreaming run or a memory
  experiment arm, limited to its own model space or `global`. `write` replaces a
  dreaming task's own model space whole (at most 8 files and 6 KiB total, file
  names `[a-z0-9-]{1,40}.md`, at least one evidence task ID from the run's
  evidence pack, score-blind and not-instructions lints; `global` and other
  models' keys are refused). `propose` proposes a global-memory note (at most 2
  proposals per dreaming run, 3 KiB cap, same provenance and lints).

## Model provenance

MCP tool schemas do not accept `model` or `agent`. Attribution comes from the
session: a managed run's `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL`, or the
crew named by `orbit mcp serve --orchestrator`. On task creation, `created_by`
records that family; a full model ID is additionally retained once as
`created_by_model`. The CLI's `--model` is the explicit override on
`orbit tool run` and the human task commands that still take it.

Family
inference on that path is case-insensitive: `gpt-*`, `o1*` and `o3*` identify
codex; `claude-*`, `opus*`, `sonnet*` and `fable*` identify claude; `gemini-*`,
`grok-*`/`grok3*` and `muse-*` identify their respective families. Unknown
values are refused, and trusted runtime identity takes precedence over a
self-reported override. An `agent` input is refused.

Audit `session_model`
ignores tool input. For an MCP call it is the brokered job's resolved crew
model, otherwise a managed run's `ORBIT_AGENT_MODEL`, otherwise the
`--orchestrator` crew's configured model, otherwise `clientInfo.name`.
`clientInfo` stays audit-only: it is `self_reported_actor`, and `session_model`
only in that last position. It is not `created_by` and it is not the
authenticated role.

### Historical agent invocations

Host invocation and explorer delegation have been removed. Dispatch work
through tasks and `orbit run ship`, or use the provider's CLI for a one-off
question. Historical invocation answers remain readable with
`orbit run show <RUN_ID>` and `orbit run logs <RUN_ID>`.

## Missing operations

A CLI-only operation is not a nonexistent product feature. It requires process
access on the owning host, within the user's authority. Where the session
requires all durable operations through a particular MCP connection, stop and
report a missing operation instead of using CLI, SQLite, HTTP, or another MCP
server as a fallback. Read-only source inspection remains separate from durable
control-plane operations.
