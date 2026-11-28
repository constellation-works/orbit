# Workspace tuning: policy

## Contents

- [Before you change anything](#before-you-change-anything)
- [1. Base branch](#1-base-branch)
- [2. Required validation: `workflow.required_validation_commands`](#2-required-validation-workflowrequired_validation_commands)
  - [Prose repairs: `workflow.unlocked_path_validation_commands`](#prose-repairs-workflowunlocked_path_validation_commands)
  - [Reviewer red-base claims: `review.baseline_commands`](#reviewer-red-base-claims-reviewbaseline_commands)
- [3. Footprint locks](#3-footprint-locks)
- [4. Generated files: `workflow.generated_paths`](#4-generated-files-workflowgenerated_paths)
- [5. Light delivery and review-skip tags](#5-light-delivery-and-review-skip-tags)

How to set a repository's workspace policy from evidence: validation gates,
footprint locks, generated files, crew routing and draw policy. The
[configuration reference](configuration-settings-workflow.md) says what each key does; this guide
says how to choose its value for the repository in front of you, in an order
where each decision has the inputs it needs.

Tune only what the user's workflow uses. Task tracking needs none of this. A
lane that ships one task at a time by hand needs only the validation gates. An
unattended drain is where every section below pays off.

## Before you change anything

1. **Read the effective policy.** `orbit --version`, then
   `orbit config show --all` (every key with its state: `workspace`,
   `global`, `environment`, `default`, or `unset`), `orbit config keys` (what
   this build accepts) and
   `orbit workspace show` (registered base branch). Never assume a built-in
   default is this host's value.
2. **Find the owner.** Policy is written on the owner host's workspace file
   (`<checkout>/.orbit/config.toml`). A follower replica never files policy;
   it gets copies of the validation lists only ([Roll it out](workspace-tuning-rollout.md#roll-it-out)).
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
   `env` are global; `[machine]` is global only.

   The three security keys
   `execution.codex.sandbox`, `execution.codex.approval_policy` and
   `execution.env.pass` stop inheriting the global values once a workspace
   policy file exists, so restate them there if the workspace needs
   non-defaults ([Where config lives](configuration-layers.md#where-config-lives)).

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
and leave those commands to the pipeline, except any the workspace guide names
as a pre-handoff check for a diff (such as `make ci-guards`), which they still
run before handoff.

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
- **Toolchain.** Validation resolves PATH from the owner's login shell.
  If the drain runs from a service
  manager or SSH, add toolchain directories to `workflow.validation_env.path`
  and check `orbit doctor`'s `validation-env` row. For build tools that
  honour an output-directory variable, list it in
  `workflow.validation_env.build_output_env` (`["CARGO_TARGET_DIR"]` for
  Cargo) so host validation never shares output with worker builds.
- **Align the agent guide.** If `AGENTS.md` tells implementers to run the
  same gates, say there that the pipeline runs them when they appear in
  `final_validation_commands`, except a pre-handoff check the guide names for a
  diff (such as `make ci-guards`), which implementers still run before handoff;
  otherwise implementers spend the time twice.

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
`review.before_landing_hosts`, [section 7](workspace-tuning-crews.md#7-review-layers)).

Choose the checks reviewers
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
`orbit config set` does not write them ([Review-skip tags](configuration-tags.md#review-skip-tags)).

Per-tag extra instructions (`[workflow.tag_instructions.<tag>]`) are the
place for repository rules that apply to one kind of work, such as a
migration checklist; see [Tag instructions](configuration-tags.md#tag-instructions).

Continue with [6. Crew routing](workspace-tuning-crews.md#6-crew-routing).
