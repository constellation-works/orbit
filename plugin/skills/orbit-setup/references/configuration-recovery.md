# Configuration: recovery settings

## Contents

- [Recovery settings](#recovery-settings)
  - [CI infrastructure reruns](#ci-infrastructure-reruns)

## Recovery settings

Read effective values with `orbit config show --all` before editing workspace
policy. These examples use installed commands; use the workspace's actual
generator and authorized documentation checks for its own files.

```toml
[workflow.generated_paths]
"generated-config-keys.json" = "orbit config keys --json > generated-config-keys.json"
```

Generated paths apply only to conflict recovery for a stopped rebase, not
ordinary edits or merges without conflicts. Relative globs use `*` within a
component and `**` across components; absolute paths and `..` are refused.
The agent resolves other files; the host takes upstream's generated file (the
replayed side if upstream deleted it), runs each distinct command once with the
validation environment and stages its output.

Each command has ten minutes;
nonzero exit, timeout, remaining conflict markers, or a change to HEAD, branch
or index refuses the repair. Commands must be trusted and must not stage or
commit. Verify the certified continuation's `regenerated_paths` and output.

```toml
[workflow]
unlocked_context_paths = ["**/*.md"]
# Example for config-documentation repairs: checks supported/effective policy.
unlocked_path_validation_commands = ["orbit config keys", "orbit config show"]
```

Unlocked paths control context locks, not permission to edit a file. An
all-matching task still locks its entries; owner pull admission still checks a
remote claim's full footprint. The globs are read when the process opens the
workspace. Keep code, build and CI definitions outside them.

For prose repairs, including conflict, step and final recovery, narrowing
compares the entire candidate tree with this run's last full pass of
`workflow.required_validation_commands`. Every changed path must be unlocked;
a code change brought in by rebase requires the full list. The first validation,
a changed required list, another process resuming the run and a claimed leaf
all require a full pass. Empty narrowed commands keep the full list.

Each validation log and step output records `validation_scope` (`full` or
`narrowed`, reason and compared trees); a narrowed pass never becomes the
comparison point and cannot satisfy completion evidence requiring the full
list. Verify that record, rather than an agent's account of what changed.

### CI infrastructure reruns

When every authenticated failure on a pull-request head is logged
infrastructure/transient, the watcher reruns the failed jobs instead of
starting a code-repair agent. Two keys bound that recovery; the read-only
[GitHub evidence sweep](automation-built-in.md#github-evidence-sweeps) lists such jobs
under `infrastructure` without rerunning them.

- `workflow.ci_infrastructure_retry.max_attempts`: total failed-jobs reruns for
one pull-request head. Default `3`; integer `0`..`10`. `0` disables this
recovery, so logged infrastructure failures keep their usual route.
- `workflow.ci_infrastructure_retry.backoff_seconds`: initial delay between
reruns. Default `60`; integer `1`..`3600` seconds. The first rerun is
immediate, then each reservation doubles the wait (by default `60`, then
`120` seconds, and so on).

`workflow.ci_infrastructure_signatures` selects what counts: a failed job
matches only when a line it emitted starts with one of those prefixes.
Declare only failures a rerun can clear; anything else keeps its repair
route, and no repair agent runs for a match.

`workflow.ci_infrastructure_retry.handoff_steps` covers a job that failed
while obtaining another job's output, whose log is often truncated or names
no failing command. Each entry is a step-name pattern matching the whole
name, case-sensitively, with `*` for any characters, such as
`["Download nextest archive", "Await the *"]`.

When every failed job's one
failed step, as the provider's job listing reports it, matches a pattern,
the set gets these reruns from the step names alone, without reading logs.
A failed job in any other step, or with several failed steps, leaves the set
to its usual classification. Default `[]` declares none.

Behavior notes:

- The whole failure set must be logged infrastructure/transient. Any
candidate failure excludes the set, including a candidate sibling beside a
transient failure. Flaky failures, conclusion-only cancellation or timeout
sets, and a failure whose log is only a ci-await timeout keep their existing
single rerun rather than this budget.
- Reservations are durable per head across restarts and workflow runs,
including unknown send outcomes; an unchanged provider attempt is never
resent. A new head gets its own budget.
- The existing watch or completion deadline is never extended. Backoff uses
durable reservation timestamps and avoids re-reading completed-job logs or
authenticating another rerun while one is owed. Provider throttling uses the
recorded read deferral and spends no rerun attempt.
- Once the budget is spent, `ci_infrastructure_retry_exhausted` appears in
task history naming `classification=transient` and the attempt count; no
repair agent runs and the candidate stays in review. A passing retry resumes
the existing observation with its merge authority unchanged.
- Inspecting this policy, or changing it where authorized, grants no
repair or merge authority by itself.

Discover, read and — only where authorized to change policy, for example a
disposable fixture — set the keys with the installed commands:

```sh
orbit config keys | grep ci_infrastructure_retry
orbit config get workflow.ci_infrastructure_retry.max_attempts
orbit config get workflow.ci_infrastructure_retry.backoff_seconds
orbit config show --all | grep ci_infrastructure_retry
# Only where authorized to change policy:
orbit config set workflow.ci_infrastructure_retry.max_attempts 3
orbit config set workflow.ci_infrastructure_retry.backoff_seconds 60
```

Confirm the exhausted-budget record with `orbit task show <task-id>` history.
Use `orbit config show --all` for effective values and provenance, and
`orbit doctor` to catch configuration errors before relying on a change.
