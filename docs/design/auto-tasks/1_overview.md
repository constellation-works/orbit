---
title: Auto-tasks — Overview
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Accepted
feature: auto-tasks
doc_role: overview
type: design
summary: Dynamically-defined recurring task templates minted by the host clock tick — periodic work as data, not code.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**"]
related_features: [auto-tasks, routines]
related_artifacts: [ORB-10149, ORB-10318, ORB-10348, ORB-10439, ORB-10446, ORB-10514, ORB-10549, ORB-10950, ORB-11054, ORB-11095, ORB-12931, ORB-12932, ORB-15327, ORB-18069, ORB-18124, ORB-18125, ORB-18126, ORB-18127, ORB-18128, ORB-18129, ORB-18134]
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
inline check sweep, ship-sweep had its own systemd unit, and each future
recurring chore would mean another hardcoded routine. The marginal cost of a new
chore was a code change, a review, and a release. Auto-tasks replace the pattern
with a primitive so the marginal cost is a YAML record. qa-sweep V1 (ORB-10148)
becomes just the first definition.

## 2. Core Concepts

- **Definition** — a `.orbit/auto_tasks/<name>.yaml` record; `name` is identity.
  Modeled on the file-backed routine convention (directory scan, fail-closed
  parse, `deny_unknown_fields`), not the SQLite-indexed artifact convention.
- **Schedule (cadence)** — a 5-field `cron`, an `every_minutes` interval, or a
  `deliveries_landed` trigger in the definition's `schedule` field. Time-based
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
  cannot be projected reads `unavailable` on its own row only.
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

These twenty-two YAML files live under `crates/orbit-core/assets/auto_tasks/` and are
registered in `DEFAULT_AUTO_TASK_FILES`. `orbit workspace init` materializes a
missing file as `enabled: false`; re-init does not overwrite a workspace-authored
definition of the same name.

- `qa-sweep` — hands-on validation of recent changes (ORB-10148, embedded by ORB-10550).
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
  commit, measures tracked text at that SHA, and splits directories or file
  sets into areas of at most 25,000 lines. Its `full-review-areas.json` artifact
  records the inventories, counts and exclusions. It files one area-review
  chore per bounded area tagged
  `full-code-review` + `no-diff-expected` — never `code-review` — at `hard`
  complexity or below with no pinned crew. Area reviewers read every partition
  of at most 10,000 lines in full, recording file/range coverage and findings
  in `read-progress.json`; resumed and re-crewed runs continue from that
  artifact. Parallel reading is used when the crew and execution rules permit
  it. Incomplete coverage reports `review_incomplete` with uncovered partitions
  in the artifact and task comment. Findings are `bug`s tagged `code-review` +
  `full-code-review`, with source-file and matching test context selectors.
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
  solely for this task. Its template reserves `dir:.` for root and
  workspace-specific documentation and does not carry `no-diff-expected`, since
  verified drift corrections produce diffs. Clean batches use validated no-diff
  evidence.
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
- `run-failure-patterns` — disabled-by-default weekly scan of the workspace's
  own run evidence (failed and interrupted runs, step failures, worker logs)
  since the previous scan's `run-failure-cursor.json` artifact. On an owner it
  also reads the failure and release settlements followers' claimed leaves
  sent, through `orbit run settlements --no-reconcile`, since those leaves
  never enter the owner's run history; a pattern seen on two hosts is one
  pattern. Parents that fail `pipeline_success_guard` fold into their child by
  `orbit run show`'s `root_cause`, and a parent of a `held` child is no
  failure. Review refusals are clustered by escalation category (`base_red`,
  `external_evidence`, `report_revision`, `scope_or_criteria`, `timeout`,
  `other`). A cluster is a pattern at 3 occurrences across 2 runs, or as a
  single occurrence when a deterministic Orbit action or tool limit blocked a
  task and would recur. Unless an existing task or friction already tracks
  it, each pattern's mechanism is verified in code and filed as one redacted
  `proposed` fix task tagged `run-failure-patterns` and `no-auto-approve`; a
  friction replaces the task only when no cause can be established. The scan
  never mutates run state — its run and settlement reads pass
  `--no-reconcile`, so an orphaned run it lists is not finalized — and a
  window with no new pattern is a successful no-op.
- `backlog-hygiene` — disabled-by-default weekly, report-only scan of blocked,
  orphaned in-progress/review, aged proposed, and dependency-unblocked idle
  tasks. Its execution summary recommends human follow-up without changing task
  status or dispatching work.
- `delivery-code-review` — disabled-by-default review of newly landed
  deliveries. Its `enabled` flag is the after-landing review switch
  [ORB-13992]; the deprecated `operation.review_policy = after-landing`
  enables a copy no operator has configured, which `list` and `show` report
  as `effective_enabled`. The dashboard groups and toggles by that effective
  value and keeps the raw flag as the compare-and-set token. The former `delivery-qa` default is retired and is not seeded;
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
  3 `proposed` guard tasks for clusters of 3 or more regressions in
  30 days. A guard is a mechanical check inside the gates that already
  exist — a lint, a type, a static guard script, a generator check, a
  skill or review-instruction line for `review_missed`, or a regression
  test for `no_test` — never a new required status check or merge gate.
  Each run attaches `escape-to-guard.json` (clusters, classification
  counts before and after, and whether each earlier landed guard's class
  recurred after its landing date) and diffs it against the previous
  run; a workspace with no regression links is a successful no-op.
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
  belongs to the `suppression-ratchet` default. Each run files at most 6
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
- `ci-budget-watch` — disabled-by-default daily measurement of the main
  hosted test job's elapsed time against the repository's documented CI
  budget. It discovers the job from the CI workflow files (a gate job that
  waits on the test jobs, otherwise the job whose steps run the test
  suite) and the budget (figure, statistic, and population, with its
  `file:line` source) from the agent guides, contributing guide, and docs;
  with no documented budget it records the trend and files nothing, and
  with no hosted CI or unreachable run history it is a successful no-op
  that states the reason. Each run attaches `ci-budget-watch/report.md`
  with the last 20 completed runs (SHA, run id, conclusion, elapsed
  seconds), the trend against the previous report, and the measurement
  gaps. When over budget it keeps exactly one open `ci-budget` task:
  an existing open one is updated with the new measurements, otherwise
  one `proposed` repair task is filed with acceptance criteria,
  `context_files`, and a `spawned_from` relation. The minted chore is
  tagged `no-diff-expected` and never edits source, CI, or tests.
- `flaky-test-hunt` — disabled-by-default weekly hunt for flaky tests in
  hosted CI history over a 14-day window, on the integration branch and
  recent pull-request runs. It finds flakes through 2 signals: a failed
  run followed by a succeeded rerun of the same SHA in the same
  workflow, and a failed run on one integration-branch commit followed
  by a succeeded run on its immediate child when the diff between the
  2 SHAs changed no test file, same-package file, excerpt-named file,
  or suite-composition file. Test names and bounded failure excerpts
  come from the failed runs' logs; runs whose logs cannot be parsed
  are recorded as gaps and contribute nothing. Each run attaches
  `flaky-test-hunt.json` with the flake count per test, every
  occurrence's run ids and SHAs, excluded candidates, gaps, and a diff
  against the previous run, and files at most 4 `proposed` fix tasks,
  highest flake count first, tagged `ci-flake` with the verified test
  file as `context_files` and a `spawned_from` relation, deduplicated
  against open `ci-flake` and CI-failure-sweep tasks. A filed task
  requires the root cause fixed or the test deleted as redundant, and
  rejects silent retries or quarantine without an exit plan. With no
  hosted CI or unreachable run history it is a successful no-op that
  states the reason. The minted chore is tagged `no-diff-expected`
  and never builds or runs tests.
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
- `structure-sweep` — disabled-by-default weekly scan in one pass over
  source files and test files. It records the thresholds in every run
  summary (a non-test source file over 1,000 lines, a module index file
  over 400 lines of mostly logic, and a test file over 2,000 lines),
  discovers languages and source and test roots from tracked files, and
  keys layout candidates to the detected language's convention, recording
  rejected leads with a reason. Each run files at most 4 `proposed`
  restructuring or test-reduction tasks, largest first and grouped by
  package or subsystem, each with `context_files` and a `spawned_from`
  relation to the minted chore, which is itself tagged `no-diff-expected`
  and never builds or runs tests. A test-reduction task keeps that file's
  coverage — every scenario survives or is named with a reason — and
  reports the test count before and after.
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

- `suppression-ratchet` — disabled-by-default weekly (`hard`) static count of
  lint suppressions, TODO/FIXME/HACK/XXX, ignored tests and language escape
  hatches at one integration-branch SHA. Patterns follow detected languages;
  package/area counts exclude vendored, generated and build output and split
  justified from unjustified occurrences. `suppression-ratchet.json` carries
  the baseline and comparison evidence; the first run files nothing. Later
  runs file ratchet bugs naming introducing commits plus at most one small
  reduction in the highest-churn eligible area, capped at three proposed tasks
  with verified context and `spawned_from`. Categories are grouped when needed
  to keep every increase covered within the cap. Justified occurrences are
  never filed. This default owns suppression counting for the portable
  `code-health-sweep`; the minted chore is `no-diff-expected` and never builds.

The former `blocked-task-triage` default (an hourly reader of escalated blocks)
is retired and is not seeded. The seeded `blocked_task_triage` routine replaces
it: its `block_escalated` state trigger admits each escalated block episode once
into the hard-tier `escalation_triage_pipeline`
([automation triggers §6.2](../automation-triggers/2_design.md#62-escalated-blocks-block_escalated),
[distributed drain §7.2](../distributed-drain/2_design.md#72-failed-run-triage)).
Its last template stays as retired provenance (`RETIRED_AUTO_TASK_FILES`), so
`orbit workspace sync` retires a seeded copy, settings-only forks included,
preserves an operator-edited one, and, when the copy was enabled, reports that
the routine replaces it. Sync never enables the routine. The
`skip_unless_blocked_candidates` precondition stays available to
workspace-authored definitions.

## Workspace-authored definitions in this repo

Orbit's own checkout also carries extra `.orbit/auto_tasks/` files that are
**not** embedded defaults. They may be enabled, name a family-specific crew, or
encode this repository's branches and gates. Re-init preserves them:

- `model-price-audit`, `release-prep`, `skill-validation`, and
  this repository's configured copies of catalog names such as `code-review` and
  `security-review`.
- `qa-full-sweep`, this repository's Orbit-specific pre-release sign-off
  ([runbook](../../runbooks/qa-full-sweep.md)). It predates the generic
  default of the same name and stays this workspace's active definition:
  workspace sync and `orbit update` preserve the file byte for byte and never
  retire it, and sync reports that it occupies the bundled default's name.

## Task References

- ORB-10149 — Auto-task primitive.
- ORB-10148 — qa-sweep V1 (first definition; depends on this).
- ORB-10439 — `orbit auto-task mint <name>`, the on-demand manual mint.
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
- ORB-12931 — Added the disabled weekly `run-failure-patterns` default that
  mines unfiled recurring run failures.
- ORB-14439 — `run-failure-patterns` files proposed fix tasks, reads follower
  leaf settlements, folds cascades by `root_cause`, splits review refusals by
  escalation category, and files blocking one-offs.
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
- ORB-11115 / ORB-11383 — Retired the shipped CI-failure auto-task;
  runner workflows only emit fail-open run/job/commit provenance, while the
  host-owned `ci_failure_sweep` routine performs durable CI-failure filing.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
