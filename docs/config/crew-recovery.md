# Crew routing for failure and recovery

## Provider failure holds

A local run can fail because of its provider rather than the work. The step error then carries a typed marker followed by `provider=<name>`. Only the failing provider CLI's own stderr, terminal error or failure frames set a marker, never the agent's transcript, tool output or Orbit envelopes (an agent reporting a GitHub rate limit is not a provider limit).

| Marker | Meaning | Excluded crews | Base backoff |
|---|---|---|---|
| `[provider_capacity]` | The selected model was at capacity. | The run's crew when the failing step has the same provider and account. If either differs, crews of the failing provider that use the failing account: the named crew for `account=<crew>`, or provider crews without `env` for the default login. If none match, the run's crew. | 15 min |
| `[provider_unavailable]` | Unusable on this host, such as failed authentication. | As for capacity. | 30 min |
| `[provider_refusal]` | Content policy refused the task: a Codex content-filter `error` or `turn.failed` frame (`This content was flagged for possible …`), or a Claude `result` with `stop_reason: "refusal"`. | Every crew of the provider, which would refuse the same content again. | 24 h |
| `[provider_limit]` | The account hit a usage limit (texts below). | The failing account's crews on that provider. If the limit names a model and at least one of them runs a model containing it (case-insensitive), only those crews; otherwise all of them. | Until the reported reset (below) |

**Accounts.** When the failing crew sets [`env`](crews.md#per-crew-credentials-env), the marker is followed by `account=<crew>` after the provider (`[provider_limit] provider=claude account=opus-credits …`): that crew is its own account. For capacity and unavailability, the table uses the failing account when the failing step's provider or account differs from the run crew's: a named account selects that crew, while the default login selects only the provider's crews without `env`. If the provider and account both match the run crew, only the run crew's family is excluded (below), not the account's other crews; if no crews match the failing account, the run crew is the fallback. A usage limit independently excludes the failing account's crews on that provider, narrowed further by a reported model as described above. A capacity, unavailability, or limit failure without `account`, including older markers, is for the provider's default login. A refusal is the provider's content policy and still excludes every crew of the provider. A stored hold keeps the crews it named; its note carries `account` when it is a crew account's.

**Crew families.** A hold matches each crew it names by family: the crew and every [`<crew>@<effort>` reference](crews.md#crewsname--which-provider-model-runs-the-task) to it, which share its model and account. The rules above still select which crews a hold names, and its note lists them as selected, but naming `opus` also keeps `opus@medium` and `opus@high` out of the draw, and a capacity or unavailability hold after a run as `opus@medium` keeps `opus` and its other effort references out too. Crews carried from an earlier hold keep their machine scope and match by family the same way. A pull drain's crew window matches its exclusions, backoffs and runnable crews by family as well.

Capacity, unavailability and refusal backoff doubles for each other hold the task got in the last 24 hours, up to 24 hours. A usage limit holds until the provider's reset, clamped to 5 minutes – 7 days after the failure; with no readable reset it starts at 30 minutes and doubles per earlier usage-limit hold on that provider account in the last 24 hours, up to 6 hours. Provider labels are parsed first, so an alias is the same provider. Crews an earlier, still-standing hold excluded stay excluded, each on the machines it was excluded on.

Recognized usage-limit texts: Antigravity's `Individual quota reached … Resets in 1h37m37s`; Gemini's `TerminalQuotaError`; Codex's `You've hit your usage limit … try again at <time>` (an `error` and `turn.failed` pair); a Claude `result` with `is_error: true` starting `You've hit your` or `You've reached your`, or a `rate_limit_event` with status `rejected`; grok's `Too Many Requests (429)` and Grok Build's `API error (status 402 Payment Required): Grok Build usage balance exhausted` (which names no reset time and takes the default hold backoff); Muse's `Subscription quota exhausted … resets at <RFC 3339 time>`, where an absent reset also takes the default hold backoff. The marker then carries `model=<model>`, `window=<window>` and `resets_at=<RFC 3339 time>`. A relative reset is read from the failure time; a Codex `try again at` time is local to the host.

**Run outcome.** An owner-local run stopped by `[provider_capacity]`, `[provider_limit]` or `[provider_unavailable]` ends `held`, with diagnostic code `provider_capacity`, `provider_limit` or `provider_unavailable`. `orbit run history` shows the hold, reliability counts it under held rather than failed, and the gate and auto parents pass their success guards without echoing a failure. Incidents retains a provider deferral record. Content refusals keep their existing failed run outcome. A routine fire backed by a held run retains the existing `succeeded` fire status; its underlying run still reads `held`.

**Handoff.** Step and final recovery skip these failures. On the PR pipeline the handoff returns `held_provider_failure`: it commits the candidate on the run's branch, publishes no branch and opens no `[BLOCKED]` PR, but pushes the candidate to `refs/orbit/candidates/<task>/<run>` on `origin` so a claim on another host can continue it (a task comment says when that push failed). Run finalization moves the task to `backlog` under a `provider_failure_hold` history event, whose note carries the class, provider, excluded crews, `not_before` and run. A `[provider_unavailable]` hold also names the host's `machine`: a failed login is that host's, so its crews are excluded only there, while crews a still-standing capacity, limit or refusal hold excluded stay excluded on every machine (`carried_crews`). A hold stored without a `machine` excludes its crews everywhere, as before. A claimed leaf on a follower that ends with `[provider_refusal]` releases its claim as a `provider` failure, and the owner records the same hold from its own crews, naming the leaf run.

**Admission during the hold.** Until `not_before`, the crew is drawn from crews the hold does not exclude: the task's own crew or pool, then the other enabled members of its purpose or type pool when the assignment came from one, then its complexity pool, then `default_crew`; `crew_selection.source` names the hold. With none left, the local drain defers the task and `orbit run readiness --json` reports `reason: "provider_backoff"` with the release time. A follower's pull is not given a held task it would run as an excluded crew (its own crew, or the follower's default for a task naming none); the pull receipt's `deferred_conflicts` names the hold. An unavailable hold placed on another host excludes from that follower only the crews it carries from an earlier standing capacity, limit or refusal hold (`carried_crews`); those stay excluded on every machine. A hold that excludes nothing (the failed run resolved no crew) still defers until `not_before`. An explicit run-input `crew` ignores the hold. A usage-limit hold on an explicit crew (`crew_source` `explicit`, or a legacy pin) follows [`workflow.provider_limit_explicit_crews`](crews.md#provider-usage-limits): under `wait` the task waits with `provider_backoff` instead of being redirected; under `pool` it draws as above. The hold ends at `not_before` or any later status change, and the next run resumes the committed candidate.

**Muse model access.** When Muse's failed implementation terminal says the requested model is inaccessible, the run fails with the provider's diagnostic. Claimed leaves retain that diagnostic on the executing follower. On automatic implementation draws, owner-local admission uses evidence from its own implementation runs in the last 12 hours (net of a newer verified success of the model on the same account) to exclude that model across tasks on the same host and provider account. It reopens a stored pool assignment when needed and draws from the remaining eligible members using the configured weights. Explicit run crews and task pins keep their normal routing. If no eligible members remain, readiness reports `provider_model_access_hold` and names the model. The exclusion lapses after 12 hours, and an eligible crew, a different host or provider account with access, or a newer verified successful invocation of that model in the same context clears it earlier.

On a pull-drain follower, a later claim of the same task reads the evidence of that follower's own claimed leaves of the task from the last 12 hours, net of a newer verified success of the model on the same account. An automatic snapshot assignment can be re-drawn under any `workflow.crew_selection`. The follower excludes every crew on a refused model and account. For a `type:` assignment whose model was refused, the follower draws from the named type pool and applies its host eligibility, provider holds, usage limits and run allowlist. When no eligible crew remains, the leaf fails at crew resolution with the typed `provider_model_access_hold` error naming the model, before any provider runs. An explicit or legacy snapshot crew and a run-input `crew` keep their crew. The owner's runs, another follower's leaves and evidence from another provider account never exclude a model on that follower, and the follower's leaves never exclude one from owner-local admission.

**Host outages.** A local run that fails `[provider_unavailable]` also records every crew sharing the failing provider account in the global store (`~/.orbit/orbit.db`, table `provider_crew_outages`), one row per provider and crew, until the hold's own backoff ends; a longer outage of the crew is kept. Until then this host's local admission draws no task onto any of those crews, not only the held one, and whatever became of the held task: a task whose own crew, complexity pool or default leaves another account's crew draws it, and the rest wait with `provider_backoff`. A crew of the same name on another provider is not covered. The record is advisory: a failure to record it logs a warning, and the task is still held. Followers and their pull drains are not affected; their own crew window handles their own logins. Operator steps: [stuck job runs](../runbooks/stuck-job-runs.md#a-task-held-after-a-provider-failure).

**Routines.** The task-pilot routine honours a host provider failure hold. It reads recent runs on this host — the failures `orbit run readiness --json` projects as `provider_auth_failures`, plus `provider_capacity` from held runs and older failed runs — over a one-hour window of finish time (falling back to start time, then creation time), and does not write a new hold record. A recently finished failure remains in that window even when its pipeline was created more than an hour ago. Clearing successes use the same finish-time window. While a `provider_unavailable` failure from the last 30 minutes, or a `provider_capacity` failure from the last 15 minutes, covers every crew the pilot would draw (the inspection-capable members of `workflow.purpose_crews.task_pilot` when it has one, else of `workflow.low_complexity_crews`, or of the next tier up when none can inspect, else `workflow.default_crew`, after the usage-limit narrowing), the routine withholds the dispatch. The withhold reason starts with `provider_hold:` and names the provider or crew, when the backoff ends, the failing run, and the crews skipped. It does not start `task_pilot_pipeline`. Unavailability covers every crew of that provider account on this host, because a failed login is the account's: the crew the failure names as its `account`, else the provider's crews without `env`. Capacity covers the crew the run ran as, or every crew of the failing step's provider when that step did not run as that crew. The routine and the readiness auth-failure card share crew attribution: `task_pilot_pipeline` uses its recorded task-pilot draw (or, for a run admitted before purpose pools, its `workflow.low_complexity_crews` draw), `blocked_task_recovery_pipeline` uses its `workflow.purpose_crews.final_recovery` draw, and a `finding_verification_pipeline` run recorded before that job was removed [ORB-22844] uses its prepared verifier partitions, including escalations. These jobs' `resolved_crew` names the tasks' crews and does not identify the agents they dispatched. A missing draw counts as no known crew. A verifier failure counts for the partition whose recorded error the failure message carries; a verifier success counts only partitions with a recorded result. Other jobs use `resolved_crew`. A pilot's provider failure is recorded on the run itself: a failed pilot fan-out prefers a provider-classified worker message over an ordinary failure message, preserving the provider marker even when an earlier worker failed on the work. The backoff is the base in the table above, measured from the latest uncleared failure's finish time; it does not double. A successful run of that provider account on this host that started after the failure, or a pull-drain auth probe that records `recovered_at` for that account on this host, clears unavailability before the backoff ends; a success that started before the failure does not, even when it finishes after it, the same rule `provider_auth_failures` uses. Capacity clears when such a success is of the held crew, or of any of the provider's crews when the hold covers the provider. A probe does not clear capacity. `provider_limit` and `provider_refusal` do not withhold the routine. When any drawable crew remains, the routine still dispatches and the draw skips the held crews, naming the hold in the draw's source. The next tick after the hold lifts dispatches again with no operator action, once the routine's own debounce has passed. Another host's failure does not hold this one. Step-failure and PR conflict recovery share that draw, so they skip a held crew the same way; the routine withhold itself is only the task-pilot routine.

Readiness and host purpose-draw holds also read trusted `provider.invocation.observed` audit events. The CLI runner records the actual provider, crew account and invocation interval independently of the ordinary step and final-recovery decision. An authentication failure during step or final recovery therefore holds that account even when the primary error is unrelated, the parent later succeeds or ends held, or the parent is still running. The primary diagnostic remains intact. Each projection reads at most 200 recent invocation outcomes in addition to its existing run pages; a busy host can push older evidence past those bounds. Unavailability and capacity keep the same backoffs and probe rules. Only a successful invocation or run of the same provider account on the same host that started strictly after the failing invocation ended clears it; a successful primary invocation does not stand in for its recovery provider. Historical invocation ledgers and free-text recovery decisions without trusted outcome events remain unknown; Orbit does not infer authentication from agent prose or an exit code alone.

**The host's limit record.** A usage-limit failure also records an observation in the global store (`~/.orbit/orbit.db`, table `provider_limit_observations`): one row per provider, model and window with the reset, run, crew and provider text. A run of a crew that sets [`env`](crews.md#per-crew-credentials-env) records its observations under that crew account instead (table `provider_account_limit_observations`, one row per provider, account, model and window), so they never replace or gate the default login's. A newer observation replaces the row, an older one never does. Recording is advisory: a failure to record logs a warning, and the run is still typed and held. After every Codex, Claude, Muse, Grok or Antigravity run, successful or not, the host also records the provider's own reading of each window (source `event`): percent used (can exceed 100), window length when known and reset.

- Codex: the last `token_count` with `rate_limits` in the run's own thread rollout (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl`, `CODEX_HOME` from `[execution.env]`, else `~/.codex`), reading at most the last 1 MiB. `primary` and `secondary` windows are account-wide.
- Claude: the last stdout `rate_limit_event` (Orbit runs `claude --output-format stream-json --verbose --include-partial-messages`, and the runner drops its redundant `content_block_*` events before capture): each `unifiedWindows` entry (`five_hour`, `seven_day`, …) plus the currently limiting window if absent there. `seven_day_opus` and `seven_day_sonnet` carry their model; an overage window is recorded with `gating` off.
- Muse: the last stdout `status` (Muse Code 1.4.12) or `run.usage` (Muse Code 1.4.13) record with `payload.kind = "run_usage"` and an allowance object. Each recognized `allowance.window` / `allowance.weekly` with a nonnegative numeric `used_percent` and valid integer `resets_at_ms` yields an account-wide reading, timestamped at run completion. Labels are preserved and window lengths remain unknown.
- Grok: after the run, the host reads `https://cli-chat-proxy.grok.com/v1/billing?format=credits` with the existing first-party CLI login in `$GROK_HOME/auth.json` (else `$HOME/.grok/auth.json`), using the child's state root. Grok CLI 1.0.67 reports `config.creditUsagePercent` and `config.currentPeriod` (`type`, `start`, `end`): the recognized weekly period yields an account-wide `weekly` reading, its duration in minutes and its end as the reset. Headless `usage` and `grok usage` contain invocation/session token and cost totals, not quota windows. This read needs `curl` on the child's `PATH`, adds no secret to the agent environment, follows no redirects and has a three-second request timeout. Missing or ambiguous credentials, credential/endpoint environment overrides, unavailable transport and unrecognized responses record nothing and never fail the run. Such accounts can still use [`provider_limit_budgets`](crews.md#budgets-for-providers-that-report-no-usage).
- Antigravity: after every run, the host invokes the same resolved `agy` launcher with `--print /usage --output-format json --print-timeout 3s`, using the child's environment and working directory. Verified with agy 1.4.6 and 1.5.13: the native command's top-level `command.name = "usage"` / `command.data.groups[].buckets[]` reports `remaining_fraction`, `window` (`5h` or `weekly`) and RFC 3339 `reset_time`. Orbit records percent used, 300 or 10080 minutes, and the reported reset, preserving the bucket ID as the window label. Shared buckets are scoped to Gemini, or to Flash, Pro, Neon, Carbon and Hydrogen when the reading carries a separate Argon bucket; third-party buckets are split into Claude/GPT scopes, and Argon has its own scope. A reading removes earlier `event` windows and scopes of the account only when every known, enabled bucket in it was readable (the known buckets are `gemini-weekly`, `gemini-5h`, `3p-weekly`, `3p-5h` and `gemini-argon-wtu-weekly`; a disabled bucket is not counted). Then any earlier `event` window or scope of that account it no longer reports is removed in the same transaction, so it stops showing and gating without waiting for its old reset. A reading with an unreadable known bucket (enabled, but its fields do not parse into a reading) upserts its readable windows and removes nothing: the unreadable bucket's earlier row, and any earlier scope it no longer reports, stay in place until a reading reads every known bucket. Limit-failure (`error`) observations keep their own expiry. A reading that parses to no windows removes nothing. It never reads assistant `response` text or treats token accounting as quota. The query runs host-side so agy can use its existing login, including the macOS login keychain, without adding a secret to the worker environment. A five-second deadline and 64 KiB output bound keep it advisory: unavailable commands, failed reads and unknown shapes record nothing and never fail the run. Older versions without structured quota data can use [`provider_limit_budgets`](crews.md#budgets-for-providers-that-report-no-usage).
- A missing rollout, another thread's rollout or an unrecognized reading records nothing. Other providers report no usage window.

**Pull drains and claimed leaves.** An authentication failure releases the claim and excludes every crew of that provider account (aliases included): the leaf's crew alone when it sets [`env`](crews.md#per-crew-credentials-env), else every crew of the provider without `env`; a declared [`auth_probe`](providers.md#executor-authentication-recovery-probes) can re-admit them in the same window. A capacity failure excludes only the leaf's crew and its `@<effort>` references, since another model may have room, and only for a backoff: 10 minutes from when the leaf ended, doubling with each further capacity failure of that crew in the window up to an hour. The exclusion carries `until` and the crew is runnable again in the same window once it passes; while it holds the owner's before-PR or before-landing review crew, the drain requests nothing and its `Requests held:` line names the review's timing and when the backoff lifts. A usage limit releases the claim with failure class `provider` and `provider_limit: true`, which does not count against the task's release budget and excludes no crew itself; the follower's own limit record excludes the provider's crews until the reading lapses.

## Final recovery pool

`workflow.purpose_crews.final_recovery` is the final-recovery [purpose pool](crews.md#purpose-crew-pools): the weighted pool the `final_recovery` activity draws one crew from, once per run, when a task fails after step-failure recovery is exhausted; that crew evaluates the failure and attempts a final typed decision and fix.

The activity filters disabled crews and applies the run's crew allowlist, but it does not
prefilter by provider threshold. With usage pressure enabled, a non-exhausted crew at 95% with
a 90% threshold remains eligible; an exhausted crew can lose its tickets only when another
drawable candidate has quota ([usage-pressure draws](crews.md#usage-pressure-draws)).

```toml
[workflow.purpose_crews]
final_recovery = ["sol:100", "opus:20"]
```

- **Unset:** `["sol:100", "opus:20"]`, filtered to crews defined in `[crews.*]`. With neither defined the pool is empty and final recovery is off, without a validation error.
- **Explicit:** standard pool grammar (`crew[@effort]` or `crew[@effort]:weight`); every named crew must exist in `[crews.*]`.
- **`[]`** disables final recovery.

Final recovery keeps task-write tools closed. A repair that will `resume` can
return an optional `repair_evidence` object beside its typed decision, containing `execution_summary`
(up to 65536 characters) and `task_artifacts` (`[{path, source_path}]`, up to
32 files, each at most 1 MiB). Sources must be regular files under the assigned
worktree's `.orbit/tmp/`; names must be unique and cannot overwrite reserved
artifacts, including `review-*`, `validation/` and `no-diff-handoff/`. The
handoff runs only for a decision with its required `fault` and `fault_evidence`:
a resume without them escalates and writes nothing. The engine writes the evidence
only after the resume's step and any repair commit are admitted, and the host
validates the entire batch before writing its summary and artifacts to the admitted
task. A resume refused at either gate escalates and leaves the task's prior summary
and artifacts unchanged. On a claimed
run it sends the bytes through the authenticated owner route. The handoff accepts no task selector or lifecycle edits; invalid
evidence refuses the decision and leaves the candidate for inspection. The
blocked-task backstop excludes `resume` and does not accept this handoff.

A final-recovery `resume` records its rationale and the failed step's refusal
and output before discarding stale checkpoints. The refusal and output are
bounded the way final recovery's own input is (64 KiB, keeping head and tail),
so the resumed agent's turn stays within provider limits. The resumed top-level
phase and its nested activities receive this typed `final_recovery_resume`
input, including in claimed pipelines and after a process restart.
`agent_implement` addresses that repair mandate first. The saved context is
passed unchanged, including any literal template tokens in its text. Later
phases do not inherit it. Run-show recovery attempts include the resume
rationale in their detail.

When the resumed phase revisits a published candidate, Orbit authenticates
the exact host commit, branch, preparation and synchronization checkpoints.
An interruption after a replay rebase can continue before the new head is
pushed: the old push and PR outputs still describe the previous publication.
The setup pin remains immutable, and ancestry alone cannot authorize a changed
HEAD. Missing or inconsistent provenance refuses the candidate. Resuming the
phase reruns candidate validation, configured review and handoff evidence;
the existing pull request is reused. Republication is leased to the exact
previously published head, including when the repeated sync has no rebase
left to perform. A changed or deleted remote branch refuses that push.

A transient read-only PR or repository observation failure gets one
host-authenticated completion retry within the original completion deadline.
The host checks the unchanged publication, validation receipts and captured
review certificate first; the retried completion reads PR state again and
checks the published head and base before merging. A host-certified managed
rebase completed before publication is eligible when its successful sync
receipt matches the published candidate; ambiguous, altered or later recovery
remains governed. No source step or publication is replayed. The recovery rationale
records `transient-observation`. Review provenance changes and the retired
capture-only after-landing timing do not add review obligations. New
validation captures bind non-review inputs separately so review-only config
drift can be checked against the candidate's captured obligations; older
captures still require their original full policy digest to match.

GitHub's rate limit on the automation account's reads is shared by every
pipeline on the host. When completion's workflow-prerequisite reads (the
repository, pull request, branch policy, checks, statuses and workflow runs it
reads before landing, and the run, jobs and pinned workflow file it reads after
them) are rate-limited, completion reads the reset from
GitHub's `rate_limit` endpoint, which spends no quota, waits for it and reads
again: at most three waits for each read pass (the snapshot, then the
evidence read around it), each ending before the completion deadline, which
is never extended and is the only bound on the whole step. A snapshot that
spends its three waits ends the step with GitHub's diagnostic; the evidence
read around it never reads the snapshot again with a fresh count. A wait only
precedes a repeated read: a reserved workflow rerun is never sent again by a
retried observation. The host's process keeps two small caches so a completion
poll can skip some repeat reads. A worktree's repository identity is reused for
ten minutes, but inconclusive snapshot evidence drops it early, so the next
read fetches it again. A workflow file's contents at a full commit SHA never
change, so they are reused while cached; the cache is shared by every worktree
in the process, holds at most 256 entries and is emptied when a new read finds
it full, and it does not outlive the process. A read the cache no longer holds
(or that another process makes) spends quota again, so count those reads when
troubleshooting a rate limit. When the reset falls
past the deadline, the delivery step fails with GitHub's diagnostic and final
recovery requeues the unchanged publication without an agent, recording
`provider-rate-limit`; the next run's completion waits for the reset within
its own budget. The task is not blocked and no decision request opens, though
the requeue counts toward final recovery's requeue bound. A claimed leaf's
failure still goes to its recovery agent.

A changed review obligation (timing, reviewer pool, budget, baseline commands or
host evidence) refuses completion reuse and records `policy-changed`. The
existing bounded requeue preserves the branch and PR for a successor delivery
with fresh validation. Any fresh independent review follows that delivery's
selected review policy, which may be off. Changed candidate/base
bindings, mutation or rewrite provenance, and missing or stale evidence
retain their refusals, recorded as `candidate-changed`. Conflicts, red checks
and permission failures keep their existing recovery path; a lost mutation
reply never grants this read-only retry.

A historical supported completion-repair grant is a separate case: it may have
renewed a deadline without reaching a repair agent. Fresh operator source
repair can acknowledge that stopped lineage when the host's unique grant
record, complete parent chain, receipt origin, step audits and unchanged
publication prove no source or mutation effects. Plain descendants must retain
the granted deadline and marker. This admission creates a repair-only attempt
under current validation and review policy; it preserves the old records and
inherits no completion, merge or automatic-fix authority. See
[repairing a kept inline publication](../runbooks/stuck-job-runs.md#repair-a-kept-inline-publication-after-completion-expired).

**Escalated blocks.** A final-recovery `requeue` is bounded to two per task
in 24 hours; past that the decision is refused and the task stays blocked.
The bound has no configuration key. Before it escalates, final recovery runs
bounded mechanical steps (resume the run, rerun CI, requeue), and a
deterministic recheck settles a block whose pull request merged outside Orbit,
whose base check went green or whose infrastructure cleared. What remains waits
in `blocked` behind a decision card (a summary, two or three options with
consequences, a recommendation) that an operator answers; the answer applies the
option and moves the task out of `blocked` in one operation. The seeded
`blocked_task_triage` routine and its `block_escalated` trigger are retired: a
`blocked-task-triage-*` routine file loads inert and `orbit workspace sync`
retires a seeded copy. See
[final recovery](../../plugin/skills/orbit-setup/references/automation-built-in.md#built-in-final-recovery-of-blocked-tasks).

## PR auto-fix repairs

`orbit run auto --auto-fix` lets the drain repair the pull requests its own runs
opened when a required check fails or the branch conflicts with its base. It is a
per-invocation flag with no configuration key, and it authorizes repair only:
merging still needs `--complete`. Without the flag nothing changes. The dashboard's
**Auto-fix PR blockers** checkbox starts the same owner drain with `auto_fix: true`.
It is off by default and independent of completion and proposed-task approval;
it requires an operator session and is unavailable on pull replicas. The receipt
and persisted run input carry the chosen repair authority. This control uses the
same watcher, crew pool and attempt ledger described below.

A delivery leaf whose worker dies after it opened its pull request and
promoted its task to `review` does not leave that pull request unwatched. The
owner drain's next pass resumes that leaf from its checkpoints, including one
the host manager stopped (`held` with `operator_stopped`), once
(a resumed attempt that dies again is left to an operator), when it holds the same `--complete` and `--auto-fix` grants
the leaf was started with, so the watcher repairs the same pull request and
head under the run's retry lineage and attempt ledger. The resumed run's crew
comes from the leaf's own input, not the adopting drain's `--allow-crew`. See
[adopting a dead leaf's pull request](../runbooks/stuck-job-runs.md#a-delivery-leaf-that-died-after-opening-its-pull-request).

An `--auto-fix` drain also repairs Orbit pull requests that no run is watching
any more. When a task's newest delivery leaf ended `failed`, `timeout`,
`interrupted`, or `held` because the host manager stopped it, and the task waits in `review`, or was blocked or requeued by that
leaf's own delivery or final recovery, each pass reads the task's recorded pull
request. If it conflicts with its base or a required check failed on its head,
the pass resumes the leaf at the last completion round it reached, which pins
the head the leaf last published, with `auto_fix` granted. The
watcher there admits the repair against the same attempt ledger and budget (three
per pull request; a repair that published no new head leaves the head open to the
next run's attempt while budget remains), and the auto-fix crew takes the
task's footprint locks before it edits, pushes to the same branch and leaves
completion to merge. The resume keeps the leaf's own `completion`, so a repair
never merges a pull request whose leaf lacked `--complete`. Operator holds,
`no-auto-approve` tasks, a leaf started with `--complete` under a drain without
it, and pull requests not opened by a delivery identity on the task's
`orbit/<task>-` branch are never repaired. Each hand-back is recorded in task
history as `pr_auto_fix_out_of_run`; a head and blocker is handed back once, and
a pull request at most as many times as its budget. `orbit run readiness`
lists live repairs and the latest drain pass's skipped candidates under
`out_of_run_auto_fix` (`state: "skipped"`, with the PR, reason, detail and
observation time). Blockers are null when a refusal preceded the forge read.
`orbit run show <drain-run> --step adopt_orphans --json` exposes the persisted
pass in `step_output`, including `auto_fix.skipped`. See
[a conflicting or red pull request no run is watching](../runbooks/stuck-job-runs.md#a-conflicting-or-red-pull-request-no-run-is-watching).

The same pass hands back a `review` task whose newest leaf ended `held` with
its pull request open on a delivery hold a fresh forge read can clear:
`delivery_check_evidence_inconclusive`, `delivery_completion_budget_expired`,
`delivery_evidence_stale`, `delivery_requirements_unsatisfied`,
`delivery_base_checks_pending`, `delivery_forge_observation_unavailable` or,
once the published head, or a head produced by an Orbit-audited clean base
refresh of it, reports a pending or passed required check or the forge
positively confirms the base branch configures no required status checks
(neither classic branch protection nor rulesets require checks),
`delivery_check_runs_absent`. Until that check exists or zero required checks
are confirmed — missing or unreadable policy is treated as unconfirmed — the
pass lists `delivery_check_runs_absent` under its code and spends no hand-back.
The
leaf reruns its completion step with a fresh completion budget and `auto_fix`
granted, under every rule above. A conflict or failed required check is
repaired through the ledger; otherwise the hand-back spends no repair budget
and records `hold_code` instead of a blocker. Such hand-backs wait 5 minutes
after the leaf held, doubled for each earlier hand-back of the pull request for
that code, and stop after 3 per pull request, head and code; the pass then
lists the task under `auto_fix.skipped` with the hold code as its `reason`. A
task that a `completion.hold` rule keeps in review is skipped as
`completion_held`. Other holds keep their own next step, and the held leaf
still counts as `held` in run history.

The dashboard's **Fix PR** action on a `review` task starts the same repair for
that one task without a drain (`POST /api/tasks/<task>/auto-fix`). It runs the
drain pass's checks and start: the same attempt ledger and budget, footprint
locks and `pr_auto_fix_out_of_run` record. It needs an operator session, governed
as `auto_drain.auto_fix`. It grants repair only: the resumed leaf keeps its own
`completion`, and a leaf started with `--complete` is resumed only when the
session also holds completion authority (`auto_drain.complete`). The button
shows only when the task records a pull request. It is enabled only when that
pull request conflicts with its base or a required check failed on its head,
or the task's newest leaf is held on a hold the drain hands back (Fix PR skips
the drain's backoff, not its ceiling); otherwise it is disabled with the reason in its title. A refused start answers
`409` with code `task_auto_fix_<reason>`, using the drain's reasons plus the
single-task ones; nothing starts. Unlike a drain pass, Fix PR also hands
back a delivery that succeeded without `--complete` and left its pull request
in review, as a dashboard ship or `orbit run ship` does. Such a delivery's own
run ends at once, so a detached `task_pr_watch_pipeline` watches the pull
request's checks for up to an hour. It records one `landing_checks_observed`
history entry on the task (`state` `passed`, `failed`, `conflicting`,
`blocked` or `pending`; for `failed`, the failed checks and the owner landing's
`classification` of them). It retries authenticated transient CI failures
through the bounded infrastructure policy below, then resumes observing the
checks. It never merges or repairs, and the task detail shows the newest entry beside Fix PR. See the
[runbook](../runbooks/stuck-job-runs.md#a-conflicting-or-red-pull-request-no-run-is-watching)
for the reasons.

The crew comes from the existing pool resolution, in this order:

1. `--allow-crew` restricts which crews the draw may pick, as for every
   activity in the run.
2. `workflow.purpose_crews.auto_fix` is the pool when it has an enabled member,
   else `workflow.low_complexity_crews` ([purpose crew pools](crews.md#purpose-crew-pools)).
   The pool is drawn once per run with its weights, disabled crews skipped and
   provider usage limits honoured, as
   [above](crews.md#system-work-draws-from-the-complexity-pools). Conflict repairs under
   `--auto-fix` use this pool too, rather than the failure-recovery or medium pool.
3. An empty or wholly disabled low pool falls back to `workflow.default_crew`.

A pool none of whose crews the run may use stops auto-fix with a diagnostic
rather than escalating to another pool. Auto-fix never changes a task's
complexity or crew.

The PR watcher already used by `--complete` does the detecting, and the
existing recovery activities, final validation and push do the repair:

- On an open blocked PR, the watcher reads authenticated required-check policy
  for the pinned current head. Queued or running required checks keep waiting
  within the original completion deadline, even beside a failed required or
  optional check. Repair admission waits until all reported required checks
  conclude and includes every required failure from the latest policy read. A
  required check that current branch policy names and a freshly pushed head
  has not reported yet also waits; it is never treated as passed. An
  unclassified or optional pending check grants no wait. Review gates and
  stale evidence stop the watcher. Untouched-file or flaky attribution does
  not delay auto-fix: unless the known-red-base path
  applies, a source failure goes to repair on its first completion poll,
  without an unchanged-head rerun or eligible-set authorization. Suspicion
  alone never records `pr_auto_fix_base_red`. Every mergeable head is checked
  against its latest head-bound required-check results before merge. A red
  required result stays blocking regardless of attribution; only a later
  passing run on that same head or on the repaired head clears it. Skipped or
  neutral reruns do not clear a required check that failed on that head, and
  neither does a skip when the head's check history is unreadable. A required
  check that concluded skipped or neutral without ever failing on the head (a
  docs-only change) is satisfied, as branch protection reads it. A failed required check
  whose job log shows a code failure is compared with the current base
  before a repair is admitted. Orbit reads the latest push run of the same
  workflow on the exact base tip. When that run skipped the job (a docs-only
  or change-scoped commit) or lists no such job, it reads back along the base
  branch's first-parent history, up to eight commits including the tip, to the
  newest commit whose same job ran to success or failure, and judges the
  failure there. If every failed required job succeeds on the base and the PR
  is behind, it refreshes the branch onto the current tip, which contains
  those green commits, through the existing pinned rebase, validation and
  lease-checked publication path, then watches checks on the new head. This
  spends no agent repair attempt. A reviewed head returns for independent
  re-review before publication. A matching failure on the base is known red:
  the watcher records it and waits within the completion deadline, spending no
  repair attempt. A failure the base already failed the same way at the head's
  fork point also waits, without spending a repair attempt, while the moved
  tip cannot prove it fixed: its same job is still pending, skipped or not
  listed, no commit within the bound ran it, or its jobs cannot be read. Each
  poll evaluates it again, so the wait ends once the tip's job concludes. Once
  a head is recorded that way (`pr_auto_fix_base_red`), a later poll that
  cannot read the base evidence again keeps waiting instead of repairing. A
  distinct candidate failure keeps the ordinary repair path and three-attempt
  budget. When a check repair is admitted, every watched task records
  `pr_auto_fix_head_failure`: the PR, the failed head, and each failed workflow
  check with its failed step and normalized error signature. A repair that
  cannot clear the head holds the run, and the task stays in `review` with its
  locks. If a `ci-failure-sweep` repair with that exact signature then waits on
  a selector the task's recovery added, admission reports the circular wait
  and raises one owner decision on a human-crew task. A conflict is rebased onto the current base by PR conflict recovery.
  If a published check repair then conflicts, recovery follows the contiguous
  auto-fix pushes recorded by the host for that PR and this run or its
  authenticated retry ancestors. The stopped rebase's branch and original head
  must both match that repaired head; its target base remains pinned. Missing,
  unrelated or disconnected push records cannot authorize a different head.
- When a required job's log is unreadable, including a truncated log with
  no complete failing command, auto-fix can rerun failed jobs once on the
  unchanged head. The provider must authenticate one workflow and its full
  failed-job set. This uses the shared unchanged-head rerun budget and the
  original completion deadline. A second unreadable failure keeps the
  original stop. A readable result follows its usual classification.
- CI rerun-planner refusals are retained on the owning task in
  `ci-rerun/<execution>/diagnostics.json`, including the observed PR, head,
  run and job identities, planner stage and a redacted reason header capped
  at 1 KiB. These are observations, not authenticated mutation authority.
  The single replaceable artifact keeps the first refusal and the latest 15
  transitions, with an omitted-transition count; identical consecutive reads
  add no history. A later successful planner read clears the current refusal.
  A changed head clears it before any completion hold, while earlier heads
  remain historical observations. Completion retains its typed outcome beside
  this journal and includes a current refusal in the stop diagnostic, including
  when the original completion budget expires. A diagnostic write failure is
  reported in a warning and never grants recovery permission.
- A rerun-planner hold in owner-local `auto_fix=true` completion is bounded.
  The owning task records `pr_auto_fix_ci_rerun_hold` with the head and
  reason when a hold begins on a head. After 15 minutes on that head, or on
  the completion's last poll, the hold expires: `pr_auto_fix_ci_rerun_hold_expired`
  is recorded and the failed checks take the ordinary repair path, whose
  known-red-base and evidence gates still admit a repair or stop with a
  reason. A dependency-only record holds only when no other job failed: a
  failed sibling the logs cannot tie to a changed file (for example a
  guardrail check) is still a candidate failure and goes to repair.
- Final recovery after `complete_pr` re-reads required checks on the current
  head and receives each failed check's name and diagnostic evidence in
  `required_check_failures`, independently of the earlier step error. The
  exception is an untouched-path failure whose base evidence is still
  incomplete: no passing same-check base run exists yet, or the base run passes
  but no same-signature `pull_request` rerun of the unchanged head is recorded.
  That wait withholds `required_check_failures` for every check, and the
  attempt requeues the published candidate instead of diagnosing it. Pending,
  unreadable or changing checks refuse a partial diagnosis without renewing
  the completion watch deadline.
- A failed required check run that no workflow produced (a provider app's
  aggregate result, such as a code-scanning check beside passing analysis jobs)
  is repaired only from its own failure annotations. The watcher reads them
  from the forge for the check run and app identity that the head-bound
  required-check read reported, and requires a complete, unchanged answer bound to the
  repository, pull request, head and base branch. Each failure annotation must
  name a relative source file that exists at that head, through an exact-head
  blob link, with a line and a message. A passing analysis job does not satisfy
  the aggregate, and a failed workflow job beside it does not hide it: every
  failed required check needs its own evidence. Duplicate, foreign, stale,
  truncated or changed provider answers, a missing identity, an aggregate
  without failure annotations, and annotations on workflow configuration, on a
  path outside the checkout or showing a runner or network failure stop the
  watcher without spending an attempt. The answer is retained under
  `auto-fix/<run>/check-annotations-<failed-head>.json` before the repair is
  admitted from the same attempt budget.
- When a failed required check is a dependency waiter, auto-fix uses the
  authenticated workflow and job evidence to include bounded excerpts from
  the failed dependencies in the repair input. If a dependency log or the
  complete proof is unavailable, the repair retains the waiter excerpt and
  explains the gap using the run and job IDs from the provider.
- A job log that exceeds the display or full-command retention limit can still
  supply bounded failure regions. Recovery requires a fully scanned source,
  complete command and region selection, and exactly one provider-reported
  failed step. Retained job/step columns must agree throughout; raw job logs
  require a unique completed failed command. Missing provenance, conflicting
  labels and partial selections stop recovery without spending an attempt.
- An admitted required-check repair may change the source files needed to fix
  its evidenced failure outside the original task description or selectors,
  including a defect already on the shared base. Ordinary step recovery keeps
  its candidate-scope boundary. The repair writes the run/head-bound scope
  record named in its recovery input, explaining the cause and each changed
  path. The host checks that record against its admission and actual edits
  before committing, retaining a pre-commit copy under
  `auto-fix/<run>/scope-<failed-head>.json` and the committed record under
  `auto-fix/<run>/<repair-head>.json`. It widens task selectors with recovery
  provenance. Before a local drain or follower pull admits more work, the owner
  reconciles recovery-only selectors against stopped local PR deliveries and
  their observed candidates.
  A selector is released only after an explicit candidate discard or a verified
  candidate/publication no longer changes that path. Original selectors stay;
  live runs, unsettled claims, kept claim candidates, still-published repairs,
  uncommitted repairs that were not discarded, and unreadable or incomplete
  outcomes keep their protection.
  Read-only readiness reports the last durable declaration; it does not perform
  this cleanup. Each cleanup records its released selectors and candidate
  observations in task history. Missing or stale records, unexplained files,
  protected metadata, environment files, symlinks and CI workflow
  edits are refused. Explanations are agent evidence for review; they cannot
  grant a repair admission or bypass validation. Required commands, test
  assertions and branch rules must not be weakened.
- An absent protected check can be recovered without an agent when the forge
  supplies complete effective classic and ruleset policy (including integration
  identity), an exact PR/head/base and latest workflow attempt, and source that
  directly links the missing job to a failed API-read prerequisite. The failed
  step must show an explicit GitHub HTTP 502, 503 or 504; credential or permission
  diagnostics veto recovery. The complete latest provider job/step table is bound
  to the immutable workflow and its bounded dependency graph. `--failed` repeats
  failed jobs and every transitive dependent, even a previously successful
  `if: always()` job. Recovery requires one uniquely diagnosed failed read job;
  additional failed jobs or unknown conclusions hold. Every source step of a
  previously executed selected job, including earlier/later steps and action
  pre/post effects, must be certified safe to repeat. Unknown provider steps,
  incomplete source/matrix coverage and unsupported repeated actions hold.
  Previously skipped selected jobs with zero steps may perform their first
  execution of the unchanged protected CI job, including analysis, local setup,
  immutable actions and check-result publication. This requires an effective
  protected context bound to that source job (or its literal matrix family) and
  the captured GitHub Actions app. Its explicit token permissions may grant
  reads and check/status/security-result publication, never unrelated writes;
  deployment environments, foreign credentials/secrets and unsupported actions
  hold. Unrelated fresh release, issue or deployment jobs have no such authority.
  Fresh unprotected jobs qualify only when their commands are supported reads.
  Source equality at the event base/head, original actor, PR/ref/head, provider
  run/attempt/suite and app bind this authority; job names or action pins alone
  grant nothing. The complete effect scope is revalidated before mutation.
  A literal `gh api repos/…` REST read may use the default GET or one explicit
  `--method GET`, `--method=GET`, `-X GET` or `-XGET`. Live PR-state workflows use
  `gh api --method GET 'repos/…' --jq '…' >> "$GITHUB_OUTPUT"`, with a fixed
  merged/open/closed projection producing one bounded `state` output. Its endpoint
  may interpolate only the provider's numeric PR expression. The historical
  `set -euo pipefail` / `state="$(gh api … | jq -er …)"` / local output and log
  template also qualifies unchanged: repository and numeric PR bindings are
  trusted, jq selects one of the same three scalars, and the only writes are
  isolated runner output and stdout. These are finite command templates, not a
  general shell parser or a workflow-specific exception.
  Repeated reads require GitHub-hosted Ubuntu using the default shell, `bash` or
  `sh` (the historical template requires `bash`). Containers, services, actions,
  working-directory and shell-startup/executable/host/config/output overrides
  hold. Token bindings must use the repository GitHub token, `GH_REPO` must use
  `${{ github.repository }}`, and `PR_NUMBER` must use the provider's numeric PR.
  Other options (including fields and input bodies), GraphQL, API writes,
  comments/echoed commands, arbitrary jq, pipelines outside the historical
  template and ambiguous shell composition hold without reservation or rerun.
  The adapter accepts literal job names and a bounded single-axis literal matrix,
  at most 100 jobs and 64 KiB of workflow source. A global cycle check visits each
  node once and each edge twice, including unrelated dependencies, so converging
  paths cannot multiply work. Cyclic, unresolved, dynamic or ambiguous
  dependencies hold. The workflow blob must match both the event base and exact
  PR head, proving the merge-ref workflow uses that DAG. Changed definitions,
  incomplete pages, source, jobs or logs, and additional administrative rules hold.
- Before rerunning failed jobs, the owner reserves one infrastructure rerun per
  repository/PR across all heads, resumes and sibling executions under the primary
  task's write lock. Its original policy, workflow source, identities and log are
  retained in history and `auto-fix/<run>/infrastructure-<workflow-run>-original.json`.
  Replies and observations are retained beside it. This budget is separate from
  the three agent repairs and draws no crew. A lost reply spends the reservation:
  subsequent polls and resumed executions observe the current attempt and status
  without repeating the mutation. A second failure, superseded attempt, changed
  policy or identity, or absent protected result holds the PR in review. Reads,
  the mutation and waiting share one absolute completion deadline. Before each
  prerequisite policy, API, source, remote or log read (including log fallbacks),
  the adapter refuses an expired budget and clamps the subprocess timeout to the
  remaining milliseconds. Expiry during a read prevents the next request and
  any new rerun or merge; pre-mutation revalidation uses that same deadline.
- Approvals, permissions, admin rules, unknown status, unreadable evidence
  outside the retry above, other infrastructure failures, and optional
  checks failing alone stop the watcher with the reason.
- The repaired candidate passes required validation, is pushed only when the
  remote branch is still at the head the repair was admitted for, and the
  watcher then checks the new head. A head that the review gate already
  approved goes back through the pipeline's re-review round before it can merge.
- Each admitted attempt is written to the primary task's history, with the
  PR, head, blocker, attempt number, crew and the draw's source, before any
  agent starts. That record removes duplicates across polls, restarts and
  owner/follower overlap: one run repairs a PR head at most once per blocker.
  A later run that finds the same head still blocked after the earlier
  attempt's run ended (it published nothing, or the head still fails) is
  admitted as a new attempt; an attempt whose run may still be live keeps
  refusing. The
  ledger caps each PR at three repairs across all heads and runs. Source-repair
  reservations for the same PR draw from that budget too, so each one leaves the
  watcher one fewer attempt. When the budget runs out, the watcher stops with
  `auto_fix_budget_exhausted`. Its message reports the combined spend (`N repairs
  are spent`), the source-repair reservation count and the watcher attempts, which
  can be an empty list when reservations use up the budget.
- A resumed job inherits repair admissions and contiguous publication edges
  only from its durable, same-job retry ancestors. It can keep watching a
  published repair or validate and lease-push an admitted local repair left
  committed before interruption. Unrelated runs confer no authority; PR,
  branch, exact-head and review guards still apply. A subsequent base conflict
  leases against that authenticated current head while retaining the original
  publication checkpoint as provenance. The refreshed candidate passes captured
  validation before publication, or before returning for independent re-review;
  concurrent remote movement is refused. Resume spends no new
  repair attempt merely to finish an inherited repair and grants no merge
  authority.
- A base refresh retains the failed and passing job identities, the base
  commit each passing job ran on and the exact base tip in
  `auto-fix/<run>/landed-<failed-head>.json` and records
  `pr_auto_fix_base_refresh` in task history before mutation. Its owner-side
  reservation bounds base refreshes independently of the repair-attempt budget.
  Old fork-point failure logs are unnecessary: the base's latest push jobs,
  read back from the tip past commits that skipped the job, supply the
  evidence. A green base alone does not prove the base fixed that one failure:
  when the head's fork point, read back the same way, passed the same job, the
  failure is the candidate's. That check does not by itself
  end the comparison. When every failed required check is candidate-owned
  that way, completion spends no base refresh and admits one ordinary repair,
  even when the pull request's one base update is already reserved. The same
  repair path holds when one check is candidate-owned and another is green
  on the base without a successful fork-point job, as long as no required
  check is known-red on the base. A known-red sibling wins over a
  candidate-owned check: the base fails that sibling with the same diagnostic,
  completion records `pr_auto_fix_base_red`, and no repair attempt is
  admitted. Other matching current-base failures also record
  `pr_auto_fix_base_red`. Ordinary agent repairs retain their existing
  admission history. Missing refresh or known-red evidence leaves the
  inherited-failure wait and the ordinary diagnosis path in
  place.
- A no-edit repair's unchanged-head rerun that fails again gets one
  base-inherited refresh of the pull request. The refresh applies when every
  failed required check passed on a newer base commit where the head's fork
  point conclusively failed the same job, no failed required check also fails
  on the base the same way, no failed log cites a path the candidate changed,
  and the base tip is not already in the head. The fork point's result is the
  newest success or failure of that job on its first-parent history, searching
  at most eight commits including the fork point and reading past commits whose
  job was skipped. If the search finds no decisive result within those eight
  commits, or the result is pending, ambiguous or unreadable, the refresh is
  held. The run reserves the pull request's base
  update before the write, recording `pr_base_update_reserved` with
  `evidence.cause: "base_inherited_refresh"` (the rerun failure, fork point,
  landed base and per-check evidence) and `pr_refresh` rows. The write merges
  the base tip into the head as one conflict-free merge commit; it never
  rebases or force-pushes. Completion adopts the refreshed head only when it
  carries base commits alone, then waits for the required checks on it. A head
  is refreshed at most once, and a refreshed head that fails again takes the
  ordinary check path. An applied refresh and a deferred throttled read are
  pending automatic recovery, not holds. A hold's reason names why no refresh
  ran or why it was refused: the failure is not base-inherited, the base tip is
  already in the head, the write conflicts with the base (the conflicting paths
  are named and the head is unchanged), the forge refused it or returned a
  result that is not a base-only merge, a reservation exists whose head did not
  move, the pull request's base update is spent, or the pull request, base or
  evidence cannot be read. A rerun already spent in CI history holds without a
  refresh.
- Closed or merged PRs, and heads superseded by a newer push, get no repair.
