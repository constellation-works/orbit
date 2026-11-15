---
title: "Eval Harness — Design"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
feature: eval-harness
doc_role: design
type: design
summary: "Design of the eval plugin: candidate tasks as the shared primitive, the replay job (snapshot, leakage, held-out tests, eval sets, readout), the bake-off job (candidate PRs, locks, pick, cleanup), capacity, spend, plugin shape, build order and open questions"
tags: [eval-harness, plugins, evaluation, experiments, crews]
related_features: [plugins, experiments, agent-scorecards, model-ranking, review-gate, distributed-drain]
related_artifacts: [ORB-24839]
---

# Eval Harness — Design

This document designs the `eval` plugin described in [1_overview.md](./1_overview.md). Nothing in it is built: every "the plugin does" below is a proposal, and every statement about existing behaviour cites the code it was read from at commit `35df669d0`. It follows the eight points of [ORB-24839] and says where it departs from the orchestrator's starting view. The build order (§13) and the open questions for Daniel (§14) close the document; no implementation task has been filed.

**Departures from the starting view, in one place:**

| Starting view | This design | Why |
|---|---|---|
| One shared primitive: a detached run of task T at base B with crew C | A **candidate task**: a copy of T with crew C, run by ordinary machinery | A detached run of T itself needs five core changes and gets no drain capacity (§1) |
| Replay runs beside live work | Replay runs in a separate evaluation workspace | It removes lock conflicts, record pollution and most leakage without core changes (§2) |
| Snapshot rebuilt from task history | Snapshot read from the run audit; history is only a fallback | History records which fields changed, not their text (§3.3) |
| Both jobs run inside drain capacity below delivery | Bake-off does, by task priority. Replay runs in its own drain, capped by concurrency and window, because no cross-workspace priority exists | §9 |
| n candidate PRs for one task | n candidate tasks with one PR each | Orbit enforces one PR and one run binding per task (§1.1) |

---

## 1. The shared primitive

### 1.1 Orbit cannot run a task detached today

The starting view asks for "run task T at base commit B with crew C, leave a branch and a diff, and do not change the task's lifecycle or land anything". No existing path does that. Each part, against the code:

| Requirement | Today | Code |
|---|---|---|
| Pin the base to a commit | The base is a branch name (`base` or `base_branch`, default `main`). The commit is resolved from it once and is an output, never an input | `setup_worktree`, `crates/orbit-engine/src/executor/automation/vcs/worktree/setup.rs:49-51`, `:105-118` |
| Leave the lifecycle alone | Worktree setup admits every task to `in-progress` and stamps the run on it | `setup.rs:195-204` |
| Run a closed task | Admission accepts only `backlog` and `in-progress` | `workflow_admissible_statuses`, `crates/orbit-core/src/application/task/transitions/admission.rs:100-111` |
| Hide later task state | The implementer's task context is built from the live record: status, plan, external refs (the PR link), execution summary and every comment | `agent_task_context_json`, `crates/orbit-core/src/adapter/engine_host/actions/task_context.rs:360-427` |
| Choose the crew per run | **Exists.** An explicit `crew` run input wins over the task's crew and the workspace default | `select_crew_name`, `crates/orbit-runtime/src/engine/crew/resolution.rs:14-35` |
| Several runs of one task at once | Each run overwrites the task's single `job_run_id` (`setup.rs:195-204`); `orbit run ship` refuses a task with a run in flight (`crates/orbit-core/src/application/job/pipeline/submit.rs:456`); opening a PR closes the task's other open delivery PRs (`close_earlier_task_pull_requests`, `crates/orbit-engine/src/executor/automation/vcs/pr/open/superseded.rs:84-99`) | as cited |
| No PR, no landing | `task_pr_pipeline` always pushes, opens a PR and promotes; `task_local_pipeline` always merges. A custom job can omit those steps, but still goes through worktree setup above | `crates/orbit-core/assets/jobs/task_pr_pipeline.yaml`, `task_local_pipeline.yaml` |
| Keep the branch afterwards | Worktree GC deletes the worktree and its branch once the run is terminal and the task is `done`, `rejected` or `archived`, which a closed task already is | `task_status_permits_deletion`, `crates/orbit-engine/src/executor/automation/vcs/worktree/gc/sweep.rs:29-34` |

The crew override and the per-run branch name (`orbit/<task>-<hash of run id>`) are the only parts that exist.

### 1.2 What a true detached run would cost

Making T itself runnable twice needs all of these in core: a commit pin on worktree setup; an admission bypass; a frozen task context for the implementer; a second run binding beside `job_run_id` (candidate resume and failure handling read that field); and exemptions in the ship in-flight guard, PR superseding and worktree GC. It would also need a capacity lane, because only the delivery leaves occupy drain slots: a run started with `orbit run job` takes none (`crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs:590-596`). That is a second delivery model threaded through the first one.

### 1.3 The primitive this design uses: a candidate task

Both jobs copy the task and run the copy.

> A **candidate task** is a new task whose title, description, acceptance criteria, selectors, tags, type and tier are copied from a source (a snapshot of a closed task, or a new task), whose `crew` is set to one arm, and which an ordinary Orbit run implements.

A copy inherits everything a task already has: a lifecycle of its own, drain admission and priority, host pinning by tag, one worktree, one branch, one optional PR, review, artifacts, cost records and GC. The source task is read once and never written. An explicit crew also keeps the candidate out of any crew-bearing live experiment (`crates/orbit-core/src/application/experiment/assignment.rs:63-66`).

The two jobs differ in where the copy lives and where its run stops:

| | Replay | Bake-off |
|---|---|---|
| Copy lives in | the evaluation workspace (§2) | the task's own workspace |
| Base | a pinned commit | the integration branch head, as usual |
| Run stops after | commit and scoring; nothing is pushed | PR opened, task in `review` |
| Delivery job | the plugin's `eval_replay_candidate` | the shipped `task_pr_pipeline` |

### 1.4 Core additions

Kept to the generic minimum. Each is useful without the plugin.

1. **`base_sha` input on `worktree_setup`** (needed by replay). When present, the worktree starts at that commit, which must exist locally; `base` still names the branch recorded as the base ref. A workaround exists and task 1 of §13 tries it first: the plugin creates a local branch at the commit and passes it as `base` with `base_sync: local`. It is not verified end to end, and it leaves one branch per entry behind, so the input is the clean form.
2. **Lock sharing between declared alternatives** (needed by bake-off). Tasks linked by a new `alternative_to` relation do not conflict with each other in the lock index; they still conflict with every other task. Today a task in `in-progress` or `review` holds its `context_files` (`holds_context_lock`, `crates/orbit-runtime/src/task/locks/index.rs:20-28`) and only tasks of the same bundle are exempt from each other (`visit_task_lock_conflicts`, `index.rs:452-476`), so n candidates with the same selectors would run one after another.
3. **Scorecard exclusion for alternatives that were not picked** (needed by bake-off). A candidate rejected because a sibling won is not a failed delivery. The scorecard already leaves out `no-diff-expected` tasks through one predicate ([agent-scorecards 2_design.md §5.2](../agent-scorecards/2_design.md)); a task rejected with an `alternative_to` relation to a landed task joins that exclusion.

Replay needs only the first, and possibly none. Everything else is in the plugin.

---

## 2. The evaluation workspace

Replay's candidate tasks live in a workspace of their own (working name `orbit-eval`): a second clone of the repository on a Linux box, initialised as an Orbit workspace, with the `eval` plugin enabled. Orbit-research is the precedent for a separate workspace over related code.

What that buys, with no core change:

- **No lock conflicts with delivery.** The lock index is per workspace. In the live workspace a replay candidate in `in-progress` would hold real `context_files` against real work.
- **Clean records.** Hundreds of candidate tasks stay out of the live task list, search, attention list, scorecards, model ranking and experiment populations.
- **A real task for every run.** The shipped activities (`worktree_setup`, `agent_implement`, `git_commit`, the review gate) work unchanged, because the candidate is an ordinary `backlog` task with no history.
- **Leakage control by configuration.** The workspace's own `config.toml` turns off crew memory (`crew_memory.enabled`), mid-run steering (`workflow.mid_run_steering.enabled`) and experiments, and holds no workspace notices.
- **A capacity cap.** The workspace's drain has its own concurrency (§9).

What it costs: a second clone with cold build caches; a `config.toml` that has to mirror the live workspace's validation commands, tag instructions and crews by hand (`config.toml` is read-only to a plugin: [plugins 1_scope.md §4.3](../plugins/1_scope.md)); and no priority relation between the two workspaces' drains.

The alternative, running replay candidates in the live workspace at `low` priority, gets priority below delivery for free but needs a lock exemption for candidates, an analytics exclusion on every surface, and leaves the original task one `orbit.search` away from the candidate. §14 Q1 asks Daniel to confirm the choice.

---

## 3. Replay: inputs, base commit and snapshot

### 3.1 Inputs

`eval.replay-start` takes an eval set (or a list of closed task ids, which it turns into an unnamed set first), a list of arms, a repeat count and a spend cap. An arm is a crew name defined in the evaluation workspace's config, optionally with an effort using the shipped crew-reference syntax (`<crew>@<effort>`, as described in the Crew references section of [CONFIG.md](../../CONFIG.md)). A bare model is not accepted: a crew is the only way a run resolves a provider, model and effort, and `orbit config set` cannot create one ([CONFIG.md](../../CONFIG.md)). Defining crews stays an operator edit.

### 3.2 Base commit

The base is the first parent of the commit the task landed as. Orbit already records both: `landed_diff_stats` holds `landed_commit` and `base_commit` per task (`LandedDiffStats`, `crates/orbit-store/src/contracts/landed_diff_stats.rs:62-85`), and the writer defines `base_commit` as the landed commit's first parent, because pull requests land by squash or merge commit (`crates/orbit-core/src/application/codebase/diff_stats.rs:7-18`). Tasks without a row fall back to the landing receipt in task history, read by `crates/orbit-core/src/application/codebase/landed.rs`.

This agrees with the starting view. It is deliberately not the base the original run started from, which is older whenever the branch was rebased before landing: the reference diff and the held-out tests are only known to work on the merge parent.

### 3.3 The snapshot

Task history cannot rebuild a task's text. An `updated` event names the documents that changed (`task fields updated: plan`), not their old or new content (`crates/orbit-types/src/task/fields_updated.rs`). History does record tag values (`tags_at`, `crates/orbit-types/src/task/tags_changed.rs`) and the selectors a run added afterwards (`crates/orbit-types/src/task/context_widening.rs`).

The exact source is the run audit. Since 2026-10-29 ([ORB-20006]) every provider invocation records a `task_scope_blob_ref`: the "bounded, redacted task scope as delivered at dispatch, not a later mutable task projection" (`CliInvocationStarted`, `crates/orbit-types/src/workflow/activity_job/audit_envelope.rs:358-372`). The snapshot is built in this order:

1. **Audit blob (preferred).** The task scope blob of the task's first `agent_implement` invocation gives title, description, acceptance criteria, selectors, type and tier exactly as the first implementer received them. Tags come from `tags_at` at that invocation's start.
2. **History fallback.** When the blob is gone, use the current record if, and only if, no `updated` event after the first dispatch names the description or acceptance criteria (the test `spec_documents` already makes, `crates/orbit-core/src/application/agents/records.rs`). Selectors are the current ones minus every `context_files_widened` addition; tags come from `tags_at`.
3. **Otherwise the task is not eligible.**

Two limits follow. Audit rows carrying the blob start on 2026-10-30 on the box, and `orbit gc audit` prunes blobs after `retention.audit_days` (default 60, `crates/orbit-cli/src/command/gc.rs`). So `eval.set-build` copies the snapshot into the eval set when the entry is added; a set never depends on the audit store again. The blob was added for the delivery-quality grader, which has since been removed (§10.1), and nothing in the tree reads it today (the only non-test use is the writer, `crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch/provider/execution.rs:320-349`). Task 2 of §13 gives it a reader so that it is not cleaned up as dead.

Always left out, whatever the source: the plan, the execution summary, external refs, the status note, relations, and every comment. The plan is dropped even when a pilot wrote one before dispatch, because the implementer overwrites it early in the run and the original text is not recoverable; this is a small departure from "as it stood at dispatch" and applies equally to every arm.

The candidate task gets its own id in the evaluation workspace. The source task id appears nowhere on it; the mapping lives in the plugin's store.

---

## 4. Replay: leakage

What reaches an implementer besides the task, and what replay does about each:

| Input | Source today | Replay |
|---|---|---|
| Task plan, comments, summary, PR link, status | Live record (`task_context.rs:360-427`) | Absent: the candidate is a fresh task |
| Repository guide (`CLAUDE.md`, `AGENTS.md`), tag-instruction files, agent corpus | The worktree, or files read at the run base | Pinned by the base commit |
| Tag selection | Live tags | Tags copied from the snapshot |
| Crew memory | Newest revision at dispatch | Off in the evaluation workspace |
| Mid-run steering, workspace notices, experiments | Live | Off or empty in the evaluation workspace |
| Project learnings | Retired; `.orbit/learnings/` is no longer read ([project-learnings](../project-learnings/4_decisions.md)) | Nothing to do |
| Skills and the `agent_implement` instruction | Embedded in the installed binary (`crates/orbit-runtime/src/assets.rs`) | **Not pinned.** Every arm runs under today's harness |
| User-level provider files (`~/.claude`) | Live host files | Not pinned; same for every arm |

**Skills and the implementer prompt are today's, on purpose.** Replay answers "which crew does this work best under the Orbit we run now", not "what would have happened then". All arms share the same harness, so the comparison between arms is fair, and each run records the Orbit version. The consequence: results from different Orbit versions are not comparable, and the readout refuses to merge them.

**Three leaks are not closed in the first version:**

1. **Git.** A worktree shares its clone's object store, and the clone holds commits after the base, including the fix. A candidate that runs `git log --all` can find it.
2. **The network.** The repository is on GitHub; a candidate with network access can read the merged pull request.
3. **Training data.** A model trained after the merge may have seen the fix. Recent tasks are the least exposed, and the audit-blob limit already forces recent tasks.

For the first two, the first version detects and does not prevent. Each invocation records its tool calls (`InvocationRecord`, `crates/orbit-store/src/contracts/invocation.rs`); the scorer scans them for history reads beyond the base and for forge or web access to the repository, and marks the run `contaminated`. Contaminated runs are listed in the readout and left out of every rate. Prevention (a per-run clone containing only ancestors of the base, and a network-denied sandbox profile) is task 10 of §13, to be built if the contamination rate shows it is needed.

---

## 5. Replay: outputs and scoring

### 5.1 The job

Each candidate task carries a `delivery:eval_replay_candidate` tag, which routes it to the plugin's delivery job through the same gate as the shipped pipelines ([plugins 1_scope.md §4.5](../plugins/1_scope.md)). The job declares `task_delivery: { modes: [local] }` and `owns_task_worktree: true`. Its step sequence and the candidate task's lifecycle status at each step:

1. `worktree_setup` at the entry's base commit (§1.4). Admission moves the candidate from `backlog` to `in-progress` and binds the run ID (`setup.rs:195-204`).
2. `agent_implement`, the shipped activity, unchanged (`in-progress`).
3. `git_commit` snapshots the candidate changes onto the run branch (`in-progress`).
4. `eval.candidate-capture` (plugin tool): writes the candidate's diff (`eval/diff.patch`) and commit id as task artifacts and keeps the commit under `refs/orbit/eval/<replay>/<candidate task>` (`in-progress`).
5. Scoring steps (§5.3), each bounded and each writing an artifact (`eval/held-out.json`, `eval/metrics.json`, `eval/validate.json`, or blind review artifacts; task remains `in-progress`).
6. `mark_review` (the shipped `update_task` activity, setting `status: review`, as in `task_local_pipeline.yaml:187-198`): explicitly transitions the candidate task from `in-progress` to `review`.
7. `task_complete`: performs the canonical `review -> done` completion transition (`complete.rs:65-80`). The candidate task ends `done`. A candidate task being `done` means "scored", not "passed".

**Completion authorization and completion holds.** The shipped `task_complete` activity requires the candidate to be in `review` and runs only for tasks a run was explicitly authorized to complete (`crates/orbit-core/assets/activities/task_complete.yaml:6-14`, `complete.rs:140-152`). Replay candidate runs receive that authorization either from the evaluation workspace's drain started with `--complete` (`orbit run auto --complete --for <window> --concurrency <n>`, §9) or by `eval_replay_candidate` defaulting `completion: done` in its job input. Under `completion: done`, `task_complete` evaluates the workspace's `completion.hold` rules against the candidate's complexity, type, or tags (`complete.rs:89-104`):

- When no hold matches, the task moves to `done`. It never pushes, opens a PR or merges. Worktree GC then removes the worktree and branch as it does for any settled task (`task_status_permits_deletion`, `crates/orbit-engine/src/executor/automation/vcs/worktree/gc/sweep.rs:29-34`); the kept ref and the task artifacts are what remain.
- When a `completion.hold` rule matches, `task_complete` leaves the candidate in `review`, records the hold on the task, and reports it in `held_task_ids`. Because worktree GC deletes worktrees and branches only for settled tasks (`done`, `rejected`, or `archived`), GC fails closed: the candidate's worktree, branch, and worktree claim remain intact on disk for operator inspection.

In either outcome, the step sequence ensures the candidate's diff and scores are captured as durable task artifacts (`eval.candidate-capture` and scoring steps) before `task_complete` or worktree GC can remove the branch.

### 5.2 Pull requests

No pull requests by default, as the starting view says. A PR option is not worth building into replay: its only use is to read a candidate in the forge UI or to run hosted CI on it, and 25 tasks times n arms times repeats would bury real delivery PRs. The diff artifact and the kept ref cover reading, and `eval.candidate-show` prints the diff. Someone who wants a PR for one candidate can push its ref by hand.

### 5.3 Signals

| Signal | What it measures | Cost | Build |
|---|---|---|---|
| **Held-out tests** | Whether the candidate fixes what the reference fixed | Minutes of build per run | **First** |
| **Cost, wall time, tokens, diff size** | Efficiency | Free: read from existing records | First, with the above |
| **Build and lint** | Whether the candidate is mergeable at all | Minutes per run | Second |
| **Blind review** | Whether the candidate meets the acceptance criteria | One reviewer run per candidate | Third |

**Held-out tests come first** because the result is objective, repeatable, and needs no judge to be trusted. The other three are either free or need calibration.

*Held-out tests.* An entry stores the paths of the test files the reference added or changed (classified by `workflow.test_paths`, the same rule `landed_diff_stats` uses for its `tests` counts) and one test command. After the candidate commits, the scoring step copies those files from the reference commit over the candidate's checkout and runs the command. Outcomes are `pass`, `fail`, `build_error` (the tests do not compile against the candidate: often a private helper the reference introduced) and `timeout`. `build_error` is reported separately and counted as not passing, because the repository's rule that regression tests enter through a public CLI or API boundary makes it the exception. An entry is admitted to a set only after `eval.set-build` proves the command **fails at the base and passes at the reference**; that check is what makes the test a measurement of the task.

*Cost, time, size.* Cost and tokens come from the candidate run's invocation rows, priced by `derive_cost_usd` over the versioned price table (`crates/orbit-common/src/model/pricing.rs`, `crates/orbit-common/assets/model_prices.yaml`); a run with any unpriced invocation reports cost as unknown, never as zero, as the experiment readout does. Wall time is the implement step's duration. Diff size is counted from the candidate commit with the reference's size beside it.

*Build and lint.* The shipped `candidate_validate` activity runs the evaluation workspace's `workflow.required_validation_commands` on the candidate. For this repository that list should be a short one for replay (format, the changed crates' clippy), not the full CI.

*Blind review.* The review gate cannot be pointed at an arbitrary diff: admission needs a job run and a task, reserves the attempt on a lineage ledger and writes the manifest and settlement to task records ([review-gate 2_design.md](../review-gate/2_design.md)). A candidate task has all of those, so the shipped steps (`review_gate_admit`, `agent_review_repair`, `review_gate_settle`) run in the candidate's own job. Three rules make it a judge:

- The reviewer crew is one fixed crew for every arm of a replay, recorded in the readout.
- The signal is the verdict (`accept`, `accept_with_fixes`, `reject`, `incomplete`) and the findings. Reviewer repairs are discarded: held-out tests and diff size are measured on the implementer's commit.
- The reviewer is blind to the reference and to the other candidates. It is not blind to the crew, which the review manifest names; §12 records that.

A reviewer's verdict is an opinion. Before it is reported as a headline number it is calibrated against the held-out result on the same candidates, and later against bake-off picks (§8.6).

### 5.4 Durable outputs

Every score is a task artifact on the candidate task (`eval/diff.patch`, `eval/held-out.json`, `eval/metrics.json`, `eval/validate.json`, plus the review gate's own artifacts), and one row in the plugin's store. Readouts are computed from those, never from an agent's final message, in line with the rule that an activity's system of record is its durable artifact.

---

## 6. Replay: eval sets

### 6.1 Choosing 20 to 30 tasks

`eval.set-build` proposes entries from a query and admits each one only after checks. The query, over `landed_diff_stats` joined to the task registry:

- type `bug`, landed, with test lines added (`tests` counts above zero). The repository admits a test only for a reproduced defect, so bug tasks are where reference tests exist. On the box this is about 1,250 tasks since the table began on 2026-10-26.
- a snapshot is recoverable (§3.3);
- one task per landed commit (no bundles), and no reviewer-authored repair in the landed range when that can be told;
- reference size within bounds (default 10 to 400 changed lines), so a set is not dominated by one-line fixes or rewrites.

The admission checks per entry: the base commit builds; the held-out command fails at the base and passes at the reference; the command finishes within the scoring time limit.

From what passes, the tool samples to a quota, stratified by tier and by top-level crate, with a fixed seed recorded in the set, and an operator reviews the list before it is frozen. Feature and refactor tasks have no held-out tests by rule; they can enter a set as "review-only" entries once the blind review is calibrated, and are kept out of the first sets.

### 6.2 Storage

A set is a directory in the plugin's state (`~/.orbit/state/plugins/eval/sets/<name>/<revision>/`): a `set.yaml` listing entries (source workspace and task id, base and reference commits, held-out paths and command, tier, type, stratum), one snapshot file per entry, the sampling seed, and a content digest. A revision is immutable; changing a set makes a new revision. Runs and scores are rows in a SQLite file beside it. `eval.set-export` and `eval.set-import` move a set between hosts as one archive.

### 6.3 Repeats

One run per task and arm is not enough: the same crew on the same task does not always produce the same result. The unit of analysis is the task, and the statistics follow from that:

- **Repeats: 3 per task and arm** by default. Each task's score for an arm is its pass fraction over the repeats. More than three adds little, because the spread between tasks is larger than the spread between repeats.
- **Comparison: paired by task.** Every arm runs every task, so the difference between two arms is the mean of per-task differences, with a bootstrap interval that resamples tasks, not runs. This is the method the experiment readout uses for its intervals (percentile bootstrap, 2,000 resamples, fixed seed: `crates/orbit-core/src/application/experiment/readout/stats.rs`).
- **What 25 tasks can show.** A rough estimate, not a measurement: with 25 tasks and 3 repeats, a difference in pass rate between two arms has to be around 15 to 20 points before its interval excludes zero. A single arm's pass rate carries an interval of about ±19 points at 25 tasks. Smaller differences need more tasks, not more repeats.
- **Cost of a replay.** 25 tasks × 3 repeats × n arms: 225 candidate runs for three arms. A first replay with one repeat measures the cost per run before the full one is paid for.

The readout states the interval with every number and prints `no difference shown` when the interval includes zero. It never names a winner on a point estimate.

---

## 7. Replay: readout

One readout per replay, computed on read from the plugin's store:

```
replay r-2026-11-20-a   set orbit-bugs3 (25 tasks)   repeats 3   orbit 0.49.0   reviewer: —
arm              runs  held-out pass      build err  contaminated  cost/run  median time  lines vs ref
fablehigh        75   0.71 [0.58, 0.83]      4          1          $2.10      14m         1.3×
opushigh         75   0.64 [0.50, 0.77]      6          0          $1.20      11m         1.1×
codexhigh        75   0.52 [0.38, 0.66]      9          0          $0.90      17m         1.8×
fable − opus     +0.07 [−0.06, +0.20]   no difference shown
fable − codex    +0.19 [+0.04, +0.33]
per task: 25 rows × arms, pass fraction per cell
```

(The numbers are invented to show the shape.) It carries: the set revision, Orbit version, arms with their resolved provider, model and effort, per-arm rates with intervals, paired differences, a per-task table, cost coverage, excluded runs with reasons, and warnings (mixed Orbit versions, incomplete arms, cap reached).

Where it lives:

- **CLI and MCP:** `orbit eval replay-readout <replay>` (`eval.replay-readout`, read-only, JSON or text). This is the first surface.
- **Dashboard:** a plugin panel (`table` and `kv`, sourced from the same read-only tool), which is what the plugin standard allows without plugin JavaScript.
- **Task artifact:** when a replay was started for an Orbit task, the tool attaches `eval/readout.json` to that task, so the decision it informed keeps its evidence.

---

## 8. Bake-off job

Bake-off is a separate job with a separate purpose: it delivers one task. It runs in the task's own workspace, through the normal drain, with real pull requests.

### 8.1 Inputs

`eval.bakeoff-start` takes one task in `backlog` (or `proposed`, once approved) and two to four arms. Four is the ceiling: each candidate is a full delivery with a PR and hosted CI.

### 8.2 Lifecycle

- **The original task T** does not run. `bakeoff-start` gives it the `no-drain` tag, which the drain already honours as a hold, and a comment naming its candidates. It stays in `backlog` until the pick.
- **Candidates C1…Cn** are new tasks: a copy of T's title (suffixed with the arm), description, acceptance criteria, selectors, tags, type and tier; `crew` set to the arm; relations `spawned_from: T` and `alternative_to` between siblings; tag `bakeoff:<T>`. They are filed in `backlog` at T's priority and delivered by the unchanged `task_pr_pipeline` without completion authority, so each stops in `review` with an open PR.
- **After the pick** (§8.5) the winner lands through normal landing, T is completed with the winner's merge as its evidence, and the others are rejected.

T's snapshot is taken at `bakeoff-start` and stored by the plugin: it is exact, with no reconstruction, which is what makes a bake-off a good source of eval entries (§8.6).

### 8.3 Locks

Each candidate holds T's `context_files` while it is `in-progress` or `review`, exactly as a single delivery of T would, so other work on those files waits as it would have. Between siblings the lock is shared (core addition 2 of §1.4). Without that addition the candidates are admitted one at a time, each waiting for the previous one to leave `review`, which never happens before the pick: the addition is a precondition for bake-off, not an optimisation.

The lock is held for as long as the pick takes. A bake-off left undecided blocks its files; §8.5 sets a deadline.

### 8.4 Review gate

Each candidate passes the review gate on its own, as any delivery does. Under `before_landing` timing the full review runs when a candidate is landed, so in practice only the winner pays for it; candidates still pass required validation before their PR opens. The review gate is not asked to compare candidates.

A candidate that fails (blocked, validation red, implementer blocker) is simply not an option at the pick. If every candidate fails, `bakeoff-status` says so and T's `no-drain` tag is removed so it can be delivered normally or reworked.

### 8.5 The pick

A person picks. `eval.bakeoff-pick` (mutating, so operator-only under the plugin authority model) takes T, the winning candidate and a reason. It:

1. records the pick (§8.6);
2. rejects each losing candidate with a note naming the winner. Rejection is a terminal decision, on which Orbit closes the candidate's delivery PR (the terms are documented at `superseded.rs:84-99`), and a `rejected` task's worktree and branch are removed by worktree GC (`sweep.rs:29-34`). Task 7 of §13 confirms the PR close on rejection and adds remote-branch deletion if GC leaves the pushed branch;
3. returns the winner's task id and PR for landing.

The tool does not merge. Landing is `orbit.workflow.land`, an operator-only operation (`crates/orbit-common/src/governance/authorization/catalog/operations.rs`), and a plugin callback holds no operator capability; the operator lands the winner the usual way. When the winner's task reaches `done`, a plugin routine (or the next `bakeoff-status` call) completes T with a verdict citing the winner's merge commit and removes `no-drain`.

**A judge is advisory.** An optional `eval_bakeoff_judge` activity (read-only, no worktree writes) reads the n diffs without crew names and the acceptance criteria, and writes `eval/judgement.json` on T: a ranking with reasons. It is shown beside the candidates in `bakeoff-status`. It never picks; §14 Q4 asks whether it ever should.

**Deadline.** `bakeoff-start` takes `decide_by` (default 3 days). Past it, `bakeoff-status` and a notice report the bake-off as overdue. Nothing is auto-picked or auto-closed.

**Where the person chooses.** First version: the CLI or MCP tool, with a dashboard panel listing candidates, PR links, check status, cost and the judge's ranking. The human decision channel cannot carry it yet: `orbit.decision` raises only on a task assigned to a human crew, and plugin-raised decisions are design only ([human-decision-channel 2_design.md](../human-decision-channel/2_design.md)).

### 8.6 What is recorded, and how picks feed replay

For every candidate, winner or not, the plugin keeps: the arm, the head commit, the diff (as a task artifact on T, so it outlives the branch), validation and check results, cost and time. For the bake-off: T's snapshot, the base commit each candidate started from, the winner, the picker, the reason, and the judge's ranking if one ran.

Each pick feeds replay in two ways:

1. **A ready eval entry.** Once the winner merges, `eval.set-build --from-bakeoffs` offers T as an entry: the snapshot is exact, the base is the winner's merge parent, the reference is the winner's merged diff, and it goes through the same fail-then-pass admission as any entry if it carries tests.
2. **Calibration for the blind judge.** A pick says "this diff over those, for this task". Replaying the judge over stored bake-offs and counting how often it ranks the picked diff first gives the judge an agreement rate with a person. That number decides whether the blind review signal (§5.3) can be trusted as a headline score, and whether a judge could ever pick.

Losing candidates are not a regression record and not an escaped defect. They are excluded from scorecards (core addition 3 of §1.4).

---

## 9. Capacity and spend

**Hosts.** Every candidate task carries `os:linux`, which admission enforces on local and pull drains alike ([CONFIG.md](../../CONFIG.md)), so no candidate is ever admitted on the Mac. The evaluation workspace exists only on a box.

**Admission.** Candidates are ordinary tasks, so they run only when a drain admits them: there is no separate launcher and no synthetic load.

- *Bake-off* candidates enter the live drain at T's own priority. They are real delivery of a task someone chose to pay n times for, and they take n slots.
- *Replay* candidates enter the evaluation workspace's drain, started as `orbit run auto --complete --for <window> --concurrency <n>` from that workspace on the box (or `eval_replay_candidate` defaulting `completion: done`), granting each replay run the completion authorization required by `task_complete` (§5.1).

**Priority below delivery.** Within one workspace a `low` task sorts last (`automatic_dispatch_cmp_with_affinity`, `crates/orbit-types/src/task/model/task.rs:848-912`), but no slots are reserved and nothing relates two workspaces' drains. Orbit has no job class or lane below delivery. So for replay, "below delivery" is achieved by three existing limits, and one honest gap:

- the evaluation drain's concurrency is small (default 2) and it runs in an operator-chosen window (`--for`), by default at night;
- the host resource throttle zeroes free slots under CPU, memory, disk or I/O pressure for every drain on the host ([CONFIG.md](../../CONFIG.md));
- builds from both workspaces queue in the one host-wide build budget, which also runs them at low CPU and I/O priority ([build-budget runbook](../../runbooks/build-budget.md));
- **gap:** when both drains want capacity at once, neither yields. If measurement shows replay slowing delivery, the generic fix is a drain option that admits only while the host's other drains have free slots; §14 Q5.

**Spend.** Orbit has no per-run or per-job dollar cap. The only budget is `workflow.provider_limit_budgets`, a rolling window per provider account that removes a crew from delivery draws near its limit (`crates/orbit-config/src/provider_limit/budget.rs`). The plugin adds a cap of its own:

- `replay-start` takes `max_usd` (required). The plugin files candidate tasks in waves (default: one repeat of the set for one arm at a time), and before each wave sums the measured cost of the replay's finished runs. At or past the cap it files nothing more and marks the replay `capped`. Overshoot is bounded by one wave.
- A run with unknown cost counts at the replay's highest measured cost per run, so unpriced models cannot run free under the cap.
- `bakeoff-start` reports the expected cost (n times the tier's median delivery cost from scorecards) before filing and needs `confirm: true`.
- The implement activity timeout (`workflow.activity_timeouts`) bounds each run's wall time; replay sets a shorter one in the evaluation workspace's config.
- Where a provider account can be dedicated to evaluation crews, a `provider_limit_budgets` entry on that account is a second, host-enforced limit.

---

## 10. Relation to live experiments

| | Experiments | Replay | Bake-off |
|---|---|---|---|
| Question | Does treatment B beat A on live work? | Which crew does known work best? | Which finished candidate should merge? |
| Work | New tasks, delivered | Closed tasks, never delivered | One new task, delivered once |
| Assignment | Random, one arm per task, sticky (`assignment.rs`) | Every arm runs every task | Every arm runs the task |
| Ground truth | Delivery outcomes over time | Held-out tests against a reference | A person's pick |
| Speed | Weeks of traffic | Hours of box time | One delivery |
| Risk to delivery | The treatment ships | None | n times the cost, one result |
| Records | `experiments` tables in the workspace store | Plugin store plus candidate task artifacts | Same |

They answer different questions and are meant to be used in sequence: replay screens a crew cheaply, an experiment confirms it on live work, where the outcomes replay cannot see (rework, escaped regressions, merge conflicts) are measured.

**What they share:**

- *Records.* Cost, tokens, duration and tool calls come from the same invocation rows; price from the same table; landed size and the test-path classification from `landed_diff_stats`. The plugin reads them through read-only tools and adds no second accounting.
- *Metric names.* The readout uses the experiment metric names where the meaning is the same (`cost_usd`, `total_tokens`, `lines_changed`: `METRIC_SPECS`, `crates/orbit-core/src/application/experiment/definition.rs`).
- *Method.* Paired bootstrap intervals and "never call a winner inside the interval", as in the experiment readout. The code is not shared: a plugin is a separate executable and cannot link `orbit-core`. The plugin reimplements the bootstrap with the same resample count and a recorded seed.
- *Treatments.* Experiments can overlay instructions, skills and tools as well as crews. Replay arms are crews only in the first version. Prompt and skill arms are the natural next step and should reuse the experiment overlay definition and its capture format, not a second one; §14 Q6.

**What they must not share:** populations. Candidate tasks carry an explicit crew, which already excludes them from crew-bearing experiments, and replay candidates are in another workspace entirely.

### 10.1 Removed predecessors

Two earlier mechanisms covered nearby ground and were removed. This design should not grow back into either.

- **The crew ladder** ran standing shadow trials of cheaper crews on a share of live tasks and was to promote them automatically. Daniel retired it on 2026-11-07 while simplifying configuration ([crew-ladder scope, archived](../_archived/crew-ladder/1_scope.md)); crews are compared with operator experiments instead. Replay keeps to that decision: it is started by an operator, changes no routing, and produces a readout a person acts on.
- **The delivery-quality grader** asked a model to grade landed code on six dimensions; it shipped default-off and was removed with decision assistance ([proposal, archived](../_archived/decision-assistance/specs/delivery-quality.md)). Its lesson is the reason held-out tests come first here and the blind review comes last and only after calibration against human picks: a model's opinion of code needs independent ground truth before anyone reads it as a score.

---

## 11. Plugin shape

**Location.** Its own repository, `constellation-works/orbit-eval`, checked out under `codebases/` like other components. Plugin trees are never vendored into a workspace repository and a `source:` inside it is refused ([plugins 1_scope.md §3](../plugins/1_scope.md)). Namespace `eval`; tools are `eval.<verb>`, CLI `orbit eval <verb>`. Installed once per host with `orbit plugin add`, pinned per workspace in `.orbit/plugins.yaml`.

**Backend.** `exec`, Python, one process per call. A plugin tool call is capped at 300 seconds (`PLUGIN_TIMEOUT_CEILING_MS`, `crates/orbit-tools/src/plugin/backend/model.rs:4`), so no tool builds or tests anything itself. Long work (the implementer, validation, held-out tests) runs as job steps; tools only file tasks, capture artifacts and compute readouts. The held-out test step is a [script step](../script-steps/1_scope.md) or the shipped validation activity with the entry's command; task 3 of §13 picks one.

**Which workspaces enable it.**

- The evaluation workspace: all tools, the replay job and its routine.
- A live workspace that wants bake-offs (first `orbit`): the `bakeoff-*` tools and the read-only set and readout tools. Replay is not enabled there.
- Nothing on the Mac.

**Contributions.**

| Kind | Name | Purpose |
|---|---|---|
| Job | `eval_replay_candidate` | Delivery job for one replay candidate (§5.1) |
| Activity | `eval_bakeoff_judge` | Read-only advisory ranking (§8.5) |
| Routine (seeded off) | `eval-replay-advance` | Files the next wave, enforces the cap, closes finished replays |
| Routine (seeded off) | `eval-bakeoff-settle` | Completes T after its winner lands; reports overdue bake-offs |
| Panels | Replays, Bake-offs | Tables over the read-only tools |
| Config | `[plugins.eval]` | `evaluation_workspace`, `default_repeats`, `wave_size`, `max_usd_default`, `scoring_timeout_seconds`, `reviewer_crew`, `decide_by_days` |

**Tools.** `ro` is `read_only`; `mut` is `mutating`, callable by an operator or a runner and never by an agent (no tool is `agent_callable`).

| Tool | Kind | Input | Output |
|---|---|---|---|
| `eval.set-build` | mut | `name`, `source_workspace`, `query?` {`type`, `landed_after`, `min_lines`, `max_lines`}, `task_ids?`, `quota`, `seed?`, `from_bakeoffs?` | `set` {`name`, `revision`, `digest`}, `admitted[]` {`task_id`, `base`, `reference`, `held_out_command`}, `refused[]` {`task_id`, `reason`} |
| `eval.set-show` | ro | `name`, `revision?` | the set file, without snapshots unless `full` |
| `eval.set-export` / `eval.set-import` | ro / mut | `name`, `revision?`, `path` | `path`, `digest` |
| `eval.replay-start` | mut | `set` (`<name>/<revision>`, matching `sets/<name>/<revision>/`; unlike `eval.set-show`, which takes `name` and optional `revision` separately), `arms[]` (crew or `<crew>@<effort>` per the Crew references section of [CONFIG.md](../../CONFIG.md)), `repeats`, `max_usd`, `signals[]`, `for_task?` | `replay_id`, `planned_runs`, `first_wave[]` (candidate task ids) |
| `eval.replay-advance` | mut | `replay_id` | `filed[]`, `state` (`running`, `capped`, `complete`), `spent_usd` |
| `eval.replay-status` | ro | `replay_id?` | per-arm progress, `spent_usd`, `state` |
| `eval.replay-readout` | ro | `replay_id`, `format?` | the readout (§7) |
| `eval.replay-cancel` | mut | `replay_id`, `reason` | candidate tasks withdrawn |
| `eval.candidate-capture` | mut (job step) | `task_id`, `workspace_path` | `commit`, `ref`, `artifacts[]` |
| `eval.candidate-score` | mut (job step) | `task_id`, `signal`, `result_path` | the stored score row |
| `eval.candidate-show` | ro | `task_id` | arm, scores, diff artifact path |
| `eval.bakeoff-start` | mut | `task_id`, `arms[]` (2 to 4), `decide_by?`, `judge?`, `confirm` | `candidates[]` {`task_id`, `arm`}, `expected_usd` |
| `eval.bakeoff-status` | ro | `task_id` | candidates with status, PR, checks, cost, judge ranking, `overdue` |
| `eval.bakeoff-pick` | mut | `task_id`, `winner`, `reason` | `winner` {`task_id`, `pr`}, `rejected[]`, `next` (how to land) |
| `eval.bakeoff-cancel` | mut | `task_id`, `reason` | candidates rejected, `no-drain` removed from T |

Schemas are JSON Schema files in the plugin, referenced from the manifest; outputs are validated by Orbit before the caller sees them.

**Authority requested** (granted by the operator at enable, per workspace):

- `orbit_tools`: `orbit.task.show`, `orbit.task.list`, `orbit.task.add`, `orbit.task.update`, `orbit.task.artifact`, `orbit.search`, `orbit.agents.scorecard`. The plugin needs no run-dispatch tool: it files tasks and the drain runs them. It cannot land, ship or start a drain, and does not ask to.
- `fs`: read on the workspace repository; write on its own state directory and on the worktree it is handed in a job step (to copy held-out test files).
- `requires.programs`: `git`, `sqlite3`.
- No `network`, no secrets, no `unsandboxed`, no `agent_tools`.

Two reads need a decision in task 2 of §13 because they are not exposed as tools today: the dispatch-time task scope blob from the audit store, and `landed_diff_stats`. Options are a narrow read-only core tool for each (generic: "task scope as dispatched", "landed diff of a task") or a read-only `fs` grant on the stores. The tools are the better form; the grant exposes far more than the plugin needs.

---

## 12. Concerns & Honest Limitations

- **Nothing here has been run.** The base-pin workaround, a plugin delivery job that stops after commit, review-gate steps inside a plugin job, PR closure on rejection and plugin delivery on a pull follower were read from code and docs, not exercised. Task 1 exists to find what is wrong before anything is built on it.
- **Replay measures today's harness.** Skills and activity prompts come from the installed binary. Results are comparable only within one Orbit version.
- **Leakage is detected, not prevented, at first** (§4): git history past the base, the public repository, and training data. A contaminated run is excluded only if the scan recognises it.
- **Held-out tests reward the reference's shape.** A correct fix with a different internal design can fail to compile against them (`build_error`), and a candidate can pass them while breaking something they do not cover. This is why the reference is called a reference and why build, lint and review follow.
- **The sets are biased.** Bug tasks with tests, small diffs, recent weeks, one repository. Results do not transfer to feature work or to other repositories.
- **25 tasks is a blunt instrument.** It separates crews that differ by roughly 15 points or more. Reading smaller gaps into it is the main way this tool could mislead.
- **The blind reviewer is not blind to the crew,** and its verdict is uncalibrated until bake-off picks exist.
- **The snapshot drops any plan and any comment written before dispatch.** For tasks where a pilot plan or an operator comment carried real direction, the candidate gets less than the original implementer did.
- **Replay does not yield to delivery.** It is limited by window, concurrency, the resource throttle and the build budget, not by priority (§9).
- **The spend cap is the plugin's, not the host's.** It is checked between waves, so a wave in flight finishes; a bug in the plugin is a bug in the cap.
- **A bake-off holds its files until someone picks.** The deadline only reports.
- **The evaluation workspace's config can drift from the live one,** which silently changes what replay measures. The readout records the validation commands and reviewer crew it ran with; it does not check them against the live workspace.

---

## 13. Build order

Not filed. Each line is one implementation task: outcome, then what it depends on. Tasks 1 to 5 deliver a first replay; 6 to 8 deliver bake-off; the rest follow evidence.

1. **Spike: one replay candidate end to end, by hand.** In a scratch workspace, a hand-written job runs one closed bug task's snapshot at its merge parent with an explicit crew and captures the diff, using the local-branch base workaround. Outcome: a written list of what failed, which settles whether core addition 1 is needed. Depends on: Q1.
2. **Core: `base_sha` on `worktree_setup`, and read tools for the dispatched task scope and a task's landed diff.** Outcome: a job can start a worktree at a commit; a plugin can read both records without a store grant. Depends on: 1 (drop the first half if the workaround holds).
3. **Plugin skeleton and eval sets.** Repository, manifest, `set-build` with the fail-then-pass admission, `set-show`, export and import. Outcome: a frozen 25-task set for `orbit`. Depends on: 2.
4. **Replay job with held-out tests and free metrics.** `eval_replay_candidate`, `candidate-capture`, `candidate-score`, `replay-start`, `replay-advance` with the spend cap, `replay-status`. Outcome: a replay of the set over two crews completes under a cap. Depends on: 3, and the evaluation workspace existing (Q1, Q2).
5. **Readout.** `replay-readout` with paired intervals, contamination scan, CLI text and JSON, artifact attach, dashboard panel. Outcome: the table in §7 from real runs. Depends on: 4.
6. **Core: `alternative_to` relation with lock sharing and scorecard exclusion.** Outcome: sibling candidates run in parallel and unpicked ones do not count against a crew. Depends on: Q3.
7. **Bake-off tools.** `bakeoff-start`, `bakeoff-status`, `bakeoff-pick`, `bakeoff-cancel`, the settle routine, PR and branch cleanup confirmed. Outcome: one real task delivered through a three-arm bake-off. Depends on: 6, 3 (plugin skeleton).
8. **Picks into sets, and the advisory judge.** `set-build --from-bakeoffs`, `eval_bakeoff_judge`, judge agreement rate. Outcome: bake-offs appear as eval entries and the judge has a measured agreement with picks. Depends on: 7, 5.
9. **Build and lint signal, then blind review signal.** Outcome: both appear in the readout, the review verdict flagged uncalibrated until task 8's agreement rate exists. Depends on: 5.
10. **Leak prevention.** Per-run clone limited to ancestors of the base and a network-denied profile for replay candidates. Outcome: contamination becomes impossible, not merely detected. Depends on: 5, and only if the measured contamination rate justifies it.
11. **Prompt and skill arms.** Replay arms accept an experiment-style overlay. Outcome: a prompt change can be replayed before it is tried live. Depends on: 5, Q6.

---

## 14. Open questions for Daniel

**Decided (Daniel, 2026-11-14): the recommendation on every question, Q1 to Q8.** Two answers still need a value from him before the task that uses them: Q2 holds only if `dk-server-1` has idle capacity between pull windows, and Q7 needs the spend cap for the pilot. The questions and options stay below as the record of what was chosen.

**Q1. Where does replay run?**
- (a) A separate evaluation workspace over a second clone (§2).
- (b) The live `orbit` workspace, candidates at `low` priority, with a core lock exemption and analytics exclusions.
- **Recommendation: (a).** It needs no core change to isolate replay, and a mistake in the plugin cannot touch delivery records. The price is a second clone and a config to keep in step.

**Q2. Which box owns the evaluation workspace?**
- (a) `dk-server-2`, beside the workspaces it already owns.
- (b) `dk-server-1`, which today owns nothing and follows.
- **Recommendation: (b), if its capacity is otherwise idle between pull windows**, because replay then competes with delivery only when that box is also pulling. This changes the rule that `dk-server-1` owns nothing, so it is Daniel's call. Plugin delivery jobs on a pull follower are untested, so sharing replay across both boxes is not assumed.

**Q3. Is the `alternative_to` core addition acceptable for bake-off?**
- (a) Yes: add the relation, lock sharing and the scorecard exclusion (§1.4).
- (b) No core change: candidates run one at a time, each rejected or parked before the next starts, and the person compares afterwards.
- (c) Give candidates disjoint or empty selectors.
- **Recommendation: (a).** (b) turns a bake-off into n serial deliveries with stale earlier candidates. (c) gives up the lock that keeps other work off the files.

**Q4. May a judge ever pick the bake-off winner?**
- (a) Never; the judge only ranks.
- (b) Yes, once its measured agreement with human picks passes a threshold over a minimum number of bake-offs, and only for tiers Daniel names.
- **Recommendation: (a) now, revisit with data.** There is no agreement data yet, and landing stays an operator action either way.

**Q5. Is a window and a small concurrency enough to keep replay below delivery?**
- (a) Yes for now: run the evaluation drain at night at concurrency 2, and measure.
- (b) Build a generic drain option first that admits only while the host's other drains have free slots.
- **Recommendation: (a).** Build (b) only if the first replays measurably delay delivery.

**Q6. Should the first version compare prompts as well as crews?**
- (a) Crews only; prompt and skill arms later, reusing the experiment overlay (task 11).
- (b) Both from the start.
- **Recommendation: (a).** The original idea names "models or prompts", but a prompt arm needs the overlay capture to work at an old base commit, which is a separate piece of work, and crews alone answer the model question.

**Q7. What spend is acceptable for the first full replay?**
- (a) A one-repeat pilot over the set for two crews, capped at a figure Daniel names, then decide.
- (b) The full 25 × 3 × n replay straight away.
- **Recommendation: (a).** Nobody has measured the cost of a replay run yet; the pilot produces that number.

**Q8. How long are candidate diffs and kept refs retained?**
- (a) Indefinitely, as task artifacts and local refs.
- (b) Until the replay is closed plus a fixed period (say 90 days), then refs are pruned and only scores and diffs remain.
- **Recommendation: (b).** Diffs are small and stay as artifacts; refs pin objects in the clone forever.

## Task References

- [ORB-24839] — wrote this design; it changes no code or configuration and files no implementation task.
- [ORB-20006] — implemented the since-removed delivery-quality grader and, with it, the dispatch-time task scope blob in the run audit, the snapshot source in §3.3.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
