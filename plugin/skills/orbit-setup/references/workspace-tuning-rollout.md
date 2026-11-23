# Workspace tuning: rollout

## Optional features

| Feature | Turn on when | Reference |
|---|---|---|
| `[workflow.mid_run_steering]` | Operators correct running tasks by comment | [Mid-run steering configuration](configuration-mid-run-steering.md#mid-run-steering) |
| `machine.auto_concurrency.mode` | Admission should follow forecast fit; start in `shadow` | [Common settings](configuration-settings-workflow.md#common-settings) |

## Roll it out

1. **Write the workspace file on the owner** from the operator's shell.
   `orbit config set <key> <value>` writes scalars and arrays. Write tables
   (`generated_paths`, `type_complexity_crews`, `lock_modes`,
   `task_types`) in the file, where they stay readable;
   `review_skip_tags` and `tag_instructions` can only be written there. Keep a dated backup of the
   previous file and record the previous values where the user tracks
   operational changes.
2. **Validate the load.** `orbit config show --all` must load without error
   and show each new value with source `workspace`; a bad crew name, empty
   type pool, unknown type or invalid regex fails the load and names the key.
   `orbit doctor` for the toolchain and disabled-crew warnings.
3. **Copy the validation lists to followers.** Each follower replica of a
   pull-drained workspace needs the same `required_validation_commands` and
   `review.baseline_commands`; `orbit doctor`'s `pull-gates` row compares them.
4. **Restart readers.** Paths, lock modes and classification lists are read
   when a process opens the workspace: start a new drain and restart
   `orbit web serve`. A claim already admitted keeps the validation lists it
   captured.
5. **Verify on real work.** For validation, the next delivery attaches one
   log per required command to the task (`orbit task show`), each with its
   result and `validation_env`. For locks,
   `orbit run readiness`. For routing, `orbit task show` prints each new task's
   `crew_source` (`type:<type>/<tier>`, `pool:<tier>`, `purpose:<purpose>`), and
   `orbit run show` prints `Crew Selection:`. Report what changed, what was
   verified, and what waits on the first delivery.

## Worked example

A repository with a TypeScript app under `app/`, a Go gateway under
`gateway/`, a generated `gateway/api/openapi.json`, and hosted CI that has
gone red three times in a month on app tests and gateway lint:

```toml
[workflow]
required_validation_commands = [
  "make lint",                   # seconds; caught one red run
  "make app-deps",               # npm ci only when the lock file changed
  "npm --prefix app run check",  # typecheck
  "npm --prefix app test",       # caught two red runs
  "make -C gateway check test",
]
unlocked_path_validation_commands = ["make lint"]
light_delivery_paths = ["docs/**", "**/*.md", "!app/**", "!gateway/**", ".github/**"]

[workflow.generated_paths]
"gateway/api/openapi.json" = "make -C gateway openapi"

[[workflow.lock_modes]]
pattern = '^gateway/api/openapi\.json$'
mode = "shared"

[[workflow.task_types]]
name = "review"
tags = ["no-diff-expected"]

[[workflow.task_types]]
name = "ui"
tags = ["ui"]
paths = ["app/**"]

[[workflow.task_types]]
name = "gateway"
paths = ["gateway/**"]

[workflow.type_complexity_crews.ui]
hard = ["<best UI crew>"]

[review]
baseline_commands = ["make baseline"]   # umbrella target for reviewer claims
```

Each value traces to the survey: the required list mirrors the CI jobs that
went red, cheapest first, with dependency installation before the commands
that need it; the API schema is regenerated and shared; the types follow the two
source trees and the tags the backlog uses. Crew names are left to the user's
pools.
