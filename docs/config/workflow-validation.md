# Workflow validation

## `[workflow.validation_env]` — the toolchain required validation runs with

```toml
[workflow.validation_env]
path = []
```

| Key | Default | What it does |
|---|---|---|
| `workflow.validation_env.path` | `[]` | PATH entries to put ahead of the resolved PATH. A leading `~/` expands to `HOME`. Agent sessions get them too ([below](#agent-sessions-share-the-configured-path)). |
| `workflow.validation_env.build_output_env` | `[]` | Output environment variable names to bind to managed host output for required validation and host evidence; see [build output](#workspace-build-output). |

Required validation commands and `local_shell` steps run in this environment over the [allowlisted agent environment](execution.md#executionenv--the-agent-subprocess-environment), so a drain started from a minimal PATH (a service manager, `env -i`, SSH) still finds the toolchain.

Orbit also sets `PYTHONDONTWRITEBYTECODE=1` for required validation commands and `local_shell` steps, commands launched through `proc.spawn`, and provider agent sessions in every lane (implementer, before-PR reviewer and before-landing reviewer). On each route, Orbit supplies `1` when the value is unset or empty and preserves an explicit nonempty value. Python then avoids writing `.pyc` files and `__pycache__` directories into delivery worktrees.

Required validation commands and provider agent sessions also default
`PYTHONPYCACHEPREFIX` to invocation-owned temporary scratch. This keeps
automatic caches out of the checkout when a command enables bytecode writes.
Orbit preserves a nonempty prefix admitted by the environment policy and
cleans up its own scratch after the invocation. Files written into the checkout
follow the usual Git candidate and clean-worktree rules.

**Probe.** The shell is the account's user-database shell, else `$SHELL`, else `/bin/sh`. Orbit always runs `<shell> -i -l -c …` (login profiles plus interactive rc files: `~/.zshrc`, or `~/.bashrc` when the bash login profile sources it), falling back to `<shell> -l -c …` on startup failure, nonzero exit, timeout or a missing environment marker. Stdin is null; output before the marker and stderr noise on success (bash's job-control warning) are ignored. Each attempt is limited to 10 seconds; the whole outcome, fallback included, is cached two minutes. Only `PATH` and the toolchain allowlist cross: `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `GOBIN`, `JAVA_HOME`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `PNPM_HOME`, `BUN_INSTALL`, `HOMEBREW_*`.

**Recorded.** Each validation log and step output records `validation_env`: the PATH; its `source` (`config` when `path` has entries, else `login_shell` when the probe returned a PATH, else `launcher_fallback`); the shell and any probe error; `probe_mode` (`interactive_login` or `login`, else null); and `fallback_reason` (why interactive startup failed when login-only succeeded, else null). When both probes fail, `login_shell_error` holds both.

**Diagnostics.** `orbit doctor`'s `validation-env` row shows the probe mode, fallback, PATH and the `python3`, `git` and `make` it finds, and warns (advisory) on a fallback or an earlier PATH entry shadowing a different executable (`/usr/bin/python3` before `/opt/homebrew/bin/python3`); duplicates and symlink aliases count once ([health checks](../runbooks/health-checks.md#run-orbit-doctor)). While required commands are configured, `orbit run auto`, `orbit run ship`, and MCP `orbit.workflow` action `auto` (`status`, `start`) and `orbit.workflow` action `ship` warn when the shell cannot be probed.

Readiness snapshots (CLI, dashboard and MCP drain status) reuse the advisory `validation_env_warning` across processes for two minutes in the local checkout’s `.orbit/state/validation-env-warning.json`. The key includes the launcher environment, shell, validation policy and Orbit version; environment or policy changes invalidate it immediately. Profile edits become visible as the process probe cache and diagnostic cache expire. Missing, invalid or unwritable cache files fall back to normal resolution. This cache holds only the warning, never the environment used to run validation or submit a drain.

### Workspace build output

`workflow.validation_env.build_output_env` maps each listed variable to one managed,
absolute host output directory. Commands must honor it, directly or through their
build-tool configuration. There is no toolchain-specific default. An empty list
allocates nothing and leaves inherited variables unchanged; command output is then
owned and cleaned according to the command's own configuration, not this contract.

For this Rust repository, merge [the repository configuration example](../../config/orbit-workspace.toml)
into the owner's ignored `.orbit/config.toml` during the separate operator rollout:

```toml
[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"]
```

A non-Rust workspace can select its own locator and consume it in a required command:

```toml
[workflow]
required_validation_commands = ['sh scripts/validate.sh']
[workflow.validation_env]
build_output_env = ["DIST_DIR"]
```

Create the example validation script in that workspace to invoke `cmake -S . -B "$DIST_DIR"` and
`cmake --build "$DIST_DIR"`. Orbit supplies no Cargo variable to that workspace.
Multiple listed variables refer to the same directory; commands can use subdirectories
below it to separate outputs. Arbitrary placement outside managed host state is not
supported by this contract.

**Precedence.** Settings resolve built-in `[]`, then global, then workspace. An explicit
workspace list replaces the global list, including `[]`. Names are trimmed,
deduplicated and sorted, and must be shell environment identifiers. Execution identity
variables (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `PWD`,
`OLDPWD`) and `ORBIT_` variables are reserved, case-insensitively for portability. After allowlist and login/PATH resolution,
configured bindings replace inherited values, even non-empty ones. They are applied
only to host required-validation/replay/evidence commands, including baseline and
recovery/reconciliation, never to agents or ordinary `local_shell` steps. A command
that overrides the binding or ignores it gives up output isolation; configure such a
command to consume the locator before admitting it as required validation.

**Placement and provenance.** On every platform, configured output is
`<primary-repository>/.orbit/state/host-validation-targets/<sha256(canonical-source-checkout)>`.
The primary repository is derived from the Git common directory; redirects in managed
output path components are refused. This host execution context stays outside the
worker's writable build areas. Different canonical source checkouts cannot share
compiled paths; repeated host checks for the same source may reuse output. Records
carry `build_output.env`, `directory`, `source` and `context: "host"`; an unconfigured
run has empty `env` and null directory/source. Baseline cache schema 9 includes the
bindings in its key, retaining prior records rather than certifying this contract with
old results. Recovery fingerprints also include the bindings.

**Cleanup and transition.** The existing host-validation-targets layout, source keys,
run attribution, base-cache locks and scratch lease lineage stay unchanged. The
[descriptor-bound collector](../runbooks/worktree-reclaim.md#host-validation-output) runs
on native Linux and macOS and continues to reclaim eligible historical and new output
after its checkout disappears; active, leased and ambiguous output stays. Host output on
other platforms remains isolated but this collector is not available there. Retain it
for operator maintenance; do not infer deletion authority from its hash. No old output, failed-run logs or audit evidence
is migrated or deleted by config resolution. Before deploying this runtime to a Rust
workspace, install the explicit binding in its owner configuration, preserving existing
settings; restart/upgrade active clients only through the supported operator procedure.
Existing clients keep the same valid source-keyed output namespace. An unconfigured
new client no longer receives the old implicit Cargo policy. Live rollout and storage
verification are separate operator work.

### Incremental compilation

A host validation target is keyed by its checkout and is built once per candidate, so Cargo's incremental cache there is write-only. When a binding is configured, required validation, host replay, detached evidence, baseline reruns and recovery set `CARGO_INCREMENTAL=0` on the child, replacing any inherited value. The variable is applied where Orbit spawns the command, not by the repository Makefile. There is no opt-out: `workflow.validation_env.cargo_incremental` was removed [ORB-22808] and loads with a deprecation warning, ignored, until 2026-12-08. With no binding, Orbit does not set the variable. An incremental directory already in a target is left in place; later builds do not add to it. Baseline result identity is unchanged: the setting changes compiler writes, not command results.

### Agent sessions share the configured PATH

A reviewer that resolves a different `python3` or `make` than host validation reports checks red that the host then refutes. So every agent session — the implementer's and reviewer's provider CLI, and the commands it runs through `proc.spawn` — starts with `workflow.validation_env.path` ahead of its [allowlisted](execution.md#executionenv--the-agent-subprocess-environment) PATH. The entries are prepended, since the rest of PATH still has to find the provider CLI. Every other entry of the resolved required-validation PATH, including what the login-shell probe found, is appended when the session's PATH lacks it, so a reviewer finds each tool required validation finds without building a PATH wrapper by trial. Appending keeps the session's own order deciding which copy runs; toolchain locators such as `CARGO_HOME` still come only from the allowlist. Implementer and reviewer sessions share this one environment. Settlement does not rerun a disputed check in the reviewer's environment: host validation stays the reference.

Codex runs each tool command through the user's shell as a login shell (`zsh -lc`) by default, and the profiles it rereads can reorder PATH: on macOS `/etc/zprofile` runs `path_helper`, which moves inherited entries behind `/etc/paths`, so `/usr/bin/python3` (3.9) would win over Homebrew's. While `path` has an entry, Orbit starts Codex with `--config allow_login_shell=false`. Its commands then run in a non-login shell (`zsh -c`, which still reads `~/.zshenv`) with the composed PATH, and Codex refuses a model request for a login shell. No `~/.zprofile` change is needed. In exchange, Codex commands no longer see variables only `~/.zprofile` or `~/.zlogin` sets; their PATH entries still arrive through the appended validation PATH. List a toolchain directory in `path` when its copy must win over the launcher's; after the session's PATH Orbit appends `~/.local/bin`, `~/.orbit/bin`, `~/.cargo/bin`, `~/bin`, `/opt/homebrew/bin` and `/usr/local/bin` when absent. With `path` empty, Codex keeps its login shell. Claude Code runs commands in a non-login shell and keeps the composed PATH. Another provider whose shell tool starts a login shell rereads profiles; check it with `zsh -l -c 'command -v python3'` against `orbit doctor`'s `validation-env` row, which warns when an agent session would resolve `python3`, `git`, `make` or `cargo` to a different executable than required validation.

### Required validation

`task_pr_pipeline` and `task_local_pipeline` run `workflow.required_validation_commands` on the exact candidate before push or merge and attach each command's log to the task.

- Commands run sequentially in list order, and the first failing command fails the step, so put cheap gates before long test suites ([rollout](../development/validation-and-ci.md#required-validation-commands)).
- These pipelines and the claimed leaves set `final_validation` on their implementer's input, and its envelope lists the commands this validation will run as `task.final_validation_commands`: this host's list, or the owner's list a claim captured. For a diff the implementer leaves those to the pipeline, but still runs before handoff each pre-handoff check the workspace guide requires (such as `make ci-guards`) even when the list names it, plus focused checks and every workspace check the list omits. A pre-handoff run does not count as pipeline final validation. The implementer records the two phases in its task execution summary: each pre-handoff command's actual outcome under the pre-handoff phase, and each listed command as `not_run` under the pipeline final-validation phase ([contributor guide](../development/validation-and-ci.md#required-validation-commands)). The logs this validation attaches to the task remain the record of committed-candidate validation. A clean no-op or already-landed handoff skips this validation, so its implementer still runs every required command. The field is absent for an empty list, for a claim whose admission captured no owner gates (it validates against this replica's copy, which the owner may refuse), and outside these pipelines, such as a manual or direct run; the implementer then runs every check itself.
- The PR pipelines also pass that handoff to conflict recovery at `sync_base`.
  The executor proves a later resolved deterministic validator with the same
  task, run and worktree bindings and the synchronized base. Its guard must
  be unconditional or shared with the rebase, and its checkpoint must not
  skip execution. A copied flag or activity name without that continuation,
  an unknown compound continuation, or nested recovery keeps advisory checks.
  Claimed recovery also requires this run's trusted worker binding and the
  failed rebase's rendered task/run identity to agree with it.
  Recovery keeps focused resolution checks and commands outside the captured
  list. The host validates after the final rebase continuation and base chase;
  if the base absorbs the candidate, `git_rebase` runs and attaches those
  commands before no-diff settlement instead. Standalone and later completion
  recovery keep their advisory validation. A new head invalidates old evidence
  normally; this handoff adds no validation cache. A repair that changed
  only unlocked paths may run a narrower list instead
  ([revalidating an unlocked-path repair](#revalidating-an-unlocked-path-repair)).
- A failure goes to step recovery, except a [missing tool](#missing-tools-block-the-task) or a failure the base shares ([held](#a-red-base-is-held-not-blocked) until the command passes on a new base tip). [Network-inconclusive](#network-flakes-are-rerun) failures are rerun first.
- Distributed handoffs must carry exact-candidate validation matching the owner's list. A before-PR claim also freezes the list in the owner's review contract at admission; a later owner change refuses the handoff rather than replacing the snapshot.
- A `no-diff-expected` task with a clean tree runs none of these commands, as the owner's pipelines skip `validate` for the tag. A claimed one hands off a `NoDiff` skip with no validation logs; the owner accepts it only while its own copy of the task keeps the tag. A clean tree that proves itself with `no-diff.json` or `already-landed.json`, and a tagged task that produces a diff, still validate.
- Re-running a task whose failed run preserved a candidate from `commit` or later runs the commands on that candidate over the new base, to decide whether implementation runs at all. A candidate preserved from implementation or earlier always returns to the implementer ([re-running a task](../../crates/orbit-core/assets/skills/orbit-orchestrate/references/workflows-preserved-candidate.md#re-running-a-task-with-a-preserved-candidate)).
- An empty list runs nothing, including on claimed handoffs; the other handoff guards still apply.
- Required validation, host replay, detached evidence, baseline reruns and recovery use the workspace's [build output bindings](#workspace-build-output). The runtime allocates no build output by default. Configured bindings isolate host output from worker builds, and logs record `validation_env.build_output`.
- An implementer whose affected-test gate passed with only deferral notices of sandboxes its lane refuses hands that gate off instead of failing, by naming the validation summary the gate wrote; Orbit derives the `deferred_sandbox_validation` record from it and runs each notice kind's probe in the implementer's lane itself [ORB-15516]: `DEFERRED: bubblewrap unavailable:` notices on Linux [ORB-15287], `SKIP: sandbox-exec cannot apply a profile:` notices on macOS, as a Mac claimed implementer's nested Seatbelt prints [ORB-15409], and `DEFERRED: ps denied:` notices from a claimed implementer on macOS, whose sandbox refuses to exec `ps` [ORB-15587]. One record may defer several kinds, such as a Mac claimed leaf's Seatbelt and ps-denied notices together, when every kind's probe refuses [ORB-15692]. The gate must be a required command or a `review.baseline_commands` entry. A required `orbit plugin test` is such a gate inside a macOS worker: given `ORBIT_VALIDATION_SUMMARY`, `ORBIT_VALIDATION_GATE` and `ORBIT_VALIDATION_BASE`, it defers each case with the Seatbelt notice instead of starting the backend, and without them it fails `[validation_route_unavailable]`, which recovery treats as a validation-environment failure ([policy-sandbox design](../design/policy-sandbox/2_design.md#the-plugin-backend-boundary)) [ORB-19224]. Validation runs it after the required commands, outside the agent sandbox (a claim's `claim_validate` on its claim host, before publication), on this run and on a resumed candidate: a pass that still prints any of these notices holds the candidate as a `validation_environment` failure, and a run that reports no executed tests is refused ([development guide](../development/testing.md#test-process-environment)).
- An implementer may hand off a criterion the task pilot typed `post_deploy` (evidence only the deployed build on a named host can produce) as `owed_criteria`, naming the host, measurement command and target, instead of failing. The implement step admits each against the pilot's finding and refuses any other owed criterion; a claimed handoff carries the list, and the owner admits it again on acceptance. The delivery runs these commands, review and landing as usual, then settles in `review` even under `--complete` or `workflow.distributed_completion = "done"`, until an operator records every outstanding criterion met with a `post-deploy: met <N>` comment (a `post-deploy: failed <N>` measurement keeps it outstanding). The newest owed declaration is authoritative; missing or unreadable contents, or a non-automation writer, refuse completion, landing, post-deploy recording and task show rather than reuse an older declaration. Claimed landing reads persisted obligations, so a repair or replacement handoff that omits `owed_criteria` preserves any earlier owed or failed criterion. Owed or failed criteria also refuse ordinary task updates to `done`, approval, desktop review completion, automation and final recovery; every owed criterion must be recorded `met` before completion. The human `--force`/dashboard force override remains available and records a `forced` status event without changing the measurement record ([task-execution reference](../../crates/orbit-core/assets/skills/orbit/references/task-execution-post-deploy.md#criteria-owed-after-deploy)).

### Missing tools block the task

A command that fails for lack of a tool says nothing about the candidate. Orbit detects exit status 127, a shell `command not found` / `not found` line, `make`'s `Error 127`, `No such file or directory` for the program, a missing cargo subcommand, or a guardrail's "`<tool>` is required … install" line for a tool absent from PATH.

The step fails with the `[validation_environment]` marker and error code `validation_environment`, naming the tool, PATH and source. The diagnostic includes the full searched PATH, even when it exceeds the bounds used for other diagnostic fields. The durable log preserves that same full PATH in its output and `validation_env.path`, and records `failure_kind: "environment"` and `missing_tool`. No step, final or blocked-task recovery runs, and no review, rework or recovery budget is spent. The handoff pushes nothing, opens no `[BLOCKED]` PR, and blocks the task under `validation_environment_blocked` with the candidate kept in its worktree. Fix the environment, check `orbit doctor`, then `orbit job resume <run>`. A claimed leaf skips repair the same way: its claim settles as a failure carrying the diagnostic and is not released for another follower to implement again.

### Supervisor stops are inconclusive

Output-capture stops, incoming signal interruptions and children terminated
without an exit code are validation-environment failures. The shared runner
uses structured metadata from the process supervisor for candidate and claim
validation, base comparisons and hold refresh, review checks and resumed
candidates. A stopped base is inconclusive and is never cached. Printing a
supervisor diagnostic on stdout or stderr cannot establish a stop: a normal
nonzero exit still gets compared with the synchronized base. The failure
message still names the first failure the captured output shows (a test
panic and its message, or else the first compiler error), so a stop caused
by a flood of failures says what they were.

### Network flakes are rerun

Network-inconclusive output (HTTP status `000`; a curl connect, resolve, timeout, TLS or receive error such as `curl: (6)`, `(7)`, `(28)`, `(35)`, `(56)`; a DNS resolution failure; a TLS handshake timeout) reruns the command up to twice, after 2 s and 6 s. Only the last attempt counts; the log records `network_retries`.

### Delivery refusals are held

A fail-closed delivery decision ends the run `held` with a typed diagnostic code.
An `--auto-fix` drain pass (or dashboard **Fix PR**) hands a `review` task held
on `delivery_evidence_stale`, `delivery_requirements_unsatisfied`,
`delivery_base_checks_pending`, `delivery_completion_budget_expired`,
`delivery_check_evidence_inconclusive`,
`delivery_forge_observation_unavailable` or, once the published head, or a head
produced by an Orbit-audited clean base refresh of it, reports a required check run
or the forge positively confirms zero required status checks on the base branch,
`delivery_check_runs_absent` back to its completion step,
within a backoff and ceiling (see [PR auto-fix repairs](crew-recovery.md#pr-auto-fix-repairs)):

| Code | Decision |
| --- | --- |
| `delivery_head_changed` | The PR reports a head different from the one this run published. |
| `delivery_evidence_stale` | Pinned delivery identity is missing or cannot be confirmed, or merge evidence is contradictory. An owner landing of an already-landed or no-diff delivery also holds here when the task's scope changed after the evidence was captured. |
| `delivery_repair_dispatched` | A repair was already admitted for the blocked head. This includes a duplicate `--auto-fix` admission for that head that observes a repair an earlier run already admitted while that run may still be live, an in-budget check repair whose `step_failure_recovery` ran but returned an unsuccessful outcome, or whose decision admits no retry (a clean no-edit result is the exception below), and a no-edit repair whose one unchanged-head rerun is already spent in CI history (no rerun or refresh is sent). A clean no-edit repair is not this hold: its failed checks rerun once on the unchanged head first. A scope or ledger refusal, where the host refuses a repair’s edits, commit or no-edit rerun record before the retry, fails the step with that refusal and is not this hold either. A check repair the job cannot route because it has no `step_failure_recovery` is not this code: that initial hold is `delivery_check_repair_activity_absent`, and no recovery ran. When that no-edit rerun ran and failed again, completion first refreshes the head to its base tip once when the failure is base-inherited (see [PR auto-fix repairs](crew-recovery.md#pr-auto-fix-repairs)). A refresh that applied is pending automatic recovery, not a hold; a `held` run names the refused or unattempted refresh in its reason. An `--auto-fix` drain pass (or dashboard **Fix PR**) hands back an in-budget unrepaired check-repair hold (`pr_auto_fix_check_repair:` without `auto_fix_stopped:` or a post-recovery `pr_complete` wrapper) through the repair ledger for its remaining attempts while a blocker remains (reporting `budget_exhausted` or policy skips with remaining budget once spent or held), or back to completion once its published head reports green required checks. If a repair is running or a refresh is pending, let it finish; a new head would be refused as `delivery_head_changed`. Once the prior run ends, inspect the hold reason and any recovery attempt. If the no-edit repair’s unchanged-head rerun is spent and checks still fail, repair and publish a new head before starting a new delivery attempt. If check recovery was unsuccessful and the no-edit rerun is not spent, configure `step_failure_recovery` if missing; a new authorized delivery run can use remaining per-PR repair budget on the same head, or you can repair and publish a new head. |
| `delivery_pr_draft` | The PR is still a draft. |
| `delivery_pr_closed` | The PR closed without merging. |
| `delivery_task_not_promotable` | The task's current status refuses promotion. |
| `delivery_landing_conflict` | Owner landing requires a conflict repair with fresh validation and a new handoff. |
| `delivery_completion_conflict` | Completion found the pull request's branch in merge conflict and no authorized repair is admitted (the run has no recovery checkpoint, or its one conflict refresh already ran). The task stays in review. Repair the conflict or rebase the branch onto its base, then start a new delivery attempt. |
| `delivery_merge_in_progress` | GitHub already has a merge in flight during owner landing. Landing took its allowed read-only status observations and did not see the pinned head merged. The run holds and does not send another merge. Check the pull request on the forge. If it has merged, retry landing so that merged evidence can complete the handoff. Do not send another merge while the forge still reports one in progress. This hold is not handed back to completion. |
| `delivery_landing_repair_requeue` | A required check was classified as caused by the candidate. The same hold covers a spent section-gap draft and a spent unreadable retry classified as the candidate's. The landing run is held. The claim ends and the task returns to the backlog with the candidate kept, or it is blocked once the landing repair budget is spent. The next run repairs that candidate. This hold is not handed back and the held run is not resumed. |
| `delivery_requirements_unsatisfied` | Required reviews or checks are unsatisfied, without a failed check. |
| `delivery_base_checks_pending` | Failed checks are inherited from, or suspected to affect, the base; wait for the base's own result before candidate recovery. |
| `delivery_completion_budget_expired` | The completion budget (deadline) expired before completion could be confirmed; the last observed pull request state was unmerged and the task stays in review. The pull request may have merged since, through auto-merge (a hold does not cancel it) or another actor, so check the forge. |
| `delivery_check_evidence_inconclusive` | An auto-fix stop whose check evidence could not be read conclusively; no repair agent runs. In completion's primary observation, after a merge or auto-merge mutation in this invocation, or recorded merge, auto-merge or unclassified completion-mutation provenance in task history, a transient required-checks read failure is held as `delivery_check_evidence_inconclusive`; the next auto-fix pass rereads PR status and reconciles any earlier mutation. After a completion rebase without a merge request, primary PR status, required-checks and merge-capability transport failures instead hold as `delivery_forge_observation_unavailable`. The auto-fix base-tip comparison and repair-admission recheck of required checks are a separate route: a required-checks or log read there that persists through the provider's bounded retries holds as this code with or without mutation provenance; no in-run observation retry follows, and the next auto-fix pass hands it back to completion, which rereads PR status. The required-checks read itself retries an empty, truncated or `unexpected end of JSON input` response a bounded number of times (never past the completion deadline) before holding. A failed job's log read that the forge throttled for the authenticated account first waits for the forge's reset (or a bounded backoff when none is reported), at most three times per job and never past the completion deadline; each wait is recorded in the task history as `pr_auto_fix_read_deferred`, and a resumed completion honors it. A prerequisite evidence read the forge throttled (the failed prerequisite job's log, or the pinned workflow file) waits for the reset the same way, at most three times per read pass and never past the completion deadline, without a history record; when the reset falls past the deadline, the stop holds as `delivery_check_evidence_inconclusive` with the forge's diagnostic and no rerun is sent. The owner's landing also holds this way when a failed required check stays unclassified, its evidence is inconclusive, or a transient failure has already used its one rerun (the "failed again after its one rerun" stay): the candidate stays in review and the run is held. |
| `delivery_forge_observation_unavailable` | Completion's primary read-only forge observation (PR status, required checks, merge policy, prerequisite evidence; not the auto-fix base-tip and repair-admission rechecks, which hold as `delivery_check_evidence_inconclusive`) kept failing with a transport error before any completion mutation: after the one authenticated completion retry, when the run has no final recovery, past the completion budget, or after repair provenance that rules the retry out. Read-only PR status, required-checks or merge-capability transport failures after a completion rebase without a merge request also hold this way. The same hold follows when final recovery successfully applies a current-policy requeue of the unchanged publication, or the host parks the requeue for a human instead. When final recovery requeued the task, it returned to `backlog`; when parked, it is `blocked` for a human; otherwise, completion has not sent a merge request and the task stays in `review`. In `review`, `orbit job resume` reads the forge again and continues completion from its checkpoints. For a requeued task in `backlog`, do not resume this held run; the next delivery attempt runs it (or completion repair readmits it). For a parked task in `blocked`, a requeue is refused while the run is held: its block decision offers no requeue and names `orbit job resume <run>`, which reads the forge again and readmits the task on the same candidate; discarding stays an explicit `--discard-candidate` choice. The owner's landing also holds this way when GitHub rate-limits its failed-check job or log reads past its bounded waits or its deadline (the reason names the reset), or when a pre-write transient forge-observation failure exhausts its three landing passes; pull-request holds record a typed `waiting_gate` (`job_logs_unavailable` or `forge_observation_unavailable`) so the owner sweep retries automatically within its retry budget, or an operator can use **Retry landing** / `orbit task retry-handoff` ([landing a handoff](../runbooks/distributed-drain.md#let-the-owner-land-follower-deliveries)). |
| `delivery_origin_unavailable` | `worktree_setup` could not reach origin during pre-admission base fetch while every task being admitted was still in backlog, including a fetch-lock wait whose bounded origin probe confirms a transport outage. The run is held without changing those tasks; a later admission retries after origin recovers. If any task is already in progress, the outage follows ordinary run failure handling (or claim settlement for a claimed leaf) instead of a hold with no generic resume path. Authentication, permission and missing-ref refusals remain failures. This applies to remote base sync; a `base_sync: local` run treats origin as best effort and cuts from its owner branch when origin cannot be fetched. |
| `delivery_dependency_not_delivered` | `worktree_setup` found a `done` dependency whose commits exist only on other refs, not in the base the run is cut from (typically a forced `done` before its pull request merged), while every task being admitted was still in backlog. The run is held without changing those tasks, and backlog selection withholds the task as `dependency_not_delivered` until the commits reach the base. A task already in progress keeps ordinary run failure handling, and `dependency_delivery: ignore` skips the check. |
| `delivery_check_repair_activity_absent` | An in-budget `--auto-fix` check repair was admitted, and the job has no `step_failure_recovery` to run it. The run holds, no repair agent runs, and the pull request stays open. The admission remains in the per-PR ledger and spends one repair attempt. This is the missing-route hold only: no recovery ran. A recovery that ran and left no admissible retry, a spent no-edit rerun, or a duplicate admission observing an earlier run's repair while that run may still be live holds as `delivery_repair_dispatched`, not this code. That row also says a clean no-edit rerun is not a hold and a scope refusal fails the step. Because the held run is terminal, configuring `step_failure_recovery` and starting a new authorized delivery run on the same head admits another repair attempt while the per-PR budget remains, or repair and publish a new head, then start a new delivery attempt. Exhausting the per-PR budget still fails the run. |
| `delivery_check_runs_absent` | The head this run pushed reported no workflow run at all, and no run of any required check, for five minutes. A workflow run the forge reports for that head, including one that is `queued`, `pending`, `waiting`, `requested` or `in_progress`, is checks still pending: completion keeps watching under its normal budget and does not re-request checks or hold. When the forge reports no workflow run, completion re-requests hosted checks once through the forge adapter, recording `pr_checks_retrigger_requested` with the pull request and head in the task history before the request; the GitHub adapter closes and reopens the pull request, which changes neither the head, the branch nor its reviews and leaves it open and ready, and asks nothing when a workflow run already exists for the head. Checks that start afterwards are watched as usual. When none has started five minutes after the request, the run holds: the task stays in review and no repair attempt is spent. One re-trigger is allowed per pull request head, so a later completion attempt on the same head holds without asking again. An auto-fix pass hands the hold back to completion once the published head, or a head produced by an Orbit-audited clean base refresh of it, reports a required check run or the forge positively confirms the base branch configures no required status checks (neither classic branch protection nor rulesets require checks); until then — including when required-check policy is missing or unreadable — the pass lists it under this code and spends no hand-back. If no check run appears, confirm the forge runs workflows for the repository, then start the required checks on that head by hand or push a new head. |
| `delivery_source_repair_decision` | `candidate_resume` refused a published candidate that needs an operator to inspect explicit source repair for the publishing run, or discard the candidate (`orbit task update <task-id> --status backlog --discard-candidate`) so the next run implements fresh. The held reasons are: no unique requeue binding, a publication that predates the current binding, a retained local branch that moved, a source that is not an exact owner-local single-task execution, and a source that is live, discarded, changed, or lacks an applied requeue. The refusal message is unchanged and names that next step. The task stays as the refusing action left it. Nothing requeues, discards, or resumes the run. Untrusted publication preservation, a missing operator reservation, a changed remote or pull-request identity, a missing local identity, and other authority refusals stay failures. |
| `delivery_stale_branch` | `worktree_setup` found an existing branch or registered checkout whose tip does not match the freshly resolved base. Setup leaves the retained branch and worktree untouched for operator inspection and does not reset or discard unexplained history. Before a `base_sync: local` run is cut, a clean owner checkout whose base is behind `origin/<base>` starts the run at the fetched fast-forward commit without changing the owner branch or checkout; a dirty or diverged owner checkout is refused here with this hold, and its message names the checkout and both commits. Local runs hold without blocking the task; claimed leaves release under an operator-inspection hold without suppressing the host or crew and settle as `delivery_hold` (`counts_as_failure: false`). |

**Stopped owner landing retries.** The owner clock sweep also consumes pending
landing requests. A stopped handoff with typed waiting-gate evidence resumes
when a fresh observation shows its base's required checks green, or its PR's
required and owner-owed checks satisfied and the PR mergeable. It uses the same
guarded reservation and fresh candidate/authority checks as `retry-handoff`.
There are at most three automatic retries per handoff, opening landing attempts
2–4 with minimum backoffs of 60, 120 and 240 seconds from the preceding durable
stop. Prior explicit retries count toward that ceiling; explicit retry remains
available after automatic retries are exhausted. Restarts, repeated ticks and changing gate
reasons do not reset that budget. The `landing_retry_reserved` history event
records the stop code, retry number and landing attempt. Live jobs, withdrawn
handoffs, candidate failures, conflicts and review refusals are left alone;
legacy stops without typed waiting evidence still need explicit retry. Dry-run
sweeps and shutdown admission holds start no landing work. Transient-failure
and unknown-mergeability policies can use this same reservation mechanism;
they do not by themselves grant waiting-gate eligibility.

`orbit.workflow` action `land` does not use that reservation. While GitHub reports
mergeability `UNKNOWN` it reads the pull request again, waiting a fixed 90
seconds: after 5, 10, 20 and 40 seconds, then once more with whatever of the
90 remains. It then refuses with the existing `pull request #N mergeability
is unknown` reason. A pull request reported conflicting is refused on the
first read. The bound is fixed, not a `config.toml` key: the delay is GitHub
recomputing mergeability after a push to the base, not an operator policy.
Deciding whether to rerun a failed check reads those checks without waiting
on mergeability. The owner landing step already treats an unknown merge state
as pending and polls inside its `max_wait_seconds` budget (default 3600
seconds).

These codes come from typed refusals, never from matching diagnostic wording.
The refusing action keeps its existing task transition and landing evidence;
run finalization does not change the task's status. Retry, step recovery, final
recovery and the failure handoff do not run for these decisions. Gate and auto
parents pass the held child, run history lists it as `held`, and reliability
counts it under held, outside the failure rate. For `delivery_origin_unavailable` and `delivery_dependency_not_delivered`,
backlog tasks remain there and a later normal admission retries worktree setup.
An already-in-progress task follows ordinary run failure handling, or claim
settlement for a claimed leaf; the held run itself has no generic resume path. Routine fires retain their
existing `succeeded` status for a held underlying run.

Missing pinned identity fields, including the PR head, and contradictory merge
evidence produce typed `delivery_evidence_stale` holds, as does an owner landing
of an already-landed or no-diff delivery whose task scope changed after its
evidence was captured: the stop is recorded and the candidate stays in review.
The same refusal on an implement-side commit remains a failure. A permanent failure to
read required-check evidence in auto-fix, including a credential or permission
refusal, holds as `delivery_check_evidence_inconclusive` before or after a
completion mutation; no repair agent runs and no merge is admitted on that
evidence. Permanent forge-read failures at other boundaries and unlisted
refusals remain failures. A candidate-caused required-check failure holds as
`delivery_landing_repair_requeue`: the claim ends, the task returns to the
backlog with the candidate kept (or blocks once the landing repair budget is
spent), and the held run is not resumed. Transport failures after a merge or
auto-merge request (in that run or recorded in the task's history)
also remain failures, while a read-only PR status or merge-capability
transport failure after a completion rebase without a merge request holds as
`delivery_forge_observation_unavailable` (still refusing the unchanged-publication
retry), and a narrowly classified synchronous HTTP
500/502/503/504 merge server error, or a merge response that ends before it
decodes (`unexpected end of JSON input` with an empty or truncated body),
permits one read-only reconciliation within the original deadline. Only
fresh `MERGED` evidence matching the published and reviewed candidate, PR,
repository, head branch and base branch permits completion; a failed
response grants no managed-merge attribution. An inconclusive read retains
the original failure and mutation provenance and
never sends another merge request. A transient auto-fix required-check read in completion's primary observation
holds as `delivery_check_evidence_inconclusive` when completion mutation
provenance is present in the invocation or task history. The auto-fix base-tip comparison and repair-admission recheck of required checks hold as that code on a read that persists through the provider's bounded retries, with or without that provenance; they get no in-run observation retry, and the next auto-fix pass hands the task back to completion. In particular,
`delivery_evidence_stale` text alone does not establish a hold. Completion's
ordinary conflict-recovery path also retains its existing failure and repair
semantics. Resolve the named refusal before starting a new delivery attempt;
apart from `delivery_forge_observation_unavailable` (when the task stays in review), these holds do not enable
generic `orbit job resume`.

### A red base is held, not blocked

The integration branch has no merge gates, so a command may already fail on the base. A command still failing after its reruns is rerun on the base commit in a clean detached worktree under `<git-common-dir>/orbit-baseline/`, cached per base commit, command and selection behind a file lock (concurrent candidates on one host share one base run). `validation/<run>/<n>.json` gets a `baseline` record; `validation/<run>/<n>.baseline.json` holds the base log. Candidate logs record `failures` and `baseline_reproduces`; baseline records and logs retain their parsed `failures`. The baseline record's `decision` describes the base run's outcome separately from whether it reproduces the candidate.

| Base run | Outcome |
|---|---|
| Comparable completed failure with the same exit status, and every identified candidate failure is a test that fails there too | Known red: the candidate proceeds (below). |
| Comparable completed failure with the same exit status and every candidate failure identity confirmed on the base, including compiler or lint errors | The task is held (below). |
| Neither run names identities, but complete normalized diagnostics and exit status match, with no timeout or coverage refusal | The task is held, with a diagnostic fingerprint as its failure identity. |
| Fails with different or additional candidate identities, unmatched diagnostics, identities missing on only one side, or a timeout | Matching failures are unconfirmed; candidate repair remains enabled, with both logs preserved. |
| Passes | Candidate failure; the error says the base passed, so step recovery can repair it. |
| Inconclusive (missing tool or network) | Candidate failure; nothing cached. |
| Not comparable (below) | Candidate failure; the `baseline` record's `decision` is `not_comparable`. |

**Known red.** The candidate introduced none of the base's failures when the candidate run did not time out, printed no host coverage refusal (sandbox-unavailable diagnostics, `DEFERRED:`, `SKIP:`, `skipping:` or `skipping ` notices), names at least one failure, and every failure it names is a failed test that also fails in the base run. Both runs' failures and the candidate's host coverage notices are read from the complete output, not the bounded log, using the identities a hold records (below). A declared deferred-sandbox replay must report executed tests before known-red continuation, including when it exits nonzero; a missing or zero executed-test count refuses the replay. The step does not fail: it records the command under `known_red` in its output (decision `passed_with_known_red`), marks the command's log `failure_kind: "known_red"` with the failures, appends a `known_red_validation` history event naming command, base commit, candidate commit and failing tests to each delivered task, and goes on to the next command. `pr_open` lists that run's known-red commands, for the published head and base, under `## Known-red validation` in a body it opens; one found by a later revalidation, after the PR exists, is only on the task and the run. The red base keeps its repair task through other holds. A compiler or lint error with an identity confirmed on the base can be held as below. Diagnostic fingerprints never qualify as known-red tests. Timeouts, unmatched diagnostics, or any candidate identity absent from the base leave the candidate requiring repair. Host coverage refusals still follow the validation-environment rules. A resumed candidate is judged the same way.

**Hold.** The step fails with the `[baseline_red]` marker and error code `baseline_red`, and the run ends `held`, not `failed`; the gate and auto parents waiting on it pass it as held. No step or final recovery runs and no budget is spent; the handoff publishes no branch and opens no `[BLOCKED]` PR. It carries the candidate to `refs/orbit/candidates/<task>/<run>` on `origin`; the task's `candidate_held` history entry records `durable_ref`, or `carry_failure` with the push error when the push was refused and the candidate stays on its host. The task moves to `backlog` under a `baseline_red_hold` history event naming base ref, base commit, command and what fails on the base (run finalization does this for a local pipeline), with the candidate kept in its worktree. The hold's `failures` record keeps up to 20 failure identities the base output names (each nextest `FAIL [..]` summary line as `test <binary-id> <test>`, a libtest `test <name> ... FAILED`, a compiler or lint error at its location) and counts the rest; when neither run names identities, equal diagnostic fingerprints establish reproduction instead. The fingerprint hashes the complete output after stripping ANSI codes, collapsing whitespace, replacing workspace/worktree roots and temporary paths, removing PATH lines and elapsed-time tokens, and sorting lines. Empty normalized output has no fingerprint. The hold records a readable SHA-256 fingerprint identity; legacy holds without identities retain up to 5 output lines instead. Older cached results without fingerprints remain readable but cannot establish identity-less reproduction from bounded logs. Admission withholds the task; `orbit run readiness --json` reports `reason: "baseline_red_hold"`, and its `detail`, like each hold verdict, names the failing tests. Operator steps: [stuck job runs](../runbooks/stuck-job-runs.md#a-task-held-for-a-red-base).

**Repair task.** The owner's clock tick files one repair task per red base failure: the first standing hold for a command and failure set files a `bug` in the workspace's `backlog` (priority `high`, complexity `medium`, tag `baseline-red-repair`, external ref `baseline-red:<digest>`) naming the failures, the first red base commit and the held task. Every later hold with the same command and failures, on another task or a later base, is added to that task as a `related_to` relation with a comment. Once it is done, rejected or archived, a hold still naming the same failures files a new one. A hold that records no failures, such as one written before holds kept them, files nothing. A hold whose base already has a passing result for its command lifts on that tick without filing or linking a repair. New holds retain a SHA-256 digest of the complete trimmed, sorted, deduplicated failure identities before limiting their display to 20 entries and a 240-byte prefix plus an ellipsis for long entries. Repair identity uses that digest, so input order, duplicate names and display truncation do not change which complete set is tracked. Older holds without a digest retain their original display-based repair key and still find existing open repairs; a new digest-bearing hold can therefore file a separate repair from an older hold for the same failures. Fingerprint holds key repairs on command plus fingerprint, so changing host paths or output tails cannot file another open repair for the same diagnostics. Legacy output-only failures continue to use the bounded output tail.

**Release.** The held command must pass on a new base tip. When a hold records runnable failing test identities and a selection, refresh first reruns only those tests: a comparable failure keeps the hold without running the full command. If those tests pass, or their replay is inconclusive or not comparable (including a command that ignores the test filter or a renamed test that executes nothing), refresh runs the full recorded selection before lifting. A hold without recorded tests, with a non-test or shortened identity, or without a selection runs the full command directly. Narrowed and full runs retain separate cache entries per base tip, command and selection; cached full results take precedence, and cached narrowed failures also suppress another refresh on the same tip. A failed or inconclusive full run keeps the hold. `orbit clock tick` never runs it (it holds the sweep lock): it reads the cached base result for the new tip, or dispatches one `baseline_hold_refresh_pipeline` run per workspace at a time to run it and record a `baseline_red_hold_verdict` event. An inconclusive attempt suppresses refreshes for that tip and command for 15 minutes; a new tip is checked at once, and a conclusive cached result overrides the attempt. Readiness, the backlog snapshot and pull admission only read verdicts, so a hold with none stays held. The next run resumes the preserved candidate.

**Self-selecting commands** (`make ci-test-affected`) report their selection and executed-test count through `ORBIT_VALIDATION_SUMMARY`, and the base run reruns the candidate's selection from `ORBIT_VALIDATION_SELECTION`; a base run with another selection, or a pass with no counted test, is not comparable ([validation summary](../development/validation-and-ci.md#validation-summary-and-base-reruns)). A hold records the selection and lifts only when that selection passes on a moved base with at least one test executed. A hold from before selections were recorded is judged by exit status, and a still-red base re-holds the next delivery with the selection. A command that writes no summary has no selection check, but reproduction still requires completed failures with the same exit status and either a nonempty candidate failure set covered by the base identities or equal diagnostic fingerprints when neither run names identities.

**Followers.** A claimed leaf validates before it pushes or opens a PR and judges a red base by the same rule against its own base run. A known-red command does not stop it: `claim_validate` reports it under `known_red` (decision `passed_with_known_red`), the leaf's `pr_open` lists it under `## Known-red validation`, and the command's claim validation log keeps the non-zero exit with a `known_red` report naming the base commit, the selection and the complete failing set. The owner does not take that report on the leaf's word. Before it accepts the handoff it judges each reported command it requires itself, on the handed-off base, which must be in the history of the base branch the owner captured when it admitted the claim (this membership rule applies to both same-OS and cross-OS settlements; a base off that branch is refused without running a validation command). For same-OS settlements, it uses a full result it has cached for that base, command and selection, else a replay of only the reported tests when the command reports a selection, else the whole command. When the follower was admitted on the same host OS as this owner, it accepts only when its own base run fails every reported test, the same way when the whole command ran, and then appends the `known_red_validation` history event to the task. A different host OS is a different validation environment: after checking that same base-branch membership, the owner does not run or consult its own base validation for that report, does not release the claim as `known_red_contradicted`, and accepts the follower's base evidence for the admitted OS, recording the same history event. The handoff is accepted, so a pull request the leaf opened stays this delivery rather than being left open while the task returns to the backlog. Owner settlement shares a ten-minute deadline across its base comparisons, below the fifteen-minute routed delivery budget, leaving room for observation, cleanup and the reply. Lock waits and base commands use the remaining time. An attempted base run that is cut or otherwise inconclusive is retained under the base, command and replay selection separately from conclusive base results: settlement retries reuse that refusal rather than starting another cold run. Partial failures from a cut never confirm known red, and ordinary candidate validation and reviewer base replays do not read these inconclusive claim records. A later conclusive base result may supersede the refusal. When the follower was admitted on this owner's OS, a report its own completed, comparable base run contradicts (the base passes the reported tests, fails without one of them, or fails the whole command with another exit status) can never be accepted, so the owner ends the claim instead of refusing it: it releases the task to the backlog as a `transient` failure whose reason, led by `known_red_contradicted`, names its verdict and counts against the task's release budget, and the follower closes its pending settlement obsolete. A report it cannot establish either way (the base run is inconclusive or not comparable, the report names another base, or the base is off the owner's base branch) refuses the settlement while the owner keeps the claim, and the leaf retries it; a non-zero log without a report is refused as before. A candidate failing a test its base lacks requires candidate repair; equal exit status alone never creates a `baseline_red_hold`. The candidate and base logs retain their complete parsed identity sets and the reproduction decision even when bounded output omits a failure. An owner running a release without this rule refuses a log carrying a report until it is upgraded.

**Candidate-only flakes.** A comparable passing base triggers one isolated candidate replay when the captured failure set contains at most three tests and the command reports a replayable selection. The affected-test gate runs only those tests serially, with retries disabled. A pass must report the exact selection and executed coverage without host refusals. Delivery then proceeds with `flaky_validation` history, captured original/base/replay evidence and a PR section naming the tests; each command/test signature is filed once as a workspace friction. Claimed handoffs carry the same pinned evidence, which the owner validates and records at acceptance. A repeated failure, an over-bound set, or missing comparable evidence keeps the normal failure handling. See [validation summaries](../development/validation-and-ci.md#validation-summary-and-base-reruns).

**Reviewer claims.** A before-PR reviewer can attach a `baseline` claim to a further failing check. Settlement runs on the host, outside the reviewer's sandbox, so it reruns only `workflow.required_validation_commands` and `review.baseline_commands` entries (never the reviewer's command text) on the final candidate and the pinned base, using the lists captured at admission. On the base it reads a full result already cached for that exact base, or else, when every failure the candidate run printed is a test and the command reports a selection, replays only those tests (`failing_tests`, [validation summary](../development/validation-and-ci.md#validation-summary-and-base-reruns)); other failures replay the whole selection. `review-baseline.json` records the path as `base_replay` (`cached_failure_set`, `failing_tests` or `full_selection`). Each claim's host runs get `review.minutes` of wall clock, the last quarter reserved for the base: the candidate rerun is stopped at three quarters (each run still at most 45 minutes), and a stopped run settles `incomplete` under `baseline_cut`, naming the cut and the failures it printed that the base shares. A confirmed claim with nothing else open holds the task as above, unless its candidate failures are known red by the rule above: then the review settles `accept` (`accept_with_fixes` over a reviewer repair), before the PR and before landing alike. The record stays `failed`; the certificate's `known_red` names the command, base, final candidate, run and tests, `review-baseline.json` records the check's `decision` as `known_red`, the step output carries `known_red`, and each task gets the same `known_red_validation` history event, which `pr_open` lists in the PR body. Outcomes for refuted or uncheckable claims: [review-gate design §4](../design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

### Agent-declared blockers stop the run

An implementer that cannot proceed returns `blocker: {kind, evidence}` (a short token and the reason) from `implement_one` or a resolved `agent_implement` target. The step fails with the `[task_blocked_by_agent]` marker and error code `task_blocked_by_agent`; the run ends `held`, and gate and auto parents pass their success guards. The held diagnostic in `orbit run show` retains the blocker kind. No step retry, step recovery or final recovery runs. On the PR pipeline the handoff commits, pushes and opens nothing, blocks the task under `task_blocked_by_agent` with the kind, and leaves the worktree as the implementer left it, uncommitted files included. `orbit job resume` refuses the held run and does not move the task back. Once the blocker is resolved, an operator saves the uncommitted candidate from the worktree, attaches it to the task and returns the task to backlog with `orbit task requeue` once the run has stopped (requeue is refused while it still runs); the next run starts fresh ([stuck job runs](../runbooks/stuck-job-runs.md#a-task-blocked-by-an-implementers-blocker)). An implementation activity cannot set status `blocked` itself (operators and other activities can). A claimed leaf stops before its next step and final recovery. Blocked-task recovery skips a marked failure note, or for a claimed leaf, a marker in the execution summary.

An implementer's exit-0 `status: "failed"` envelope with the exact code `unresolved_policy_decision` and a non-empty `error.message` uses the same hold. The task history records the decision reason, and the operator can resolve the decision before restarting work.

A blocker whose kind names an Orbit upgrade refusal (`upgrade_pending`, `upgrade_admission_refused`, `orbit.generation_switch_pending` and similar spellings) is the host's state, not the task's. It ends any agent step, reviewers included, with error code `upgrade_pending` instead. The step is not retried or recovered, the candidate is kept, and on an owner run (`upgrade_pending_requeued`) or an in-budget claimed release (`claim_released`) the task returns to the backlog rather than `blocked`; a claimed release that exhausts the 2-in-24h release budget instead moves the task to `blocked` (`claim_release_budget_exhausted`) until an operator returns it to the backlog. See [in-flight agent steps during a binary swap](../runbooks/upgrades.md#in-flight-agent-steps-during-a-binary-swap).

`step_failure_recovery` declares the same blocker with an `external_blocker` decision when a human must act on a cause outside the run; the post-recovery attempt and final recovery are skipped and the task is blocked as above. For an upgrade refusal the outcome is the one in the previous paragraph, not always backlog: an owner run returns the task to the backlog (`upgrade_pending_requeued`), an in-budget claimed release returns it to the backlog (`claim_released`), and a claimed release that exhausts the release budget blocks it (`claim_release_budget_exhausted`). A recovery that writes no decision gets its one post-recovery attempt only if Orbit sees the worktree, the run's base-ref tip or the validation environment change while it ran. Mechanism: [activity-job design](../design/activity-job/2_design.md#recovery-authority-is-host-only).

### Owner sign-off holds the run (`owner_signoff_required`)

An implementer that needs recorded owner approval it cannot record itself stops with error code `owner_signoff_required` (for example, a stable-tier breaking change with no approval on the task). The run ends `held`, not `failed`, and gate and auto parents pass their success guards. No step retry, step recovery or final recovery runs: another agent cannot record that approval. On the PR pipeline the handoff (`held_owner_signoff`) commits the candidate on the run's branch and tries to push it to its durable candidate ref `refs/orbit/candidates/<task>/<run>` on `origin`. It opens no PR and records a `candidate_held` history entry and a system comment naming the run, failed step, branch and head. If the push fails, the comment says the candidate is host-local and includes the source machine and failure; a run on another host cannot fetch it, so recover or push the named head before routing a run there. Run finalization blocks the task with the declared message under a `workflow run held:` note (not `workflow run failed:`), and the blocked-task recovery backstop does not recover that block. `orbit job resume` refuses this held run. Once the approval is recorded on the task, move the task back to `backlog` (or `in-progress`) and start a new delivery run: `candidate_resume` treats `held_owner_signoff` as preserving, so that run resumes the held candidate instead of implementing afresh (an implement-step candidate resumes as a repair for the implementer to finish). Operator steps: [stuck job runs](../runbooks/stuck-job-runs.md#a-task-held-for-owner-sign-off).

---

## Revalidating an unlocked-path repair

A conflict recovery, step recovery or final recovery that only reconciled prose need not rerun the whole required list. Set `workflow.unlocked_path_validation_commands` to the commands such a candidate needs instead, for example the docs and formatting gates:

```toml
[workflow]
unlocked_path_validation_commands = ["make ci-fast"]
```

When required validation runs on a candidate, Orbit compares the candidate's tree with the last tree this run passed the full `workflow.required_validation_commands` list on. If every changed path matches `workflow.unlocked_context_paths`, only these commands run. Any other change runs the full list, for example a rebase onto a base that brought a code change, because that combination has never been validated. The decision rests on the tree diff alone, never on what an agent reports it changed.

- Only a full pass becomes the comparison point. A narrowed pass does not, so successive prose repairs are always compared with code that passed the full list.
- The comparison point lives in the memory of the Orbit process that ran the validation, where an agent cannot write it. A run resumed by another process, a claimed leaf, a changed required list and the first validation of a run all run the full list.
- Each validation log and the step's output record `validation_scope`. It holds `kind` (`full` or `narrowed`), `reason`, the `tested_tree`, and, when trees were compared, the `validated_tree` and `validated_head` with the changed paths. A narrowed record also lists the `required_commands` it stood in for, so review and landing can audit the substitution. Completion evidence that requires the full list does not accept a narrowed pass.
- Unset or `[]`, every candidate runs the full list, as before the setting existed.
