---
title: Auto-tasks — Overview
owner: claude
last_updated: 2026-11-01
last_validated: 2026-10-29
status: Accepted
stage: shipped
next: "—"
feature: auto-tasks
doc_role: overview
type: design
summary: Dynamically-defined recurring task templates minted by the host clock tick — periodic work as data, not code.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**"]
related_features: [auto-tasks, routines]
related_artifacts: [ORB-10149, ORB-10318, ORB-10348, ORB-10439, ORB-10446, ORB-10514, ORB-10549, ORB-10950, ORB-11054, ORB-11095, ORB-12931, ORB-12932, ORB-15327, ORB-18069, ORB-18124, ORB-18125, ORB-18126, ORB-18127, ORB-18128, ORB-18129, ORB-18134, ORB-20704]
---

# Auto-tasks — Overview

Auto-tasks turn recurring chores into **data instead of code**. An auto-task
definition is a YAML record under `.orbit/auto_tasks/` (per-user checkout state) with a schedule, an `enabled` toggle,
a task template, and a dedupe policy. One generic scheduler pass reads the
enabled definitions, fires the due ones, and mints a task from each template.
Adding a new periodic chore is a new definition (`orbit auto-task add`), never
new orbit code or a new routine.

The scheduler pass is called directly by the host clock tick (`orbit clock tick`, alias
`orbit sweep`) for every registered owner checkout. No routine, job, activity, or job run
mediates auto-task evaluation. See
[Auto-task definitions are evaluated by the host tick, not fired by a routine](./4_decisions.md#auto-task-definitions-are-evaluated-by-the-host-tick-not-fired-by-a-routine).

## 1. Motivation

Every periodic need in orbit used to be bespoke: qa-sweep (ORB-10039) was an
inline check sweep, ship-sweep had its own systemd unit (since retired), and each future
recurring chore would mean another hardcoded routine. The marginal cost of a new
chore was a code change, a review, and a release. Auto-tasks replace the pattern
with a primitive so the marginal cost is a YAML record. qa-sweep V1 (ORB-10148)
becomes just the first definition.

## 2. Core Concepts

- **Definition** — a `.orbit/auto_tasks/<name>.yaml` record; `name` is identity.
  Modeled on the file-backed routine convention (directory scan, fail-closed
  parse, `deny_unknown_fields`), not the SQLite-indexed artifact convention.
- **Schedule (cadence)** — a 5-field `cron`, an `every_minutes` interval, a
  `deliveries_landed` trigger, or a `condition` predicate in the definition's `schedule` field. A `condition` fires once per subject edge on the workspace owner ([condition triggers](../condition-triggers/1_scope.md)); `cron`, `every_minutes` and `deliveries_landed` take an optional `when` guard. Time-based
  catch-up always collapses: a downtime gap mints one make-up task, not one per
  slot. Delivery triggers use verified landings and coverage checkpoints.
  Cadence is per-definition data, not
  a knob in the identity `config.yaml` ([L-0014] keeps runtime config out of
  `config.yaml`).
- **Time cursor** — per-definition last-fired state for cron and interval
  schedules, host-local at `<orbit_dir>/state/auto-tasks.json`, so the
  definition YAML is never churned by a scheduler fire. Delivery-trigger
  definitions use durable Automation checkpoints, batches and receipts in the
  host SQLite store instead.
- **Scheduler** — the host clock tick calls the evaluator directly after routine
  evaluation. Scheduling an auto-task creates no job run; fire evidence is the
  minted task and tick report, with time cursors for cron/interval definitions
  and durable batch/receipt state for delivery-trigger definitions. Auto-tasks
  are shown on the Operations auto-task panel, not on the routines surface.
- **Listing state** — whether a definition fires on this host is decided once,
  in `OrbitRuntime::auto_task_listing_state`, and `orbit auto-task list`, MCP
  `orbit.auto_task.list` (plain and bounded) and `GET /api/auto-tasks` render
  that decision. It carries the effective enabled state (the toggle, or for
  one release the deprecated `after-landing` policy), the skip reason when it
  never fires here — its seeding plugin is off, or it is an enabled delivery
  definition another machine owns — and the schedule state with the next due
  slot. The CLI labels the row `inactive`, `skipped`, `enabled` or `disabled`
  and prints each skip reason on stderr. The bounded view and the dashboard
  show `scheduled`, `waiting`, `never_observed`, `disabled` or `unavailable`.
  A definition owned elsewhere, an unreadable cursor file or a schedule that
  cannot be projected reads `unavailable` on its own row only. The bounded
  list also reports up to 20 redacted, bounded `load_errors` with file paths
  and reasons, plus `load_errors_total` and `load_errors_truncated`; these
  diagnostics are independent of item pagination and `cursor_state_error`.
  If no definition loads and any fails, the list fails closed.
- **Dedupe & provenance** — each minted task carries an `auto-task:<name>` tag;
  `skip_if_open` uses that tag to avoid firing while a prior instance is open.
- **Manual mint** — `orbit auto-task mint <name>` mints one task from a
  definition immediately, reusing the scheduler's mint path so the result is
  indistinguishable from a fired instance. Unconditional and cursor-inert: it
  ignores schedule, `dedupe`, and `enabled`, and never reads or writes
  `<orbit_dir>/state/auto-tasks.json`. Lookup and mint share that file's lock
  with deletion, so a definition removed before admission is not minted
  (ORB-10439, ORB-13432).
- **Default catalog** — Orbit embeds a small catalog of workspace definitions.
  Initialization materializes a missing catalog file under `.orbit/auto_tasks/`
  but every default is `enabled: false`. Seeding neither mints a task nor
  enables a routine or auto-task; an operator must explicitly enable the
  definition or use the existing manual-mint surface. Re-initialization
  preserves an existing definition byte-for-byte, including with workspace
  reconciliation `--force`; destructive lower-level initialization reseeds
  only because it recreates the Orbit directory.

## 3. At a Glance

| Concern | File | Task |
|---|---|---|
| Definition schema | `crates/orbit-types/src/workflow/auto_task/definition.rs` | ORB-10149 |
| Discovery (fail-closed) | `crates/orbit-automation/src/auto_tasks/loader.rs` | ORB-10149 |
| Due-math + catch-up | `crates/orbit-automation/src/auto_tasks/schedule.rs` | ORB-10149 |
| Host-local cursor | `crates/orbit-core/src/application/auto_tasks/state.rs` | ORB-10149 |
| Scheduler pass | `crates/orbit-core/src/application/auto_tasks/scheduler.rs` | ORB-10149 |
| CRUD (CLI + MCP shared) | `crates/orbit-core/src/application/auto_tasks/crud.rs` | ORB-10149 |
| Manual mint (`mint`, CLI + MCP) | `crates/orbit-core/src/application/auto_tasks/crud.rs` | ORB-10439, ORB-10798 |
| Operator settings over bundled bodies, fork migration | `crates/orbit-automation/src/auto_tasks/settings.rs`, `crates/orbit-core/src/application/auto_tasks/settings.rs` | ORB-14909 |
| Retired scheduler routine migration template | `crates/orbit-core/assets/routines/retired/auto_task_scheduler.yaml` | ORB-12237 |
| Default auto-task catalog | `crates/orbit-core/assets/auto_tasks/…` | ORB-10549, ORB-10550, ORB-10950 |

## Embedded default catalog

These YAML files live under `crates/orbit-core/assets/auto_tasks/` and are
registered in `DEFAULT_AUTO_TASK_FILES`. The directory also holds the retired
`blocked-task-triage.yaml`, which `RETIRED_AUTO_TASK_FILES` registers instead.
A definition joins this catalog only on proven yield
([promotion](#promotion-to-the-shipped-defaults)). `orbit workspace init` materializes a
missing file as `enabled: false`; re-init does not overwrite a workspace-authored
definition of the same name.

- `qa-sweep` — hands-on validation of recent changes (ORB-10148, embedded by ORB-10550).
- `host-health` — disabled-by-default daily review of host doctor findings and Orbit service logs, with actionable operator resolution steps.
- `friction-curation` — disabled-by-default daily evidence-first pass that deduplicates open
  friction records, re-verifies survivors against current behavior, resolves
  records that no longer reproduce, and files non-duplicate fix tasks for
  verified-real issues (ORB-10440, embedded by ORB-10549).
- `security-review` — disabled-by-default weekly evidence-backed review of
  applicable application code, dependencies, secret handling, and configuration;
  each actionable finding is filed as a durable Orbit task, and a clean review
  is a successful no-op (ORB-10950).
- `code-review` — disabled-by-default six-hourly review of commits merged
  since the previous sweep's recorded cursor. Its cursor selector requires the
  `auto-task:code-review` provenance tag, so only a minted sweep can be the
  cursor task.
- `full-code-review` — disabled-by-default, minted on demand (its monthly cron
  stays off until enabled). The minted coordinator pins one integration-branch
  commit and counts only source in programming or scripting languages, tests,
  build and maintenance scripts, and CI workflow definitions at that SHA.
  Exclude documentation and prose, lockfiles, dependency manifests and
  declarative configuration, test fixtures and other test data, and generated,
  vendored, minified, binary and static asset files; record exclusions as path
  patterns with reasons and file counts. Split candidate areas above 25,000
  included lines, then merge every candidate below 10,000 lines with related
  candidates while the combined area stays within 25,000 lines. If included
  code totals 25,000 lines or fewer, make it one area. Do not file an area under
  5,000 lines unless no in-bound merge is possible, and record the reason in
  `full-review-areas.json`. That artifact records the inventories, counts and
  exclusions. It files at most 24 area-review chores per run, ranking
  hard-to-review areas (concurrency, persistence, security, process control)
  first and then the least recently reviewed, and records the rest as `deferred`.
  Each filed chore is one bounded area tagged
  `full-code-review` + `no-diff-expected` — never `code-review` — at `hard`
  complexity or below with no pinned crew. Area reviewers read every partition
  of at most 10,000 lines in full, recording file/range coverage and findings
  in `read-progress.json`; resumed and re-crewed runs continue from that
  artifact. Parallel reading is used when the crew and execution rules permit
  it. Incomplete coverage reports `review_incomplete` with uncovered partitions
  in the artifact and task comment. Findings are `bug`s tagged `code-review` +
  `full-code-review`, with source-file and matching test context selectors
  [ORB-18948]. Reviewers skip `nit` findings, anything already fixed at the
  integration tip or tracked by any task, and re-search before retrying a
  failed filing [ORB-20705].
- `qa-full-sweep` — disabled-by-default, minted on demand (its monthly cron
  stays off until enabled). It is the hands-on counterpart of
  `full-code-review`: where `qa-sweep` validates only the changes since its
  cursor and `full-code-review` reads the tree without running it, this sweep
  runs the whole product at one pinned commit. Its body is product-agnostic.
  The minted coordinator pins the integration-branch tip by full SHA and
  inventories the user-facing surface from the repository's README, setup and
  run instructions, command help trees, API routes, UI pages and documented
  workflows — or from the repository's own full-QA runbook or harness when it
  ships one. It attaches `qa-full-sweep-inventory.json` (`schema_version`,
  `pinned_sha`, areas with their scenarios, exclusions with reasons) and files
  one surface-QA chore per area of at most 20 required scenarios, tagged
  `qa-full-sweep` + `no-diff-expected`, at `hard` complexity or below with no
  pinned crew; it files nothing while an earlier sweep's tasks are open. Area
  testers build the pinned commit and exercise the area through its real
  boundaries in isolated, disposable roots, reusing `qa-sweep`'s sandbox
  guidance. Each scenario is recorded as `PASS`, `FAIL`, `BLOCKED` or
  `NOT_RUN` with its command and evidence in `qa-progress.json`, which a
  resumed or re-crewed run continues from; a check that needs a missing
  capability (authentication, a provider, another OS, a browser, public
  post-release state) is never `PASS`. Non-duplicate failures are `bug`s
  tagged `qa-full-sweep` with a reproducer, the pinned SHA and source/test
  context selectors. Areas never repair, dispatch, release, tag, publish or
  deploy.
  **The release decision lives on a roll-up chore.** The coordinator files it
  after the areas, tagged `qa-full-sweep` + `qa-full-sweep-rollup` +
  `no-diff-expected` + `auto-task:qa-full-sweep`, with no crew and no `os:`
  tag. Its description and acceptance criteria carry an owner-evidence
  contract: area-task status reads and each area's `qa-progress.json` are
  evidence only the owner task store can produce, because a claimed
  `orbit.task.show` or `orbit.task.artifact.get` refuses an id other than
  the claimed task. Preparation records that contract as `required_machine`,
  so a follower is not offered the roll-up and the owner's drain may start
  it. The coordinator sets the roll-up's `dependencies` to every area task
  with `orbit.task.update` on the owner, and finishes only when that field
  lists every area. When the update is unavailable or refused, it returns a
  `qa_rollup_dependencies_unwired` blocker for the owner to apply; a comment
  that names the area IDs is not the wiring. The roll-up is dispatched only
  once every area is `done`. It reads each area's `qa-progress.json` on the
  owner and attaches `qa-full-sweep-decision.json`: `PASS` only when every
  area is done and every required scenario passed at the pinned SHA; any
  required `FAIL`, `BLOCKED` or `NOT_RUN`, or an area whose progress is
  missing, unreadable, or names another SHA or area, makes it `INCOMPLETE`.
  Area tasks stay portable: they do not carry the contract. Because the
  roll-up carries the definition's provenance tag, `skip_if_open` sees the
  sweep as open until the decision is made, so a scheduled tick never files a
  second set while areas are open. The roll-up also writes the swept commit as
  `sweep-cursor.json`, which the definition's `skip_if_unchanged` cursor
  (keyed on `qa-full-sweep-rollup`) reads to skip a tick when nothing has
  landed since.
- `doc-duties` — disabled-by-default daily validation of the
  least-recently-attempted tracked documentation. Candidates exclude generated
  and historical records, including paths in the repository's link-check or
  doc-index exclusion list, when present; each candidate is checked against
  that list before selection. Each run attaches a
  `doc-duties-ledger.json` artifact with an outcome (`clean`, `fixed`,
  `partial`, `skipped`) and attempt date per selected document. Each ledger
  also carries a cumulative `state` map holding every path's two latest
  attempts, rewritten in full every run. The next run starts from the newest
  readable ledger's `state` (read through the artifact tools, never from
  summary prose); ledgers from before `state` existed are layered in from the
  60-run history window. Retention therefore does not depend on how many
  completed tasks the window covers: a bounded window alone would forget old
  attempts and starve later documents. Rotation orders by the latest attempt,
  then an existing `last_validated` date, then the git last-touched date, so a
  skipped document moves to the back instead of heading every batch. A document
  whose two latest attempts were `skipped` or `partial` is held back, even after
  those attempts leave the window, and reported as needing its own task.
  `last_validated` advances only on `clean`/`fixed`, and no frontmatter is added
  solely for this task. Its template locks `dir:docs` plus the tracked
  top-level `README.md` and `CONTRIBUTING.md` documents its brief validates
  (never the whole tree, which starves on a busy drain) and does not carry
  `no-diff-expected`, since verified drift corrections produce diffs. Clean
  batches use validated no-diff evidence.
- `comment-hygiene` — disabled-by-default daily check of code comments, the
  counterpart of `doc-duties` for source files. At most six files and about
  1,500 lines per run, judged against a generic language-agnostic standard a
  workspace may sharpen through a `comment-hygiene` tag instruction. It
  reuses the doc-duties ledger (`comment-hygiene-ledger.json`, `state`
  carry-forward, `clean`/`fixed`/`partial`/`skipped`, repeat-skip escalation,
  clean no-diff evidence), adding a per-row `checked_through` commit so a
  file unchanged since its last `clean`/`fixed` check is not re-read. Its
  diff is comment-only, real defects become filed tasks, and it mints as
  `proposed` without `context_files`, so pre-admission preparation (task
  pilot or operator) establishes the selected `file:` selectors before it
  reaches `backlog` — never a selector-free whole-tree holder in a drain
  (design §4e).
- `run-failure-patterns` — disabled-by-default weekly, rate-driven scan of the
  workspace's own pipeline failures. It always analyses the 7 days before scan
  start (the prior cursor only splits out new-since-cursor occurrences) and
  also computes 30-day rates, storing per-job raw and folded failure rates with
  deltas in `run-failure-report.json` (schema version 2). On an owner it also
  reads the failure and release settlements followers' claimed leaves sent,
  through `orbit run settlements --no-reconcile`, since those leaves never
  enter the owner's run history. A follower settlement duplicates an owner
  history row only when all three match: the row's `executed_on.machine_id`
  equals the settlement's `machine_id`, the row's `run_id` equals the
  settlement's `leaf_run_id`, and the row's `task_ids` lists the settlement's
  `task_id`. Run ids come from each store's own sequence, so a same-id leaf
  seen on another host is a different run and is counted; a row with null
  `executed_on` never dedupes [ORB-19455]. Parents
  that fail `pipeline_success_guard` fold into their child by `orbit run
  show`'s `root_cause`, and a parent of a `held` child is no failure. Review
  refusals are clustered by escalation category (`base_red`,
  `external_evidence`, `report_revision`, `scope_or_criteria`, `timeout`,
  `other`). For each qualifying job it accounts for the Pareto set — the top
  classes covering at least 80% of folded failures plus every class with at
  least 3 occurrences across 2 runs in 7 days — giving every Pareto class a
  category (product defect, misclassified hold, provider/capacity, upstream
  task quality, environment/flake, true one-off) and a disposition; no class
  is dismissed by kind alone. A done covering fix is checked for deployment on
  each failing host before later occurrences count as regressions, and fixes
  the scan itself filed get an effectiveness verdict (effective, not
  effective, not yet deployed, too early). Open coverage is pushed forward
  with a comment rather than a duplicate; at most 5 new redacted `proposed`
  fix tasks are filed per pass, tagged `run-failure-patterns` and
  `no-auto-approve` with a `spawned_from` relation; a friction replaces the
  task only when no cause can be established. The scan runs on the owner:
  preparation records a `required_machine` owner-evidence contract before
  admission, and a follower that still starts it stops with `wrong_host`
  without filing or attaching a cursor. The scan never mutates run state —
  its run and settlement reads pass `--no-reconcile`, so an orphaned run it
  lists is not finalized — and a pass with nothing new is a successful no-op
  that still stores the report and cursor.
- `pilot-block-patterns` — disabled-by-default daily upstream counterpart of
  `run-failure-patterns`. A first pass, with no prior cursor, uses the 3 days
  before scan start; a later pass uses the 14 days before scan start, unless the
  prior cursor has `resume_after`, in which case the pass continues that window.
  Candidates come from `escalation_triage_pipeline` and
  `blocked_task_recovery_pipeline` run history plus current proposed and blocked
  tasks, and a pass reads at most 60 tasks. A capped pass still completes, with
  `pilot-block-report.json` marked `partial` and `pilot-block-cursor.json`
  carrying `resume_after`. It reads the dispositions `escalation_triage_pipeline`
  recorded (`pilot_warning_triaged` and, on older history, `blocked_task_triaged` markers
  and comments, plus final-recovery decision cards, grouped first by their
  recurring human actions), then untriaged
  pilot holds and final-recovery escalations from task history, `pilot_warnings`
  and run records. Each cause that recurs (at least 3 occurrences across 2
  tasks) is routed to its source: the filer, the task-pilot activity, the
  final-recovery activity, `run-failure-patterns` (where it only ensures a fix
  is open) or host setup. It dedupes against open and 30-day-closed work, files
  at most 5 `proposed` fixes per pass, compares groups with the prior pass's
  `pilot-block-report.json`, and files a regression when a done fix did not
  reduce its group. It never acts on an individual blocked or held task, and
  runs only on the owner (ORB-20516).
- `delivery-code-review` — disabled-by-default review of newly landed
  deliveries. Its `enabled` flag is the after-landing review switch
  [ORB-13992]; the deprecated `operation.review_policy = after-landing`
  enables a copy no operator has configured, which `list` and `show` report
  as `effective_enabled`. The dashboard groups and toggles by that effective
  value and keeps the raw flag as the compare-and-set token. The former
  `delivery-qa` default is retired and is not seeded;
  hands-on QA stays with `qa-sweep` (recent changes) and `qa-full-sweep`
  (the whole product at one commit).
- `architecture-audit` — disabled-by-default audit every 14 days of the
  package and module dependency graph, built from the ecosystem's own
  metadata tool or an import scan (the method is named in the run summary).
  It records cycles — including up-calls hidden in behavior attached to a
  shared type from another module — distributed types, public-surface
  width, concept placement, sizes and rebuild reach in an
  `architecture.json` artifact carried between passes, and reads one scored
  area in full. Layering is judged against the repository's documented
  architecture when one exists; without one, the run reports the layers it
  inferred and files only true cycles and up-calls, never layering-drift
  tasks. A split, merge, or move candidate whose case rests on unmeasured
  numbers is filed first as a `no-diff-expected` measurement task with a
  go/no-go, and only then as a restructuring task depending on it. Each run
  files at most 3 `proposed` improvement tasks plus measurement
  prerequisites, each with `context_files` and a `spawned_from` relation to
  the minted chore, which is itself tagged `no-diff-expected` and never
  builds or runs tests.
- `escape-to-guard` — disabled-by-default weekly pass that turns recurring
  regression classes into prevention. It classifies `unclassified` and
  absent escapes from the regression task and the culprit's landed diff,
  recording each classification through the supported `orbit.task.update`
  relations write (or, when that write cannot change an existing link, in
  its artifact plus one deduplicated gap task), clusters regressions by
  nature × escape × layer plus a one-line mechanism, and files at most
  3 `proposed` remedy tasks for clusters of 3 or more regressions in
  30 days. Each task names the highest-reliability expressive rung and why
  higher rungs cannot cover it: mechanical guard in an existing gate,
  in-line hazard comment, nested `AGENTS.md`/`CLAUDE.md` pair, tag instruction,
  then global instruction. Review-instruction remedies remain available for
  `review_missed`. Rungs 3–5 and review instructions remove or merge an
  existing rule with no word-count growth per file, or leave the choice to
  the operator; comments are exempt. Tag remedies specify tag assignment;
  host-config mapping is an operator step outside task acceptance criteria.
  The run never edits guidance or proposes new required status checks,
  merge gates or human-only steps. Durable rung tags and structured cluster
  keys/counts at filing identify remedies, including legacy review
  instructions. Follow-up waits 30 days from verified delivery landing:
  `observing` before then, `effective` for zero recurrences in that first
  month, `partial` for fewer than at filing (kept), and `ineffective` for
  the same or more. Ineffective mechanical guards compete for strengthening;
  ineffective instructions get deduplicated retirement proposals under a
  separate cap of 3, with a higher-reliability replacement where possible.
  Each run attaches schema-version-2 `escape-to-guard.json` with cluster
  rungs and higher-rung rejections, classification counts, landing dates,
  filing/post-landing counts, verdicts and retirement IDs, diffed against
  the previous run (including version 1). A workspace with no regression
  links is a successful no-op.
- `code-health-sweep` — disabled-by-default sweep every 3 days of dead code,
  compatibility paths, and one churn-selected area read in full for
  duplication, simplification and architectural drift. Repository shape
  (languages, source roots, gate commands, public surface) is discovered from
  tracked files. A dead-code candidate is kept only after the macro,
  string-registry, serialization, FFI and reflection routes are checked, and a
  public command-line, API, configuration or persisted-format surface is never
  filed for deletion by the dead-code pass. Compatibility paths are always
  inventoried, but retirement tasks are filed only when the repository
  documents a support window; without one the inventory goes in the run
  summary and nothing is filed for retirement. Lint-suppression counting
  belongs to a workspace's own `suppression-ratchet` definition, when it has
  one. Each run files at most 6
  `proposed` tasks with `context_files` and a `spawned_from` relation to the
  minted chore, which is tagged `no-diff-expected` and never builds or runs
  tests.
- `dependency-freshness` — disabled-by-default monthly inventory of direct
  dependencies across every detected ecosystem (for example
  `Cargo.toml`/`Cargo.lock`, `package.json` with a lockfile,
  `pyproject.toml`/`uv.lock`, `go.mod`), using each ecosystem's own
  outdated, unused, and duplicate tooling and recording unavailable tooling
  as a gap. Dependencies covered by a security advisory or an open advisory
  task are excluded (that work belongs to the security sweeps). Each run
  attaches `dependency-freshness.json` diffed against the previous run, and
  files at most 3 `proposed` upgrade or removal tasks ranked by how far
  behind multiplied by import or call-site usage, each quoting the breaking
  changes that apply from upstream release notes. The minted chore is
  tagged `no-diff-expected`, never bumps the repository's own version,
  tags, or publishes, and with no network or registry access finishes as a
  successful no-op stating why.
- `path-risk-review` — disabled-by-default review every 3 days of the paths
  the codebase analytics rank as regressing, missing from their declared
  footprint, blocking, rejected, or contending for locks. It reads the 30-day
  and 90-day rankings, follows the tasks behind each rate, investigates at most
  five paths, and names one dominant cause per path with task and code
  evidence, carrying the record between runs in a `path-risk.json` artifact.
  Each run files at most 3 `proposed` fixes, tagged `path-risk-review` and
  `no-auto-approve`, with `context_files` and a `spawned_from` relation to the
  minted chore. A fix changes the mechanism behind its path class (a footprint
  or lock rule, a generator check, or a split of a coupling hotspot), never one
  regression. A workspace whose rankings all sit below `min_support` is a
  successful no-op that reports each ranking's below-support counts and files
  nothing.
  The minted chore is tagged `no-diff-expected` and never builds or runs tests.
- `agent-guidance-audit` — disabled-by-default audit every 3 days of agent
  guidance files: `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.claude/skills/`,
  `.agents/skills/`, `copilot-instructions.md` under `.github`, and Cursor rule
  files. A generated skills mirror is reviewed once, at its source. Each run
  pins one integration-branch commit, audits a rolling batch of at most 8
  files and about 2,000 lines ordered least-recently audited first, and
  attaches `agent-guidance-audit.json` (pinned SHA, paths audited, and a
  cumulative `state` map) for the next run to read. It files at most 3
  `proposed` bugs, each naming the file, line, claim, and the `--help`,
  tracked-path, or tool-registry evidence that the claim is stale or
  contradictory, with `context_files` and a `spawned_from` relation to the
  minted chore. Stylistic rewrites are not filed. The chore is tagged
  `no-diff-expected` and does not edit the repository. A workspace-authored
  `skill-validation` definition is left in place.

- `auto-task-yield-review` — disabled-by-default monthly review of whether
  this workspace's enabled auto-task definitions pay for themselves. For
  each enabled definition over the last 30 days it reports minted chores
  by final status and no-op successes, cost per run from run evidence
  (reported as `unknown`, never zero, when missing), the tasks those
  chores filed through `spawned_from` and their outcomes, filed tasks that
  later regressed through `regression_from`, and duplicate or invalid
  rejections, deriving landed tasks per run and cost per landed task. Each
  run attaches `auto-task-yield.json` diffed against the previous run,
  carries the same table in its execution summary, and files at most 3
  `proposed` retuning tasks (a cadence change, a merge of overlapping
  definitions, a template fix, or disabling the definition), each naming
  its definition and numbers with `context_files` and a `spawned_from`
  relation to the minted chore. Filed tasks with no `spawned_from`
  linkage count as `unknown`, and that gap is filed at most once after a
  duplicate search. The minted chore is tagged `no-diff-expected` and
  never edits, toggles or mints definitions; changing configuration is
  the operator's decision.

The former `blocked-task-triage` default (an hourly reader of escalated blocks)
is retired and is not seeded. Final recovery decides blocked tasks now: it runs
bounded mechanical steps, then escalates or opens a decision card
([automation triggers §6.2](../automation-triggers/2_design.md#62-retired-escalated-blocks-block_escalated),
[distributed drain §7.2](../distributed-drain/2_design.md#72-failed-run-triage)).
Its last template stays as retired provenance (`RETIRED_AUTO_TASK_FILES`), so
`orbit workspace sync` retires a seeded copy, settings-only forks included,
preserves an operator-edited one, and, when the copy was enabled, reports that
the routine replaces it. Sync never enables the routine. The
`skip_unless_blocked_candidates` precondition stays available to
workspace-authored definitions.

## Promotion to the shipped defaults

Every shipped default reaches every Orbit user, so the catalog holds only
definitions that have proven their yield. A new recurring chore starts as a
workspace-local definition (`orbit auto-task add`). It may be proposed for the
shipped catalog only after it has shown yield in at least one workspace: tasks
it filed that landed, or a diff of its own that merged, with a low share of
duplicate or invalid rejections. `auto-task-yield-review` is the measure; its
`auto-task-yield.json` reports those counts per definition over 30 days. The
change that adds a default cites that evidence: the workspace, the window, the
runs, the landed tasks or merged diffs, and the rejection share. A default
that stops paying for itself is demoted the same way.

In Orbit's own workspace, the workspace-authored `default-auto-task-audit`
definition checks each newly shipped default against this rule and files a
demotion proposal when the evidence is missing.

### Demoted defaults

A demoted default had not proven its yield and nothing replaced it.
ORB-20704 demoted `backlog-hygiene`, `ci-budget-watch`, `crew-ladder-review`,
`design-sync`, `flaky-test-hunt`, `structure-sweep`, `suppression-ratchet`
and `workflow-health-audit`. Their names are listed in
`orbit_config::DEMOTED_DEFAULT_AUTO_TASKS`, which `seeding.auto_tasks`
admission still accepts, and their templates are no longer embedded.

Unlike a retired default, a demoted default's existing copy is kept. `orbit
workspace sync` drops its managed-manifest provenance before reconciliation
and reports one `migrated` action, leaving the file, its settings-table entry,
its schedule and its cursor untouched. From then on the copy is a
workspace-local definition: sync no longer refreshes it, `orbit auto-task show`
reports its body as user-authored, and doctor does not report it as
deprecated. The demoted bodies stay in `body-history.json`.

## Workspace-authored definitions in this repo

Orbit's own checkout also carries extra `.orbit/auto_tasks/` files that are
**not** embedded defaults. They may be enabled, name a family-specific crew, or
encode this repository's branches and gates. Re-init preserves them:

- `model-price-audit`, `release-prep`, `skill-validation`,
  `default-auto-task-audit` (which audits the shipped defaults), and
  this repository's configured copies of catalog names such as `code-review` and
  `security-review`.
- `qa-full-sweep`, this repository's Orbit-specific pre-release sign-off
  ([runbook](../../runbooks/qa-full-sweep.md)). It predates the generic
  default of the same name and stays this workspace's active definition:
  workspace sync and `orbit update` preserve the file byte for byte and never
  retire it, and sync reports that it occupies the bundled default's name.

## Task References

- ORB-20704 — Demoted eight unproven defaults to workspace-local, keeping
  existing copies, and made proven yield the condition for shipping a default.

- [ORB-18557] — Adds the portable design-sync default and its reconciliation ledger.

- ORB-10149 — Auto-task primitive.
- ORB-10148 — qa-sweep V1 (first definition; depends on this).
- ORB-10039 — the bespoke inline qa-sweep check sweep that auto-tasks generalize.
- ORB-10439 — `orbit auto-task mint <name>`, the on-demand manual mint.
- ORB-13432 — serializes manual mint with non-force deletion.
- ORB-10798 — narrowed the auto-task MCP surface to list and mint.
- ORB-10440 — Daily friction-curation definition.
- ORB-10514 — Original workspace-authored CI-failure auto-task.
- ORB-10549 — Embedded the portable, disabled friction-curation default and
  workspace materialization contract; [Auto-task primitive: file-backed recurring task templates + one generic scheduler routine](./4_decisions.md#auto-task-primitive-file-backed-recurring-task-templates--one-generic-scheduler-routine) should be updated through the
  Orbit ADR surface after this task lands.
- ORB-10550 — Added the disabled qa-sweep default and standardized agent-facing
  friction tool invocations on the registered `orbit tool run` surface.
- ORB-10950 — Added the disabled weekly `security-review` default to the
  embedded catalog so new workspaces can opt into a recurring security review.
- ORB-11054 — Split this overview so repo-local workspace-authored definitions
  are no longer listed as if they were embedded defaults.
- ORB-11095 — Added centralized finding-title provenance and established the
  shipped definition's canonical `code-review` name.
- ORB-12237 — moved auto-task evaluation into the host clock tick and retired
  the scheduler routine, job and activity.
- ORB-14909 — keeps settings-only enable or crew changes to a managed bundled
  definition in the settings table without forking its body; body edits and
  edits to existing forks or user-authored definitions still write a file.
- ORB-12931 — Added the disabled weekly `run-failure-patterns` default that
  mines unfiled recurring run failures.
- ORB-14439 — `run-failure-patterns` files proposed fix tasks, reads follower
  leaf settlements, folds cascades by `root_cause`, splits review refusals by
  escalation category, and files blocking one-offs.
- ORB-18556 — Made `run-failure-patterns` rate-driven: a rolling 7-day window
  with 30-day trend and per-job raw/folded rates, Pareto classes with a
  category and disposition and no dismissal by kind, deployment-aware
  regression checks, fix-effectiveness verdicts, and owner placement through
  the `required_machine` contract with a `wrong_host` stop.
- ORB-12932 — Added the disabled weekly, report-only `backlog-hygiene` default.
- ORB-13636 — Added the disabled `full-code-review` coordinator default and
  keyed the `code-review` cursor selector on its provenance tag after
  hand-filed full-review chores shadowed the last real sweep.
- ORB-15327 — Added the disabled, product-agnostic `qa-full-sweep`
  coordinator default with per-area surface-QA chores and a roll-up release
  decision.
- ORB-18069 — The generic roll-up carries an owner-evidence contract so
  preparation holds it on the owner machine, and the coordinator wires
  dependencies with `orbit.task.update` or a blocker handoff rather than a
  comment.
- ORB-18126 — Added the disabled, portable `structure-sweep` default
  (weekly): one pass over source and test files at the thresholds of 1,000
  lines for a non-test source file, 400 lines of mostly logic for a module
  index file and 2,000 lines for a test file, with layout candidates keyed
  to the detected language's convention, rejected leads recorded with a
  reason, and at most 4 `proposed` restructuring or test-reduction tasks
  per run, largest first and grouped by package or subsystem. A
  test-reduction task keeps that file's coverage and reports the test
  count before and after.
- ORB-18124 — Added the disabled, portable `code-health-sweep` default
  (3-day cadence): language-neutral dead-code evidence rules, compatibility
  retirement gated on a documented support window, and a churn-selected area
  read in full, capped at 6 proposed tasks per run.
- ORB-18125 — Added the disabled, portable `architecture-audit` default
  (14-day cadence): graph discovery from the ecosystem's metadata tool or an
  import scan, documented-architecture layering checks with an
  inferred-layers fallback that files only cycles and up-calls, and
  measurement-first split, merge, and move filing capped at 3 improvement
  tasks per run.
- ORB-18135 — Added the disabled monthly `auto-task-yield-review` default:
  per-definition runs, cost, filed-task outcomes, regressions and
  duplicate or invalid rejections over 30 days in an
  `auto-task-yield.json` artifact diffed against the previous run, with at
  most 3 proposed retuning tasks per run and no definition edits.
- ORB-18127 — Added the disabled, portable `ci-budget-watch` default
  (daily cadence): job discovery from the CI workflow files, budget
  discovery from the repository's documented CI budget, a
  `ci-budget-watch/report.md` artifact with the last 20 runs and the
  trend, no-op paths for no hosted CI, no documented budget, and
  unreachable history, and the one-open-`ci-budget`-task rule.
- ORB-18134 — Added the disabled, portable `flaky-test-hunt` default
  (weekly cadence): same-SHA rerun and adjacent-commit flake signals
  over a 14-day hosted-CI window, unparseable logs recorded as gaps, a
  `flaky-test-hunt.json` artifact diffed against the previous run, and
  at most 4 proposed `ci-flake` tasks per run that reject silent
  retries or quarantine without an exit plan.
- ORB-18129 — Added the disabled, portable `agent-guidance-audit` default
  (3-day cadence): a least-recently-audited batch of agent guidance files,
  checked at one pinned commit, with at most 3 proposed findings per run.
- ORB-18128 — Added the disabled `path-risk-review` default (3-day cadence)
  over the codebase analytics rankings: min-support no-op reporting each
  ranking's below-support counts, at most five paths investigated, and fixes capped at 3
  proposed tasks per run that change a footprint or lock rule, a generator
  check, or a coupling hotspot.
- ORB-17245 — Added the disabled hourly `blocked-task-triage` default and the
  `skip_unless_blocked_candidates` precondition.
- ORB-17744 — Retired the `blocked-task-triage` default in favour of the
  `blocked_task_triage` routine on the escalation triage pipeline.
- ORB-20629 — Retired that routine: final recovery is the single decider for
  blocked tasks and opens the decision card.
- ORB-11115 / ORB-11383 — Retired the shipped CI-failure auto-task;
  runner workflows only emit fail-open run/job/commit provenance, while the
  host-owned `ci_failure_sweep` routine performs durable CI-failure filing.
- ORB-13992 — preserves deprecated, warned `operation.review_policy` as a
  legacy input: `after-landing` enables an otherwise-unconfigured
  `delivery-code-review` definition until its flag is explicitly configured.
- ORB-18948 — corrected the `full-code-review` partition rules after a run
  diverged from what the docs stated.
- ORB-19455 — corrected the follower-settlement dedupe rule to the three-part
  machine, run and task match.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
