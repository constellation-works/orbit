# Auto-tasks: seeded definitions, reviews and sweeps

## Contents

- [The 21 seeded definitions](#the-21-seeded-definitions)

## The 21 seeded definitions

`orbit workspace init` seeds the defaults selected by `seeding.auto_tasks`
(`"all"` by default) from this catalog, disabled,
except any you deleted (see [Deleting a definition](auto-tasks-deleting.md#deleting-a-definition)).
`host-health` seeds disabled, like every other default.

In an existing
workspace, `orbit workspace sync` or `orbit workspace init` turns off a
`host-health` no operator has configured (its file still holds the bytes Orbit
wrote and no `orbit auto-task toggle` set its flag); `orbit init` leaves
existing files alone. After a toggle or a file edit, that choice decides:
`orbit auto-task toggle host-health on` keeps it on. Only the workspace
owner mints it; a replica never does.

For ongoing selection and reconciliation, follow [Seeding policy](configuration-layers.md#seeding-policy):

- **`qa-sweep`** (`medium`) — hourly. Identifies recent changes, exercises them hands-on
  through their real user-facing paths rather than just re-running the test
  suite, and files a task for each non-duplicate issue found. In agent-executor
  sandboxes and linked job-run worktrees, the managed `.git` mount is read-only
  by design (must not be worked around by chmod or host-side gitdir writes).

  Use `mkdir -p .orbit/tmp && mktemp -d .orbit/tmp/qa-base.XXXXXX` for a new, empty
  directory, then `git archive <sha> | tar -x -C <that path>` to build a baseline
  revision without writing `.git`, and `git show HEAD:<path> > <path>`
  to revert a tracked file when `git checkout --` cannot take `index.lock`.
- **`friction-curation`** (`medium`) — daily. Deduplicates the open friction corpus against
  task history, verifies each survivor still reproduces, resolves the ones that
  don't, and files fix tasks for the ones that do. A survivor owned by another
  workspace is re-homed there, or marked `rehome_to` when that workspace is not
  reachable, instead of blocking the run. → [friction.md](../../orbit/references/friction.md)
- **`security-review`** (`hard`) — weekly. Reviews applicable application code,
  dependencies, secret handling, and configuration with evidence; files a
  durable Orbit task for each non-duplicate finding with severity and impact; a
  clean review is a successful no-op.
- **`code-review`** (`hard`) — every six hours. Reviews the two least recently
  reviewed bounded, disjoint modules read in full at one pinned integration
  commit. Each confirmed high or critical finding becomes its own task; a
  module's medium and low findings are grouped into its single cleanup task
  tagged `code-review`. Per-delivery diff review stays separate under
  `delivery-code-review`.

  Rotation history lives in the `reviewed-modules.json`
  artifact; the `sweep-cursor.json` artifact is the scheduler cursor.
- **`full-code-review`** (`medium`) — on demand (`orbit auto-task mint
  full-code-review`); its monthly cron stays off until you enable it. The minted
  task is a coordinator: it pins the integration branch's tip and counts only
  source in programming or scripting languages, tests, build and maintenance
  scripts, and CI workflow definitions.

  It excludes documentation and prose,
  lockfiles, dependency manifests and declarative configuration, test fixtures
  and other test data, and generated, vendored, minified, binary and static
  asset files; exclusions are recorded as path patterns with reasons and file
  counts. It splits candidates above 25,000 included lines, then merges every
  candidate below 10,000 lines with related candidates while the combined area
  stays within 25,000 lines.

  If included code totals 25,000 lines or fewer, it
  makes one area. It does not file an area under 5,000 lines unless no in-bound
  merge is possible, and records the reason in `full-review-areas.json`. It
  files at most 24 area-review chores per run (hardest and least recently
  reviewed first; the rest are recorded as `deferred`), each tagged `full-code-review` and
  `no-diff-expected` (never `code-review`), at `hard` complexity or below with
  no pinned crew.

  Each area
  reviewer reads its whole area at that commit in partitions of at most 10,000
  lines and files confirmed findings as bugs tagged `code-review` and
  `full-code-review`;
  a clean area is a successful no-op.
- **`qa-full-sweep`** (`medium`) — on demand (`orbit auto-task mint
  qa-full-sweep`); its monthly cron stays off until you enable it. Where
  `qa-sweep` validates only the changes since its cursor and
  `full-code-review` reads the whole tree without running it, this sweep runs
  the whole product.

  The minted task is a coordinator: it pins the integration
  branch's tip, inventories the user-facing surface from the repository's own
  docs and entry points (or from its own full-QA runbook or harness when it
  ships one), attaches `qa-full-sweep-inventory.json` with the areas and
  every exclusion, and files one surface-QA chore per area of at most 20
  required scenarios, tagged `qa-full-sweep` and `no-diff-expected`, at `hard`
  complexity or below with no pinned crew.

  Each area tester builds that commit
  and exercises the area through its real boundaries in disposable roots,
  recording every scenario as `PASS`, `FAIL`, `BLOCKED` or `NOT_RUN` in a
  resumable `qa-progress.json`; a check that needs a missing capability is
  never `PASS`. Non-duplicate failures become bugs tagged `qa-full-sweep`.

  The
  coordinator also files a roll-up chore that depends on every area and
  records the release decision in `qa-full-sweep-decision.json`: `PASS` only
  when every area is done and every required scenario passed at the pinned
  SHA, otherwise `INCOMPLETE`, including when an area's progress is missing,
  unreadable, or names another SHA or area. The roll-up's description and
  acceptance criteria carry an owner-evidence contract.

  A claimed worker can
  read ordinary task records and non-review artifacts for any task in its
  claimed workspace; `review-*` artifact reads remain limited to the claimed
  task. This roll-up still requires owner-local evidence by policy, so
  preparation records `required_machine` and only the owning machine runs it.
  Area tasks stay portable, with no crew and no `os:` tag.

  The coordinator sets those dependencies with `orbit.task.update` on the
  owner and finishes only when the roll-up's `dependencies` field lists
  every area. If that write is unavailable, it returns a
  `qa_rollup_dependencies_unwired` blocker for the owner to apply; a comment
  that names the IDs is not the wiring. The roll-up carries
  `auto-task:qa-full-sweep`, so no new sweep is minted by
  the schedule until it is done, and it records the swept commit as the
  `skip_if_unchanged` cursor.

  A workspace that already has its own
  `qa-full-sweep.yaml` keeps it as its active definition: sync preserves the
  file byte for byte and warns that it occupies the bundled default's name.
- **`delivery-code-review`** (`hard`) — reviews each frozen delivery batch and
  records typed coverage evidence: for each delivery, the changed paths it
  examined (or skipped, with a reason) matching the frozen diff, a verdict and
  a rationale. A review whose agent persists no execution summary leaves its
  batch owed. Hands-on QA of recent changes is
  `qa-sweep`; hands-on QA of the whole product at one commit is
  `qa-full-sweep`.
- **`doc-duties`** (`low`) — daily. Validates a small batch of the
  least-recently-attempted workspace documentation against current behavior,
  and corrects factual drift and broken links. Each run attaches a
  `doc-duties-ledger.json` artifact recording an outcome (`clean`, `fixed`,
  `partial` or `skipped`) and attempt date per selected document, plus a
  cumulative `state` map of every path's two latest attempts.

  A run discovers
  each candidate's artifacts through the metadata-only `orbit.task.show`
  (`field: "artifacts"`) projection and fetches a ledger only when listed, so
  absent ledgers are skipped without a failed artifact read. It reads only
  the newest listed ledger's `state` (older ledgers without it are read
  through the 60-run window), so attempts survive task-history truncation.

  Selection orders by the latest attempt, then `last_validated` frontmatter,
  then the git last-touched date, so a skipped document rotates to the back. A
  document whose two latest attempts were `skipped` or `partial` is held back
  and reported as needing its own task. A batch whose claims
  are already accurate is a successful no-diff run with validated no-diff
  evidence.

  Its template declares `dir:docs` plus the tracked top-level
  `README.md` and `CONTRIBUTING.md` that its brief validates, so it doesn't
  lock the whole tree, and omits `no-diff-expected` so it holds a lock on
  that scope while correcting drift. The executing worker selects the batch
  at run time. Task-pilot preparation keeps those template selectors: it
  does not replace them with the selected batch or with a document the
  brief excludes.
- **`comment-hygiene`** (`low`) — daily. The code-comment counterpart of
  `doc-duties`: checks at most six tracked source files and about 1,500 lines
  against a generic comment standard stated in its body (comments say why,
  doc comments state the contract, no history narration or commented-out
  code, no ownerless TODOs, escape hatches state their reason, stale comments
  are bugs, density stays low), which a workspace can sharpen through a
  `comment-hygiene` tag instruction.

  Each run attaches a
  `comment-hygiene-ledger.json` artifact with an outcome (`clean`, `fixed`,
  `partial` or `skipped`), attempt date and `checked_through` commit per
  selected file, plus the same cumulative `state` carry-forward as
  `doc-duties`; a file unchanged since its last `clean`/`fixed` check is not
  re-read, files in an open task's footprint are skipped, and two
  `skipped`/`partial` outcomes in a row escalate to a dedicated task.

  Its
  diff is comment-only — everything outside comments and doc comments stays
  byte-identical — and a stale comment that reveals a real defect is filed
  as a task (at most two per run) instead of being fixed in place.

  Like
  `doc-duties` it mints as `proposed`; unlike `doc-duties` it declares no
  `context_files`, so a minted instance is no backlog whole-tree holder; pre-admission
  preparation (the task pilot, or an operator by hand) selects the batch
  and persists exactly its `file:` selectors before the task is approved
  to `backlog`, and the run reads them back and edits only those files.

  A clean batch is a
  successful no-diff run with validated no-diff evidence, and the ledger is
  attached on every run.
- **`run-failure-patterns`** (`medium`) — weekly. Rate-driven: it always
  analyses the 7 days before scan start (the prior cursor only splits out
  new-since-cursor occurrences), computes per-job raw and folded failure rates
  for 7 and 30 days with deltas, and stores them in `run-failure-report.json`.

  On an owner it also counts the failure and release settlements its followers'
  claimed leaves sent (`orbit run settlements --no-reconcile`), counting a
  settlement as a failure only when its typed `accounting.counts_as_failure`
  is true and reporting designed holds (`review_decision_hold`,
  `evidence_hold`, `operator_cancel`, `upgrade_paused`, `delivery_hold`) in a
  separate `held_7d`
  counter, so raw occurrences equal failures plus holds.

  A run an Orbit upgrade
  paused (`upgrade_paused` in its history row) counts there too, while a lost
  or killed worker stays a failure. It folds
  parent pipelines into their child by the recorded `root_cause` (a parent of
  a `held` child is no failure) and splits review refusals by escalation
  category.

  It accounts for the Pareto set — the top classes covering at least
  80% of a qualifying job's folded failures plus every class with at least 3
  occurrences across 2 runs in 7 days — giving each a category and disposition
  and dismissing none by kind alone. A done fix is checked for deployment on
  each failing host before later occurrences count as regressions, and its own
  done fixes get effectiveness verdicts.

  Open coverage gets a comment with
  fresh counts; at most 5 new `proposed` fix tasks are filed per pass, tagged
  `run-failure-patterns` and `no-auto-approve`, so each waits for human
  approval; a friction replaces the task only when no cause can be
  established, never both. It runs on the owner: preparation records a
  `required_machine` owner-evidence contract before admission, and a follower
  that still starts it stops with `wrong_host` without filing.
- **`host-health`** (`low`) — disabled-by-default weekly review of host-local
  `orbit doctor --json` findings, Orbit service logs, and a read-only Orbit
  version and migrations check.

  That check records `orbit --version`, the
  executable path, SHA-256 and any recorded local-candidate source commit,
  `orbit update --check`, how far that commit trails the workspace integration
  branch, `orbit update --preflight`, `orbit migrate --dry-run` for each
  workspace (`orbit workspace list --all`), and doctor's `retired-config-keys`
  findings. The pass covers the owner host only.

  Preparation records
  `required_machine` for that owner and refuses any other machine, so
  automatically pinned follower collectors are unavailable and every other host
  is missing coverage, not a drift finding. It files standing doctor, migration
  and update findings and recurring log errors, and routes operator fixes to
  the human crew with
  runnable `## Resolve` steps that name the install-type command or the
  printed migrate remediation.

  It never installs, confirms a migration, adopts
  authority, or removes retired keys.
