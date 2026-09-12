# Changelog

## Unreleased

### Highlights

- **Task-pilot assessment precision**: task-pilot instructions define evidence gaps as rating-changing unknowns rather than unexecuted validation, and validate_recommendations allows assessed complexity with evidence gaps when confidence is low or medium. ([ORB-12232])

## 0.21.0

### Breaking Changes

- **Layout-v3 checkout task projections removed**: `.orbit/tasks` symlink projections and projection fields are removed in favor of canonical storage; stop older binaries before running layout migration v3 to prevent recreating obsolete links. ([ORB-11994], [ORB-12078])
- **Worktree GC activity schema updated**: custom `worktree_gc` invocations and automations must supply `target_run_id` instead of `run_id` for single-run scoping. ([ORB-11998])
- **Public config path Result API**: `OrbitRuntime::config_path` now returns `Result<PathBuf, OrbitError>` instead of an infallible `PathBuf`; callers must handle config validation errors. ([ORB-12023])
- **Linux runtime sandbox requires descriptor mounts**: Linux runtime write sandboxes now require Bubblewrap `--bind-fd` support; environments lacking descriptor-binding cannot mount writable runtime roots. ([ORB-12042], [ORB-12063])
- **Task-pilot admission contract enforced**: automatic and explicit implementation admission now withhold unassessed tasks until complexity is evaluated, while `no-diff-expected` tasks are exempt. ([ORB-11991], [ORB-12053], [ORB-12118])
- **Task complexity required on update**: human and agent `task.update` surfaces now reject `complexity: unassessed` with the create-time validation error; only automated system pipelines may assign unassessed status. ([ORB-12116])
- **Strict new-file delivery declarations**: task delivery now enforces explicit new-file intent declarations and rejects unknown untracked paths before index mutation; declare new files via `file:` selectors. ([ORB-12050])

### Highlights

- **Read-only observational Orbit modes**: SQLite databases can be safely observed without write locks even when uncheckpointed WAL frames exist, and workspaces can start without requiring semantic indexing. ([ORB-12090], [ORB-12092], [ORB-12093])
- **Rust path-validation and descriptor hardening**: file inspection and sandbox directory creation hold file descriptors through verification and mount operations, closing race conditions across runtime roots. ([ORB-12048], [ORB-12051], [ORB-12054], [ORB-12065])
- **Task-pilot automated repair assessment**: code-scanning tasks are minted without speculative selectors, while task-pilot evaluates repair complexity and persists modification targets atomically before implementation. ([ORB-11991], [ORB-12118])
- **CLI and MCP surface reliability**: fixes across tool execution, workspace filtering, MCP caller authorization, routine naming, credential redaction, and CLI output consistency improve agent ergonomics. ([ORB-12104], [ORB-12106], [ORB-12108], [ORB-12113])
- **End-to-end full-QA coverage**: comprehensive QA verification suites, deterministic process fixtures, and browser capability evidence retention ensure stability across supported platforms. ([ORB-12010], [ORB-12047], [ORB-12062], [ORB-12085], [ORB-12086])

## 0.20.0

### Breaking Changes

- **Task workspace flags clarified**: `task add --workspace` now selects the target workspace; use `--workspace-path` for task workspace-path metadata. ([ORB-11598])
- **Tool-run flags removed**: `orbit tool run` no longer accepts `--timeout` or `--output`; remove those flags from existing invocations. ([ORB-11618])
- **Operation grants upgraded**: operation-policy version 2 introduces independent review and repair controls; replace version-1 grants before delivery. ([ORB-11333])
- **Task-registry schema upgraded**: task action keys require schema version 6; older binaries that only support version 5 cannot open an upgraded registry. ([ORB-11529])
- **Semantic-index layout migrated**: semantic search now uses chunk storage with external-content FTS5; coordinate runtime upgrades because older index layouts are incompatible. ([ORB-11695], [ORB-11753])

### Highlights

- **Independent delivery review**: configure review timing and repair budgets separately from implementation, with direct repairs and explicit delivery admission. ([ORB-11333])
- **Flexible task lifecycle**: reopen completed work and make other explicitly authorized status transitions while retaining the audit trail. ([ORB-11449])
- **Shared-root workspace isolation**: multiple repositories sharing an Orbit root keep task writes and bundles in the selected workspace. ([ORB-11807])
- **Usable dashboard operations**: authorized sessions can mint and toggle automation, with compact controls that work on narrow screens. ([ORB-11557], [ORB-11559])
- **Filesystem policy for child processes**: Linux `proc.spawn` applies the declared filesystem policy to indirect child-process access. ([ORB-11514])

## 0.19.0

### Breaking Changes

- **Task-pilot activity schemas migrated**: custom `prepare_task_pilot`, `task_pilot`, and `apply_task_pilot_results` payloads must use the current required fields and `succeeded|failed` status contract. ([ORB-11330], [ORB-11331])
- **CI-failure filing schema migrated**: custom `file_ci_failure_tasks` producers must supply pilot partitions and match evidence under the current activity contract. ([ORB-11331])

### Highlights

- **More provider lanes and effort controls**: crews can forward validated effort settings, and Pi, OpenCode, and Antigravity join the supported executor catalog. ([ORB-11279], [ORB-11286], [ORB-11295], [ORB-11296], [ORB-11299])
- **Task artifacts render safely**: retrieve task attachments through the new read-only tool, with bounded presentation for validated raster images. ([ORB-11413])
- **Safer source-remote changes**: inspect or rebind a workspace source remote while preserving workspace, task, and checkout identity. ([ORB-11426])
- **Operator-controlled agent investigation**: authorized operators can invoke a bounded host-side agent run for diagnostics without changing task delivery state. ([ORB-11354])
- **Constellation-works distribution cutover**: active distribution references and the pre-release checklist now point to the current project owner. ([ORB-11427])

## 0.18.0

### Breaking Changes

- **SSH MCP caller authorization requires a protected launcher**: `orbit mcp callers authorize` now requires `--launcher` and no longer falls back to the current executable; configure the validated protected launcher before authorizing callers. ([ORB-11184])

### Highlights

- **Accurate task-flow lifecycle reporting**: `orbit task flow` now derives outflow and boundary openness from recorded transitions, including rejected tasks that later reopen. ([ORB-11183])
- **Safer direct lock reservations**: direct task-lock reservations canonicalize in-workspace selectors and reject outside-workspace paths, so they reliably conflict with task-scoped locks. ([ORB-11163])
- **Selected roots reach sweep execution**: `orbit sweep` now honors an explicit `--root` or `ORBIT_ROOT`, including when the process HOME points elsewhere. ([ORB-11165])

## 0.17.1

### Highlights

- **Managed workspace convergence**: `orbit workspace sync` safely refreshes shipped routines and other managed artifacts while preserving local edits, identity, registration, and automation choices. ([ORB-11149])
- **Security finding automation**: the disabled-by-default security sweep can collect Dependabot, Code scanning, and secret scanning findings into deduplicated, evidence-complete remediation tasks without exposing credentials to agent sandboxes. ([ORB-11124])
- **Corrective work prioritized automatically**: bounded `run ship` and `run auto` discovery now selects critical, bug, code-review, and security-review tasks ahead of ordinary feature work. ([ORB-11141])
- **Actionable friction triage**: the dashboard defaults to active frictions, adds status filtering, and provides keyboard-operable responsive detail views. ([ORB-11136])

## 0.17.0

### Breaking Changes

- **session_log tools withdrawn**: `orbit.session_log.append`, `orbit.session_log.list`, and `orbit.session_log.resolve` are gone from the public MCP/CLI agent surface → no public replacement. ([ORB-11097])
- **ci-failure-remediation auto-task removed**: the shipped `ci-failure-remediation` auto-task, catalog entry, and seed file are gone; existing workspace copies stay until removed by hand. ([ORB-11115])
- **code-review auto-task renamed**: the seeded `code-review-sweep` auto-task is now `code-review`; the old name no longer loads. ([ORB-11095])
- **Remote MCP `--operator` ignored**: destination-side caller authorization no longer honors caller `--operator` argv; a missing `~/.orbit/mcp-callers.toml` yields `agent` only. ([ORB-11052])

### Highlights

- **Agent plugins restored**: Claude Code, Codex, and Cursor plugin distribution ships again after the 0.15.0 retirement. ([ORB-11038])
- **Federated MCP includes local workspaces**: federated serve now includes the accepting machine's local workspaces automatically, so a missing destinations file is a valid local-only server. ([ORB-11044])
- **Task publication V1**: operators can bind a private Git publication repo, publish owner-only snapshots, inspect labelled read-only state, and restore without renumbering. ([ORB-11077])
- **Managed workspace routing**: managed CLI and MCP calls now keep the catalog workspace selector instead of inferring from a linked-worktree cwd. ([ORB-11117])
- **Richer task.show projections**: `orbit.task.show` can project every stable public task field, including relations, job run, and attribution. ([ORB-11119])

## 0.16.0

### Breaking Changes

- **Replica task writes report capability refusal**: `orbit.task.add` on a replica now returns `capability_refused` instead of `invalid_input`; update clients that branch on the old error code. ([ORB-11012])
- **Replica MCP access is capability-gated**: workspace-scoped control-plane reads and writes now return `capability_refused` on replica checkouts; route those calls to the owner selector. ([ORB-11021])

### Highlights

- **Federated multi-host MCP**: register an opt-in federated server, discover remote workspaces over configured SSH destinations, and route workspace-scoped calls with host-qualified selectors. ([ORB-11016])
- **Cross-workspace search**: one CLI or MCP search can query multiple registered workspaces, fuse ranked results, and attribute every hit to its source workspace. ([ORB-11027])
- **Official MCP Registry launch**: versioned registry metadata and the npm proxy now provide a validated, fail-closed path from registry installation to an unbound Orbit MCP server. ([ORB-11029])
- **Explicit docs-root overrides**: configured docs roots can opt out of gitignore filtering while retaining Orbit's `.git` and `.orbit` exclusions. ([ORB-11025])
- **Policy-bound process spawning**: activity `proc.spawn` calls now honor the child-environment allowlist and the activity's filesystem sandbox. ([ORB-11031])
- **Private SQLite state**: Orbit creates and repairs its state databases, WAL files, sidecars, and sensitive directories with private Unix permissions. ([ORB-11032])

## 0.15.0

### Breaking Changes

- **Agent plugin channel retired**: Orbit no longer ships Claude, Codex, or Cursor agent plugins; install and use the standalone Orbit CLI instead. ([ORB-10995])

### Highlights

- **Built-in code-review sweep**: fresh workspaces include a disabled code-review auto-task that teams can enable to schedule recurring reviews. ([ORB-10997])
- **Reliable concurrent task updates**: task writes now coordinate safely across simultaneous CLI, MCP, and dashboard activity. ([ORB-10988])
- **Simpler binary-first onboarding**: the Quick Start now leads with installing the standalone CLI and brings your existing repository conventions into Orbit. ([ORB-10992])

## 0.14.0

### Breaking Changes

- **Fresh installs no longer seed the legacy `qa` crew**: new `orbit init` workspaces use the supported `system` crew; update prompts that name the seeded `qa` crew. Existing user-authored `qa` configuration remains compatible. ([ORB-10955])
- **`proc.spawn` activities now require an allowlist**: v2 activities that grant `proc.spawn` must declare `proc_allowed_programs`; add the allowed programs explicitly. ([ORB-10959])

### Highlights

- **Cursor Agent Plugin support**: Orbit now ships as a Cursor-compatible Agent Plugin alongside the Claude and Codex plugin formats. ([ORB-10943])
- **More managed CLI executors**: GitHub Copilot CLI and Cursor Agent CLI are first-class workflow executors, with improved installed-agent detection. ([ORB-10946])
- **Operator-authorized MCP setup**: `orbit workspace init --mcp` can generate MCP configuration for a workspace. ([ORB-10960])
- **Dashboard workspace shell**: the dashboard gains a command rail, two-mode dock, and log status bar for faster operator navigation. ([ORB-10972])
- **Security-review automation**: fresh workspaces include a disabled security-review auto-task that teams can enable when ready. ([ORB-10950])

## 0.13.0

### Breaking Changes

- **Store schema 15 → 16**: audit actor identity splits `role` into `actor_kind` plus a normalized actor. Workspaces migrate on open; a newer store still refuses an older binary. ([ORB-10888])
- **Store schema 16 → 17**: unauthenticated MCP calls persist a self-reported actor. Same migrate-on-open / no-downgrade rule. ([ORB-10890])
- **Task complexity is required**: CLI `task add`, web `POST /api/tasks`, and `orbit.task.add` reject a missing `complexity` — pass `low`, `medium`, or `hard`. ([ORB-10892])

### Highlights

- **Dashboard delivery controls**: inspect, toggle, and mint auto-tasks; operate routines and the sweep clock; the Tasks view is clearer with safer live-log and inline edits. ([ORB-10876])
- **Complexity is first-class**: every new task must set it, and the dashboard charts it as a dimension. ([ORB-10892])
- **Governed MCP tools need an operator**: unauthenticated sessions can no longer invoke those tools. ([ORB-10916])
- **Claude Keychain OAuth under sandbox**: Claude can read the user Keychain for OAuth; an explicit `denyRead` still wins. ([ORB-10929])
- **System crew is seeded**: jobs can name a portable `system` crew; `qa` stays for compatibility. ([ORB-10877])

## 0.12.1

### Highlights

- **Release cut**: version files and the GitHub tag aligned at 0.12.1. ([ORB-10870])

## 0.12.0

### Breaking Changes

- **Seeded skill directories retired**: `orbit-task`, `orbit-search`, `orbit-workflow`, and the plugin `orbit-task-pilot` skill are gone — use the `orbit` router skill and its `references/`. ([ORB-10860])

### Highlights

- **One Orbit skill**: four seeded skills collapsed into a single `orbit` router with on-demand references. ([ORB-10860])
- **Stale skill links cleaned on upgrade**: `workspace init` removes dangling Claude/Codex skill symlinks left after skill retirement. ([ORB-10869])
- **Doctor reports leftover skill directories**: orphaned skill dirs without SKILL.md are a warning, not a clean bill of health. ([ORB-10862])
- **Safer artifact redaction**: env-value redaction no longer silently eats ordinary words in task prose. ([ORB-10867])

## 0.11.0

### Breaking Changes

- **Store schema 11 → 15**: workspaces migrate on open (friction SQLite, workspace-claim scope, learning-projection removal, invocation audit context). A newer store still refuses an older binary. ([ORB-10680])
- **Native project learnings removed**: `orbit learning`, `orbit.learning.*`, and learning search are gone — use tasks, docs, feature ADRs, and frictions. ([ORB-10736])
- **Native ADR store and tools removed**: `orbit adr` / `orbit.adr.*` and `.orbit/adrs/` are gone — author decisions in each feature's `4_decisions.md`. ([ORB-10726])
- **Duel, independent review, and `role:` retired**: planning-duel and review CLI/MCP/jobs are gone; activity YAML `role:` no longer loads — use `crew:` or the run-resolved crew, and ship via `task_pr_pipeline`. ([ORB-10628])
- **Jobs are asynchronous by default**: `orbit run job` returns a durable run immediately; `--backend http|cli|auto` is gone — use `agent_cli` and pass `--wait` when you need to block. ([ORB-10801])
- **MCP and `fs.*` builtins trimmed**: auto-task MCP advertises only list+mint; `fs.read`, `fs.delete`, and remaining `fs.*` tools are gone — use CLI/`orbit tool run` for auto-task authoring and provider-native file tools. ([ORB-10798])

### Highlights

- **Asynchronous job execution**: job submission returns a durable run immediately, with `--wait` and terminal diagnostics when you need to block. ([ORB-10801])
- **Epic-owned continuous delivery**: an epic run drains descendant work through one owned worktree and lands the epic branch only after the epic's own destination checks pass. ([ORB-10815])
- **One workspace selector grammar**: CLI and MCP accept the same registered name, logical `ws_*` id, or absolute checkout path. ([ORB-10758])
- **Network MCP transports**: `orbit mcp listen` and related tunnel/loopback paths serve isolated sessions with explicit workspace routing and claim-gated remote execution. ([ORB-10690])
- **Fail-closed Linux sandbox**: Bubblewrap plus policy-derived grants give Linux CLI agents and task-pilot a read-only, fail-closed execution path. ([ORB-10552])

## 0.10.0

- **Telemetry no longer discards completed runs**: the `invocations` insert-bound columns (`cache_create_1h_tokens`, `provider_cost_usd`) reach existing databases through a new `invocation_telemetry_columns` migration (schema v10) instead of only the v1 baseline, and a failed invocation-trace write is now logged and recorded as a `telemetry.persist_failed` event on the run rather than failing the job. ([ORB-10367])
- **PR review status clearing is documented**: `orbit.task.update` now describes its empty-string clear convention, with tool-host coverage for the persisted null result. ([ORB-10229])
- **Task PR status updates persist in v2 storage**: local tool and dashboard PATCH updates now retain `pr_status` alongside status and execution-summary changes. ([ORB-10223])
- **Bundled skills are repository-agnostic**: the embedded skill tree drops Orbit-source paths, private Constellation names, workspace-local artifact IDs, and fixed design-doc filenames; a portability regression test and byte-aligned plugin mirrors guard against reintroduction. ([ORB-10208])
- **CLI agent envelopes reach workflows**: provider-wrapped, prose-prefixed successful responses now validate and project their result object before workflow templating; malformed or failed envelopes fail closed. ([ORB-10216])
- **Temporary init validation isolates skill discovery**: test fixtures now sandbox HOME before seeding skills, preserving existing agent discovery links after temporary-root cleanup. ([ORB-10220])
- **Verbose CLI agent runs stay healthy**: stdout and stderr capture now retain bounded diagnostic prefixes and protocol-safe tails while continuing to drain providers, preserving final response envelopes and recording observed byte counts instead of killing successful runs. ([ORB-10221])
- **Scheduled provider discovery fixed**: systemd routine sweeps now use a portable user PATH so provider launchers installed in `~/.local/bin` remain available. ([ORB-10214])
- **Log rotation and pipeline spawning tolerate replaced paths**: missing log archive directories are silent no-ops, while long-lived Orbit processes resolve Linux deleted-inode executable paths back to the installed binary before spawning workers. ([ORB-10213])
- **Skill guidance reflects friction triage**: shipped skills use positional learning-show IDs and document mutable friction statuses, direct triage commands, and task-driven resolution. ([ORB-10210])
- **Store schema v4 compatibility restored**: the append-only `job_run_archive_stage` migration remains supported after an `agent-main` history rewrite dropped its registry entry, so current CLIs reopen already-upgraded stores without a destructive downgrade. ([ORB-10209])
- **Workspace routines are opt-in**: init seeds disabled auto-task, triage, and workspace-local ship-sweep definitions, preserves authored routines on re-init, and delegates shipment synchronously to the normal backlog pipeline. ([ORB-10207])
- **Workspace re-init honors explicit routing**: re-running init safely merges explicit ship-mode/base-branch changes while preserving identity, creation time, and authored routines; workspace list/show report the effective ship mode. ([ORB-10215])
- **Legacy task friction status removed**: tasks no longer deserialize, list, update, admit, or render `status: friction`; standalone friction artifacts are the only supported surface. Task attribution and record-parameter construction now share implementations without changing admission or triage behavior. ([ORB-10202])
- **Activity and job catalogs share layered loading**: typed adapters now share recursive discovery, directory deduplication, first-wins layering, and duplicate detection while preserving their distinct precedence and validation policies. ([ORB-10201])
- **Configuration admission is table-driven**: fixed settings now declare parsing, defaults, validation metadata, and CLI projection once, with generated registry/snapshot lookup completeness checks. ([ORB-10199])
- **Activity/job precedence docs match execution trust rules**: the design now distinguishes workspace-preferred job listings from default-authoritative named job and activity execution. ([ORB-10203])
- **Systemd routine workers survive sweep exit**: the user sweep service now limits shutdown to its main oneshot process, allowing detached pipeline workers to claim and complete clock-dispatched runs. ([ORB-10153])
- **Task creation surface is narrower and consistent**: task creation now exposes only legal initial statuses, list flags share repeat/comma parsing, and redundant agent/comment/instructions inputs are removed while model and managed identity attribution remain intact. ([ORB-10155])
- **CLI command meaning has one compiler-enforced registry**: dispatch, runtime bootstrap, audit metadata, JSON error formatting, and hook suppression now derive from one exhaustive command-operation declaration, eliminating wildcard policy defaults without changing CLI behavior. ([ORB-10200])

### Breaking Changes

- **Task locks returned to task administration**: `orbit locks` is now `orbit task locks`. This partially reverses ORB-10000: unlike the other removed task verbs, which were consolidated into existing verbs, locks were only relocated to a more prominent top-level surface. Moving them back completes the original surface-reduction intent without changing the MCP tool surface. ([ORB-10428])
- **Learning vote and comment surfaces removed**: `orbit.learning.upvote` and all `orbit.learning.comment.*` tools and CLI subcommands are gone — use `priority` + search rank for ranking, `update`/`supersede` for corrections, and `evidence` for provenance. ([ORB-10046])
- **`orbit task` trimmed from 19 to 12 subcommands**: seven state-transition and read-projection verbs removed. `approve`/`reject`/`unarchive`/`delete` → `orbit task update --status …`; `locks` → `orbit locks`; `prune-context` → `orbit task lint --fix`; `templates` dropped. MCP tool surface unchanged. ([ORB-10000])

### Highlights

- **Graph-tool guidance lives only in the MCP surface**: removed graph-tool invocation instructions from activity prompts and deleted the `orbit-graph` skill; graph tools self-describe when present, and essential selector/usage knowledge now lives in the `orbit.graph.*` tool descriptions. ([ORB-10151])
- **CI green again — MCP `tools/list` snapshot synced**: ORB-10149's four `orbit_auto_task_*` tools reached the MCP surface without a snapshot refresh, failing `mcp_serve_tools_list_matches_production_snapshot` on every push; the round-trip snapshot now includes them (additive — no tools removed). ([ORB-10152])
- **QA sweep is an auto-task**: the checked-in six-hour definition routes hands-on validation to `crews.qa`, dedupes open runs, and files findings through Orbit; `no-diff-expected` exempts side-effect-only tasks from workflow diff gates. Legacy QA code, config, CLI, routines, and deployment units are removed. ([ORB-10148])
- **Auto-task primitive**: recurring chores are data, not code. `orbit auto-task add/list/show/update/toggle` (+ `orbit.auto_task.*` tools) define `.orbit/auto_tasks/*.yaml` templates with a cron/interval schedule and dedupe policy; one seeded scheduler routine mints tasks from the due ones with catch-up collapse and `skip_if_open` dedupe. Provider-neutral (ADR-0217). ([ORB-10149])
- **Ship `--mode pr` fails loudly on an empty diff**: an implement step that writes nothing no longer reports success — an empty commit and a `0 commits ahead` branch now hard-fail (no empty branch pushed, no PR, no promote-to-review), the premature pre-commit `push` is removed, and the agent envelope gets `repo_root` plus a fail-closed `workspace_path`. ([ORB-10134])
- **Codex plugin documentation parity**: Website install/MCP guides now cover Codex plugin installation, upgrades, fresh-task discovery, and read-only MCP smoke checks alongside Claude; the npm proxy README clarifies how both supported plugins consume the binary without manual asset copying. ([ORB-10117])
- **Routine scheduler health over HTTP**: `GET /api/routines` on the dashboard exposes each routine's last fire (timestamp, ok/error outcome, duration) and next-due slot, so a stopped scheduler is visible remotely as a stale `last_fire` without box ssh. ([ORB-10138])
- **ADR create over HTTP**: `POST /api/adrs` records a Proposed ADR (mirroring `orbit.adr.add`), so remote orchestrators can author decisions without an on-box run; malformed payloads get a structured 400. Friction/learning create routes stay absent. ([ORB-10141])
- **Learning + ADR update routes on the HTTP API**: `PATCH /learnings/:id` (summary/scope/tags/body/evidence/priority) and `PATCH /adrs/:id` (status/tags and mutable metadata) delegate to the CLI tools, preserving supersede-don't-delete and invalid-transition rejection; no create routes added. ([ORB-10143])
- **Default failed-run triage**: a seeded `task_triage_pipeline` (+ hourly routine, `orbit run triage`) diagnoses tasks blocked by failed runs, re-backlogs environmental failures with a bounded retry budget, and leaves the rest blocked with a diagnosis attached. See ADR-0215/ADR-0216. ([ORB-10129])
- **Orphaned pending job runs now reconcile to `interrupted`**: workers claim their queued run's pid, the open-time orphan scan and `orbit doctor` cover pending runs, and `orbit run cancel <run_id>` terminalizes stuck runs. ([ORB-10070])
- **Remote dashboard task actions now reach the selected workspace**: `approve`/`reject`/`archive` used a raw `fetch()` that skipped the workspace-routing helper, so they silently no-op'd against a non-default remote workspace; the workspace selector also no longer renders its filesystem path beneath the dropdown. ([ORB-10124])
- **CI runner ENOSPC fixes**: the coverage and `ci` jobs reclaim ~25-30 GB of preinstalled runner bloat before building, set `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_TEST_DEBUG=line-tables-only` to shrink `target/`, and the coverage cache is now deps-only (no instrumented `target/`). ([ORB-10126])

- **Constellation crew catalog uses model-level names**: the checked-in Orbit workspace now offers Claude `opus`/`sonnet`/`fable` and Codex `sol`/`terra`/`luna` crews, defaulting to `opus`. ([ORB-10133])

- **Codex init default avoids nested sandboxing**: fresh Orbit configurations now seed Codex with `danger-full-access`, leaving Orbit and the host as the execution boundary. ([ORB-10131])
- **Crews are one provider-model assignment**: `[crews.<name>]` now uses flat `model`, `provider`, and `backend` fields shared by every activity role; legacy three-role crews load through their implementer assignment. See ADR-0213. ([ORB-10130])
- **Model defaults de-hardcoded and centralized**: a new `orbit-common::model_defaults` module is the single source of truth for production model defaults; default Claude CLI models are now the unversioned `opus`/`sonnet` aliases and default Codex is `gpt-5.6-terra`. Existing workspaces are unchanged until `orbit init --refresh-defaults`. See ADR-0211. ([ORB-10051])
- **`orbit web serve` defaults to global (multi-workspace) mode**: the all-workspaces dashboard dropdown is always present; `--global` is now a deprecated no-op. ([ORB-10029])
- **`orbit-cmd` extracted from `orbit-core`**: the CLI-facing command layer moved into a new internal `orbit-cmd` crate (depends on orbit-core, never the reverse). Import paths `orbit_core::command::{doctor,migrate,diagnostics,…}` → `orbit_cmd::…`; CLI/MCP/on-disk behavior unchanged. See ADR-0203. ([ORB-10016])
- **Ops runbook — `docs/OPERATIONS.md`**: a command-first day-2 operations guide (state inventory, WAL-safe backup/restore, stuck-job debugging, corrupted-DB recovery, log rotation, health checks, `orbit migrate`), linked from the README. ([ORB-10014])
- **Crate-boundary error translators enforced**: every typed error crossing into `OrbitError` now goes through its owning crate's `*_error_to_orbit` translator, enforced mechanically by a new `scripts/check-error-translation.sh` guardrail. ([ORB-10013])
- **Versioned `.orbit/` upgrade path — `orbit migrate`**: the whole workspace layout is now versioned via a `state/layout.version` marker and an ordered migration registry; older layouts auto-migrate on open, newer ones refuse to open. `orbit migrate [--dry-run]` reports and inspects. ([ORB-10012])
- **One `Selector` parser instead of two**: the duplicated selector grammar is unified on the `orbit-common` implementation, with `orbit-graph-extract`'s module now a re-export shim; orbit-common's surface now recognizes `module:`/`command:`. See ADR-0202. ([ORB-10011])
- **CI guardrails: coverage visibility, CHANGELOG freshness, declared MSRV**: an informational `cargo-llvm-cov` coverage job, a new `check-changelog-freshness.sh` guardrail, and a declared `rust-version = "1.88"` MSRV enforced by a pinned-toolchain CI job. ([ORB-10010])
- **Policy deny rules can no longer be dodged by respelling a path**: glob-path normalization rebuilds paths from their normal components so `secret/./key.txt`, `secret//key.txt`, and trailing-slash spellings all match rules for `secret/key.txt`; adds Linux enforcement integration tests and a CI sandbox/policy job. ([ORB-10009])
- **MCP round-trip integration tests + a session-workspace wire fix**: end-to-end MCP coverage over real transports with snapshotted `tools/list` payloads; fixes a wire bug where `_meta.orbit.workspace` never reached tool dispatch over a real transport. ([ORB-10007])
- **Dashboard endpoint integration tests**: axum-level coverage of the `/api` surface over on-disk temp workspaces — `GET /api/audit`, adversarial artifact-serving traversal/symlink cases, `POST /workflows/ship` in global mode, and workspace-selection error paths. Test-only. ([ORB-10008])
- **Task migration between machines**: new `orbit task export`/`import` pack and restore task bundles as a portable `tar.zst` with integrity validation and collision renumbering; `orbit task reindex` rebuilds the index from bundles, and `--task-id-start` seeds disjoint id ranges. ([ORB-00034])
- **`orbit web connect <ssh-host>`**: view a remote workspace's dashboard over an auto-managed SSH tunnel — picks a free port, starts remote serve, forwards, opens the browser, and tears down on Ctrl-C. ([ORB-00029])
- **Global, multi-workspace dashboard**: `orbit web serve` outside any workspace (or with `--global`) serves every registered workspace via a workspace-keyed runtime map, with a header selector and an aggregate "All workspaces" task view. ([ORB-00030])
- **Dashboard shows each workspace's filesystem location**: the global dashboard surfaces where each workspace lives on disk (home-abbreviated) in the header selector and task Details box, disambiguating same-named workspaces. ([ORB-00037])
- **Aggregate dashboard view no longer 400s**: an `isAggregateView()` predicate gates per-workspace fetches so the "All workspaces" view fetches only the aggregate task list and shows a placeholder for workspace-specific panels instead of erroring. ([ORB-00039])
- **Aggregate 400-guard extended to the remaining dashboard tabs**: the shared `isAggregateView()` guard now also covers the Audit, Knowledge, and scoreboard-window fetches in the "All workspaces" view. ([ORB-00040])
- **Aggregate 400-guard completed**: closes the last gaps — the Diagnostics tab and the stale learning/ADR/friction detail panels now show the aggregate-mode placeholder instead of firing per-workspace endpoints. ([ORB-00044])
- **Versioned store-schema migrations**: the store SQLite DB tracks numbered migrations in a `schema_meta` ledger, applies each atomically, adopts pre-ledger DBs via an idempotent baseline, and refuses to open a DB written by a newer orbit. ([ORB-10003])
- **SQLite reads no longer queue behind writes**: the shared store checks out per-call read-only connections from a small pool while writes keep the single WAL writer, so a read completes even while a write transaction is open. ([ORB-10004])
- **Job-run checkpoint/resume for hard interruptions**: the v2 executor persists a recovery checkpoint after each completed step; dead-owner runs finalize to a new `interrupted` state, and `orbit job resume <run_id>` replays only the remaining steps. ([ORB-10002])
- **One shared SQLite pragma helper**: WAL/busy_timeout/foreign_keys/synchronous setup is now centralized in `orbit_common::utility::sqlite::apply_default_pragmas`, fixing per-store drift. ([ORB-10004])
- **Always-on operability: detailed healthz and `orbit doctor`**: `/healthz?detailed=true` runs per-check probes, and `orbit doctor` runs workspace pass/warn/fail health checks. ([ORB-10005])
- **Retry hygiene for external calls**: step backoff now uses full jitter, the agent-CLI and embedding-companion transports classify retryable-vs-permanent failures, and `validate_job` rejects invalid `retry:` blocks before execution. ([ORB-10006])
- **Internal refactor — execution-context decomposition + scoreboard consolidation**: `orbit-engine`'s `context.rs` is split into a `context/` module by concern, and `orbit-store`'s scoreboard shapes share one implementation. No behavior change. ([ORB-10015])
- **Dashboard task-create workspace selection hardened**: `POST /api/tasks` uses `?workspace=<id>` for selection (unknown → 404, inactive → 400) and now rejects a stray `workspace` body key with a 400 — fixing the bridge `task_create` workspace no-op. ([ORB-00042])

## 0.9.2

### Highlights

- **`orbit graph` is back as a thin CLI wrapper**: the v2 graph is now reachable from the single `orbit` binary via `orbit graph {sync, search, show, refs, callees, impact, trace, overview, implementors, deps, version, db-path, clean}`, not only the standalone `orbit-graph-cli` binary or the in-process MCP adapter. `orbit-graph-cli` is lib-ified (lib + bin) so both front ends share one command layer with no duplication; the agent-facing graph surface is unchanged (still MCP-only). This amends the ADR-0198 consequence that dropped the subcommand. ([ORB-00396], ADR-0199)
- **Task store crash durability hardened**: the task registry SQLite connection now runs with `synchronous=FULL`, and task-bundle creation fsyncs the bundle directory's parent before returning — so a power loss or OS crash mid-write can no longer leave the task store or a freshly-created bundle in a torn, unrecoverable state. ([ORB-00395], [ORB-00394])

## 0.9.1

This release completes the orbit-graph v2 migration — v1 (orbit-knowledge) is fully decommissioned and v2 is now the only graph surface — and adds a stable protocol for plugging in out-of-process executors.

### Highlights

- **orbit-graph v2 is now the sole graph surface**: the orbit-knowledge (v1) crate and its `orbit.graph.*` builtin tools are removed, the `orbit graph` CLI command is dropped (the v2 graph ships as the standalone `orbit-graph-cli` binary plus an always-on in-process MCP adapter), and `orbit init` no longer builds a graph up front — v2 syncs on demand. The MCP graph surface is now search / show / refs / callees / impact / trace / overview / implementors / deps; `pack` is gone and `callers` is folded into `refs`. ([ORB-00391], [ORB-00389])
- **External Executor Protocol v1**: register a homegrown agent or CLI as an Orbit executor without forking core, through a documented stdin/stdout envelope contract and dynamic out-of-process registration. ([ORB-00384])
- **Graph reads are faster and more accurate**: queries are index-backed and decoupled from a full-worktree rescan, `show` returns UTF-8 text instead of a raw byte array, `trace` resolves far more command handlers, and `refs`/`impact` fall back to fuzzy-name matches when the precise floor is empty. ([ORB-00377], [ORB-00380], [ORB-00386], [ORB-00387], [ORB-00381])

## 0.9.0

This is a security-hardening release: the bulk of the changes close secret-leak, sandbox-escape, network-exposure, and workspace-containment issues found in a focused audit (ORB-00355–ORB-00374).

### Breaking Changes

- **`Shell` activity/job type removed**: the v2 `type: shell` activity surface is deleted end to end — a workspace activity or job declaring `type: shell` now fails to deserialize at load instead of executing. The shell program allowlist was self-asserted within the same workspace-supplied YAML (the gate was a tautology) and `run_shell` spawned with no OS sandbox, cwd confinement, or policy consultation. The `shell_reference` asset and the pure-shell job fixtures/demos are removed; use the sandboxed agent-CLI execution path instead. ([ORB-00374], [ORB-00363])
- **`[execution.env] inherit` config key removed**: an auto-discovered workspace `config.toml` could set `inherit = true` and — because workspace config replaces global — flip agent subprocess spawning to full environment inheritance, leaking the orbit process's API keys and tokens into spawned agents. Env inheritance is now fixed to `false` and is no longer configurable; only the `pass` allowlist remains tunable. Stale `inherit` keys are silently ignored. ([ORB-00365])

### Highlights

- **Secret redaction hardened across audit and logs**: redaction now covers non-UTF-8 audit blobs (previously bypassed entirely), error/bytes fields recorded via `tracing`, and Gemini's HTTP transport no longer places the API key in the URL query string. The pattern redactor also learned major Google / SCM / cloud key formats it previously missed. ([ORB-00358], [ORB-00367], [ORB-00371], [ORB-00359])
- **Sandbox and execution confinement tightened**: the macOS sandbox profile no longer grants unrestricted `file-read*` plus `network*`, an SBPL `(?i)` inline-flag bug that disabled sandbox-exec for all macOS CLI runs is fixed, the `run_command` automation action no longer shell-interpolates rendered templates, and unbounded child stdout/stderr capture (a memory-exhaustion DoS) is now bounded. ([ORB-00370], [ORB-00372], [ORB-00364], [ORB-00362])
- **Dashboard network exposure closed**: `orbit web serve` now refuses to bind any non-loopback host (only `127.0.0.0/8` and `::1`) — the unauthenticated read/write API can no longer be exposed to the network via `--host 0.0.0.0`; bind loopback behind an authenticated tunnel/reverse proxy for remote access. Stored XSS from untrusted task/learning/ADR/friction content rendered in the dashboard is also fixed. ([ORB-00360], [ORB-00355])
- **Secret-file policy matching fixed**: `denyRead`/`denyModify` globs now match dotenv-variant secret files (`.env.local`, etc.), policy glob matching is no longer case-sensitive on case-insensitive filesystems, and audit blobs and JSONL logs are no longer written world-readable. ([ORB-00357], [ORB-00366], [ORB-00368])
- **Workspace containment enforced**: a malicious workspace can no longer override seeded `.orbit/resources/{jobs,activities}/` assets, and graph operations stay confined to the worktree even when an MCP session omits the workspace. ([ORB-00356], [ORB-00361])

## 0.8.2

### Fixes

- **Intel-macOS semantic companion dropped from the release**: `orbit-search-companion` cannot link on `x86_64-apple-darwin` — `ort-sys` (2.0.0-rc.12) ships no ONNX Runtime prebuilt for Intel macOS. The Intel-mac release leg now builds the `orbit` CLI only, and the release publishes companion binaries for macOS-arm64, Linux x86_64, and Linux arm64 only. The Intel-mac CLI tarball still ships; semantic search is unsupported on Intel Macs (it never successfully shipped). Together with the glibc fix in 0.8.1, this completes recovery of the `v0.8.0` release, whose GitHub Release never published while `@orbit-tools/cli@0.8.0` was already on npm. ([ORB-00351])

## 0.8.1

### Fixes

- **Linux release build of the semantic companion**: `orbit-search-companion` failed to link on the release runners — its prebuilt ONNX Runtime (via `ort` / `fastembed`) references glibc 2.38+ symbols (`__isoc23_*`) that don't exist on `ubuntu-22.04` (glibc 2.35), failing both Linux build legs and blocking the GitHub Release. The release workflow now builds the Linux companion on `ubuntu-24.04` (glibc 2.39) while keeping the `orbit` CLI on `ubuntu-22.04`, so the CLI's glibc-2.35 compatibility floor is preserved. This fixes forward after the `v0.8.0` GitHub Release failed to publish while `@orbit-tools/cli@0.8.0` had already been published to npm. ([ORB-00350])

## 0.8.0

### Breaking Changes

- **Agent MCP surface slimmed; `orbit.review-thread.*` alias dropped**: six tools moved off the agent-facing MCP surface to `register_inactive` — `orbit.task.reject`, `orbit.friction.list`, `orbit.friction.show`, `orbit.friction.resolve`, `orbit.learning.comment.list`, and `orbit.learning.upvote`. They stay reachable via the CLI (`orbit tool run`) and `runtime.run_tool`, but `tools/list` over MCP no longer advertises them and `orbit mcp serve` rejects agent calls to them — these are operator/triage actions, while agents file friction via `orbit.friction.add` and discover learnings via `orbit.search`. Separately, the duplicate `orbit.review-thread.*` alias (added in ORB-00273) is removed; the canonical `orbit.task.review_thread.*` form that the README, the `orbit-review-task` skill, and the audit/redaction policy reference is unchanged. Seeded activity allowlists and skill docs were updated to drop the stale references. `EXPECTED_INACTIVE_TOOL_NAMES` length canary moves 21 → 27. ([ORB-00346], [ORB-00348])

### Highlights

- **Dashboard and scoreboard redesign**: the dashboard adopts the new visual mock and the scoreboard gets a phase-1 reskin plus a window-aware backend — a `?window=` selector (1h / 24h / 7d / 30d / all) now scopes scoreboard metrics, on an additive snapshot schema bump (v5 → v6). ([ORB-00345], [ORB-00336], [ORB-00337])
- **Crew and duel composition adapt to detected agents**: `[workflow].default_crew` and `[duel].candidates` are now derived from the agent families detected at `orbit init` rather than hard-coded, so a workspace's defaults match the agents actually installed. ([ORB-00347])
- **Semantic-companion install hardened**: the release workflow now publishes the standalone `orbit-search-companion-*` binaries alongside the CLI tarballs, so `orbit semantic install` can resolve signed companion assets from the GitHub Release. ([ORB-00299])

## 0.7.1

### Fixes

- **Rotate release signing key and recover package installs**: adds `orbit-release-key-3` to the shell and npm installer trust sets, makes it the canonical `plugin/npm/release-signing.pub`, and documents the new current signing path. This patch fixes forward after the `v0.7.0` GitHub Release publish failed before assets were created while `@orbit-tools/cli@0.7.0` had already been published. ([ORB-00292])

## 0.7.0

### Breaking Changes

- **Agent MCP surface trimmed**: six admin/destructive tools moved off the agent-facing MCP surface — `orbit.task.delete`, `orbit.task.lint`, `orbit.semantic.uninstall`, `orbit.adr.list`, `orbit.learning.prune`, `orbit.learning.comment.delete`. They remain registered (`register_inactive`) and reachable via the CLI (`orbit task delete`, `orbit adr list`, etc.) and `runtime.run_tool`; `tools/list` over MCP no longer advertises them and `orbit mcp serve` rejects calls to them. Agents that need ADR enumeration use `orbit.search --kind adr`. A new `orbit adr list` CLI subcommand routes through the tool to preserve filter parity (`--include-remote`, `--status`, `--owner`, etc.). `EXPECTED_INACTIVE_TOOL_NAMES` length canary in `crates/orbit-cli/src/command/mcp/tests/mod.rs` bumps 15 → 21. ([ORB-00289])
- **Semantic companion lookup no longer searches `$PATH`**: normal runtime lookup uses the managed `~/.orbit/embed/bin/` install only. Dev/CI overrides must set `ORBIT_SEARCH_COMPANION=<absolute path>` plus `ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE=1`; managed downloads now require a release-signed checksum manifest before replacement or execution. ([ORB-00265])
- **Envelope and embedding rebuild verbs renamed**: `orbit docs reindex` / `orbit.docs.reindex` is now `orbit docs index` / `orbit.docs.index` for doc-corpus embeddings, and `orbit learning reindex` / `orbit.learning.reindex` is now `orbit learning sync` / `orbit.learning.sync` for YAML-to-SQLite envelope reconciliation. No compatibility aliases are retained. ([ORB-00206], [ORB-00244])
- **`orbit search` mode split and per-kind status syntax**: the query surface now has three visible CLI forms: `orbit search <query>`, `orbit search similar <id>`, and `orbit search path <path>`. The old `orbit search --semantic <id>` and `orbit search --path <path>` forms are removed. `--status` now requires `kind:value` tokens such as `task:open,doc:active,adr:proposed`; bare tokens like `--status open` are rejected. CLI `--field` / `--model` and MCP `field` / `embedding_model` are removed from `orbit search`; MCP `model` remains provenance-only. ADR-0179 supersedes ADR-0175. ([ORB-00205])
- **Per-domain `search` subcommands removed; cross-kind filters on `orbit search`**: `orbit task search`, `orbit docs search`, `orbit learning search` (and the matching `orbit.task.search`, `orbit.docs.search`, `orbit.learning.search` MCP tools) are removed. Replacement: `orbit search --kind {task,doc,learning,adr,all} <query>` for content similarity and kind-specific `list` commands for structural filters. `orbit search --all`, `--tag`, per-kind `--status`, and `orbit search path <path>` cover the previous include-superseded/path/tag use cases. `orbit learning list --path` now uses glob containment rather than exact string match. ([ORB-00202])
- **`orbit search` flag rename**: free-text vector ranking is now `--hybrid` / `hybrid: true`; task-neighbor lookup is now `--semantic <id>` / `semantic: "<id>"`. The old `--semantic` boolean and `--related <id>` surfaces are hard-removed; JSON mode values are now `hybrid` and `neighbor`. Historical phase-1 audit payloads carrying `semantic: true` are orphaned by the no-shim rename. ([ORB-00204])
- **Design-doc tooling retired**: `orbit design check`, `orbit design init/list/show`, `orbit.design.*`, the `orbit-design` skill, the wrapper script, and `make check-design-docs` are gone. Use `orbit-docs`, `orbit.docs.*`, and the same-PR doc update rule; `orbit workspace init --design` still seeds `docs/design/CONVENTIONS.md`. ([ORB-00112], [ORB-00177], [ORB-00182])
- **Search namespace split**: `orbit.semantic.search`, `orbit.semantic.related`, and the `orbit-semantic` skill are removed in favor of `orbit.search` and `orbit-search`; `orbit semantic reindex` is now `orbit semantic index`. Historical `semantic.search` / `semantic.related` audit event names are orphaned by this hard break because there are no external audit-history consumers yet. ([ORB-00196])
- **Low-use CLI commands retired**: `orbit scoreboard`, `orbit metrics`, and `orbit adr migrate` are removed. Their supported replacements are the dashboard scoreboard and metrics views, dashboard HTTP metrics endpoints, and the already-migrated `orbit.adr.*` artifact tools. ([ORB-00183], [ORB-00190], [ORB-00197])
- **Legacy executor `models:` compatibility removed**: executor YAML must use `model_pair_override:`; the one-release `models:` alias and deprecation warning path are gone. ([ORB-00051])

### Highlights

- **Unified knowledge search**: lexical and hybrid search now spans tasks, docs, learnings, and ADRs, with doc/learning/ADR embeddings, task-neighbor lookup, path applicability, and a single `orbit search` mental model. ([ORB-00196], [ORB-00202], [ORB-00203], [ORB-00205], [ORB-00206], [ORB-00216], [ORB-00217])
- **Worktree-aware artifact foundations**: Orbit now distinguishes shared and local `.orbit/` roots, adds a global artifact ID allocator, and lays the groundwork for worktree-local ADR/Learning artifacts while keeping existing stores on the shared root. ([ORB-00199], [ORB-00200], [ORB-00201])
- **Docs and learning context in the agent loop**: PreToolUse learning reminders, `orbit-docs`, indexed design docs, and `task.show --with-context` make project knowledge easier for agents to retrieve before editing. ([ORB-00160], [ORB-00162], [ORB-00163], [ORB-00166])
- **Dashboard and steering improvements**: Audit / Diagnostics side panels, scoreboard grouping, a task resolver, metrics panels, review-thread message panels, and hook-driven async steering make the dashboard a fuller operations surface. ([ORB-00142], [ORB-00144], [ORB-00154], [ORB-00191], [ORB-00211], [ORB-00213], [ORB-00273], [ORB-00274])
- **Release and runtime hardening**: installers authenticate release archives, semantic-companion binaries require signed checksums, key rotation is documented in the installer trust set, and the companion freshness check closes a TOCTOU window. ([ORB-00265], [ORB-00266], [ORB-00270], [ORB-00271])
- **Safer agent writes and execution**: artifact-write redaction, ambient MCP session context, and restricted v2 `proc.spawn` execution reduce the odds of leaking secrets or running in the wrong workspace. ([ORB-00138], [ORB-00256], [ORB-00262])

## 0.6.0

### Release scope

- **Grok onboarded as the fourth first-class agent family**: `all_agent_families()` 3 → 4, sandbox profile, MCP init, executor YAML, commit identity, and a parity sweep across scoreboards/duels/docs. ([ORB-00043], [ORB-00044], [ORB-00045], [ORB-00046], [ORB-00047], [ORB-00048], [ORB-00049], [ORB-00050], [ORB-00052])
- **Agent identity collapsed to family**: model strings are configuration-only; family (`codex` / `claude` / `gemini` / `grok`) is the durable identity across tasks, scoreboards, friction, audit, planning-duel artifacts, and git author lines. ([ORB-00080], [ORB-00081], [ORB-00088], [ORB-00089], [ORB-00090], [ORB-00091], [ORB-00106])
- **Per-task `crew` abstraction**: replaces role-keyed `[agent.<role>]` config with named `[crews.*]` registries selectable per task, and gives the duel-plan agent pool the same configurable surface. ([ORB-00058], [ORB-00072], [ORB-00076], [ORB-00078])
- **First-class Knowledge tab in dashboard**: Learnings/Frictions/ADRs subtabs with inline lifecycle controls, plus task-detail enrichment (tags, external_refs, relations, job_run_id, review_threads, locked-files panel, per-task crew selector). ([ORB-00060], [ORB-00061], [ORB-00062], [ORB-00063], [ORB-00067], [ORB-00068], [ORB-00069], [ORB-00073], [ORB-00076], [ORB-00082], [ORB-00083], [ORB-00084], [ORB-00097])

### Breaking Changes

- **Branching model flipped**: `main` is now the release/production branch; `agent-main` is the dev integration branch where task PRs land. Each release tags on `agent-main` then promotes to `main` via merge commit; hotfixes branch from `main` and back-merge to `agent-main`. Install URLs in `README.md` and the website now point at `main`. Retired stub `crates/orbit-core/assets/activities/examples/promote_agent_main.yaml` removed. See `RELEASING.md` §10b and §Hotfix flow. ([ORB-00054])
- **Crew registry replaces role-keyed agent config**: `[agent.planner]`, `[agent.implementer]`, `[agent.reviewer]` blocks are removed in favor of named `[crews.<name>]` registries selected via `[workflow].default_crew` or per-task `crew`. Workspaces with stale schema are rejected at load. ([ORB-00058])
- **`ship-auto` and `ship-local` aliases removed**: `orbit run ship` is now the unified async-by-default command — empty task IDs trigger auto-backlog mode, explicit IDs queue-and-wait instead of fail-fast. ([ORB-00075])
- **Agent identity schema collapsed to family**: `PlanningRoleAssignment` drops `model`; planning-duel artifact paths and signatures rename to `{slot}.md` and `*authored by: {family} / {slot}*`; `resolve_agent_model_pair*` helpers and alias canonicalization removed; scoreboard `by_model` → `by_family`. Includes a read-side migration; downstream consumers indexing on `by_model` keys break. ([ORB-00080])
- **Family-identity migration script required after upgrade**: `scripts/migrate_family_identity.py` rewrites persisted task/friction/run/audit/scoreboard records and four SQLite stores to family-keyed shapes. Dry-run by default; back up before applying. ([ORB-00081])
- **Audit `task.locks.*` and `job run-pipeline-worker` events populate `task_id`/`job_run_id` semantically**: previously these overloaded `target_id`. Downstream analytics joining on `target_id` for these events need updating. ([ORB-00085])
- **`FrictionStatus::Default` derive swap**: enum default impl moved from a hand-rolled `impl` to `#[derive(Default)]` to satisfy a clippy-deny lint. Behaviorally a no-op; listed for completeness. ([ORB-00086])
- **Task relation enum gains `Produces`/`Resolves`; frictions auto-close on Review → Done**: relation enum extension is additive but the auto-friction-close on task completion is a semantics change. A `resolved_by_task` back-pointer is added to friction records. ([ORB-00093])
- **Learning storage layout: flat → per-entity directories**: `.orbit/learnings/<L-id>.yaml` moves to `.orbit/learnings/<L-id>/learning.yaml`. Legacy-layout load returns a typed error directing operators at `orbit learning migrate-layout`. ([ORB-00096])
- **ADR allocation policy**: new ADR headings must first allocate globally via `orbit.adr.add`; local 3-digit ADR headings under `4_decisions.md` are grandfathered. `docs/design/CONVENTIONS.md` §4 updated. ([ORB-00098])
- **Knowledge-graph workspace_root attribution fixed**: graph refs are now keyed on the worktree's actual branch rather than falling back to the main repo's `agent-main`; the missing-ref case rebuilds rather than silently reading the default branch. Cached selectors keyed off the old behavior may need refresh. ([ORB-00099], [ORB-00105])
- **`orbit.task.update` persists `source_task_id`**: the property was previously silently dropped on writes; clients that depended on the drop now see persistence. Empty-string clears the field. ([ORB-00101])
- **MCP schema emitter no longer degrades `object_list` params to `"string"`**: `evidence` and similar fields now emit array-shaped schemas. MCP clients that worked around the bug by string-encoding payloads must send arrays. ([ORB-00102])
- **Ship batch commit message template**: `feat: parallel batch [ORB-id]` is replaced by a deterministic template — `<type>: <truncated title>… [ORB-id] [EXT-id]…` with an optional full-title line, execution-summary paragraph, and `Planned-By` / `Implemented-By` trailers. Release-note builders and `git log --grep 'parallel batch'` workflows break. ([ORB-00107])

### Features

- **Grok onboarded as fourth agent family**: sandbox state dir + SBPL allowances; CLI runner + executor YAML; `orbit mcp init --client grok` writes `.grok/config.toml`; duel / scoreboard / friction-stats render zero-grok rows; design folder, commit identity, and docs updated. ([ORB-00043], [ORB-00044], [ORB-00045], [ORB-00046], [ORB-00047], [ORB-00048], [ORB-00049], [ORB-00050], [ORB-00052])
- **Knowledge tab in dashboard**: Learnings subtab (list, supersede, stats), Frictions subtab (triage, resolve, stats, tag-picker sourced from YAML), ADRs subtab (accept, supersede, related-task deep-links), task-detail enrichment, Locked Files panel, per-task crew selector, markdown rendering. ([ORB-00060], [ORB-00061], [ORB-00062], [ORB-00063], [ORB-00067], [ORB-00068], [ORB-00069], [ORB-00073], [ORB-00076], [ORB-00083])
- **Duel-plan agent pool configurable** via `[duel] candidates` + `[duel.models]` in `config.toml`; runtime-host trait methods for candidate/model resolution; preserves fallback for non-duel callers. ([ORB-00072])
- **Cross-artifact task relations**: `Produces` / `Resolves` variants enable typed task ↔ friction / learning links; auto-resolves frictions on Review → Done with a `resolved_by_task` back-pointer. ([ORB-00093])
- **Learning enrichments**: per-learning `comments.jsonl` with push-injection rendering; decay-weighted upvotes with task-anchored idempotency; learning-creation wired into agent activity loops with a checkpoint. ([ORB-00077], [ORB-00094], [ORB-00095])
- **Direct-agent runtime model injection**: `ExecutorDef.model_flag` enables data-driven `-m` / `--model` flag dispatch per step. ([ORB-00053])
- **Ship command unified to async-by-default**: `orbit run ship` empty-task-IDs → auto mode, explicit IDs → gated; waiting-reason fields surfaced through `orbit run history` / `show`. ([ORB-00074], [ORB-00075])
- **Backlog dependency gating**: `list_backlog_tasks` filters by `task_dependencies_ready`, fixing out-of-order auto-pipeline execution. ([ORB-00057])
- **Skill quality nudges**: `orbit-create-task` now teaches optional `complexity`, `dependencies`, `parent_id`, and cross-artifact `relations`; `orbit.friction.add` description enumerates the tag taxonomy from YAML. ([ORB-00064], [ORB-00070], [ORB-00104])

### Fixes

- **Identity attribution end-to-end**: runtime ToolContext wire-up, automation-driven Review / Done transitions, git author resolver, and the ship-batch Done loop closed the recurring `implemented_by: "system"` bug across PR-open, ship, and review paths. ([ORB-00067], [ORB-00088], [ORB-00089], [ORB-00090], [ORB-00091], [ORB-00106])
- **Concurrent worktree setup**: SHA-resolution + bounded retry eliminates `.git/config` lock races; post-failure cleanup is idempotent. ([ORB-00059])
- **Policy dashboard denial identity**: real `JobRun` IDs are separated from synthetic audit `execution_id`; task-lock denials now expose actor / requested-files / conflicts. ([ORB-00066])
- **CI failure-recovery task type**: corrected from invalid `"issue"` to `"bug"` (valid types: feature / bug / refactor / chore). ([ORB-00056])
- **Clippy `expect()` violations** in `orbit-store::legacy_models_warns` that blocked PRs after the branching flip. ([ORB-00055])
- **Dashboard task-row ID column** no longer leaves 50px of dead space before the title — column sizes to content. ([ORB-00097])
- **YAML stack-overflow advisory** resolved (pre-ORB-scheme task). ([T20260430-16])

### Chores

- **Family-identity migration script** (`scripts/migrate_family_identity.py`) ships with backups, dry-run-by-default, SQLite normalization, and scoreboard regeneration. ([ORB-00081])
- **Learning storage migration helper** (`orbit learning migrate-layout`) ships alongside the flat → per-entity layout flip. ([ORB-00096])
- **ADR corpus reconciled**: backfilled agent-families, project-learnings, and design-docs orphan ADRs into the global allocation. ([ORB-00103])
- **ORB-00080 coverage gap closed**: end-to-end planning-duel regression test, crew-driven CLI invocation test, and projection field labeling. ([ORB-00087])
- **Dashboard cleanup**: API route inventory footer removed; "rejected" status chip removed. ([ORB-00082], [ORB-00084])
- **Local CI cleanup**: warning-deny clippy across MCP / tools / core / engine / CLI (large-enum boxing, items-after-test ordering, expect formatting, bind-vs-map); task-review scoring coverage extended to the Grok reviewer; design-doc `Last updated:` refresh across eight docs. ([ORB-00108])
- **Unattributed commits**: README refreshes ([commit b35724da], [commit 836b307c], [commit 64f9685d], [commit f9397605], [commit 008d172a]); artifact backfill (ADR-0164, L20260517-7, L20260517-8) ([commit 44889e63]); `make cleanup-branches` target ([commit 51d0777a]); `.gitignore` lock files ([commit 8f00912a]); track `.orbit/learnings/` + `.orbit/adrs/` in git ([commit 0106ff3a]); duel configuration in orbit settings ([commit bcefa2a4]); learning-search absolute-path handling fix ([commit 052376ff]); agent model-pair updates for Codex and Claude ([commit 0657b991]); rename executor model-pair override (PR #240) ([commit 2925a4b7]).

## 0.5.4

### Features

- **Project-learnings push-injection (L1/L2/L3)**: relevant learning summaries are now injected into agent context at three layers — engine pre-prompt before runtime spawn, MCP sidecar on path-bearing tools (`orbit.graph.show`, `orbit.graph.refs`, `orbit.task.show`), and a Claude Code `PreToolUse` hook on `Edit | Write | Read`. Summary-only payloads with per-session dedup, per-call caps, and an `ORBIT_SESSION_ID` envelope for cross-process dedup. ([ORB-00009])
- **First-class design-docs surface (`orbit.design.*` + `orbit design check` CLI)**: four MCP tools (`init`, `list`, `show`, `check`) plus a Rust port of the design-doc decay checker, with `orbit workspace init --design` seeding `docs/design/CONVENTIONS.md` when absent. `make check-design-docs` and `scripts/check_design_doc_decay.py` now wrap the Rust path. ([ORB-00019])
- **Default-seeded skills aligned across asset, registry, plugin, and router catalogs**: `orbit-learning` and `orbit-design` onboarded; `orbit-review-task` and the semantic-search skill brought into the default seed and the plugin's `skills/` symlinks; three drift-detection unit tests guard the four catalogs against recurrence. Default seed bumped 7 → 11. ([ORB-00020], [ORB-00022])
- **`orbit workspace init --inject-agent-rules`**: opt-in flag writes an idempotent Orbit-rules block into `CLAUDE.md` and `AGENTS.md` at the workspace root, delimited by `<!-- orbit-managed:start/end -->` markers. Block content sourced from an editable asset; malformed marker pairs refuse to write. ([ORB-00023])
- **Inline task status transitions in the dashboard**: per-task actions row gains a status selector wired through the existing `PATCH /tasks/:id` backend, ordered by `STATUS_ORDER` with `done` last and excluding `rejected`/`archived`/`friction`. Surfaces a "no longer shown in dashboard list" notice when transitioning to `done`. ([ORB-00025])
- **`orbit.learning.*` exposed over MCP**: the full eight-tool learning surface (`add`, `list`, `search`, `show`, `update`, `supersede`, `prune`, `reindex`) is now reachable from every MCP client, not just `orbit tool run`. Restores parity with the `orbit-learning` skill instructions. ([ORB-00039])

### Fixes

- **PID identity & stale-run probe stability**: versioned `ps-lstart-utc-v1:` token replaces raw `lstart` output (recorded under `TZ=UTC`/`LC_ALL=C`) so live workers are no longer falsely marked failed across timezone changes; a new `OwnerIdentity::ProbeUnavailable` outcome distinguishes transient `ps` failures from genuinely dead PIDs, preventing single-probe terminalization of live workers. Adds arbiter-side `orbit.duel.plan.winner` regression coverage. Backward-compat read path (`LegacyLiveUnverified`) handles existing persisted tokens. ([ORB-00036], [ORB-00037])
- **Job-step error messages reach the dashboard**: `V2AuditEventKind::{StepFinished, RunFinished}` gain an optional `error_message` field; the failure reason is preserved at emit time and surfaced by the audit reader so the Steps and Events tabs no longer show a bare red dot. Backward-compatible — older audit files load unchanged. ([ORB-00026])
- **Gemini direct-agent sandbox + planner/arbiter artifact persistence**: narrow macOS sandbox allowances for Orbit child-runtime writes (global logs, DB sidecars, task workspace bundles, workspace semantic DB), tightened HOME-derived defaults, and improved planning-duel missing-artifact diagnostics so `orbit.duel.plan.add`/`winner` work under sandbox without home-directory re-allow. ([ORB-00027], [commit f3919a99], [commit e706d596])
- **Gemini CLI token accounting**: the shared response usage parser now reads `stats.models.<model>.tokens.{input,prompt,cached,candidates}` without double-counting role aggregates, so Gemini direct-agent invocations no longer persist as zero-token traces. ([ORB-00028])
- **Graph diagnostics quality**: `orbit.graph.show` now returns `did_you_mean` suggestions for unresolvable method selectors; exact-name `orbit.graph.search` ranks definition kinds (`trait`/`struct`/`enum`/`type`/`function`/`module`) above impl-method selectors; `orbit.graph.pack` carries typed `unresolved` reasons (`not_found` / `outside_indexed_roots` / `stale_snapshot`); `orbit.graph.overview` carries a typed `downgrade_reason` with threshold/actual. ([ORB-00029], [ORB-00030], [ORB-00031], [ORB-00032])
- **Task bundle creation no longer leaks a lock sentinel or double-dots its name**: `TaskBundleStoreV2::create_bundle` now locks on the bundle directory target and unlinks the `.ORB-XXXXX.lock` sentinel inside the locked closure; the old pre-dotted `create_lock_path` helper was removed. Concurrent-create serialization preserved. ([ORB-00033])
- **PR signature attribution corrected**: `batch_pr_signature` no longer falls back to `created_by` (which often names a planner or a human filer); when no task carries `implemented_by`, the signature falls back to the PR-opening agent's model identity, which is by construction the author of the commits. ([ORB-00034])
- **Dashboard AGENT LOGS rendering**: `<pre>` switched to `white-space: pre` with horizontal scroll so long single-line stdout (e.g. Codex JSON envelopes) and multi-word stderr stay legible instead of fragmenting one token per row. ([ORB-00035])
- **Dashboard friction-chip cleanup**: removed dead `friction` task-status references from `STATUS_ORDER` and the approve/reject eligibility sets. The diagnostics-tab friction surface (`/api/diagnostics/friction`) is untouched. ([ORB-00024])

### Chores

- **`make ci-fast` introduced for pre-handoff checks**: fmt-check + guardrail scripts, no compile. `make ci` stays the canonical merge gate via PR CI. Agent guidance updated to clarify when `make ci` failures classify as unrelated CI blockers vs task regressions. ([commit 4c22fa19], [commit 89ebc578])
- **Release docs cross-linked**: `RELEASING.md` and `docs/RELEASE.md` now reference each other so a first-time releaser landing on either file finds both. ([ORB-00040])
- **Deprecated activity/executor YAML assets removed**: cleanup of seeded resources that are no longer referenced by the runtime. ([commit c9cf36cc])
- **`devalue` bumped 5.7.1 → 5.8.1** in `website/`. ([commit f08446e1])

## 0.5.3

### Features

- **Claude Code plugin SessionStart hook**: the Orbit plugin now ships a `SessionStart` hook (`plugin/hooks/check-workspace.sh`) that detects an uninitialized workspace via a pure filesystem walk and surfaces a `systemMessage` instructing the user to run `orbit init` / `orbit workspace init`. Closes the silent-no-op gap where `orbit mcp serve` would attach with zero tools and no in-session signal. ([ORB-00018])

### Fixes

- **No-diff PR handoffs**: `pr_open` now treats branches with zero commits ahead of the configured base as successful repository-noop handoffs, moves completed tasks to `review`, returns `pr_created: false`, and avoids creating an empty GitHub PR. ([ORB-00016])

### Chores

- **Workspace lint guardrails**: codified Rust practice lints for panic surfaces, stdout/stderr usage, and async lock guards in `[workspace.lints]`, with scoped allowlists and `CLAUDE.md` updated to separate enforced rules from conventions. ([ORB-00013])
- **README workspace and MCP surface docs**: documented the `.orbit/` workspace layout, committed project-memory directories, and the agent-facing MCP tool namespaces. ([commit b99d3796])

## 0.5.1

### Fixes

- **Deprecation warning routes through `tracing`**: the `knowledge.task_id_pattern` deprecation notice was emitted via `writeln!` directly to stderr from `orbit-core`, contradicting the `CLAUDE.md` "`tracing` for diagnostics; `eprintln!` only in `orbit-cli`" rule and leaking into `cargo test` output. Now goes through `tracing::warn!` with a structured `config` field.

### Chores

- **Release metadata recovery**: backfilled the CHANGELOG entry for v0.5.0 (tagged without one) and aligned `Cargo.toml` workspace version + `Cargo.lock` to v0.5.1. v0.5.0 binaries shipped reporting `orbit --version` `0.4.0` because the workspace Cargo version was not bumped at tag time; v0.5.1 binaries report `0.5.1`. Tag-message and commit-message formats now follow `RELEASING.md`.

## 0.5.0

### Release scope

- **Task artifact v2 cutover**: the v1 single-file task store is replaced end-to-end by a bundle-based v2 store with status-neutral directories, append-heavy sidecar layout, a SQLite registry for fast lookups, a runtime backend, generated indexes, atomic delete, reservation scoping by workspace binding, search refinements (binary-artifact gating, UTF-8 validation), and a forward-only YAML migration framework keyed on `schema_version`. ADRs 001/002/003/004/007 for the v2 design flipped to Accepted; the v1 store, the legacy migration helpers, and the `[task] artifact_store = "legacy"` config gate are all gone.

### Breaking Changes

- **Legacy task artifact store removed**: the v1 single-file task store and its migration helpers were deleted in favor of the v2 bundle store. `[task] artifact_store` accepts `"v2"` as a no-op for forward compatibility and rejects `"legacy"` with an explicit migration error. Workspaces still on legacy storage must migrate under v0.4.0 before upgrading. ([commit e9582eba], [commit 222f6020], [commit 123f89f7])
- **`Task.workspace_path` / `Task.repo_root` dropped from the update path**: `TaskAutomationUpdate`, `TaskRecordUpdateParams`, and `TaskDocumentUpdateParams` no longer accept these fields, and the v2 document layer rejects them at load. Worktree setup and parallel-batch dispatch no longer thread them through. The fields remain on the public DTO via projection from workspace metadata, but write-path inputs that include them now fail. ([commit 6beb14a2])

### Features

- **ADR-artifact subsystem (`orbit.adr.*`)**: ADRs lift out of per-feature `4_decisions.md` markdown into first-class artifacts at `.orbit/adrs/<status>/<id>/{adr.yaml,body.md}` with globally-unique `ADR-NNNN` IDs and a three-state lifecycle (proposed/accepted/superseded). Ships domain types, a SQLite envelope index, a file store with per-ADR `fs2` locks, five tools (`add`, `show`, `list`, `update`, `supersede`) with lifecycle audit rows, an `orbit adr migrate` one-shot, and a parser/sweeper hardened against rollup-bullet form and code-fenced examples. Migration imported 142 ADRs (97 accepted / 39 proposed / 6 superseded) across the existing corpus. The envelope's `legacy_id: String` field was renamed to `legacy_ids: Vec<String>` mid-development per the ADR-002 amendment; pre-GA churn, no external consumers yet. ([T20260510-27], [T20260510-28], [T20260511-1], [T20260511-2], [T20260511-3], [T20260511-11])
- **Project learnings subsystem (`orbit.learning.*`)**: workspace-scoped `LearningFileStore` under `.orbit/learnings/<id>/learning.yaml` with a SQLite `learnings_index` for fast scope-glob lookups on the injection hot path. Eight MCP tools (`add`, `list`, `search`, `show`, `update`, `supersede`, `reindex`, `prune`) plus matching `orbit learning <verb>` CLI; `orbit learning migrate-layout` upgrades legacy flat workspaces. Learnings travel with the repo via a carved-out `.gitignore` entry. End-to-end dispatch latency p95 ≈ 0.04 ms at 500 records. ([T20260511-5], [T20260511-6], [ORB-00096])
- **Plugin install contract locked down**: `make release-check` enforces version lockstep across `plugin/npm/package.json`, `plugin/.claude-plugin/plugin.json`, `npm view @orbit-tools/cli`, and `gh release list -L 1`; `scripts/smoke-plugin-install.sh` exercises the published `@orbit-tools/cli@latest` postinstall + MCP `tools/list` handshake; `.github/workflows/smoke-plugin-install.yml` runs the smoke weekly and on every `v*` tag on macOS and Linux; `docs/RELEASE.md` codifies the chain. v0.5.0 switched the npm publish step to manual after the workflow ran into account-level 2FA. ([ORB-00012], [ORB-00014])
- **Typed error discriminators**: `KnowledgeError.kind` is now the typed `KnowledgeErrorKind` enum, and ten per-kind `OrbitError::*NotFound(String)` variants consolidate into `NotFound { kind: NotFoundKind, id: String }` with exhaustive matches in every error-code translator. JSON wire shape is preserved via `#[serde]`; internal Rust matchers move to the typed kind. ([ORB-00001])
- **Property + snapshot tests on protocol boundaries**: proptest-backed `Selector` display/parse roundtrip (256 cases per variant), multi-threaded `GraphLockGuard`/`LockStore` concurrency with a 5-second deadline, and committed JSON-snapshot coverage for `AuditGuard` success/failure/denied events. ([ORB-00002])
- **Knowledge graph workflow relocation**: `build`/`show`/`search`/`history` workflows moved from `orbit-tools` into a new `orbit_knowledge::workflows` module, so `orbit-knowledge` owns both the tool surface (`commands/`) and the host application surface (`workflows/`). ([commit 5fc2b72c])
- **Forward-only YAML migration framework**: `orbit_common::migration::Plan` chains `Value → Value` steps keyed on `schema_version`; `OrbitError::Migration` separates chain failures from store/parse errors; the v2 task-bundle envelope read path now flows through `task_migrations::envelope_plan()` (empty chain today — next schema bump adds one `add_step` call). ([commit 01928e76])
- **Semantic companion install hardening**: companion installation is version-aware, `--force` reinstalls supported, and background-companion stderr is suppressed during task-mutation indexing. ([T20260510-26])

### Fixes

- **`pr_open` template tolerates legacy `batch_id` overrides**: `worktree_setup` emits `batch_id` as a deprecated alias equal to `job_run_id`, and `required_job_run_id` accepts `job_run_id` → `run_id` → legacy `batch_id` in that precedence. Recovers `task_pr_pipeline` runs whose stale resource overrides still reference `{{ steps.worktree.output.batch_id }}`. ([ORB-00010])
- **Retired v1 / stub paths fail loudly instead of silent success**: `OrbitRuntime::get_job` and `RuntimeHost` v1 job lookup return an explicit retired-v1 error instead of `Ok(None)`; the `promote_agent_main` and `revert_on_red` deterministic actions fail with a "retired stub" error instead of returning skipped JSON. Tightens validation on inputs that were already invalid by spec. ([ORB-00007])
- **Architecture and design-doc drift fixed**: `ARCHITECTURE.md` was regenerated from `cargo metadata` to reflect the live workspace graph; `make check-design-docs` was cleared by refreshing stale `Last updated:` metadata and replacing moved/deleted file references across ten design folders. ([ORB-00006])
- **Smoke plugin-install assertion fixed**: assertion was checking `"orbit\.` but MCP wire names use underscores (`sanitize_tool_name` in `orbit-mcp/src/adapter.rs` replaces `.` with `_`). Without this fix the on-tag smoke would have false-failed. ([ORB-00014])

### Chores

- **Per-crate stability tier markers**: `[package.metadata.orbit] stability = ...` added to all fourteen workspace crates (`stable`: `orbit-common`, `orbit-store`; `experimental`: `orbit-embed-companion`, `orbit-registry`; `internal`: the remaining ten). `scripts/check-stability.sh` enumerates members via `cargo metadata`, validates the marker, and fails closed with named offenders; wired into `make stability` and `make ci`. ([ORB-00005])
- **Missing-docs guardrail**: workspace-wide `missing_docs = "warn"` in `[workspace.lints.rust]`; `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` wired into `scripts/ci-guardrails.sh`; 278 accidentally-`pub` items narrowed to `pub(crate)` (6.8% of the 4,116-item baseline). Legacy allow-fences in place for the remainder. ([ORB-00004])
- **Community health files**: `CODE_OF_CONDUCT.md` (Contributor Covenant v2.1, contact via GitHub Security Advisories), `.github/PULL_REQUEST_TEMPLATE.md` with linked Orbit task ID + `make ci` / `make check-design-docs` checkboxes, and three `.github/ISSUE_TEMPLATE/*.yml` issue forms with a 14-crate dropdown sourced from live `crates/` listing. ([ORB-00011])
- **Release runbook (`RELEASING.md`)**: pre-1.0 versioning policy with explicit breaking-vs-non-breaking criteria, 11-step release checklist, CHANGELOG conventions, and tag-push CI workflow description. ([T20260510-24])
- **Semantic search surfaced in agent instructions**: new semantic-search skill modeled on `orbit-graph`, plus pointers from `orbit-create-task`, `orbit-execute-task`, `orbit-review-task`, `agent_implement.yaml`, `agent_review.yaml`, `epic_orchestrator.yaml`, and `dispatch_agent.yaml`. All references use "if available" / "optional" language so missing companion never hard-fails a workflow. ([T20260511-4])
- **Design-pattern reference docs added**: `docs/design-patterns/{command,strategy,raii_guard,newtype,error_translation}.md` so feature work can copy from documented references instead of inventing new shapes. ([commit 3adcd838], [commit c713cc30], [commit 66389575], [commit f2f82bf1], [commit aa407aa0])
- **Design-doc decay check**: new `scripts/check_design_doc_decay.py` and `make check-design-docs` flag `docs/design/*` docs whose `Last updated:` precedes the last commit on any referenced `crates/.../*.rs` file. ([commit 18c48744])
- **Workspace lint table introduced**: `[workspace.lints]` in root `Cargo.toml` with each crate inheriting via `lints.workspace = true`; mechanical lint rules moved out of `CLAUDE.md` prose. ([commit 0cbb037d])
- **`CLAUDE.md` refactored to point at `ARCHITECTURE.md`**: crate layering moved to a dedicated architecture doc; `CLAUDE.md` shrinks to project rules and judgment calls. ([commit b7c590aa], [commit 9362730b], [commit 7582af9d])
- **MCP server configuration for additional environments**: `.codex/config.toml`, `.gemini/settings.json`, and `.vscode/mcp.json` added so Codex/Gemini/VS Code agents pick up Orbit MCP out of the box. ([commit 200ee6fd])
- **README simplification**: Quick Start trimmed; positioning sentences moved to design docs. ([commit 75699824])
- **Agent skills symlink**: `.agents/skills` symlink added so external agent harnesses pick up the seeded skill set. ([commit 9f7bf89b])
- **Lessons log update**: `docs/LESSONS.md` extended with a workflow lesson. ([commit 024013af])

## 0.4.0

### Release scope

- **Pivot to "auditable agentic task management"**: README and landing-page positioning realigned around intent attribution and audit trails, with throughput and parallel-execution sections refreshed to match.
- **Knowledge graph reads on SQLite**: per-build `graph_index.sqlite` sidecar with read-only fast paths for `graph.overview`, `graph.search`, and `graph.show`, plus an output-equivalence harness against the JSON fallback.
- **Semantic search foundation (preview)**: hybrid (BM25 + cosine + RRF) retrieval over tasks, delivered as a separately-installed `orbit-embed-companion` binary. Preview status — surface may change before v1.

### Breaking Changes

- **Friction reports relocated**: friction is no longer a task type or status. Records live as append-only markdown under `.orbit/frictions/{yyyy}-{mm}/F{nnn}.md` and are managed through `orbit.friction.add/list/show/stats`. `orbit.task.add` rejects `type: friction` / `status: friction`; web API and scoreboard JSON drop `friction_bounty`. ([T20260510-13])
- **Task type taxonomy reduced**: the `task | feature | epic | issue | bug | chore | refactor | friction` enum collapses to `feature | bug | refactor | chore`. `orbit.task.add` and `orbit.task.update` reject the removed values; existing tasks were migrated. ([T20260510-14])
- **Attribution narrowed to `model`**: the `agent` field is removed from `Actor`; `orbit.task.add` rejects an `agent` parameter and Orbit infers the agent family from `model` via `agent_from_model`. MCP `orbit_task_list`, `orbit_task_search`, and `orbit_task_review_thread_list` responses are now object-shaped (previously top-level arrays) so Cursor and VS Code accept them. ([T20260510-15])
- **Knowledge-graph leaf IDs unified across extractors**: Python, Rust, Java, and TypeScript leaf selectors now use a single canonical form so SQL and JSON paths return set-equivalent results. `GRAPH_SQLITE_INDEX_SCHEMA_VERSION` bumps; consumers caching selectors must rebuild. ([T20260510-7])
- **Semantic search requires a companion binary**: `orbit-embed-companion` is installed separately via `orbit semantic install`; `orbit semantic *` and the matching MCP tools fail until it is present. ([T20260510-9], [T20260510-10])
- **`JobV2Step` rejects multi-body shapes**: YAML steps that previously parsed silently with both `target` and `parallel` (or any other body combination) now fail at load. ([T20260509-31])
- **`orbit-locks` skill removed**: the seeded `orbit-locks/SKILL.md` and the ad-hoc `orbit.task.locks*` instructions in the seeded `orbit` skill are gone — the gate pipeline still owns reservations. External agent prompts referencing the skill must be updated. ([T20260510-17])

### Features

- **Knowledge graph SQLite read facade**: per-build `graph_index.sqlite` with versioned schema, read-only facade with graceful JSON fallback, and SQL fast paths for `graph.overview` (aggregation), `graph.search` (exact-name, path-prefix, and substring), and `graph.show` (selector lookup with `children` repopulated via a forward-pointer edge table). ([T20260509-70], [T20260509-71], [T20260509-72], [T20260509-73], [T20260509-74])
- **Knowledge graph latency wins**: lazy source hydration via `GraphReadOptions`, a bounded default-ranking work cap on search, and a `BinaryHeap` top-K in `overview.top_files`. ([T20260509-65], [T20260509-67], [T20260509-68])
- **Knowledge command surface**: graph business logic — ranking, classification, fast-path orchestration — relocated into `orbit_knowledge::commands::*` so non-tool consumers share canonical behavior. ([T20260510-5])
- **Semantic search subsystem**: `orbit-embed` client, `orbit-embed-companion` binary, `embeddings` and `tasks_fts` SQLite schema, paragraph chunker, BLAKE3 dedup, task-mutation index hooks, and `orbit semantic install/uninstall/reindex/stats/search/related` CLI plus MCP surface. ([T20260510-3], [T20260510-9], [T20260510-10], [T20260510-20])
- **Task tags**: first-class `tags: Vec<String>` field with normalized SQLite index and `--tag` filtering on `orbit task list/search`. ([T20260510-12])
- **Activity/job runtime polish**: wildcard-aware tool allowlists honored at dispatch and HTTP-loop schema advertisement, asset-load-time allowlist validation, agent-loop `on_denial: continue`, literal-boolean condition atoms, and exclusive locking on duel scoreboard appends. ([T20260509-15], [T20260509-22], [T20260509-23], [T20260509-25], [T20260509-32])
- **Recovery role configurability**: seeded step-failure recovery activity uses `role: reviewer` and resolves agent/model from `[agent.reviewer]` config instead of hardcoded Codex. ([T20260509-14])
- **Done-task sync cap**: website task sync caps generated pages to the 100 most recent `done` tasks. ([T20260509-20])
- **Debug-job-failure skill**: seeded `orbit-debug-job-failure` SKILL.md teaching agents how to investigate failed/stuck/cancelled job runs across run state, audit events, blobs, and live processes. ([T20260509-79])
- **Graph-latency benchmark**: split `benchmarks/CONVENTIONS.md` into agent vs perf RESULTS schemas, scaffolded `benchmarks/graph-latency/` with three-tier Python/Java/Rust corpora, and ran v1/v2 sweeps against the post-SQLite read paths. ([T20260509-63], [T20260509-87], [T20260510-4])

### Fixes

- **Output-equivalence between SQL and fallback paths**: `graph.search` SQL widened to substring match aligned with the navigator and `graph.show` repopulates `children` via a forward-pointer edge table. ([T20260510-1], [T20260510-2])
- **Workflow stop on implementer envelope failure**: `peek_response_status` extracts embedded Orbit envelopes from CLI stdout that contains explanatory prose before the JSON, so failed implementations no longer advance to push/PR. ([T20260509-15])
- **`ship-auto` empty backlog**: condition evaluator skip guards no longer fail when `bundle_count` is zero. ([T20260509-11])
- **Parallel dispatcher hang on worker timeout**: scoped-thread workers now exit through a cancellable boundary so the pipeline returns within its own timeout. ([T20260509-38])
- **Subprocess timeout cleanup**: bare `spawn_with_timeout` starts children in a process group/session so grandchildren are killed and pipes don't leak. ([T20260509-40])
- **Stdout no longer duplicated into `DispatchOutcome`**: blob refs are the source of truth, with a bounded preview retained. ([T20260509-43])
- **Path-traversal hardening**: task store ID validation, policy candidate-path component checks, resource-name validation in policy/executor stores, and an absolute-path probe for `sandbox-exec`. ([T20260509-26], [T20260509-27], [T20260509-28], [T20260509-30])
- **Tool deletion guard**: `orbit.task.delete` MCP tool respects the same protected-status guard as `orbit task delete`. ([T20260509-44])
- **Backend resolution**: invalid `[runtime] backend` values reject before dispatch instead of falling through to preview HTTP. ([T20260509-45])
- **Architecture guardrail**: `scripts/check-dependency-direction.sh` derives the workspace-crate list from `cargo metadata` so new crates can't drift past the check. ([T20260509-46])
- **JSON output purity**: `orbit task approve --all-proposed --json` and reject equivalents emit pure JSON on stdout. ([T20260509-47])
- **Dashboard dependencies**: dependency-status index includes `done` and `archived` tasks so visible rows don't misreport completed deps as missing. ([T20260509-48])
- **Reject help/help-truth alignment**: top-level task help describes the actual reject transition matrix. ([T20260509-50])
- **Symlink scanning**: knowledge scanner skips and canonicalizes symlinked dirs to prevent index escape and cycles. ([T20260509-33])
- **Graph freshness**: manifest persists exact Git identity rather than relying on committer timestamp. ([T20260509-34])
- **GitHub PR result validation**: `github.pr.review` and `github.pr.comment.reply` validate JSON shape before reporting success with id `0`. ([T20260509-36])
- **MCP name collisions**: dot-to-underscore name mapping detects ambiguity on startup. ([T20260509-37])
- **Git author identity**: workflow commits set per-implementer author dynamically without writing repo-local `git config user.*`. ([T20260508-22], [T20260509-12])
- **CI clippy guardrails**: cleared `manual_contains`, `needless_borrow`, `useless_conversion`, `match_like_matches_macro`, `empty_line_after_doc_comments`, `too_many_arguments`, `doc_lazy_continuation`, and `question_mark` violations under `-D warnings`. ([T20260509-18], [T20260509-61], [T20260510-15], [T20260510-22])
- **`fast-uri` Dependabot alerts**: addressed and documented dev-only `fast-uri` advisories on `website/package-lock.json`. ([T20260509-57])

### Chores

- **Module decomposition**: split `command/web/api.rs` (3,376 LOC), `activity_job/job_executor.rs` (2,841 LOC), `activity_job/cli_runner.rs` (2,161 LOC), `runtime/orbit_tool_host/mod.rs` (2,033 LOC), `command/mcp/setup.rs` (1,964 LOC), and the `activity_job/groundhog.rs` runner. ([T20260509-1], [T20260509-2], [T20260509-3], [T20260509-4], [T20260509-5], [T20260509-19])
- **Embed crate ownership**: relocated `vector::*` and the semantic command surface from `orbit-store` and `orbit-core` into `orbit-embed`, reversing the dep arrow so `orbit-store` no longer knows the embedding feature exists. ([T20260510-20])
- **Panic audit**: classified ~1,864 `unwrap` / `expect` / `panic!` sites and removed the accidental ones in execution-critical paths. ([T20260509-6])
- **Test coverage on highest-risk seams**: focused tests for the activity/job DAG executor and the macOS sandbox/policy boundary. ([T20260509-7])
- **Plan-duel `context_files` extraction**: duel resolver auto-populates `task.context_files` from the winning plan's Context Files section. ([T20260509-9])
- **Knowledge-graph, policy-sandbox, and Groundhog doc hygiene**: refreshed owned design docs to current surface.
- **Task lineage design (first draft)**: seeded `docs/design/task-lineage/` with edge schema, three derivers, bipartite bridge, `feature` closure, and symbol-biography renderer. ([T20260510-21])
- **Project learnings design (seed)**: seeded `docs/design/project-learnings/` with hook-injection layer rationale; deferred until semantic search is Accepted. ([T20260510-11])
- **Semantic search v2 design pivot**: switched to companion-binary architecture per ADR-005. ([T20260510-3])
- **Orbit-create-task skill**: tightened `context_files` rule (existing modified or deleted files only, prefer file-level selectors). ([T20260509-83])
- **`make ci` alignment**: `make build` failures resolved alongside the dep-direction script. ([T20260510-22])
- **Release metadata**: bumped Cargo workspace, plugin manifests, and npm proxy metadata to v0.4.0.

## 0.3.1

### Features

- **Pipeline dispatch reliability**: hardened parallel, gate, and epic pipelines with failed-child completion handling, longer task lock coverage, epic timeout/convergence fixes, resolved workspace subprocess cwd, and per-step agent log/error surfacing. ([T20260427-34], [T20260427-36], [T20260427-38], [T20260427-40], [T20260508-8], [T20260508-14])
- **Metrics and public docs**: split the public metrics surface into Operations and Scoreboard views, added done-task sync pages for orbit-cli.com, refreshed positioning/reference docs, and refined the website UI. ([T20260508-4], [T20260508-16], [T20260508-19], [T20260508-20], [T20260507-21])
- **Registry and benchmark tooling**: added the `orbit-registry` crate and identity-key benchmark harness for exercising knowledge graph selector stability. ([T20260507-12], [T20260508-2])

### Fixes

- **macOS sandbox and CLI execution**: allowed Claude's `$HOME/.claude.json` lock/tmp siblings, re-allowed the active job-run worktree after global deny rules, and demoted successful CLI exits when the inner Orbit envelope reported failure. ([T20260508-13], [T20260508-17])
- **Workflow defaults and links**: made workflow base branches resolve from `[workflow] base_branch` when CLI flags are omitted, and fixed task-ID links in generated PR bodies with an opt-in URL template. ([T20260508-11], [T20260508-12])
- **CI clippy guardrails**: grouped macOS sandbox spawn inputs into a request struct so strict workspace clippy passes under `-D warnings`. ([T20260508-21])

### Chores

- **Release metadata**: bumped Cargo workspace crates, plugin manifests, install examples, and npm proxy metadata to v0.3.1. ([T20260508-21])
- **Release packaging**: kept GitHub Release tarballs, checksums, Homebrew tap updates, and installer smoke tests as the supported release path, while removing the npm publish step from the tag workflow.

## 0.3.0

### Release scope

- **Stable surface: CLI agent backends.** v1 supports `backend: cli` as the stable agent invocation path, running Codex, Claude Code, Gemini CLI, and other official CLIs as supervised subprocesses. `backend: http` (`LoopTransport`) and the Groundhog checkpoint runner remain preview-only for v1; they are exercised in tests but can change before v2.

### Breaking Changes

- **Activity/job schema v1 removed**: loaders now reject `schemaVersion: 1` activity/job assets, the v1 reconcile/runtime/store paths are gone, and `schemaVersion: 2` is the canonical activity/job surface. ([T20260419-2156], [T20260420-0036])
- **Workflow commands reorganized**: stable entrypoints are `orbit run ship <TASK_ID>...`, `orbit run ship --mode local <TASK_ID>...`, `orbit run ship-auto`, `orbit run duel-plan <TASK_ID>`, and `orbit run job <JOB_ID>`. The direct `orbit run <JOB_ID>` shorthand and workflow-specific `run ship list/show` and `run duel list/show` commands were removed; use `orbit run history` and `orbit run show` for job-run inspection. ([T20260417-0248], [T20260419-0355], [T20260425-2010], [T20260426-0742])
- **Task attribution history moved to `orbit graph history`**: selector history is graph-owned, so the query now lives next to `orbit graph search/show`, and rebuilds use `orbit graph build`. Both `orbit graph build` and `orbit graph history` accept `--task-id-pattern <regex>`; workspace config `knowledge.task_id_pattern` is the steady-state setting, with CLI flag > config > Orbit default precedence. The selected pattern is recorded in `manifest.json`, and mismatches emit a stderr warning. `orbit.graph.history` exposes the same surface to MCP clients. ([T20260426-0507])

### Features

- **Activity/job v2 runtime**: added schema v2 activities and jobs with typed DAG blocks (`parallel`, `fan_out`, `loop`, `retry`, `when`), activity name resolution, `backend: auto` normalization, `backend: cli` dispatch, HTTP agent loops, session-bound loop steps, and a v2 audit envelope with workspace provenance. ([T20260418-2018], [T20260418-2019], [T20260418-2143], [T20260418-2210], [T20260419-0002], [T20260419-0104])
- **Seeded task pipelines**: added load-bearing seeded workflows for PR, local, gate, auto-dispatch, and epic shipment, including task reservations, backlog bundling, admission-controlled dispatch, and session-backed epic orchestration. ([T20260419-0622-3], [T20260419-0623], [T20260419-0623-2], [T20260419-2347])
- **Knowledge graph**: added the Rust `orbit-knowledge` graph, `orbit graph build/update/search/show`, graph MCP tools, compact overviews, callers/implementors/dependency navigation, edit buffering, shared locks, auto-refresh, branch-scoped refs, task-ID attribution metadata, and markdown/config/table extraction. ([T20260411-0424], [T20260412-0645-2], [T20260412-0645-3], [T20260421-0358], [T20260421-0528], [T20260422-1540])
- **MCP integrations**: added the `orbit-mcp` crate, `orbit mcp serve`, safe default graph/task tool exposure, external MCP/plugin tooling, and `orbit mcp init/remove` setup for Claude, Codex, and Gemini clients. ([T20260418-0336], [T20260419-0236], [T20260422-1713], [T20260426-0354])
- **Dashboard and observability**: added `orbit web serve`; task, job, audit, scoreboard, and dashboard APIs; diagnostics and recent-runs views; task actions; copyable task IDs; connection health; skeleton/loading states; markdown rendering; and live-data animations. ([T20260417-0346], [T20260417-0412], [T20260417-0427], [T20260417-0437], [T20260417-0528], [T20260418-2004], [T20260426-0354])
- **Task planning and search**: added structured task plans, dependency support, epic task type support, selector-first task context, agent task search, and richer task field projection for agent/tool callers. ([T20260419-2300], [T20260420-0509-2], [T20260420-0521], [T20260421-0445], [T20260422-1756])
- **Groundhog execution model (preview)**: added Groundhog chronicle serialization, workspace snapshots, verb tools, checkpoint verification, and a dedicated Groundhog v1 activity runner. ([T20260420-0509], [T20260420-0509-3], [T20260420-0509-4], [T20260420-0510], [T20260420-0510-2])
- **Provider and evaluation support**: added Gemini support, configurable agent/model selection, provider invocation traces, HTTP LoopTransport implementations for Anthropic/OpenAI-compatible/Gemini providers, planning duels, scoreboard attribution improvements, and versioned knowledge-graph benchmark harnesses. ([T20260411-1937-2], [T20260412-0457-2], [T20260412-1939], [T20260412-2129], [T20260418-0645], [T20260418-0759], [T20260422-1609])

### Fixes

- **Job run observability**: `orbit run ship --json`, `orbit run history`, `orbit run show`, and direct `orbit job run` now retain actionable failure details and durable run-state/job-history records, including synthetic job-level steps for early v2 pipeline failures. ([T20260423-0445], [T20260423-2004-4], [T20260425-2010], [T20260426-0742])
- **Branch-scoped knowledge graph refs**: graph builds now write `.orbit/knowledge/graph/refs/heads/<branch>.json` files that point at immutable per-build indexes, reads default to the current git branch with default-branch fallback, and legacy `.orbit/knowledge/graph/refs/current.json` stores auto-migrate on first open/write. ([T20260421-0358])
- **Knowledge graph hardening**: graph reads and refreshes recover from corrupted stores, avoid stale worktree data, gate refresh/search hot paths, prune missing context files from locks, and hydrate task IDs idempotently during attribution. ([T20260416-0719], [T20260417-0307], [T20260420-0540], [T20260421-0652])
- **Dispatch and locking correctness**: task locks now detect directory/file overlaps, backlog selection filters locked groups, failed task-scoped runs move tasks to blocked with job/run/error context, and drained local batches no longer fail spuriously. ([T20260412-0443], [T20260417-0301], [T20260419-2109], [T20260420-0014])
- **Workflow compatibility**: merged object-valued job defaults with caller input, aligned the Quick Start approval flow with the current task lifecycle, and routed retired workflow inspection docs/errors to `orbit run history/show`. ([T20260423-0445], [T20260423-0447], [T20260423-2004-2], [T20260425-2010], [T20260426-0742])
- **Release and developer tooling**: restored release CI targets, repaired advertised developer targets, kept custom roots isolated, and fixed crashes/empty listings after seeded activity/job initialization. ([T20260419-2347], [T20260423-2004], [T20260423-2004-3], [T20260423-2004-5])
- **Security and concurrency hardening**: added localhost origin checks for web write endpoints, serialized diagnostics JSONL appends, hardened task-store concurrency, tightened filesystem/tool-runtime path boundaries, and strengthened agent protocol handling. ([T20260417-0557], [T20260417-0558], [T20260418-1928])

### Chores

- **Crate architecture**: extracted `orbit-common`, `orbit-knowledge`, and `orbit-mcp`, merged the older `orbit-types` surface into `orbit-common`, decomposed execution/runtime modules, and kept crate dependency direction aligned with the documented architecture. ([T20260411-0008], [T20260419-2014])
- **Documentation and positioning**: added Orbit positioning docs, design-doc conventions, activity-job/knowledge-graph/Groundhog design docs, benchmark reports, and README updates for the current workflow and MCP surfaces.

## 0.2.0

### Features

- **Parallel batch execution**: dispatch and execute multiple tasks in parallel with file-level conflict detection and shared worktrees
- **Auto-cleanup on merge**: ship workflow now deletes the remote branch after a successful PR merge

### Fixes

- **`--parallelism` flag**: serialized as JSON integer instead of string, fixing schema validation failure on `orbit run ship --parallelism N`
- **Stale default artifacts**: `orbit workspace init` now always refreshes default skills, activities, and jobs to their latest embedded versions (custom artifacts are preserved)
- **Clippy warning**: resolved unused-mut warning and removed clippy from CI

### Chores

- Default branch renamed from `agent-main` to `main`
- Removed `orbit` label from PR creation
- Agent configuration updates

## 0.1.0

Initial release of Orbit.

### Core

- **Task lifecycle**: propose, approve, implement, review, and archive tasks with full history tracking
- **Activity system**: reusable operations with defined input/output schemas and three spec types (agent_invoke, cli_command, automation)
- **Job engine**: composable multi-step pipelines with conditional execution, retry logic, nested jobs, and parallel dispatch
- **Workflow aliases**: `orbit run ship`, `orbit run ship-local`, `orbit run review` as ergonomic entry points over raw job invocation
- **Multi-agent orchestration**: parallel task workers with file-level locking in shared worktrees
- **Multi-model strategy**: configurable agent/model per job step (e.g., Opus for planning, Codex for implementation)

### CLI

- Grouped command surface: run workflows, manage work, configure and inspect
- JSON and table output modes across all commands
- Audit event logging for every CLI invocation

### Infrastructure

- Layered Rust crate architecture (types, policy, exec, tools, store, agent, engine, core, cli)
- Two-root workspace model: global (`~/.orbit/`) and workspace-local (`.orbit/`)
- File-based (YAML) and SQLite persistence
- RBAC policy evaluation engine
- Process sandboxing and timeout handling
- Skill system for agent prompt composition
