---
title: "Scope: Hidden acceptance for unattended delivery"
owner: claude
last_updated: 2026-10-24
last_validated: 2026-10-24
status: Draft
feature: hidden-acceptance
doc_role: scope
type: design
summary: "Scope: an acceptance lane whose scenarios implementers cannot see or edit, written from the specification by another agent family and run after landing, so a green result is an independent signal rather than the implementer grading its own work"
tags: [hidden-acceptance, qa, auto-tasks, plugins]
---

# Scope: Hidden acceptance for unattended delivery

Status: proposed.
Bearing: The [full pre-release QA sweep](../../runbooks/qa-full-sweep.md) already
builds an attested candidate (`--build-candidate` binds every result to HEAD, the
tracked diff and untracked inputs), runs an inventory of end-to-end scenarios in an
isolated Orbit root, records NOT_RUN and BLOCKED honestly, and files deduplicated
defects without repairing them. [Automation triggers](../automation-triggers/2_design.md)
provide the `deliveries_landed` schedule that `delivery-code-review` uses for
after-landing review. The [plugin standard](../plugins/1_scope.md) lets a plugin ship
tools, auto-tasks, jobs and a private `{{plugin_state}}` directory from one manifest.
[Finding verification](../finding-verification/1_overview.md) establishes that a review-filed
defect is confirmed before it becomes work. This scope builds on all four and changes
none of their contracts.

## Problem

Orbit lands work on `agent-main` with no merge gates and no human code review; the
only continuous check after landing is `delivery-code-review`, an LLM reading the
same diff the implementer wrote. The release sign-off is meant to be the independent
check, but today the implementer writes the exam:

- **The scenarios live in the implementer's tree.** `scripts/qa-full-sweep-inventory.json`
  holds 89 scenarios: 52 are `cargo-test` pointers into the repository's own test
  suites, 22 are commands (17 of which run in-repo test scripts such as
  `crates/orbit-web/tests/http_api/dashboard_agents_browser.mjs`), 14 are harness
  builtins and 1 is a recorded coverage gap. A worker that changes a behaviour can
  change the test that checks it in the same diff.
- **A guard forces that co-edit.** The inventory pins `surface_contracts.sha256` over
  the sorted CLI, MCP, job and activity surface lists, and CI runs
  `scripts/test-qa-full-sweep.py --check` from `scripts/ci-guardrails.sh`, which fails
  when the live surface no longer hashes to the pinned value. Every surface-changing feature therefore edits its own acceptance
  entry to get green: the file has 142 commits on `agent-main`, and recent ones come
  from ordinary feature work ([ORB-17767], [ORB-17744], [ORB-17743], [ORB-17736]).
- **Scenarios are derived from the code, not from the intent.** They describe what was
  built, so they confirm behaviour rather than test it against the task's acceptance
  criteria or the public documentation.
- **It rarely runs.** `qa-full-sweep` is `enabled: false` and minted by hand before a
  release; the recent-change `qa-sweep` is disabled as well. Between releases nothing
  independent exercises the shipped behaviour.

The result: a green sweep means "the implementer's tests pass on the candidate". With
no human reviewer that is the only behavioural signal Orbit has, and it is not
independent of the thing it measures.

## Goal

An acceptance signal that implementers can neither see nor edit, written from the
specification by a different agent family, run continuously after landing, and fed
back as verified remediation tasks. It never blocks a merge and adds nothing to the
gate pipeline. The existing harness's candidate attestation, evidence handling and
honest NOT_RUN/BLOCKED accounting are reused unchanged; `qa-full-sweep` stays the
release sign-off and gains the hidden result as an input.

## Shape

### Two sets

| Set | Lives in | Who may read it | Content |
|---|---|---|---|
| **Public** | the repository (today's inventory, trimmed) | everyone, including workers | smoke checks, `cargo-test` pointers, and retired hidden scenarios kept as regressions |
| **Hidden** | the `acceptance` plugin's store, outside every repository | the author lane, the runner, the operator | black-box scenarios only |

A hidden scenario drives the installed candidate through public surfaces only: CLI,
MCP tools, the HTTP API and the dashboard. It may not reference repository test code,
`cargo test` targets or internal crate paths, so a worker cannot satisfy it by editing
a test.

A scenario record:

```yaml
id: acc-task-requeue-after-block       # stable, never reused
story: "An operator requeues a blocked task and it runs again on the next drain."
spec_refs: [ORB-NNNNN#ac-2, docs/guides/task-lifecycle.md]   # what it was derived from
surfaces: ["cli:task", "mcp:orbit_task_requeue"]
capability: local                       # local | browser | macos | linux | provider
setup: [...]                            # public commands that build the starting state
steps: [...]                            # public commands or tool calls
expect:                                 # deterministic checks first
  - exit_code: 0
  - json_path: ".status" equals "backlog"
judge: null                             # or { rubric: "...", crew: <family> } for fuzzy outcomes
authored_by: { crew: grok, model: <id>, from_delivery: <id> }
state: hidden                           # hidden | retired
```

### Store and isolation

The hidden set is a private git repository (proposed: `constellation-works/orbit-acceptance`)
owned by the `acceptance` plugin. It is never cloned into a workspace checkout or a
worker worktree, never named in a task's `context_files`, and never pasted into a task
body.

Isolation is by physical absence, not by sandbox policy. `denyRead` is enforced on
macOS by `sandbox-exec`, but Linux Bubblewrap confines writes only, so a denial would
not hold on `dk-server-2`. Each runner and author invocation clones the store into a
fresh directory under `{{plugin_state}}`, uses it, and deletes it before finishing.
A `denyRead` rule over that directory is added as defence in depth where it is
enforced.

The aim is to stop incidental overfitting (an implementer aiming at the checks it
can see), not to resist an agent that goes looking. The leak scan below covers the
rest.

### Author lane

A plugin auto-task, `acceptance-author`, runs on `deliveries_landed` (threshold 1,
`max_wait_minutes` 120, matching `delivery-code-review`). For each landed delivery it
writes new scenarios, updates or retires contradicted ones, or records a no-op reason.

- **Inputs:** the task's title, description and acceptance criteria, the design docs
  it cites, the public documentation, and the candidate binary's own `--help` and
  tool schemas.
- **Excluded inputs:** the diff, test files and implementation files. The author
  writes from intent, not from what was built.
- **Different family:** the author crew is drawn from a family other than the
  delivery's implementer (for example grok or gemini when a Claude or sol crew
  implemented it). Shared model blind spots are the main way hidden tests still miss
  defects.
- **Writes through the plugin, not a worktree:** an `orbit.acceptance.scenario.put`
  tool commits to the store. That tool is granted only to author sessions.

**Surface coverage moves here.** The runner computes the live CLI, MCP, job and
activity surface lists from the candidate. A surface with no hidden scenario becomes
an author obligation on the next pass. It never fails the implementer's CI. This
replaces the `surface_contracts.sha256` guard as the coverage mechanism.

### Runner

A plugin job, `acceptance-run`, runs on two schedules:

- **Nightly:** the whole hidden set against `agent-main` HEAD.
- **After a delivery:** the scenarios touched since the last pass, triggered by
  `deliveries_landed` after the author lane has finished for that delivery.

Each run reuses the qa-full-sweep candidate build and attestation, so results are
bound to one candidate ID. It runs scenarios in a scratch Orbit root, checks the
deterministic `expect` clauses first, and calls the LLM judge only for scenarios that
declare a rubric. The judge sees the story and the observed transcript, never the
code. The run attaches a report artifact with a per-scenario outcome and a
satisfaction rate per feature family (the inventory's existing `feature_families`
grouping carries over).

### From failure to remediation

1. **Rule out a flake:** a failing scenario is rerun `flake_reruns` times on the same
   candidate.
2. **Verify:** a verifier on a different crew from the author confirms the failure,
   following the finding-verification pattern.
3. **File the fix:** a confirmed failure becomes one remediation task tagged
   `acceptance`, deduplicated against open and closed ws_orbit tasks as qa-full-sweep
   does today. It carries the symptom, public reproduction steps and the candidate ID.
   Nothing is blocked; landing continues.
4. **Retire the scenario:** a scenario whose reproduction steps reached a task is
   spent. It moves to the public set as a regression check, and the author lane writes
   one or two fresh variants of the same behaviour for the hidden set. Hidden sets age
   this way in machine learning too.

### Signals that implementers are gaming the checks

- **Overfit gap:** per feature family, the public pass rate minus the hidden pass
  rate, tracked over time. A widening gap means implementers are aiming at what they
  can see.
- **Leak scan:** a weekly search of ws_orbit commits, PR bodies and task text for
  hidden scenario IDs and distinctive story strings. A hit retires the scenario and
  records a friction.
- **Retirement rate:** how many hidden scenarios were spent per week. A high rate
  means the hidden set is mostly reaching remediation tasks and needs more authoring.

### Fake versions of outside services (later)

Ship, PR and recovery flows depend on GitHub and on provider CLIs. A fake GitHub
(PRs, check status, merge) and scripted provider replies would let those flows run as
hidden scenarios without real tokens or rate limits. They are a later phase and live
in the plugin, not in Orbit core.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `plugins.acceptance.store_remote` | — | Git remote of the hidden store; unset disables the plugin |
| `plugins.acceptance.author_exclude_implementer_family` | `true` | Draw the author crew from a family other than the delivery's implementer |
| `plugins.acceptance.author_crews` | `["grok", "gemini"]` | Pool the author draws from after exclusion |
| `plugins.acceptance.judge_crew` | `sol` | Crew for rubric-judged scenarios |
| `plugins.acceptance.flake_reruns` | `2` | Reruns before a failure counts |
| `plugins.acceptance.nightly_cron` | `"30 4 * * *"` | Full hidden-set run |
| `plugins.acceptance.delivery_subset_max` | `40` | Upper bound on scenarios in one delivery-triggered run |
| `plugins.acceptance.run_budget_minutes` | `45` | Wall-clock cap for one run; the rest is reported NOT_RUN |

### Surfaces

- **CLI:** `orbit acceptance status`, which shows the last run, the overfit gap and
  pending author obligations. `orbit acceptance run` starts a run by hand.
  `orbit acceptance retire <id>` lets the operator retire a scenario.
- **MCP:** read-only `orbit.acceptance.status` for any session. The scenario write
  tools are granted to author sessions only.
- **Dashboard:** an Acceptance panel showing the hidden pass rate and overfit gap per
  feature family, and the latest report artifact.
- **qa-full-sweep:** the release decision cites the latest hidden run on the release
  candidate as an input. The public inventory shrinks to the public set, and
  `scripts/test-qa-full-sweep.py` drops the surface-hash assertion.

## Non-goals

- **No merge gate.** Hidden results feed remediation tasks. They never block landing,
  in keeping with the no-merge-gates rule for `agent-main`.
- **No new gate-time tests.** Hidden scenarios run outside the gate pipeline, so they
  add nothing to gate time. The policy of adding tests only with bug fixes is
  unchanged for the repository suite.
- **Not in Orbit core.** Everything ships as the `acceptance` plugin. Core keeps
  providing auto-tasks, triggers, artifacts and jobs, with no QA-specific behaviour.
- **Not secret from the operator.** Daniel can read the store. Orchestrator sessions
  can read status but do not copy scenario text into tasks.
- **Not a security boundary.** Isolation targets incidental overfitting, not a
  determined adversary.
- **Not replacing unit tests or CI.** The repository suite still guards the code. The
  hidden set guards behaviour against intent.

## Concerns and honest limitations

- **Only as good as the specification.** Scenarios derived from thin acceptance
  criteria will be thin. The author lane exposes vague criteria rather than fixing
  them; a recurring "no testable intent" no-op is a signal to improve task authoring.
- **Intentional behaviour changes cause false failures.** A delivery that deliberately
  changes behaviour contradicts older scenarios. Ordering the author lane before the
  runner for that delivery bounds this: the author sees the new criteria and retires
  or rewrites contradicted scenarios first. Misses will still happen, and each one
  costs a verifier pass.
- **Judge noise.** Rubric judging is probabilistic. Deterministic `expect` clauses are
  preferred, and the judge is limited to scenarios that need it. Flake reruns plus a
  verifier on another crew bound the false-positive rate, but do not remove it.
- **Leaks are designed in.** Every remediation task spends a scenario. If authoring
  cannot keep up with retirement, the hidden set shrinks; the retirement rate makes
  that visible.
- **Isolation on Linux is weak.** Workers on the same host could read a live clone
  during a run. Deleting the clone after each run and running the leak scan reduce
  the window. Moving the runner to a host with no worker drains would remove it.
- **Different families share some blind spots.** Excluding the implementer's family
  helps, but all frontier models share training data.
- **Cost.** A nightly run plus per-delivery author passes spend provider capacity. The
  run budget and the cap on delivery-triggered runs keep it bounded, and authoring is
  incremental per delivery rather than a full rewrite.

## Phasing

1. **Decouple and measure.** Remove the `surface_contracts.sha256` assertion from
   `scripts/test-qa-full-sweep.py`; the coverage report moves into the runner. Stop
   adding `cargo-test` scenarios to the inventory. Run qa-full-sweep nightly in
   report-only mode for two weeks to get a baseline pass rate and runtime.
2. **Hidden store and runner.** Create the `acceptance` plugin skeleton, the private
   store and the runner, reusing the candidate attestation. Seed the hidden set from
   the specification, using an author on a different family from the original
   implementers, for the inventory's 11 feature families. Do not move the existing
   scenarios over, since they come from the implementers.
3. **Author lane and remediation.** Add the `deliveries_landed` author auto-task with
   family exclusion, verified remediation filing, and retirement plus replacement.
4. **Signals.** Add the overfit gap, the weekly leak scan, the retirement rate and the
   dashboard panel.
5. **Fake outside services and rubric judging.** Add the fake GitHub and scripted
   providers, and enable rubric-judged scenarios for fuzzy outcomes.

## Open questions

1. **Which host runs the runner?** `dk-server-2` holds the candidate and the store but
   also runs the widest drains. `dk-server-1` has fewer workers, which strengthens
   isolation, but it is a follower. Followers do not file tasks, so filing would go
   through the owner.
2. **How to apply family exclusion to mixed deliveries?** A delivery may involve a
   reviewer fix commit from another family, and some sessions run Opus-led
   orchestration. Options: exclude every family that touched the delivery, or only
   the implementer's.
3. **Should the release decision require a hidden pass threshold?** Or should the
   result only be reported? A threshold makes the release honest but could hold a
   release on judge noise.
4. **Ship to other workspaces?** Should the plugin ship a generic default for other
   workspaces (nebula, orbit-graph, pulsar), or stay ws_orbit-only until the signals
   prove useful?
5. **How visible should the hidden set be to orchestrator sessions?** Status-only
   access is the proposal. Is that enough for on-call triage, or does a confirmed
   failure need the scenario text, which retires it anyway?

## Acceptance (feature level)

- Given a ws_orbit PR that adds a CLI subcommand, CI passes without the PR touching
  any acceptance file. The new surface appears as an author obligation on the next
  author pass.
- Given any ws_orbit worker worktree, task `context_files` entry or task body, no
  hidden scenario file or ID is present, and the weekly leak scan reports zero hits
  or a filed friction.
- Within 120 minutes of a delivery landing, the author lane records new scenarios,
  updated scenarios or a no-op reason for it. The recording crew's family differs from
  the delivery implementer's.
- The nightly run attaches a report artifact bound to one candidate ID, with an
  outcome for every hidden scenario and a satisfaction rate per feature family.
  Scenarios over budget are reported NOT_RUN, never PASS.
- A hidden failure that survives the flake reruns and verification produces exactly
  one remediation task with public reproduction steps. That scenario moves to the
  public set, and at least one replacement is authored.
- No merge on `agent-main` is ever blocked or delayed by the acceptance lane.

## Task References

- [ORB-17767] — removed the Incidents "Last hour only" filter and edited the QA inventory in the same PR
- [ORB-17744] — moved blocked-task triage to an escalation-job trigger and edited the QA inventory in the same PR
- [ORB-17743] — added hard-tier escalation for pilot-held proposed tasks and edited the QA inventory in the same PR
- [ORB-17736] — removed the implementers score-against-cost tier and edited the QA inventory in the same PR

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
