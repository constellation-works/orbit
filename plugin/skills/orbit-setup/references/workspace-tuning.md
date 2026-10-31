# Tuning a workspace

How to set a repository's workspace policy from evidence: validation gates,
footprint locks, generated files, crew routing and draw policy. The
[configuration reference](configuration.md) says what each key does; this guide
says how to choose its value for the repository in front of you, in an order
where each decision has the inputs it needs.

Tune only what the user's workflow uses. Task tracking needs none of this. A
lane that ships one task at a time by hand needs only the validation gates. An
unattended drain is where every section below pays off.

## Before you change anything

1. **Read the effective policy.** `orbit --version`, then
   `orbit config show --all` (every key with its source: `workspace`,
   `global`, `built-in`), `orbit config keys` (what this build accepts) and
   `orbit workspace show` (registered base branch). Never assume a built-in
   default is this host's value.
2. **Find the owner.** Policy is written on the owner host's workspace file
   (`<checkout>/.orbit/config.toml`). A follower replica never files policy;
   it gets copies of the validation lists only ([Roll it out](#roll-it-out)).
3. **Survey the repository.** Collect, with file paths:
   - the hosted CI workflows and which jobs are *required* (branch protection
     or rulesets), plus how often each has gone red on the integration branch
     recently (`gh run list --branch <base> --status failure`) and why;
   - the local entry points (`Makefile`, `package.json` scripts, `justfile`,
     `scripts/`), their runtime, and any that need the network, secrets, a
     GPU or a running service;
   - the repository's agent guide (`AGENTS.md`, `CLAUDE.md`) and the gates it
     tells implementers to run;
   - generated files: lock files, indexes, catalogs, schema dumps, goldens and
     snapshots, and the command that regenerates each;
   - hot files many tasks touch (module registration lists, route tables,
     changelogs) — `git log --since=60.days --name-only --format= | sort |
     uniq -c | sort -rn | head -30` is a quick start;
   - the kinds of work the backlog carries (`orbit task list` tags and
     context files), which drives task types and crew routing.
4. **Keep scope straight.** Repository policy (validation, paths, types, type
   pools, review) goes in the workspace file. Crew definitions and per-crew
   `env` are global; `[machine]` is global only. The three security keys
   `execution.codex.sandbox`, `execution.codex.approval_policy` and
   `execution.env.pass` stop inheriting the global values once a workspace
   policy file exists, so restate them there if the workspace needs
   non-defaults ([Where config lives](configuration.md#where-config-lives)).

## 1. Base branch

The registered workspace `base_branch` wins over `workflow.base_branch`;
`--base` overrides both for one run. For a two-branch repository (agents land
on `agent-main`, humans promote to `main`) register with
`--base-branch agent-main` and leave the config key alone. Every
validation and baseline comparison below is relative to this branch, so settle
it first.

## 2. Required validation: `workflow.required_validation_commands`

The commands every delivered candidate must pass on the host, in order, after
base synchronization and before push or merge. The first failure stops the
list and goes to step recovery; a missing tool blocks the task as
`validation_environment`; a failure the base shares is held, not blamed on the
candidate. Implementers receive the list as `task.final_validation_commands`
and leave those commands to the pipeline.

**Decide whether you need it.** An empty list is a valid policy: the hosted
required checks gate merges and red integration-branch CI is repaired by
sweep tasks. Add local validation when hosted CI keeps going red after
merge, when red PRs cost more than local minutes, or when no hosted CI
exists. Use the CI survey: each recent red run whose cause a local command
would have caught argues for that command.

**Derive the list from CI, not from imagination.**

- Take the jobs that have actually failed, and the cheap gates that catch the
  most (format, lint, typecheck), and express each as one command that runs
  from the repository root with no prompt. Prefer an existing `make` or
  package-script target over an inline pipeline, so the repository owns the
  command and CI and Orbit run the same thing.
- **Order cheapest first.** Format and lint before typecheck, typecheck
  before unit tests, slow integration suites last.
- **Make dependency installation idempotent and first.** A fresh worktree has
  no `node_modules` or virtualenv. Add one stamp-file target that installs
  from the lock file only when it changed (for example a Makefile rule on
  `node_modules/.package-lock.json` that runs `npm ci --prefer-offline`) and
  put it before the commands that need it.
- **Skip what cannot run on the host.** Commands needing secrets, a
  deployment, a GPU without the host label, or an external service fail every
  delivery; leave them to hosted CI.
- **Prefer self-selecting test commands on large repositories.** A command
  that tests only what the diff affects and reports its selection through
  `ORBIT_VALIDATION_SUMMARY` lets Orbit replay only the failing tests on the
  base, isolate candidate-only flakes and lift red-base holds narrowly.
  A whole-suite command still works; it just costs more per rerun.
- **Watch the total.** The list runs on every candidate and again after every
  repair or rebase that changes code. Time it on the owner (`time make …`) and
  say what it adds per delivery when you propose it.
- **Toolchain.** Validation resolves PATH from the owner's login shell
  (`workflow.validation_env.shell_probe`). If the drain runs from a service
  manager or SSH, add toolchain directories to `workflow.validation_env.path`
  and check `orbit doctor`'s `validation-env` row. For build tools that
  honour an output-directory variable, list it in
  `workflow.validation_env.build_output_env` (`["CARGO_TARGET_DIR"]` for
  Cargo) so host validation never shares output with worker builds.
- **Align the agent guide.** If `AGENTS.md` tells implementers to run the
  same gates, say there that the pipeline runs them when they appear in
  `final_validation_commands`, or implementers spend the time twice.

Run every command once on the owner, from a clean checkout of the base branch,
before you configure it. A command that fails on the base today sends every
delivery through red-base handling (known-red test failures proceed with a
note; confirmed compiler or lint errors hold the task) until the base is
repaired: fix the base first, or leave that command out.

### Prose repairs: `workflow.unlocked_path_validation_commands`

When a recovery changed only paths in `workflow.unlocked_context_paths` since
the run's last full pass, these commands replace the full list. Set it to the
cheap gates that check prose (docs lint, link check, format), for example
`["make ci-fast"]`. Leave it `[]` when the required list is already cheap.

### Reviewer red-base claims: `review.baseline_commands`

Extra host commands, beyond the required list, that review settlement may
rerun on the candidate and the pinned base to confirm a reviewer's claim that a
failing check also fails on the base. They run only when a review runs, so
set them when a pre-landing review layer is on (`review.timing` or
`review.before_landing_hosts`, section 7). Choose the checks reviewers
typically run that the required list omits, such as the lint or affected-test
gate, or one umbrella target that runs the repository's whole gate set (`make
baseline`). A command the reviewer runs that appears in neither list cannot
confirm a claim. The affected-test gate whose sandbox deferrals the host
replays must also appear in one of the two lists.

## 3. Footprint locks

Two tasks whose `context_files` overlap wait for each other. Loosen this only
where an overlap cannot produce a silently wrong merge.

- **`workflow.unlocked_context_paths`** (default `["**/*.md"]`): prose and
  design docs that many tasks read beside their code. A matching entry takes
  no lock in a mixed task; a task whose entries all match still locks them.
  This list also decides when the narrowed prose validation above applies, so
  keep code, build files and CI definitions (`Makefile`,
  `.github/workflows/*`) out of it.
- **`workflow.lock_modes`**: regexes for files that merge cleanly or are
  regenerated, marked `shared`, such as append-only registration lists
  (`(^|/)mod\.rs$`, a route index) or a generated catalog. An overlap at a
  shared path is waived; the second task to land resolves any conflict. Order
  matters: the first matching entry wins, so put `exclusive` carve-outs before
  broad `shared` patterns. Anchor with `^` and `$`.

Use the hot-file list from the survey. A file that appears in most recent
deliveries and conflicts trivially belongs in `lock_modes`. A file whose
merges need judgment (a schema, a shared config, the CI workflow) stays
exclusive however hot it is. Verify with `orbit run readiness` and
`orbit task eligible` that the waits you expected to disappear did.

## 4. Generated files: `workflow.generated_paths`

A table of path glob to the command that rebuilds the file. When a stopped
rebase conflicts on a matching path, the recovery agent skips it, the host
takes upstream's side, runs the command (once per distinct command, at most ten
minutes, with the validation environment) and stages the output. This
executes commands during recovery, so map only trusted repository commands.

Map a file when all of these hold:

- it is fully derived from other tracked files, and one command rebuilds it
  deterministically from the tree (no timestamps, no network, no prompt);
- the command rewrites every path the glob matches, and neither stages nor
  commits;
- a conflict on it is mechanical, never a decision.

Typical entries: doc indexes (`"docs/INDEX.md" = "make docs-index"`),
generated key or schema catalogs, fingerprint or history files written by a
script. For a lock file use the package manager's lock-only resolve
(`npm install --package-lock-only --ignore-scripts`), never a command that
upgrades every dependency, and only when the host may reach the registry.

Do **not** map test oracles: goldens, snapshots and expected-output fixtures.
Regenerating them on conflict blesses whatever the code prints and hides the
regression the test exists to catch. Leave those to the recovery agent.

Pair each mapped file with a `shared` `lock_modes` entry, so tasks that both
touch it run in parallel and the regeneration settles the overlap. Verify a
recovery that used the mapping through its certified continuation's
`regenerated_paths` and the command result.

## 5. Light delivery and review-skip tags

`skip-review` and `skip-validation` let a task land on hosted checks alone
while its candidate changes only `workflow.light_delivery_paths`. The default
list excludes Orbit's own code trees; for any other layout replace it with the repository's docs and CI paths and exclude every code
tree with `!`. `[]` disables both tags.

For recurring low-risk work with its own tag (doc duties, comment hygiene),
map the tag in `[workflow.review_skip_tags.<tag>]` with `paths` and, when the
path test is not enough, a `guard` script. A mapped tag skips agent review
only; validation and hosted checks still run. Edit these tables in the file:
`orbit config set` does not write them ([Review-skip tags](configuration.md#review-skip-tags)).

Per-tag extra instructions (`[workflow.tag_instructions.<tag>]`) are the
place for repository rules that apply to one kind of work, such as a
migration checklist; see [Tag instructions](configuration.md#tag-instructions).

## 6. Crew routing

Routing decides which crew runs each task. Decide it in this order, because
each layer falls through to the next.

```text
explicit crew → purpose pool → type pool → complexity pool → default_crew
```

### Crews and the default

Start from `orbit doctor providers` and `orbit config show`: only crews whose
provider is installed and authenticated on the executing hosts can run. Set
`workflow.default_crew` to the strongest generally available crew; it catches
every empty or fully disabled pool. Disable a crew (`enabled = false`) rather
than deleting it, so history and pools that name it still load.

### Complexity pools: `workflow.<tier>_complexity_crews`

The backbone. Rank the available crews by quality and cost for this
repository (the user's experience, then the implementer scorecard once it
has history) and assign:

- `low`: the cheapest crews that still follow instructions reliably;
- `medium`: mid-tier crews;
- `hard` and `xhard`: the strongest crews; `xhard` is the tier the pilot never
  demotes from, so keep it to the very best.

Weight members (`["opus:100", "sol:50"]`) to set the share each gets; a pool
is all bare or all weighted. Weight `0` parks a crew without removing it.
Spread each tier across providers where quality allows, so one provider's
usage limit or failure hold redraws inside the pool instead of stalling it.

### Purpose pools: `workflow.purpose_crews.<purpose>`

Orbit's own work: `task_pilot`, `failure_recovery`, `final_recovery`,
`auto_fix`, `review`, `pre_landing_review`, `baseline_red_repair`,
`sweep_tasks`, `explorer`. `orbit init` writes them as explicit copies of the
complexity pools, so editing a complexity pool later does **not** move them:
revisit each one whenever you retune the tiers. Guidance:

- `task_pilot`, `auto_fix`, `explorer`: cheap, fast crews; the pilot also
  needs crews whose executor declares `inspection_args`.
- `failure_recovery`: crews good at diagnosis; recovery sees failures from
  every tier.
- `review` and `pre_landing_review`: a different provider family from the
  usual implementers, so review is independent; the draw already prefers a
  crew that did not implement the work.
- `final_recovery`: one strong crew; `[]` turns final recovery off.
- `baseline_red_repair`, `sweep_tasks`: leave unset to route by type and
  complexity, or pin to crews that fix CI well.

### Task types and type pools

`workflow.type_complexity_crews.<type>.<tier>` overrides one tier's pool for
one kind of work, such as routing hard UI work to the crew that renders and
checks UI best. Types come from `[[workflow.crew_performance_types]]`, which
also feed the implementer scorecard, so define them first:

- Keep the list short (about five to eight types) so each type and tier
  accumulates enough deliveries to score.
- A task takes the **first** type whose `tags` it carries or whose `paths`
  match one of its `context_files`. Put specific types before broad ones,
  and a `review` type keyed on `no-diff-expected` first so audits never
  classify as code work.
- Derive tags and paths from the backlog survey: the tags the repository
  already uses, and the source trees each kind of work lives in.
- Editing the list re-sorts history and makes the performance snapshot
  `type_list_changed` until its next hourly refresh.

Then add type pools only where a kind of work measurably needs a different
crew, usually at `hard`. Every member must be a defined, enabled crew with a
positive weight; remove a key instead of leaving it empty. Untyped work and
unlisted tiers keep the complexity pool.

### Draw policy: `workflow.crew_selection` and `workflow.crew_performance`

- **`crew_selection`**: `static` draws on the configured weights.
  `usage_pressure` scales each member by its provider's weekly usage pressure,
  so capacity about to reset is spent first. Use it when the pools span
  subscription providers that report weekly usage windows; leave `static` for
  pay-per-use accounts or a single provider.
- **`crew_performance`**: weighs draws by each crew's implementer score on the
  task's type and tier. Always start in `shadow_comparative` or
  `shadow_absolute`: draws stay unchanged and the factor is recorded. After
  draws accumulate, read
  `orbit agents scorecard --role implementers --by-type --shadow --window 30d`
  and switch to the live mode only when the cells you care about have enough
  deliveries (`n`) and the would-be shares look right. `comparative` moves
  each crew toward the types it does best; `absolute` moves work toward the
  best crew for each type. Choosing to go live is the user's decision.
- **Provider limits.** Keep `workflow.provider_limit_max_used_pct` near its
  default so a nearly exhausted window stops new draws. Use
  `provider_limit_overrides` (`provider:100` to wait for exhaustion,
  `provider:off` to only record readings) for providers whose readings
  mislead, and `provider_limit_budgets` for providers that report no usage.
  `provider_limit_explicit_crews = "pool"` lets a pinned task draw elsewhere
  instead of waiting.

## 7. Review layers

`review.timing` picks at most one pre-landing review layer: `off` (default),
`before_pr` (a fresh reviewer fixes what it finds before the PR opens) or
`before_landing` (the PR opens and is reviewed while hosted CI runs).
`review.before_landing_hosts` turns before-landing review on for named
followers only. After-landing review is the `delivery-code-review` auto-task,
not a key. Set `review.minutes` above the time the required list takes,
because review reruns checks. Pair a pre-landing layer with
`review.baseline_commands` (section 2) and a `pre_landing_review` pool
(section 6).

## 8. Analytics classification

`workflow.test_paths` and `workflow.non_code_paths` class landed lines as
tests, other or code. The defaults suit most layouts. Override only when the
repository keeps tests or assets in unconventional places, and copy any
default entries you still want, since an array replaces the default wholesale.
Changes apply to deliveries recorded afterwards.

## Optional features

| Feature | Turn on when | Reference |
|---|---|---|
| `[workflow.mid_run_steering]` | Operators correct running tasks by comment | [orbit-orchestrate](../../orbit-orchestrate/SKILL.md#mid-run-steering) |
| `workflow.explore.*` | Implementers would benefit from cheap read-only helpers; needs an `explorer` pool | [Common settings](configuration.md#common-settings) |
| `[workflow.explore_brief]` | The task pilot should store context briefs for chosen tags | [Explore briefs](configuration.md#explore-briefs) |
| `[lease]` | Interactive editors and agents share a checkout | [Path-lease policy](configuration.md#path-lease-policy) |
| `machine.auto_concurrency.mode` | Admission should follow forecast fit; start in `shadow` | [Common settings](configuration.md#common-settings) |
| `[workflow.crew_ladder]` | You want shadow evidence on whether a cheaper crew holds up for a tier; needs `experiments.enabled` | [Crew ladder](configuration.md#crew-ladder) |

## Roll it out

1. **Write the workspace file on the owner** from the operator's shell.
   `orbit config set <key> <value>` writes scalars and arrays. Write tables
   (`generated_paths`, `type_complexity_crews`, `lock_modes`,
   `crew_performance_types`) in the file, where they stay readable;
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

[[workflow.crew_performance_types]]
name = "review"
tags = ["no-diff-expected"]

[[workflow.crew_performance_types]]
name = "ui"
tags = ["ui"]
paths = ["app/**"]

[[workflow.crew_performance_types]]
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
