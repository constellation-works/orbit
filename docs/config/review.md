# Review and completion

## `[review]` and `[operation]` — automatic review

Both tables resolve built-in → global → workspace. Unknown keys warn and are ignored; known values still validate at load. Design: [review gate](../design/review-gate/2_design.md); operations: [review-gate runbook](../runbooks/review-gate.md).

| Key | Default | What it does |
|---|---|---|
| `review.timing` | `off` | The one review layer before landing. `off` (recommended default) and `before_landing` (recommended when a pre-landing review is wanted) are the supported values. `before_pr` is deprecated but still holds PR creation for a fresh reviewer that fixes what it finds; an explicit `before_pr` keeps before-PR semantics with a bounded `review.timing` deprecation warning naming the file. `before_landing` opens the PR first, reviews its published head while hosted CI runs, and merges only the head the review settled; a reviewer fix is validated and pushed onto the PR under a lease, and any outcome but approve leaves the PR open and unmerged, with the task in `review` and a typed reason (see [before-landing review](../design/review-gate/2_design.md#32-before-landing-review-of-the-open-pr-orb-14849)). PR route only. Captured at submission, so a run in flight keeps its value. New setups use `off` or `before_landing`; new run requests use `--review before-landing`. |
| `review.before_landing_hosts` | `[]` | Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries do not. See the per-host note below the table. |
| `review.minutes` | `30` | Wall-clock limit for one candidate's review, fix commit and final validation included (1..=1440). Retries and resumes share the minutes; the reviewer is stopped when they run out, and a spent review is not restarted. A base replay stopped at its review deadline is inconclusive and is not cached [ORB-15350], so raising `review.minutes` allows a retry to complete without being suppressed by an earlier cutoff. A changed candidate (after a completion rebase, say) is a new review. |
| `review.baseline_commands` | `[]` | Commands settlement may rerun on the host, besides `workflow.required_validation_commands`, to confirm a [reviewer's red-base claim](workflow-validation.md#a-red-base-is-held-not-blocked). A listed command is binding: it needs a passing record or a claim settlement reproduces, or the review settles incomplete. Captured when the delivery or claim is admitted. With the required commands, also the gates an implementer may hand off when its sandbox deferred only one kind's tests (Bubblewrap, Seatbelt, or `ps` on a macOS claimed leaf) ([required validation](workflow-validation.md#required-validation)). |
| `[[review.host_evidence]]` | `[]` | Checks a claimed leaf owes for the paths it changed or the tags its task carries; see [owed host evidence](#owed-host-evidence). A workspace list replaces the global one. |
| `workflow.purpose_crews.review` | unset | Crew for automatic review: review tasks the after-landing `delivery-code-review` auto-task mints (unset: that definition's template crew; set, it replaces that crew), and the reviewer `review.timing` runs while `workflow.purpose_crews.pre_landing_review` is unset. One crew name, or a weighted pool. See the review crew pool note below the table. |
| `workflow.purpose_crews.pre_landing_review` | unset | Crew for before-PR and before-landing review only, including the reviewer a claim captures for a `review.before_landing_hosts` machine. Unset or `[]`: `workflow.purpose_crews.review`. Written like it. See the review crew pool note below the table. |


**Review timing compatibility.** Readers also accept `none`, `after_landing` and `after-landing` as `off`, `before-pr` as `before_pr`, and `before-landing` as `before_landing`. The retired after-landing timing captures no run review; after-landing review is controlled by the `delivery-code-review` auto-task. Config writes, including doctor repairs and unrelated edits, persist only `off`, `before_pr` or `before_landing`, which the previous release also reads. Existing canonical values keep their spelling. During a mixed-version deployment, new writers therefore keep the config readable by older CLI and MCP processes. Normalize any already-written aliases with a new build before older readers use that file; editing aliases by hand while older readers remain active is unsupported. Alias normalization preserves the resolved policy and its source, so published-run policy digests remain unchanged and admissions need not be drained for this spelling-only rewrite. `review.timing = before_pr` (and `before-pr`) is deprecated but accepted with a bounded warning naming `review.timing`, the written value, and the file; the effective value and provenance stay truthful and before-PR behavior is unchanged. Deprecation alone never fails `orbit doctor` and never conceals a genuine configuration or gate error.

**Deprecated review keys.** `review.before_pr` and `review.before_landing` load into `review.timing` with a deprecation warning: `before_pr = true` is `before_pr`, `before_landing = true` is `before_landing`, both off is `off`. A workspace that sets one of them keeps the other from the global file, as it did before. Load fails, naming the keys, while they would both be on, since there is one review layer before landing. `operation.review_crew` loads into `workflow.purpose_crews.review`. A file that also sets the replacement keeps the replacement. Runs, claims and pipeline state recorded with the old switches keep their captured review: the captured admission and ship contract still carry `before_pr` and `before_landing` flags.

**Deprecated `before_pr` timing and its intentional migration [ORB-20099].** `review.timing = before_pr` (and `before-pr`) is deprecated; new setups use `off` or `before_landing` and new run requests use `--review before-landing`. An explicit `before_pr` still resolves to before-PR with its deciding layer/source, a bounded warning on load (`review.timing`, the written value, the file) and at submission (`review.timing` with the `global`/`workspace` source, or `--review before-pr` with the `run-option` source), and unchanged admission, publication order, reviewer, and budget. Migrate intentionally with the existing scoped audited writer: `orbit config set review.timing before_landing` for the workspace (add `--global` only when the global layer actually decides the timing; use explicit owner/workspace targeting for other stores). Show before/after effective policy and source (`orbit config show`); the migration preserves minutes, crew pools, validation requirements, host evidence, and unrelated settings, and distinguishes a workspace override from editing the global setting it masks. Ordinary writes, spelling normalization (`before-pr` to `before_pr`), default refresh, and doctor retired-key repair never silently change the timing nor materialize a masked global timing into another layer. Deferred removal of accepted `before_pr` input and internal before-PR handlers is a later separately scoped decision (consumed by ORB-20087 only after this deprecation lands).

**Per-host before-landing review.** `review.before_landing_hosts` is read only by the owner, and only for claimed leaves. A leaf on a listed machine reviews its PR before it lands, as `review.timing = before_landing` makes every leaf do; the owner's own deliveries and unlisted machines are unaffected. Entries are trimmed and de-duplicated. Each must be an `hm_` machine id, with letters, digits, `_` or `-` after the prefix; any other entry fails load, naming its index. The list is redundant while `review.timing` is `before_landing`, and loading fails while it is `before_pr` and the list is non-empty. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result, so a follower that echoes its probed contract matches. The machine label is caller-chosen, so this is policy, not a security boundary. Protocol revision is unchanged; the contract is in the [`orbit.task.pull` spec](../design/distributed-drain/specs/task-pull.md).

**Review crew pool.** `workflow.purpose_crews.review` is one crew name (`"sol"`) or a pool (`["sol", "grok"]`, or `["sol:3", "grok:1"]`), written like the [complexity pools](crews.md#weighting-a-pool): all bare or all weighted, and at least one member above weight `0`. Each review draws one crew by weight after three filters, each applied only when some member survives it: a member that is disabled or undefined here drops out; a member whose provider is at a usage limit here drops out; a member that implemented the reviewed work drops out (a distributed claim has no implementer, so it skips this filter). If no member survives the first filter, the review refuses to start with that member's refusal. A distributed owner draws one crew per claim, when it answers the probe. The review tasks `delivery-code-review` mints discard members with weight `0`, then apply a hard filter before those three: a member whose executor declares a `host_os` that shares no OS with the definition's `os:` tags drops out, since no host could run that task, and a single configured crew is never replaced by another. The draw is seeded by the batch, so a retried mint carries the same crew while the pool and tags are unchanged. When the filter leaves no member, the consumer holds the batch instead of failing every tick: see the after-landing note below. `[]` unsets it. Load refuses an empty name, a mix of bare and weighted entries, a weight that is not a non-negative whole number, a crew the registry does not define, a weighted pool that names a crew twice, and a pool whose weights are all `0`.

**Pre-landing review pool.** `workflow.purpose_crews.pre_landing_review` is written, admitted and drawn exactly like `workflow.purpose_crews.review`, and splits the two layers: before-PR and before-landing review (local deliveries, and the review contract an owner captures for claims) draw from it, while the review tasks `delivery-code-review` mints keep drawing from `workflow.purpose_crews.review`. So a host can review before landing on cheap, fast crews (`pre_landing_review = ["luna", "haiku"]`) and keep a stronger after-landing pool. While it is unset, or a later layer sets it to `[]`, the reviewer before landing falls back to `workflow.purpose_crews.review`, so a config without it behaves as before. A review admission records the layer of whichever key supplied its crew as `crew_source`. Before-PR and before-landing review refuse to start while neither key names a crew, and the refusal names both keys. `orbit config show --all` prints the effective pool on the before-PR and before-landing lines, with the key and layer it came from (and `workflow.purpose_crews.pre_landing_review is unset` when it is the fallback); `--json` carries it as `review.before_pr.crews`, `crew_source` and `crew_key`, and `review.after_landing.crews` and `crew_source` are the after-landing pool. The dashboard's Config tab shows both pools on the review-layer steps.

**After-landing review** is the `delivery-code-review` auto-task's `enabled` flag (`orbit auto-task toggle delivery-code-review on|off`), not a config key. `orbit config show --all` (and the `review` object in `--json`), `orbit doctor`'s `review` check, the dashboard Config tab and `orbit.drain.probe` report all three switches with their source (minutes for the first two, the next batch due for after-landing). The default text view already prints the `review.*` rows and omits that summary. The `review` check names the commit observed against `origin/<branch>` and fails once the oldest unobserved first-parent commit has waited the batch's `max_wait_minutes`, even if the tracking tip is recent. Doctor and auto-task inspection read the existing tracking ref without fetching (the local branch when there is no origin; a missing tracking ref leaves verification unknown). Delivery evaluation fetches, and defers on fetch failure without spending a coverage retry. A batch counts as reviewed only on [per-delivery evidence](../design/automation-triggers/references/operations.md#evidence-submission) whose examined and skipped paths match each delivery's frozen diff, from a review task whose agent persisted its own execution summary; otherwise the batch stays owed with a typed reason. A landing on a base that moved after review is covered too, labelled `rebased_clean`, when the landed PR's head is the reviewed candidate and a conflict-free merge of that change onto the new base reproduces the landed tree exactly ([review gate §7](../design/review-gate/2_design.md#7-delivery-coverage)). Any other base change, a head pushed after review, or a conflicting base stays an ordinary obligation.

The review tasks it mints carry `workflow.purpose_crews.review` when that key is set, whatever crew the definition names; `orbit auto-task update delivery-code-review --crew <crew>` stores the crew but prints a warning that the pool replaces it, and `orbit auto-task show` prints the same note (`crew_overridden` in `--json`). The definition's crew takes effect while the key is unset. If no positive-weight member of the pool can run under the definition's `os:` tags (each excluded member's executor declares a `host_os` outside them), the claimed batch is held and the consumer records one `crew_os_ineligible` stall instead of an error per tick. The stall names the definition, the pool key, each excluded crew with its executor and `host_os`, and the tags: `orbit auto-task show delivery-code-review` prints it (`automation.state.stall.crew_hold` in `--json`), and `orbit doctor`, `orbit config show` and the dashboard Config tab report it as an after-landing problem. Every tick re-reads the configuration; once the pool or the tags admit a positive-weight crew that can run under the `os:` tags and, while a model is refused, whose model is not refused on this host and account, the stall clears and that tick mints the held batch, with no operator recovery, unless the held batch is a retry (attempt 2 or later) whose deadline passed while it was held. That tick then marks the retry `exhausted` with reason `retry_deadline_expired` and mints nothing; `orbit auto-task recover <name> --reissue-action --reason <why>` reissues it over the same frozen obligations. A batch an earlier release left claimed with no action after its mint failed is minted the same way on the next tick. The same hold applies when a pool's members are all excluded because this host's provider account was verified to be refused their Muse model: a failed owner-local implementation run on this host (any task) records the exact requested model and provider account, and for 12 hours a newly minted review batch skips every pool member carrying that model on that account, drawing by weight and cross-implementer preference among the rest. With none left, the stall names each crew's model (`crew_hold.model_refused`) with when the refusal lapses; the frozen batch and cursor stay put. A newer verified success of the model on that host and account, or the 12-hour expiry, releases it; a different host or provider account, claimed follower evidence, generic authentication errors and a successful response that quotes the refusal never exclude a model. Explicit task crews and the drain allowlist are unaffected.

**Tasks tagged `skip-review`.** A candidate skips agent review when all its tasks carry `skip-review`, none carries `security-state-review`, and all changed paths match `workflow.light_delivery_paths`. The gate records `skip_review_light_delivery`, and the after-landing sampler excludes the landing. See [light delivery](workflow-tags.md#light-delivery-skip-review-and-skip-validation).

**Local-only delivery.** Before-PR and before-landing review need a PR, so both are refused for local-only delivery ([captured timing](../design/review-gate/2_design.md#2-captured-timing)). In a local-delivery workspace with either on, `orbit doctor`'s `review` check names the deciding layer (`review.timing = before_pr (global)` or `(workspace)`) and the remedy (ship through the PR route, or set `review.timing = off` for local delivery); readiness reports otherwise eligible local tasks as `eligible: false` with `reason` `local_route_before_pr` or `local_route_before_landing` and the layer in `detail`; and the local drain withholds them before spawning delivery (earlier per-task exclusions keep their own reasons). A workspace `timing = "off"` clears the hold, but direct `task_local_pipeline` admission still fails closed whenever its captured policy enables before-PR review.

### Owed host evidence

Each `[[review.host_evidence]]` rule has `kind` (`codeql` or `host_sandbox_test`), `name`, `paths` (workspace-relative globs), optional `tags`, `os` (`linux` or `macos`), the exact `command`, and a result `artifact` (a relative `.json` path, unique across rules). A rule Orbit could never fulfil fails load.

- A rule is owed when the candidate changes a path matching `paths`, or, when `tags` is set, when the claimed task carries any listed tag, whatever paths it changed. Tags compare case-insensitively after trimming; an empty or blank tag fails load. For example, `tags = ["security", "security-review"]` on a Linux `codeql` rule makes a security-tagged task claimed on macOS owe the Linux run even when it changes only a JavaScript manifest, so the implementer records the command `not_run` and the review holds for the owner's run. A rule without `tags` is sent to followers in its older shape; adding the field changes the distributed-drain protocol fingerprint, so a follower on an older build refuses with `protocol_skew` before it can claim.

- A `codeql` rule is owed on a host of another OS and fulfilled by the owner on `os`.
- A `linux` `codeql` rule may add `hosted_check`, the pull request check that runs exactly its `command` on every
  candidate, named as the forge reports it (`CodeQL / Analyze (rust)`, or the check's own name). On a
  before-landing review on a claim host of another known OS, such as macOS, the requirement is then
  owed to the owner (`owner_owed_evidence`) instead of holding the review: owner acceptance refuses a
  certificate that owes it any check its captured rules did not declare that way, and the owner's
  landing merges the pull request only after that check reports an explicit success on the reviewed
  head (a CheckRun `SUCCESS` or a StatusContext `SUCCESS`). It waits while the check is absent, still
  running, or completed without a success (`SKIPPED` or `NEUTRAL`). When the check failed, landing
  recovers or stops as for any failed pull request check: a failure inherited from the base gets a
  branch update, and the owner waits within the landing deadline for a newer passing base, up to three
  updates per handoff. Logged infrastructure/transient failures use the
  configured per-PR-head `workflow.ci_infrastructure_retry.max_attempts`
  budget (default 3) and its exponential backoff, when the complete
  authenticated failed-job set is eligible; a candidate failure in the set
  vetoes that shared action. Flaky failures and sets transient solely because
  jobs concluded `cancelled` or `timed_out` retain one shared rerun. A required
  check whose only unsuccessful jobs concluded `cancelled` or `timed_out` is
  transient from that conclusion even when the job log cannot be read. An
  aggregator whose log is only a `ci-await` of those conclusions, or only a
  `ci-await` timeout, counts the same way. A job that concluded `failure`
  keeps log classification, and a mixed failure is not reduced to this set.
  When that single rerun is already spent and the set is still only those
  conclusions or a `ci-await` timeout, completion records `required CI jobs
  keep cancelling or timing out` with the job names and dispatches no repair
  agent. For an untouched-path failure, a
  successful same-check push run on the accepted base ancestry authorizes one rerun of the original
  `pull_request` run; this base run may predate the candidate failure, and a base pass alone does not
  attribute the failure to the candidate. Both owner completion with `auto_fix=true` and claimed
  landing use the shared bounded eligible-set planner. It authorizes the workflow-wide action only
  for a complete authenticated failed-job set of eligible roots and dependency-only waiters; an
  independent candidate failure vetoes that retry, and uncertain evidence holds. The generic
  versioned producer contract authenticates the workflow and the producer's complete event-base
  source checkout and the workspace-owned `isolated-source-v1` execution promise. The trusted
  invocation must exclude ambient imports, shell hooks, configuration and executable search, and
  re-establish the actual producer bytes after the last candidate operation; this workspace uses
  an absolute isolated interpreter, a cleared environment and a raw git byte check before launch
  anchored through hash-verified commit/tree objects from the immutable trusted base,
  excluding mutable refs, clean filters and replacement-object indirection; execution uses
  those verified bytes without reopening the helper path.
  Runner/OS and filesystem integrity remain prerequisites; this is no attestation against a
  compromised runner. Changed/legacy/spoofed sources, an overwritten helper or records grant no
  dependency-only authority. `ci_check_rerun_reserved` consumes the shared once-per-head budget
  before sending, and survives restart or an unknown reply without another request. `auto_fix=false`
  keeps its conservative manual-repeat hold. No new recovery setting weakens required checks.
  See the [eligible-set recovery contract](../design/distributed-drain/2_design.md) and
  [operator recovery procedure](../runbooks/distributed-drain.md).
  Orbit runs `gh run rerun <id> --failed`; GitHub retains the
  original event's `GITHUB_SHA` and `GITHUB_REF`, while Orbit CI's candidate-build jobs explicitly
  check out the PR head rather than a merge with the current base. A passing rerun clears the failure;
  the same signature failing again with the passing base result returns the candidate to the backlog
  for repair. If that retry's evidence cannot be read or authenticated for
  `max_base_ci_cycles` consecutive polls, a same-check base pass sends a flaky failure to repair
  (or requeue once spent). For a candidate set with no cause evidence (no aggregate, no changed-file
  citation, and no transient, inherited, unknown or inconclusive job), an eligible
  set on a head with no prior rerun first gets one classification rerun on the
  unchanged head. `ci_check_rerun_reserved` spends the shared once-per-head budget
  before sending; a repeated candidate failure returns the task to the backlog.
  Other candidate failures keep their requeue route, and
  inherited failures keep their base update/wait route. Other failure classes with unreadable
  retry evidence stop in review naming the read failure. If untouched-path evidence remains
  incomplete, landing holds the candidate in review as `delivery_base_checks_pending` after
  `max_base_ci_cycles` observation cycles (default 3) or `max_wait_seconds` (default 3600 seconds),
  whichever comes first. At the cycle limit, a same-check pass on a base-branch push run at a commit
  containing the candidate's base, with no same-check failure there or on the landing tip, reclassifies
  the base-suspect failure as candidate (`ci_base_suspect_reclassified`) and requeues it instead of
  holding; a hold at the `max_wait_seconds` deadline never reclassifies (see the
  [distributed drain runbook](../runbooks/distributed-drain.md)). Branch
  protection's own reading of a skipped or neutral required check is unchanged. Without
  `hosted_check` — a custom query, or a workspace with no hosted CodeQL — the requirement holds the
  review like any other owed check. Declare it only for a check that really runs the rule's command:
  Orbit compares the name, not the queries the job runs. Any other kind or OS with `hosted_check`, or
  an empty one, fails load.
- A `host_sandbox_test` rule is owed on a host of `os`, which runs it outside the agent sandbox at settlement, so its command must be one settlement admits. A held `linux` rule the leaf could not run is fulfilled by a Linux owner whose host can create namespaces.
- The owner captures the rules on each claim. The reviewer records an owed check `not_run` and never attempts it, and a verdict whose only gaps are owed checks holds for them instead of blocking.

Example: the [CodeQL runbook](../runbooks/codeql-local.md#non-linux-hosts) rule (`kind = "codeql"`, `paths = ["**/*.rs"]`, `os = "linux"`, `command = "scripts/codeql-rust-local.sh …"`, `artifact = "evidence/codeql-rust-linux.json"`, and `hosted_check` only where a hosted job runs that command). The built-in fulfilment runs only that script, with optional `--ram` and `--toolchain` and one query selector. Obtain it from <https://raw.githubusercontent.com/constellation-works/orbit/v0.54.0/scripts/codeql-rust-local.sh> and save it at `scripts/codeql-rust-local.sh` in the workspace, then make it executable (`chmod +x scripts/codeql-rust-local.sh`, committed as mode 100755): the fulfilment executes that path directly, so a copy saved without the executable bit cannot run. Commit `.github/codeql/codeql-config.yml` too: the script exits 1 with `missing CodeQL configuration` without it, and the fulfilment runs the held commit's checkout. The minimal file is in the [setup reference](../../crates/orbit-core/assets/skills/orbit-setup/references/configuration-review.md). A custom `codeql` command still loads, and that fulfilment does not run it. Design: [owed evidence](../design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

### Distributed PR delivery

At claim admission the owner captures `review.timing` as the contract's `before_pr` and `before_landing` flags and, when either is on, a review contract: its version, the reviewer crew from `workflow.purpose_crews.pre_landing_review`, else `workflow.purpose_crews.review` (for a pool, one crew drawn when the owner answers the probe, which the pull that echoes it keeps), `review.minutes`, `[[review.host_evidence]]`, `workflow.required_validation_commands` and `review.baseline_commands`. The follower runs that contract, never its own settings: it needs a matching build and protocol, a before-PR gate and the captured reviewer crew (else `before_pr_reviewer_unavailable`; the implementer's crew is never substituted), and its own `before_pr` is declared for diagnostics only. The handoff's typed evidence (reviewed head and base, reviewer, certificate) must satisfy the contract, with a passing required record per captured command and the captured `review.baseline_commands` unchanged; if the owner's current lists no longer match the snapshot, acceptance fails closed with fresh-claim guidance. Protocol revision 8 carries the list: explicit `[]` means no required checks, and a contract without the field is refused. A claim on a machine listed in `review.before_landing_hosts` captures `before_landing` on while the owner's own `review.timing` is not `before_landing`; `before_pr` stays off for it. Under `before_landing` the leaf reviews its own PR after `pr_open`, merges nothing, and hands off evidence for the head it settled; evidence of the other timing or another head is refused. After-landing review stays owner-side and does not refuse a pull. Wire contract and refusal codes: [`orbit.task.pull` spec](../design/distributed-drain/specs/task-pull.md).

## `[[completion.hold]]` — keep risky completions in review

`--complete` (`orbit run ship --complete`, `orbit run auto --complete`, the dashboard and the desktop drain) lets a run merge its pull request and move the task to `done` with nobody looking, and under `workflow.distributed_completion = "done"` an owner lands claimed leaves the same way. `completion.hold` is an ordered list of rules that keep such a task in `review` instead, so a person approves it. A rule can only lower a granted `done` to `review`; it never grants completion, which stays an explicit per-invocation authorization. A run without `--complete` is unchanged.

| Key | Default | Description |
|---|---|---|
| `completion.hold` | `[]` | Ordered `[[completion.hold]]` tables. Each sets at least one of `paths` (globs matched against the files the delivery changes; a `!`-prefixed glob excludes), `complexity_at_least` (`low`, `medium`, `hard` or `xhard`), `types` (`feature`, `bug`, `refactor`, `chore`) and `tags` (compared case-insensitively), plus an optional `name` that labels the rule in the hold record. |

A rule holds when **any** of its fields matches; write one rule per reason so each hold names it. Rules are checked in order and the first match is recorded. `paths` reads the real delivery diff — for a pull request `git diff <base>...<published head>` in the run's worktree, the files the forge shows, and for a local or claimed candidate the same merge-base diff against its base — never the task's declared `context_files`; when that diff cannot be read, a rule with `paths` holds rather than let an unchecked change land.

What a hold does:

- `pr_complete` decides it before it merges anything, including after a conflict rebase or a final-recovery resume, which run `pr_complete` again on the new head. The decision is re-evaluated against the actual candidate at every merge or auto-merge request, so a repaired head the run adopts or publishes, and any refreshed head, is held on its own diff even when the original push checkpoint matched nothing. The pull request stays open and ready and the task stays in `review`, exactly as a run without `--complete` leaves them. With `--auto-fix` the run still watches and repairs the pull request as it would without `--complete`, and never merges it.
- The task gets a comment and a `completion_held` history entry naming the rule and the match, for example `held for review: paths .github/workflows/** matched .github/workflows/ci.yml (rule ci-workflows)`. `orbit task show` prints it as `Completion Hold:`, the JSON record carries it as `completion_hold` (`rule`, `field`, `pattern`, `matched`, `run_id`, and `matched_task_id` when another task of a bundle matched) while the task is still in review, and the dashboard shows it under **Held for review** and in the task history.
- The `complete_pr` step succeeds and its output names the tasks in `held_task_ids` with the `completion_hold`; nothing is completed or reported as failed.
- A no-diff completion (`no-diff-expected`, already landed) and a final-recovery `complete_no_diff` carry no diff of their own, so only `complexity_at_least`, `types` and `tags` can hold them.
- Local delivery (`--mode local --complete`) decides the rules in its merge step, against the candidate's diff, before anything lands on the base. A held candidate stays on its branch, is not pushed and not completed; the run ends with the task in `review` and the merge step's output names `held_task_ids` with the `completion_hold`.
- An owner's landing of a claimed leaf under `workflow.distributed_completion = "done"` decides the rules before it merges the pull request or fast-forwards the landing branch, against the accepted candidate's diff (the owner's rules, in the owner's config). A held candidate stops the landing attempt unmerged and unlanded, with the same comment and `completion_held` record, and its task stays in `review`. Under the `review` policy nothing is held: every landing there was already approved by a person. A candidate a person already merged or landed is settled as that fact, never held after the fact.
- The `task_complete` transition itself also refuses a task whose complexity, type or tags match a rule, so no completion route reaches `done` past it.

Approve a held task as any task in review: merge the pull request if it is still open (or land the local candidate), then `orbit task update <id> --approve` (`review -> done`), or approve it in the dashboard. Retrying the same `--complete` run or owner landing re-decides the rules, so it holds again until the change or the rules change.

Load fails on an unknown key in a rule, a rule that sets nothing, a blank `name`, tag or glob, a glob that is not a confined relative path or a `paths` list that only excludes, an unknown type, and a complexity outside `low`, `medium`, `hard`, `xhard` (`unassessed` is also refused). Only errors found after a rule decodes name the rule by its 1-based position (`completion.hold[2]`): a rule that sets nothing, a blank `name`, tag or glob, a bad glob, a `paths` list that only excludes, or `complexity_at_least = "unassessed"`. Schema errors name only `completion.hold` and the bad field or value, without a rule position: an unknown key, or an unknown complexity or type value. A typo never silently lets a risky change land.

Worked example — the rules Orbit's own workspace uses:

```toml
# CI workflows (the Homebrew formula is generated by the release workflow's
# bump-homebrew-tap job, so this rule covers it too).
[[completion.hold]]
name = "ci-workflows"
paths = [".github/workflows/**"]

# Release and installer files.
[[completion.hold]]
name = "release-and-installers"
paths = [
  "npm/**",
  "install.sh",
  "server.json",
  "RELEASING.md",
  "scripts/release*",
  "scripts/check-installer-pubkey.sh",
  "scripts/check_npm_package.py",
  "scripts/smoke-npm-install.sh",
]

# Store migrations.
[[completion.hold]]
name = "store-migrations"
paths = [
  "crates/orbit-store/src/driver/sqlite/host/schema/**",
  "crates/orbit-common/src/storage/sqlite/ledger/**",
  "crates/orbit-common/src/storage/compatibility.rs",
  "crates/orbit-store/src/driver/file/task_bundle/migrations.rs",
  "crates/orbit-common/src/migration/**",
  "crates/orbit-core/src/bootstrap/task_migration.rs",
  "crates/orbit-search/src/lexical/migration.rs",
  "crates/orbit-auth/src/store/schema.rs",
]

# Sandbox and credential code.
[[completion.hold]]
name = "sandbox-and-credentials"
paths = [
  "crates/orbit-auth/**",
  "crates/orbit-exec/src/sandbox.rs",
  "crates/orbit-exec/src/linux_sandbox/**",
  "crates/orbit-exec/src/linux_landlock/**",
  "crates/orbit-exec/src/macos_sandbox/**",
  "crates/orbit-exec/src/build_sandbox/**",
  "crates/orbit-exec/src/credential_paths.rs",
  "crates/orbit-common/src/security/**",
  "crates/orbit-core/src/adapter/engine_host/actions/sandbox/**",
  "crates/orbit-core/src/application/job/pipeline/claude_credential.rs",
  "crates/orbit-core/src/app/plugin/secret.rs",
  "crates/orbit-runtime/src/git_sandbox.rs",
  "crates/orbit-runtime/src/plugin/sandbox_mask.rs",
  "crates/orbit-runtime/src/plugin/secrets.rs",
]

# The hardest tasks.
[[completion.hold]]
name = "xhard"
complexity_at_least = "xhard"
```

Start strict and loosen with evidence: drop a rule once its held tasks are routinely approved unchanged. Design: [completion holds](../design/review-gate/2_design.md#61-completion-holds-orb-23322).
