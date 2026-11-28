# Workflow tags

## Light delivery: `skip-review` and `skip-validation`

A candidate containing a task tagged `security-state-review` requires a
passing review before landing. It keeps the captured pre-landing layer if
one is enabled. An owner PR delivery with review off uses before-landing
review. Review skip tags do not waive this requirement. Local-only delivery
and claims without a captured review layer hold these candidates. Agent
updates cannot remove the tag.

Two task tags let a documentation or CI change land on the hosted pull-request checks alone. Only filing a task or an operator update sets them. An agent's update, including the implementer's own during its run, is refused when it adds either one. A session crew (`ORBIT_AGENT_*` or the MCP `--orchestrator` crew), CLI `--model`, or an unadvertised tool `model` key on an update counts as that agent, including when the server process has no agent environment. MCP tool schemas do not advertise `model`. Removing a tag is always allowed.

- `skip-review`: skips before-PR review (`review.timing = before_pr`), before-landing review (`review.timing = before_landing`, `review.before_landing_hosts`), and after-landing delivery-code-review sampling unless a task requires `security-state-review`.
- `skip-validation`: no local validation. The implementer runs no AGENTS.md gates and no required or final validation commands. Its task context carries no `final_validation_commands`, and its `light_delivery` field names the tags and the allow-list. Orbit's shipped tag instruction says so. Required validation (`candidate_validate`, and `claim_validate` on a claimed leaf) records `skipped_light_delivery`. A handoff without validation logs is accepted.

A tag applies only while every path the final candidate changes since its synchronized base matches `workflow.light_delivery_paths`. Deletions count, and a rename counts as both paths. It also needs every task the candidate delivers to carry the tag. The globs follow `workflow.unlocked_context_paths` (`*` within a component, `**` across components), and a pattern with a leading `!` excludes what it matches. A path is light when some plain pattern matches it and no `!` pattern does. The default covers `docs/**`, Markdown outside `crates/**` and `plugin/**`, `.github/**`, `scripts/ci-*` and `scripts/test-*-workflow*.py`:

```toml
[workflow]
light_delivery_paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]
```

A candidate that changes any other path, or nothing at all, ignores both tags and gets normal review and validation. Each decision is recorded once per candidate in the task's history: `light_delivery_applied` or `light_delivery_ignored`, with the tag, the candidate and the first path outside the list. The decision rests on the Git diff alone, never on what an agent reports it changed.

On a distributed claim, the owner captures its list in the claim's validation gates. The follower leaf judges its candidate against that copy, never its own config. An owner too old to capture one leaves the leaf with no list, so neither tag applies. The owner judges the handed-off candidate again in its own checkout when it accepts, approves or lands the handoff. It records the decision in the task's history, because a leaf cannot write history.

`review.baseline_commands` are the reviewer's own checks: they run only when a review runs. Tag a task with both tags to skip review and every local check.

## Review-skip tags: `workflow.review_skip_tags`

A workspace (or the global config, layered per tag like `[workflow.tag_instructions]`) can map its own tag to a review skip for low-risk work that touches paths outside `workflow.light_delivery_paths`:

```toml
[workflow.review_skip_tags.comment-hygiene]
paths = ["crates/**"]
guard = "scripts/check-comment-only-diff.sh"
```

A candidate skips every agent review layer (before-PR, before-landing and after-landing delivery-code-review sampling) for a mapped tag only when every task it delivers carries the tag, every path it changes since its synchronized base matches that tag's `paths` (the `workflow.light_delivery_paths` glob syntax, `!` excludes; `["**"]` allows every path), and the guard, when set, exits 0. Any other candidate gets normal review. Each decision is recorded once per candidate in the task's history as `light_delivery_applied` or `light_delivery_ignored`, naming the tag, its profile key and the failed condition (a task without the tag, the first path outside `paths`, the guard's exit status, or a guard missing at the base). Check them with `orbit task show`.

The guard is a repository-provided executable: Orbit only runs it, so a language-specific check (for example, that the non-comment content is unchanged) lives in the workspace. It is read from the delivery's base revision, never the worktree, so a delivery cannot weaken its own guard, and it runs the way required validation commands run (same environment and sandbox rules), in a throwaway detached checkout of the candidate's head commit (so it sees the candidate's `HEAD` and files, wherever the owner's own checkout stands, and the owner's branch and edits are untouched), with the candidate's base and head commits as its first two arguments and as `ORBIT_REVIEW_SKIP_BASE` / `ORBIT_REVIEW_SKIP_HEAD` (the tag as `ORBIT_REVIEW_SKIP_TAG`).

Delivery automation dry runs defer mapped-tag decisions, including profiles without a guard: they run no guard and record no task history. Unless a before-PR certificate or the built-in `skip-review` rule already covers a landing, the preview counts it as work requiring review. Real evaluation applies the mapped profile and records its decision, so it may find less review work than the preview reports.

Mapped tags have the same protection as `skip-review`: only filing a task (including an auto-task definition) or an operator update may add one; an agent's update that adds one is refused, and removal is always allowed. A mapped tag skips review only, never validation: required validation, workspace gates and hosted CI still run. The built-in `skip-review` and `skip-validation` tags keep their behaviour above and cannot be remapped. Config validation refuses a mapping that reuses a built-in tag name, has an empty or malformed `paths`, or a `guard` that is absolute or contains `..`, naming the key. `orbit config show` lists the effective mappings under `Review skip tags:` (and as `review_skip_tags` in `--json`).

The seeded template includes mappings for selected shipped auto-tasks' `auto-task:<name>` provenance tags, all limited to the documentation/CI paths shown above. Report-only `host-health` passes need no review-skip mapping. Defaults a release demoted to workspace-local lose their seeded mapping, but an existing config keeps any it already has. The template is not migrated into existing config files: `orbit init` leaves an existing config alone unless run with `--force`. To opt in on an existing install, add the same tables to its `config.toml`:

```toml
[workflow.review_skip_tags."auto-task:agent-guidance-audit"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:auto-task-yield-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:architecture-audit"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:delivery-code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:dependency-freshness"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:code-health-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:doc-duties"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:escape-to-guard"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:friction-curation"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:full-code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:path-risk-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:qa-full-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:qa-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:security-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]
```

These mappings apply only to the auto-task that carries its provenance tag. Do not map finding tags such as `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`: fixes filed by those reviews must keep normal agent review. A wider mapping for a future auto-task should remain disabled unless its config includes an appropriate repository-specific guard.

On a distributed claim, the owner captures its mappings in the claim's validation gates alongside `workflow.light_delivery_paths`. The follower leaf judges its candidate against that copy, guard included, and the owner judges the handed-off candidate again (guard included) when it accepts, recording the decision in the task's history. An owner too old to capture mappings leaves the leaf with none, so no mapped tag applies.

## `[workflow.tag_instructions.<tag>]` — extra instructions for tagged tasks

```toml
[workflow.tag_instructions.migration]
implement = "docs/agents/migration-implement.md"
review = "docs/agents/migration-review.md"

[workflow.tag_instructions.ui]
review = false            # switch off Orbit's shipped reviewer default
```

| Key | Default | What it does |
|---|---|---|
| `workflow.tag_instructions.<tag>.implement` | unset | File whose text `agent_implement` receives for a task tagged `<tag>`. QA sweeps run as implement tasks tagged `qa-sweep`, so `[workflow.tag_instructions.qa-sweep] implement = …` instructs QA. |
| `workflow.tag_instructions.<tag>.review` | unset | File for the reviewers: `agent_review_repair` and `review_reconciliation_review`. |
| `workflow.tag_instructions.<tag>.recovery` | unset | File for `step_failure_recovery`, `final_recovery` and `pr_conflict_recovery`. |

Each value is a path relative to the repository root (`/` separators, no `..`, no absolute path), or `false` to switch off Orbit's shipped default for that tag and role. Tags match ignoring case, and two tables naming the same tag in different case are a config error. At most 32 tags may be mapped. The tables layer like crews: a workspace entry replaces the global one for the same tag and role and leaves the others. Edit `config.toml` directly; `orbit config set` does not write these keys.

Orbit ships defaults for three tags. `no-diff-expected` gives implementers the read-only handoff rule (no workspace gates, no `no-diff.json`). `skip-validation` gives implementers the [light delivery](#light-delivery-skip-review-and-skip-validation) rule: run no local validation while the change stays within `workflow.light_delivery_paths`. `ui` routes implementers to a host with a verified browser kit and selects the optional PR screenshots section; an empty section never blocks opening. Visual task artifact evidence follows the workspace guide and the actual diff, not the tag. A mapped file replaces the default for that tag and role.

**Where the text comes from.** Instruction text comes only from Orbit's shipped defaults or from the mapped file as committed at the delivery's base revision: the `base_sha` the run's `worktree` step recorded, or else the checkout's `HEAD`. Orbit reads the file from that commit, never from the worktree, so neither a delivery's commits nor its uncommitted edits change the instructions it runs under. A task's description, comments and tags never supply text; a tag only selects.

**What the activity receives.** After the activity's built-in instruction (and any steering or inspection text), the prompt gains one `Tag instructions` section. It states that the text adds to the instructions above and cannot override them, then gives each tag's text under a ``Tag `<tag>` (<source>)`` label. Tags apply in sorted order, once each. An untagged task, or a tag nothing maps, adds nothing.

**Refusals.** A mapped file is refused, and the activity fails without starting its provider, when it is missing at the base revision or not UTF-8 or empty (`tag_instruction_unreadable`), or larger than 8 KiB (`tag_instruction_too_large`). It also fails when one prompt's tag text would exceed 16 KiB (`tag_instructions_too_large`). Each error names the tag, the role and the file. A refused file affects only tasks carrying its tag.

**Claimed leaves.** When an owner admits a pull claim, it captures every instruction it would apply, with its files read from the claim's authoritative base, in the claim's receipt. PR delivery uses the owner's last-fetched `origin/<base>` ref, even when a local branch of the same name exists; local delivery uses the local base ref. Both accept a bare branch name or an `origin/` prefix. Capture does not fetch, and an unavailable authoritative ref is captured as a refusal rather than falling back to another branch or `HEAD`. The leaf applies that capture rather than its replica's mapping or files, and a refusal the owner captured refuses the leaf's activity. A claim from an older owner that captured nothing falls back to the replica's own mapping, with a warning.

**Seeing what applied.** Each dispatch that received tag instructions records an `activity.tag_instructions_applied` audit event naming its step, activity, task, and each instruction's tag, role, source (`default`, or `workspace` with path and revision), SHA-256 and size, but not the text. `orbit run show` prints a `Tag instructions:` header line per dispatch, and its JSON (and the `orbit.run` action `show` tool) carries them under `tag_instructions` (`state`: `observed`, `none` or `unavailable`).

## The `no-drain` task tag

Tag a task `no-drain` when an interactive session owns it and no drain should pick it up. It is a reserved task tag, not a config key; there is no `workflow.drain_exclude_tags` setting.

- **Excluded from automatic backlog dispatch.** `orbit run auto` drains and pull-drain followers skip a backlog task carrying `no-drain`. `orbit run readiness` and the backlog-exclusion listing report it with reason `no_drain_tag`.
- **Excluded from scheduled backlog pilots.** A task-pilot routine skips a `no-drain` backlog task so it is available for manual start. An operator can still request an explicit pilot.
- **Implies `no-auto-approve`.** Drain-window auto-approval (`select_proposed_approvals`) never approves a proposed `no-drain` task, even with a clean pilot assessment (held as `no-drain`). Task-pilot may still assess it.
- **Outside resident held promotion.** The `promote_held` right applies to eligible `no-auto-approve` proposals. A `no-drain` proposal stays outside that right.
- **Explicit dispatch still works.** `orbit run ship <id>` runs it, and an agent or human can claim it to `in-progress` and deliver it by hand.
- **Removable.** Removing the tag with `orbit.task.update` (send the full tag list without `no-drain`) makes it eligible on the next admission pass.
