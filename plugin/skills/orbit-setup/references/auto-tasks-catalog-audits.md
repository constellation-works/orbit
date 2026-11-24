# Auto-tasks: seeded definitions, audits and promotion

## Contents

- [Seeded definitions: audits and health](#seeded-definitions-audits-and-health)
- [Shipped defaults are promoted on proven yield](#shipped-defaults-are-promoted-on-proven-yield)

## Seeded definitions: audits and health

- **`architecture-audit`** (`hard`) — every 14 days.

  Audits the package and
  module dependency graph, built from the ecosystem's own metadata tool or
  an import scan (the method used is named in the run summary): cycles,
  including up-calls hidden in behavior attached to a shared type from
  another module; distributed types; public-surface width; concept
  placement; and sizes with rebuild reach, all recorded in an
  `architecture.json` artifact carried between passes, plus one scored area
  read in full.

  Layering is judged against the repository's documented
  architecture when one exists; without one, the run reports the layers it
  inferred and files only true cycles and up-calls, never layering-drift
  tasks. A split, merge, or move candidate whose case rests on unmeasured
  numbers is filed first as a `no-diff-expected` measurement task with a
  go/no-go, and only then as a restructuring task depending on it.

  Each run
  files at most 3 `proposed` improvement tasks plus measurement
  prerequisites, each with `context_files` and a `spawned_from` relation to
  the minted chore. The chore itself is tagged `no-diff-expected` and never
  builds or runs tests.
- **`escape-to-guard`** (`medium`) — weekly.

  Classifies `unclassified` and
  absent regression escapes from the regression task and the culprit's
  landed diff, records each classification through the supported
  `orbit.task.update` relations write (or in its artifact plus one
  deduplicated gap task when that write cannot change an existing link),
  clusters regressions by nature × escape × layer plus a one-line
  mechanism, and files at most 3 `proposed` remedy tasks for clusters of
  3 or more regressions in 30 days.

  Remedies choose the highest-reliability
  expressive rung: mechanical guard in an existing gate, in-line hazard
  comment, nested `AGENTS.md`/`CLAUDE.md` pair, tag instruction, then global
  instruction, explaining why each higher rung cannot work. Review-instruction
  remedies remain available for `review_missed`. Rungs 3–5 and review
  instructions remove or merge an existing rule with no word-count growth
  per edited file, or leave the choice to the operator; comments are exempt.

  Tag remedies explain tag assignment; host-config mapping stays an operator
  step outside task acceptance criteria. No new required status checks, merge
  gates or human-only steps are proposed, and the run never edits guidance.

  Rung tags and structured cluster keys plus filing counts support follow-up
  of every landed remedy: `observing` until 30 days after verified delivery,
  then `effective` for zero recurrences, `partial` for fewer than at filing
  (kept), or `ineffective` for the same or more in the first 30 days.

  Ineffective guards compete for strengthening under the normal cap;
  ineffective instructions get deduplicated retirement proposals, at most 3
  separately per run, with a higher-reliability replacement where possible.
  Each run attaches schema-version-2 `escape-to-guard.json` with cluster
  rungs and higher-rung rejections, classification counts, landing dates,
  filing/post-landing counts, verdicts and retirement IDs; version-1 artifacts
  remain comparable.

  Legacy review-instruction remedies are included. A
  workspace with no regression links is a successful no-op.
- **`code-health-sweep`** (`hard`) — every 3 days. Discovers the repository's
  languages, source roots, gate commands and public surface from tracked
  files, then scans for dead code, inventories compatibility paths, and reads
  one churn-selected area in full for duplication, simplification and
  architectural drift.

  A dead-code candidate is kept only after the macro,
  string-registry, serialization, FFI and reflection routes are checked, and a
  public command-line, API, configuration or persisted-format surface is never
  filed for deletion by that pass. Compatibility retirement tasks are filed
  only when the repository documents a support window; otherwise the
  inventory is recorded in the run summary and nothing is filed for
  retirement.

  It does not count lint suppressions; a workspace's own
  `suppression-ratchet` definition owns that, when it has one. Each run records a `code-health.json` artifact carried
  between passes and files at most 6 `proposed` tasks, each with
  `context_files` and a `spawned_from` relation to the minted chore. The
  chore itself is tagged `no-diff-expected` and never builds or runs tests.
- **`dependency-freshness`** (`medium`) — monthly. Inventories direct
  dependencies in every detected ecosystem (for example
  `Cargo.toml`/`Cargo.lock`, `package.json` with a lockfile,
  `pyproject.toml`/`uv.lock`, `go.mod`) with each ecosystem's own outdated,
  unused, and duplicate tooling, recording unavailable tooling as a gap.
  Dependencies covered by a security advisory or an open advisory task are
  excluded; that work belongs to the security sweeps.

  Each run attaches
  `dependency-freshness.json` diffed against the previous run and files at
  most 3 `proposed` upgrade or removal tasks ranked by how far behind
  multiplied by import or call-site usage, each quoting the breaking
  changes that apply from upstream release notes. It never bumps the
  repository's own version, tags, or publishes, and with no network or
  registry access it finishes as a successful no-op stating why.
- **`path-risk-review`** (`medium`) — every 3 days. Reads the codebase
  analytics for paths that keep regressing, land outside their declared
  footprint, block, get rejected, or contend for locks, and follows the tasks
  behind the riskiest. It files at most three `proposed` fixes per run, each
  tagged `path-risk-review` and `no-auto-approve`, and each changes a
  mechanism (a footprint or lock rule, a generator check, or a split of a
  coupling hotspot) rather than patching one regression.

  A run where every
  ranking sits below `min_support` is a successful no-op that reports each
  ranking's below-support counts and files nothing.
- **`auto-task-yield-review`** (`medium`) — monthly. Reports whether each
  enabled auto-task definition in this workspace pays for itself over the
  last 30 days: minted chores by final status and no-op successes, cost
  per run from run evidence (unknown, never zero, when missing), tasks
  filed through `spawned_from` and their outcomes, filed tasks that later
  regressed, duplicate or invalid rejections, landed tasks per run and
  cost per landed task.

  Each run attaches `auto-task-yield.json` diffed
  against the previous run and files at most 3 `proposed` retuning tasks
  (cadence, merge, template fix or disable), each naming its definition
  and numbers. It never edits, toggles or mints definitions.

- **`regression-label-audit`** (`medium`) — weekly. Re-labels 10
  `regression_from` links from the last 7 days that another model family
  filed, blind to the filer's labels and to any model, provider or crew
  name of the filer or of the culprit's implementer or reviewer.

  The
  filer's labels travel in the `relations` field of the task-list
  response, so the judging model never reads that response: the brief
  carries a small Python helper that lists, filters and draws outside the
  model's context and prints only each link's filing and culprit task ids
  and the evidence packet (title, description, criteria and context files).

  It uses `created_by` and `created_by_model` only to skip the auditor's
  own family, and never prints them. The helper reads the recorded labels only after the model
  has saved its second labels, then scores agreement. Sample size and
  period are definition fields in the brief.

  Each run attaches
  `regression-label-audit.json` with the second escape, nature and
  severity and per-field agreement, and the execution summary reports the
  sample's disagreement rate for each field. It never edits a regression
  link, a scorecard or a rank. The run needs a shell with `python3` and
  `orbit` on its path.

- **`agent-guidance-audit`** (`medium`) — every 3 days. Audits agent guidance
  (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `.claude/skills/`, `.agents/skills/`,
  `copilot-instructions.md` under `.github`, and Cursor rule files) at one pinned
  commit, least-recently audited first, at most 8 files and about 2,000 lines.
  A generated skills mirror is reviewed only at its source.

  Each run attaches
  `agent-guidance-audit.json` for the next run to read, and files at most 3
  `proposed` tasks for claims that are stale or contradictory, each with the
  file, line, claim, and evidence. Stylistic rewrites are not filed. The
  minted chore is tagged `no-diff-expected` and does not edit the repository.
  A workspace-authored `skill-validation` definition is left in place.

Read them before enabling. They are also the best worked examples of how much
instruction a minted task's body should carry.

Finding tasks tagged `qa-sweep`, `security-review`, `code-review`, or
`friction-curation` receive the matching bracketed title prefix at the shared
task-creation boundary. If more than one of those tags is present, the fixed
precedence is `qa-sweep`, `security-review`, `code-review`, then
`friction-curation`, regardless of tag order. Existing matching prefixes are
not duplicated.

Scheduler-minted parent tasks continue to use `[auto-task] `,
which takes precedence when an `auto-task:<name>` provenance tag is present.

## Shipped defaults are promoted on proven yield

A definition does not start as a shipped default. It starts workspace-local
(`orbit auto-task add`) and joins the shipped catalog only after it has shown
yield in at least one workspace: tasks it filed that landed, or a diff of its
own that merged, with a low share of duplicate or invalid rejections, as
`auto-task-yield-review` measures them. A proposal to ship a new default cites
that evidence: the workspace, the window, the runs, the landed tasks or merged
diffs, and the rejection share.

A default that stops paying for itself can be
demoted the same way.

These former defaults were demoted for unproven yield: `backlog-hygiene`,
`ci-budget-watch`, `design-sync`, `flaky-test-hunt`, `pilot-block-patterns`,
`recovery-fault-audit`, `run-failure-patterns`, `structure-sweep`,
`suppression-ratchet` and `workflow-health-audit`. `blocked-task-triage` was
retired earlier, superseded by final recovery, and `crew-ladder-review` was
retired with the crew ladder it reviewed.

Orbit
no longer seeds a demoted default, but a workspace that already has a copy
keeps it: `orbit workspace sync` leaves the file, its settings and its schedule
as they are and reports it once as `migrated` while it drops the copy's managed
provenance. From then on it is a workspace-local definition that sync no longer
refreshes, and `seeding.auto_tasks` still accepts its name. Delete it with
`orbit auto-task delete <name>` if you no longer want it.
