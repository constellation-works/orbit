# Tools: registry tool details and provenance

Contents:

- [Registry tool details](#registry-tool-details)
  - [GitHub run logs](#github-run-logs)
  - [GitHub auth status](#github-auth-status)
  - [Resident tools](#resident-tools)
  - [Distributed drain tools](#distributed-drain-tools)
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
rights (`dispatch`, `promote`, `merge`, `plugin_upgrade`, `refresh`, `recover`) and covered workspaces.

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
