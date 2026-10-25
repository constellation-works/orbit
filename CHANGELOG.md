# Changelog

## Unreleased

## 0.33.0 — 2026-10-25

### Breaking Changes

- **Job-run task recency migration**: opening a workspace applies SQLite v59 (`job_run_task_recency`). The migration is additive (a new table, index and triggers older binaries never touch) but changes persisted storage; upgrade hosts together. ([ORB-18182])
- **Cursor, Pi and OpenCode lanes retired**: configuration and dispatch now reject the `cursor`, `pi` and `opencode` providers; recorded history still displays them.
  Crews on those providers → move them to a supported provider. ([ORB-18096])
- **New task history decision**: tag edits record a `tags_changed` decision, which v0.32.0 and older cannot read, and the distributed admission contract now carries review-skip tags.
  Mixed-version hosts → upgrade the owner and followers together. ([ORB-18078])
- **`orbit tool run --pretty` removed**: the hidden error-formatting alias is gone, so scripts passing it now fail.
  `--pretty` removed → drop the flag. ([ORB-17997])

### Highlights

- **Decision channel**: Orbit records decision requests on held work and raises them through a new state trigger. Operators list and answer them from `orbit decision`, MCP or the dashboard, and registered plugins can submit authenticated answers. ([ORB-18176])
- **Why stuck**: a dashboard card links each blocked or held task to the run or finding that stopped it, with one-click requeue, settle-merged and pilot-admission actions. ([ORB-18232])
- **Workspace delivery conventions**: configure pull request titles, labels, commit messages and branch names per workspace so automated deliveries follow the repository's conventions. ([ORB-17982])
- **Review-skip tags and default review**: workspaces can map a task tag to the paths whose changes skip agent review, and `workspace init` seeds before-landing review for PR-route workspaces. ([ORB-18088])
- **Portable maintenance sweeps**: new built-in auto-task definitions cover code health, architecture, structure, dependency freshness, flaky tests and CI budgets. ([ORB-18124])

## 0.32.0 — 2026-10-25

### Breaking Changes

- **Crew performance snapshot migration**: opening a workspace applies SQLite v58 (`crew_performance_snapshot`). The migration is additive (two new tables older binaries never touch) but changes persisted storage; upgrade hosts together. ([ORB-17816])
- **Pull protocol fingerprint**: follower pull requests now carry per-crew usage pressure, which changes the distributed-drain protocol fingerprint.
  Mixed-version pull drains → upgrade the owner and followers together. ([ORB-17773])
- **Blocked-task triage auto-task retired**: `orbit workspace sync` removes seeded copies of the `blocked-task-triage` auto-task, even enabled ones, and keeps edited copies.
  `blocked-task-triage` auto-task removed → set `enabled: true` on the `blocked_task_triage` routine. ([ORB-17744])
- **`workspace init --refresh-defaults` removed**: the hidden no-op flag is gone, so scripts passing it now fail.
  `--refresh-defaults` removed → drop the flag. ([ORB-17868])
- **Rust API**: `AgentFamily::Muse` and `StateTriggerKind::{PilotWarningsHeld, BlockEscalated}` are new variants; `OrbitError::RemoteTool.code` is `RemoteToolCode`, `AutomationError::Deferred` carries `DeferredReason`, and `PolicyDecision` is removed. Unused `orbit-store` and `orbit-common` items are deleted.
  Downstream exhaustive matches and constructors → update them. ([ORB-17620])

### Highlights

- **Usage-pressure crew selection**: with `workflow.crew_selection = "usage_pressure"`, draws favour crews whose weekly allowance would otherwise go unused, and admission re-draws system-chosen crews. Pull followers weigh their own readings; the dashboard and `orbit run readiness` show pressure per account and crew. ([ORB-17770])
- **Performance-weighted crew draws**: `workflow.crew_performance` weights draws by an hourly per-task-type implementer snapshot. `orbit agents scorecard --by-type` shows the cells, and `--shadow` compares recorded shares with what the factor would give. Off by default. ([ORB-17817])
- **Escalation triage routines**: new `pilot_warning_triage` and `blocked_task_triage` routines, disabled by default, send pilot-held proposed tasks and escalated blocks to a hard-tier triager that records one requeue, proposal or human-action report per episode. ([ORB-17743])
- **Wider provider usage coverage**: Grok, Antigravity and Muse runs now record usage-window readings, so provider limits, holds and usage pressure cover them, and Muse crews run on Linux hosts. An auto-task whose crew hits a usage limit is reassigned within its complexity pool before it runs. ([ORB-17774])
- **Built-in `argon` crew**: Gemini 4 Argon through `agy` is a built-in crew, and `orbit init` adds it to the hard complexity pool when `agy` is installed. ([ORB-17942])
- **Dashboard refresh**: Settings is split into per-topic tabs, task History reads as a timeline, Context files group by crate or folder, and provider usage chips open a detail dropdown. Edits to `config.toml` now show without restarting the server. ([ORB-17974])

## 0.31.0 — 2026-10-22

### Breaking Changes

- **Executor census migration**: opening a workspace applies SQLite v55 (`executor_census`). The migration is additive at runtime but changes persisted storage; upgrade hosts together. Rust callers must replace `automatic_dispatch_cmp_for_host` and `AdmissionOrdering.owner_os` with the affinity/census inputs. ([ORB-17304])
- **Crew accounts migration**: opening a workspace applies SQLite v56 (`provider_crew_accounts`). Environment-backed crews now have separate account identities → explicitly allow those crews when an allow-list previously relied on their provider's default account. ([ORB-17288])
- **Handoff storage compatibility**: SQLite v57 moves handoff settlements into a sidecar and guards stale inline writers. Older binaries reopened afterward are read-only; already-running intermediate writers reject handoffs → upgrade and restart Orbit clients, including long-lived MCP servers, to resume writes. Rust settlement constructors and exhaustive matches also need updates. ([ORB-17530])
- **Workspace source credentials**: fresh workspace registration rejects Git origins containing embedded credentials; discovery redacts legacy values without rewriting registrations → use credential-free origins and a credential helper. ([ORB-17222])
- **External-merge Rust API**: `ClaimMutation` adds `SettleExternalMerge`; downstream exhaustive matches must handle the new variant. ([ORB-17078])

### Highlights

- **Crew-specific provider accounts**: environment-backed crews have separate usage limits, holds, auth exclusions and budget targets. Crews without an environment retain the provider's default account. ([ORB-17288])
- **Scarcity-aware task admission**: distributed and owner-local drains use a live executor census to prioritize work that fewer eligible hosts can run, with a workspace setting for affinity reach. ([ORB-17304])
- **Drain controls**: the dashboard composer covers local and pull drains, auto-run options, recorded settings and pull claim stages. ([ORB-17302])
- **Task detail navigation**: context files have a dedicated tab beside History, Runs, Artifacts and Comments, with the existing editor and keyboard navigation. ([ORB-17508])

## 0.30.0 — 2026-10-20

### Breaking Changes

- **Distributed pull compatibility**: the pull request fingerprint now covers browser-kit and forecast-fit admission fields, so an owner and followers on different builds refuse each other → upgrade every host together. ([ORB-16627])
- **`plugin.legacy_callback_identity` removed**: the legacy environment-token and process-ancestry callback authentication is gone, and the key now only warns → plugin backends must preserve the inherited callback descriptor. ([ORB-16772])

### Highlights

- **Read-only exploration**: implementing and review-repair agents can ask an explorer crew bounded code-reading questions in the same worktree. The explorer runs read-only under a restricted tool policy and returns the paths it read with its answer. ([ORB-16814])
- **Mid-run steering**: an opt-in setting delivers operator comments added after dispatch into managed agent runs. Orbit reruns a bounded checkpoint and holds promotion until the run has consumed the new guidance, including on claimed work. ([ORB-16601])
- **Agent scorecards**: evidence-linked scorecards rank orchestrators, implementers, reviewers and pilots, with drill-down to the tasks and runs behind each score. The dashboard has role tabs, and `orbit agents` and `orbit.agents.*` expose the same scores. ([ORB-16952])
- **Drain readiness and failure history**: `orbit run readiness` and `orbit run show` name admitted and waiting tasks and why they wait, and `orbit run history --failures` reports each leaf failure once by root cause, with a link to its logs. ([ORB-16918])
- **Host resource analytics**: Analytics charts host CPU and memory profiles alongside forecast accuracy and measured delivery peaks, so operators can see where forecasts miss when tuning auto-concurrency. ([ORB-16616])
- **Desktop fleet awareness**: the desktop tray watches registered remote hosts and notifies when a drain ends or a task is held. Linux `.deb` and `.AppImage` bundles for x86_64 and arm64 are attached to the release. ([ORB-16646])

## 0.29.0 — 2026-10-18

### Breaking Changes

- **`orbit logs` removed**: the hidden top-level `orbit logs <run-id>` compatibility command is gone → use `orbit run show` for steps and `orbit run logs` for raw output. ([ORB-16506])
- **`orbit run ship-local` removed**: the hidden refusal-only alias now fails as an unknown command → use `orbit run ship --mode local`. ([ORB-16505])
- **`orbit tool list --include-hidden` removed**: the hidden alias no longer parses → use `orbit tool list --all`. ([ORB-16456])
- **Scoreboard API reduced**: `GET /api/scoreboard` now returns the `GET /api/cost` subset with deprecation headers, dropping `agents`, `highlights`, `notable_completions` and `coverage`, until its 2026-11-12 sunset → read `/api/cost`. `scoring.enabled` is ignored. ([ORB-15922])
- **Forecast memory threshold must sit below the throttle**: config load now rejects `machine.auto_concurrency.admit_memory_percent` (default 75) at or above `workflow.resource_throttle.memory_high_percent`, even with auto-concurrency off → keep the throttle above 75 or lower the admit percent. ([ORB-15769])

### Highlights

- **Muse Code executor**: Meta's Muse Code joins the built-in executors as the `muse` lane on macOS, with a seeded crew, `orbit init` detection and sandbox grants for its state directories. Existing crews and pools are unchanged. ([ORB-16577])
- **Forecast-based admission**: with `machine.auto_concurrency.mode` set to `shadow` or `enforce`, drains fit deliveries against measured CPU and memory profiles and shared host reservations. `orbit run profiles --accuracy` reports how good the forecasts were, and `orbit run auto --auto-concurrency` picks the mode per window. ([ORB-15772])
- **Fleet drain overview**: the dashboard's All workspaces view shows every registered host and workspace's drain, with phase, slots in use, eligible work and the most common wait reason. Clicking a row switches to that host and workspace. ([ORB-16360])
- **Review for one run**: `orbit run auto|ship --review before-pr|before-landing` adds a review layer to a single run without changing workspace policy. It can only strengthen the configured review, and run details show the layer used. ([ORB-16572])
- **Auto-fix PR blockers**: `orbit run auto --auto-fix` repairs a delivery PR's failed required checks and branch conflicts through the existing recovery and validation path. It never merges; `--complete` stays the merge authority. ([ORB-16070])
- **Post-deploy criteria stay owed**: criteria that can only be measured on the deployed build are recorded as owed instead of failing the delivery, and repair handoffs or ordinary completion can't drop them. ([ORB-16514])

## 0.28.3 — 2026-10-12

### Highlights

- **Settings and Incidents redesign**: Settings shows every key's effective value, where it comes from and its built-in default, across the Effective, Workspace and Global views. Health › Incidents groups failures by a deterministic cause, with class tiles and a 24-hour timeline per incident. ([ORB-15430])
- **Faster dashboard polling**: polls reuse memoized scoreboard and diagnostics results, diagnostics payloads are bounded, JSON responses are compressed, and routine status no longer fetches from the remote, so typical polls drop from seconds to milliseconds. ([ORB-15529])
- **Faster on Apple silicon**: binary digests use hardware SHA-2 and the CPU baseline is shared, so each `orbit` process starts sooner and `orbit run readiness` drops from about 1.2 s to under 0.1 s. ([ORB-15531])
- **Sturdier distributed claims**: a failed claimed leaf keeps its uncommitted candidate, and Orbit builds the deferred-sandbox record itself. The owner captures its validation gates into the claim, and tests that also fail on the base, or pass when rerun alone, no longer block a candidate. ([ORB-15511])
- **Review-finding verification**: the `finding_verification_pipeline` job confirms or rejects review-filed bugs with file-and-line evidence. The opt-in `review.verify_findings` setting, off by default and toggleable in Settings, makes promotion and admission wait for a confirmed verdict. ([ORB-15662])
- **System crew retired**: system work (auto-task templates, the task pilot and recoveries) draws its crew from the complexity pools. `workflow.system_crew` and `crew: system` still load for one release with a deprecation warning → remove them. ([ORB-15663])

## 0.28.2 — 2026-10-11

### Highlights

- **Extend a live drain**: `orbit run extend <run-id> --for <duration>` moves a running drain's deadline later without restarting it, so in-flight tasks, claims and authority carry over. `--concurrency` retunes it in the same call. It never shortens a window and refuses stopped or windowless drains. ([ORB-15383])
- **Review crew pools**: `operation.review_crew` accepts a pool such as `["sol", "grok"]` (or weighted entries) as well as one crew. Each before-PR, before-landing or after-landing review draws one crew, preferring one that did not implement the work and skipping disabled or provider-limited crews. ([ORB-15195])
- **Red base gets an owner**: a baseline-red hold names the tests failing on the base in its hold text, verdicts and `orbit run readiness`. The owner's clock tick files one deduplicated `baseline-red-repair` bug per failure set, and reviewer red-base claims replay only those tests. ([ORB-15374])
- **One incident per failure**: a gate or auto run that failed only because a child failed records that leaf as its `root_cause`, and `orbit run history --incidents` and the dashboard fold them into one row. Red-base holds now end `held`, and parents whose unsuccessful children were all cancelled end `cancelled`. ([ORB-15202])
- **Dashboard redesign and codebase analytics**: tasks show as status cards whose readiness dot says why backlog work waits, and drain status and controls get their own cards. Audit › Codebase replaces Audit › Policy and ranks files, directories or crates by regression rate, lock contention, footprint misses and blocked runs. ([ORB-15355])
- **Linux worker containment**: `machine.worker_cpu_quota` caps contained worker scopes with systemd `CPUQuota=`, and `orbit doctor` reports the limit. Pass-listed credentials reach workers through their environment instead of `systemd-run` arguments, so a process listing no longer shows provider tokens. ([ORB-15196])

## 0.28.1 — 2026-10-10

### Highlights

- **Provider-limit-aware routing**: drains and crew-pool draws skip crews whose provider is near or at its usage limit and resume them after the reset. `orbit run readiness` shows the limits and gated crews, and operators can declare rolling budgets for providers that report no usage. ([ORB-14697])
- **Before-landing review**: `review.before_landing` reviews a task's open PR while hosted CI runs, and the PR lands only on an accepted review. It is an alternative to before-PR review and is off by default. ([ORB-14849])
- **Dashboard host switcher**: pick a registered host above the workspace picker, and every panel, action, log tail and resource chip follows it. The selection is URL-addressable as `?host=`. ([ORB-14680])
- **Run history by task**: `orbit run history` shows each run's task and duration and filters by `--task`, `--state` and `--since`, so you can find the run that delivered a task and how long it took. ([ORB-14597])
- **Declared reclaim paths**: `worktree.reclaim` globs declare which build outputs in kept run worktrees may be swept, replacing the hard-coded `target/`. ([ORB-14850])
- **macOS `.env` deny hardening**: renaming a containing directory out of the workspace no longer bypasses a `**/.env` sandbox deny on macOS. ([ORB-14929])
- **Claude worker token on macOS**: a Claude activity that receives neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` is now refused before `claude` starts, instead of falling back to the Claude Desktop login, which the Desktop revokes mid-run (HTTP 401). Create a token with `claude setup-token` and list it in `[execution.env] pass`. For scheduled runs, the clock tick now exports credentials from `~/.orbit/clock.env` (mode `0600`) under the names `pass` admits, and `orbit doctor`'s `claude-worker-token` row warns when a routed Claude crew would run without the token. ([ORB-15154])

## 0.28.0 — 2026-10-08

### Breaking Changes

- **Distributed pull protocol**: the revision goes from 5 to 10, and a newer owner refuses older followers → run matching owner and follower versions. ([ORB-14261])
- **Job-run state migration**: SQLite migration 38 moves run state into `job_run_states` and drops `job_runs.pipeline_state_json`. It is declared Breaking, so older binaries refuse the migrated store. ([ORB-14585])
- **`terra` is no longer a built-in crew**: a config that names terra without defining `[crews.terra]` must define it. Existing `[crews.terra]` tables keep working. ([ORB-14672])

### Highlights

- **Host registry and routing**: `orbit host add|list|show|remove` keeps a registry of named hosts. Task IDs route to the host that owns their prefix, `--host` selects one explicitly, and Settings › Hosts manages the registry from the dashboard. ([ORB-14448])
- **Linux sandbox setup**: Linux installs can use Orbit's signed, pinned Bubblewrap when the host wrapper is missing, and package-manager discovery follows host capabilities. `orbit init` finishes with a readiness warning if preparation fails, and execution stays fail-closed until the sandbox is ready. ([ORB-14075])
- **Faster run, dashboard and scoreboard queries**: run listings no longer scan every run's pipeline state, and scoreboard failure aggregation and cold dashboard paths are optimized, taking pages that took many seconds down to a fraction of that. ([ORB-14585])
- **Distributed claimed review and recovery**: claimed PR work runs the owner's before-PR review contract and gets one automatic repair when landing stops on a stale or conflicting base. Operators can reconcile a merged PR whose final head differs from the candidate. ([ORB-13908])
- **Proposed-task intake**: `orbit run auto --approve-proposed` promotes qualifying proposed tasks and holds uncertain ones. The dashboard auto-drain panel offers it when starting a window, and proposed and backlog tasks show a readiness chip. ([ORB-14117])
- **Built-in Haiku crew**: fresh configs ship a Claude Haiku crew, and `orbit init` seeds complexity pools from the provider families it detects. ([ORB-14671])

## 0.27.0

### Breaking Changes

- **Review config split**: before-PR review is `review.before_pr`; after-landing review is the `delivery-code-review` auto-task's `enabled` flag and now covers follower landings. `operation.review_policy` and `review_minutes` still load with a warning, and `review.minutes` now limits one candidate's review. ([ORB-13992], [ORB-13894])
- **`delivery-qa` auto-task removed**: it is no longer seeded, and the `integrated_qa_v1` coverage is refused for new or updated schedules. Existing copies still load. QA is covered by `qa-sweep` and `qa-full-sweep`. ([ORB-13993])

### Highlights

- **Final recovery for blocked tasks**: a task that ends up `blocked` after its step recovery is used up now gets one last recovery agent, drawn from `workflow.final_recovery_crews` (default `sol:100, opus:20`). That agent can repair, requeue or resume the task instead of leaving it for an operator; `[]` turns it off. ([ORB-13898])
- **Before-PR reviewer fixes what it finds**: the reviewer commits its fixes as a second commit on the candidate instead of sending the task back for rework. Each candidate gets one review, limited by `review.minutes`. ([ORB-13989])
- **Re-runs resume from a preserved candidate**: re-running a task that has a `[BLOCKED]` candidate rebases and re-validates that candidate instead of implementing it again. ([ORB-13985])
- **Host resource throttle**: Orbit samples CPU, memory and disk, shows them as KPI chips and in a Settings › System tab on the dashboard, and holds drain and ship admissions while the host is under pressure. It is on by default, with warnings over the CLI and MCP. ([ORB-11415])
- **Agent invocations you can wait on**: `orbit run agent --wait` returns the agent's structured answer (summary, findings, next steps), `orbit run logs --follow` streams output, and `orbit_workflow_run_show` reports progress while it runs. ([ORB-13899])
- **Orbit closes its own PRs**: when a task lands, is rejected or is archived, Orbit closes the task's open delivery and `[BLOCKED]` PRs that it authored, keeping the PR that landed open. Human PRs are never touched. Turn it off with `pr.close_on_terminal = false`. ([ORB-13984])

## 0.26.0

### Breaking Changes

- **Public MCP tool surface consolidated**: `orbit_crew_list`, `orbit_friction_list`, `orbit_friction_rehome`, `orbit_auto_task_toggle`, `orbit_auto_task_delete`, and `orbit_workflow_run_workers` are gone. Use `orbit_workspace_list` with `include: ["crews"]`, `orbit_search` with `kind: "friction"`, `rehome_to` on `orbit_friction_update`, `enabled` on `orbit_auto_task_update`, and `orbit_workflow_auto` with `action: "resize"`. ([ORB-13809])
- **Distributed-drain tools removed from MCP**: `orbit_task_pull` and the `orbit_drain_*` probe, receipt, bind, and settle tools are no longer public, and calls to them are refused. `orbit run auto --pull` is unchanged. ([ORB-13806])
- **Older-client desktop compatibility removed**: the desktop MCP server no longer answers MCP peers that predate the guarded desktop contracts. Upgrade the desktop client to this release.
- **Desktop controls moved to guarded domain contracts**: desktop task, auto-task, and drain controls go through guarded MCP contracts, and guarded `orbit_task_*` fields are refused on older MCP peers. Upgrade MCP peers with the server.

### Highlights

- **Orbit mod for Claude Code**: the Claude Code plugin adds a band above the prompt, a status line, and an Orbit pane with Board, Ship, and Map tabs, so you can act on tasks and watch a ship without leaving the session. ([ORB-13862])
- **Desktop control center**: the desktop app gains a control center with guarded task review, and its Tasks list groups tasks by status, defaults to active ones, and carries the task controls. ([ORB-13843])
- **No bare agent spawns**: dispatch now refuses when a shipped agent executor has no sandbox backend for the host OS instead of running it unsandboxed. ([ORB-13857])
- **Disabled external tools stay manageable**: a disabled external tool remains listed and inspectable, and can be re-enabled.
- **Workspace-safe dashboard**: task edits, approvals, run actions, friction results, and plugin toggles made around a workspace switch no longer land on, or display under, the wrong workspace.

## 0.25.1

### Highlights

- **Cancel stops the agent**: `orbit run cancel` now stops the provider CLIs a run spawned, not just its owner, and `orbit run ship` refuses a task the pipeline cannot start from instead of reporting it submitted.
- **Symlinked homes and checkouts**: an Orbit home or workspace reached through a symlink (macOS `/tmp` and `/var`, a symlinked `~/workspace`) no longer breaks auto-task edits, plugin enablement, routine disables or `workspace remove`, and the plugin write-root containment check resolves `..` as the kernel does.
- **Tighter sandboxing and secret masking**: CLI workers inherit the sandbox on Linux and macOS, onboarding prepares the Linux sandbox prerequisites, and delivered plugin secrets and credential-bearing dotenv files are masked. ([ORB-13689])
- **MCP annotations and field projection**: `tools/list` advertises read-only, destructive and open-world hints so clients can auto-approve read-only tools, and `orbit_task_list` accepts `fields` to return only the fields you need.
- **Faster listings, search and dashboard**: filtered `task list`, search, the dashboard scoreboard and auto-drain readiness read task metadata instead of whole task bundles, cutting them from hundreds of milliseconds to tens at about 1,500 tasks.
- **Clearer runs and drains**: `orbit run show` summarizes a drain's leaf outcomes and names the holder of a waited-on lock, blocked tasks name their next step, and every confirmation command honours `--format json`.

## 0.25.0

### Breaking Changes

- **Plugin source root moved**: plugin sources must live under `.orbit-plugin/`; a top-level `plugin.yaml` is refused by `orbit plugin add`, `validate`, `test`, `sync`, and `upgrade`, with no fallback. Move `plugin.yaml` and its tree into `.orbit-plugin/` → `orbit plugin scaffold` creates the new shape. ([ORB-13626])

### Highlights

- **Follower pull**: `orbit run auto --pull` lets a follower machine pull one owner-admitted task at a time, run it, and hand the result back; the owner keeps landing authority. ([ORB-13625])
- **Plugin secrets and Python backends**: plugins declare secrets that the host stores, delivers to backends, and rotates, and exec backends can be written in Python. ([ORB-13080])
- **Brokered plugin calls and per-workspace enablement**: sandboxed agents call plugin tools through a broker, and plugins are enabled per workspace from the CLI or dashboard. ([ORB-13249])
- **New recurring auto-tasks**: disabled-by-default documentation, run-failure, and backlog-hygiene auto-tasks, plus delete and restore for shipped defaults with a durable opt-out. ([ORB-12930])
- **Deny-list agent policy**: shipped agent activities now name the operations they deny instead of allow-listing tools; custom allow-list activities keep working, and strict worker containment is opt-in. ([ORB-13315])
- **Safer upgrades**: upgrade admission checks store and layout compatibility, so an older binary can read but no longer write a store migrated by a newer one. ([ORB-13631])

## 0.24.0

### Breaking Changes

- **Activity, executor, and policy commands removed**: `orbit activity`, `orbit executor`, and `orbit policy` are gone with no aliases and now fail as unknown subcommands. `job`, `tool`, and `plugin` only moved in help. ([ORB-12893])
- **Operation mode removed**: the `orbit operation` CLI group, `orbit run auto --grant`, and the `orbit.operation.*` tools are gone. Store migration v26 drops operation grants, so older binaries fail against an upgraded store. ([ORB-12770])
- **Docs command and tools removed**: `orbit docs` and the five `orbit.docs.*` tools are gone. A leftover `[docs]` config section is ignored with a one-time warning → delete it. ([ORB-12741])
- **Host renamed to machine**: `orbit host` is removed; `~/.orbit/host.toml` moves into `[machine]` in global `config.toml` (copied automatically this release), and `host` fields become `machine_name` / `job_run_machine`. Migration v23 renames audit columns, so older binaries fail against an upgraded store. ([ORB-12725])
- **Semantic and hybrid search removed**: `orbit semantic`, `orbit search similar`, `--hybrid`, and the `semantic` / `hybrid` inputs on `orbit.search` are gone. Lexical `orbit search` / `orbit.search` is unchanged. ([ORB-12742])

### Highlights

- **Plugin standard**: `plugin.yaml` v2 plugins install and manage through `orbit plugin`, run under enforced grants in a sandboxed exec or `mcp` backend, and can ship tools, jobs, routines, auto-tasks, skills, and a schema-derived `orbit <ns> <verb>` CLI. ([ORB-12735])
- **Distributed drain v1**: drains across machines admit work atomically through durable claims with replayable receipts, pull and bind leaves uniquely, and hand results back to the owning workspace. ([ORB-12493])
- **Complexity crew pools**: a new `xhard` tier, weighted `name:weight` pool entries, and pool routing for ordinary `orbit run ship` admission; the drawn crew is fixed when the task is created. ([ORB-12605])
- **State-triggered task-pilot**: task-pilot runs as a `preparation_eligible` state routine and batches due tasks into one run instead of polling on a cron. ([ORB-12745])
- **Dashboard Config tab and runnable jobs**: the dashboard shows effective config grouped with provenance and inline edits, and the Jobs pane can start runs. ([ORB-12724])
- **Bounded worker runs**: on Linux, pipeline worker runs execute inside a bounded cgroup, so a runaway run cannot run the host out of memory. ([ORB-12903])

## 0.23.0

### Breaking Changes

- **MCP task-write response projections**: `orbit.task.add`, `orbit.task.update`, and `orbit.task.reject` no longer include `comments` and `history` by default; request those fields explicitly with `fields` or `field`. ([DANI-10379])

### Highlights

- **Federated semantic search**: search fans out concurrently across workspaces, reuses one query embedder, and batches embedding and reindex work for faster large-corpus queries. ([DANI-10365])
- **Workflow failure recovery**: agent-declared failed envelopes now enter step recovery instead of sending otherwise recoverable tasks directly to `blocked`. ([DANI-10438])
- **Safer process supervision**: execution cleanup validates process-group identity before signaling, preventing PID reuse from killing an unrelated process group. ([DANI-10447])
- **Reliable worktree cleanup**: garbage collection can continue cleaning stale worktrees whose large build directories exceed a single cleanup window. ([DANI-10448])
- **Actionable job diagnostics**: strict job catalog reads now surface malformed job files instead of silently hiding them from listing, inspection, and execution. ([DANI-10500])

## 0.22.1

### Highlights

- **`orbit run task-pilot`**: named entrypoint for `task_pilot_pipeline`, matching `orbit run triage` for on-demand discovery or explicit task IDs. ([ORB-12437])
- **`make install` targets `~/.orbit/bin`**: the source-checkout install now lands in the same directory as `install.sh` (override with `INSTALL_BIN_DIR`), so a stale `~/.cargo/bin/orbit` can no longer shadow the deployed binary on `PATH`.
- **Dashboard: force any task status**: a human can force any task to any status from the dashboard, matching the CLI's `--force` escape hatch. ([ORB-12445])
- **Forward-compatible store opens**: Orbit no longer hard-fails when a workspace's store schema or layout version is newer than the running binary supports; it opens read-only or auto-migrates instead. ([ORB-12434])

## 0.22.0

### Breaking Changes

- **Task start and approve tools consolidated**: `orbit task start` CLI command and `orbit.task.start` / `orbit.task.approve` MCP tools are removed in favor of `orbit.task.update`. ([ORB-12268])
- **Operation tools withdrawn from MCP**: all five `orbit.operation.*` tools are removed from the MCP surface; operations are managed via the Orbit CLI. ([ORB-12267])
- **MCP task listing response shape updated**: `orbit.task.list` returns `{ tasks, total, truncated }` instead of `{ items }`; update MCP callers to consume the `tasks` array. ([ORB-12195])
- **Routine hosts and source role configuration removed**: routine `hosts:` pins and `[routines] role = "source"` configuration schemas are removed in favor of host clock consolidation. ([ORB-12236])

### Highlights

- **Dashboard inline task editors**: the expanded task detail edits complexity, description, tags, acceptance criteria, and context files in place, each saved as a single-field `PATCH /api/tasks/:id` with inline errors and an allow-missing-context escape. ([ORB-12235])
- **Unified host clock consolidation**: `orbit clock tick` evaluates routines and auto-tasks in a single host loop, retiring separate auto-task schedulers and simplifying multi-host execution. ([ORB-12237])
- **Owner-wins cross-host task sync**: cross-host synchronization safely imports foreign-prefix task bundles while preserving local task state and preventing overwrites. ([ORB-12126])
- **Delivery auto-task reset and self-healing**: added `auto-task reset` and automated replay proof for diverged history recovery instead of silent per-tick deferrals. ([ORB-12346])
- **Effective provider sandbox visibility**: `orbit.agent.invoke` surfaces the active execution sandbox and warns when running under full-access mode. ([ORB-12249])

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

### Breaking Changes

- **Task locks returned to task administration**: `orbit locks` is now `orbit task locks`. This partially reverses ORB-10000: unlike the other removed task verbs, which were consolidated into existing verbs, locks were only relocated to a more prominent top-level surface. Moving them back completes the original surface-reduction intent without changing the MCP tool surface. ([ORB-10428])
- **Learning vote and comment surfaces removed**: `orbit.learning.upvote` and all `orbit.learning.comment.*` tools and CLI subcommands are gone — use `priority` + search rank for ranking, `update`/`supersede` for corrections, and `evidence` for provenance. ([ORB-10046])
- **`orbit task` trimmed from 19 to 12 subcommands**: seven state-transition and read-projection verbs removed. `approve`/`reject`/`unarchive`/`delete` → `orbit task update --status …`; `locks` → `orbit locks`; `prune-context` → `orbit task lint --fix`; `templates` dropped. MCP tool surface unchanged. ([ORB-10000])

### Highlights

- **Graph-tool guidance lives only in the MCP surface**: removed graph-tool invocation instructions from activity prompts and deleted the `orbit-graph` skill; graph tools self-describe when present, and essential selector/usage knowledge now lives in the `orbit.graph.*` tool descriptions. ([ORB-10151])
- **QA sweep is an auto-task**: the checked-in six-hour definition routes hands-on validation to `crews.qa`, dedupes open runs, and files findings through Orbit; `no-diff-expected` exempts side-effect-only tasks from workflow diff gates. Legacy QA code, config, CLI, routines, and deployment units are removed. ([ORB-10148])
- **Auto-task primitive**: recurring chores are data, not code. `orbit auto-task add/list/show/update/toggle` (+ `orbit.auto_task.*` tools) define `.orbit/auto_tasks/*.yaml` templates with a cron/interval schedule and dedupe policy; one seeded scheduler routine mints tasks from the due ones with catch-up collapse and `skip_if_open` dedupe. Provider-neutral (ADR-0217). ([ORB-10149])
- **Routine scheduler health over HTTP**: `GET /api/routines` on the dashboard exposes each routine's last fire (timestamp, ok/error outcome, duration) and next-due slot, so a stopped scheduler is visible remotely as a stale `last_fire` without box ssh. ([ORB-10138])
- **ADR create over HTTP**: `POST /api/adrs` records a Proposed ADR (mirroring `orbit.adr.add`), so remote orchestrators can author decisions without an on-box run; malformed payloads get a structured 400. Friction/learning create routes stay absent. ([ORB-10141])
- **Learning + ADR update routes on the HTTP API**: `PATCH /learnings/:id` (summary/scope/tags/body/evidence/priority) and `PATCH /adrs/:id` (status/tags and mutable metadata) delegate to the CLI tools, preserving supersede-don't-delete and invalid-transition rejection; no create routes added. ([ORB-10143])
- **Default failed-run triage**: a seeded `task_triage_pipeline` (+ hourly routine, `orbit run triage`) diagnoses tasks blocked by failed runs, re-backlogs environmental failures with a bounded retry budget, and leaves the rest blocked with a diagnosis attached. See ADR-0215/ADR-0216. ([ORB-10129])
- **Orphaned pending job runs now reconcile to `interrupted`**: workers claim their queued run's pid, the open-time orphan scan and `orbit doctor` cover pending runs, and `orbit run cancel <run_id>` terminalizes stuck runs. ([ORB-10070])
- **Remote dashboard task actions now reach the selected workspace**: `approve`/`reject`/`archive` used a raw `fetch()` that skipped the workspace-routing helper, so they silently no-op'd against a non-default remote workspace; the workspace selector also no longer renders its filesystem path beneath the dropdown. ([ORB-10124])
- **Codex init default avoids nested sandboxing**: fresh Orbit configurations now seed Codex with `danger-full-access`, leaving Orbit and the host as the execution boundary. ([ORB-10131])
- **Crews are one provider-model assignment**: `[crews.<name>]` now uses flat `model`, `provider`, and `backend` fields shared by every activity role; legacy three-role crews load through their implementer assignment. See ADR-0213. ([ORB-10130])
- **`orbit web serve` defaults to global (multi-workspace) mode**: the all-workspaces dashboard dropdown is always present; `--global` is now a deprecated no-op. ([ORB-10029])
- **Versioned `.orbit/` upgrade path — `orbit migrate`**: the whole workspace layout is now versioned via a `state/layout.version` marker and an ordered migration registry; older layouts auto-migrate on open, newer ones refuse to open. `orbit migrate [--dry-run]` reports and inspects. ([ORB-10012])
- **Policy deny rules can no longer be dodged by respelling a path**: glob-path normalization rebuilds paths from their normal components so `secret/./key.txt`, `secret//key.txt`, and trailing-slash spellings all match rules for `secret/key.txt`; adds Linux enforcement integration tests and a CI sandbox/policy job. ([ORB-10009])
- **Task migration between machines**: new `orbit task export`/`import` pack and restore task bundles as a portable `tar.zst` with integrity validation and collision renumbering; `orbit task reindex` rebuilds the index from bundles, and `--task-id-start` seeds disjoint id ranges. ([ORB-00034])
- **`orbit web connect <ssh-host>`**: view a remote workspace's dashboard over an auto-managed SSH tunnel — picks a free port, starts remote serve, forwards, opens the browser, and tears down on Ctrl-C. ([ORB-00029])
- **Global, multi-workspace dashboard**: `orbit web serve` outside any workspace (or with `--global`) serves every registered workspace via a workspace-keyed runtime map, with a header selector and an aggregate "All workspaces" task view. ([ORB-00030])
- **Dashboard shows each workspace's filesystem location**: the global dashboard surfaces where each workspace lives on disk (home-abbreviated) in the header selector and task Details box, disambiguating same-named workspaces. ([ORB-00037])
- **Aggregate dashboard view no longer 400s**: an `isAggregateView()` predicate gates per-workspace fetches so the "All workspaces" view fetches only the aggregate task list and shows a placeholder for workspace-specific panels instead of erroring. ([ORB-00039])
- **Versioned store-schema migrations**: the store SQLite DB tracks numbered migrations in a `schema_meta` ledger, applies each atomically, adopts pre-ledger DBs via an idempotent baseline, and refuses to open a DB written by a newer orbit. ([ORB-10003])
- **Job-run checkpoint/resume for hard interruptions**: the v2 executor persists a recovery checkpoint after each completed step; dead-owner runs finalize to a new `interrupted` state, and `orbit job resume <run_id>` replays only the remaining steps. ([ORB-10002])
- **Always-on operability: detailed healthz and `orbit doctor`**: `/healthz?detailed=true` runs per-check probes, and `orbit doctor` runs workspace pass/warn/fail health checks. ([ORB-10005])

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

### Highlights

- **Grok onboarded as fourth agent family**: sandbox state dir + SBPL allowances; CLI runner + executor YAML; `orbit mcp init --client grok` writes `.grok/config.toml`; duel / scoreboard / friction-stats render zero-grok rows; design folder, commit identity, and docs updated. ([ORB-00043], [ORB-00044], [ORB-00045], [ORB-00046], [ORB-00047], [ORB-00048], [ORB-00049], [ORB-00050], [ORB-00052])
- **Knowledge tab in dashboard**: Learnings subtab (list, supersede, stats), Frictions subtab (triage, resolve, stats, tag-picker sourced from YAML), ADRs subtab (accept, supersede, related-task deep-links), task-detail enrichment, Locked Files panel, per-task crew selector, markdown rendering. ([ORB-00060], [ORB-00061], [ORB-00062], [ORB-00063], [ORB-00067], [ORB-00068], [ORB-00069], [ORB-00073], [ORB-00076], [ORB-00083])
- **Duel-plan agent pool configurable** via `[duel] candidates` + `[duel.models]` in `config.toml`; runtime-host trait methods for candidate/model resolution; preserves fallback for non-duel callers. ([ORB-00072])
- **Cross-artifact task relations**: `Produces` / `Resolves` variants enable typed task ↔ friction / learning links; auto-resolves frictions on Review → Done with a `resolved_by_task` back-pointer. ([ORB-00093])
- **Learning enrichments**: per-learning `comments.jsonl` with push-injection rendering; decay-weighted upvotes with task-anchored idempotency; learning-creation wired into agent activity loops with a checkpoint. ([ORB-00077], [ORB-00094], [ORB-00095])
- **Direct-agent runtime model injection**: `ExecutorDef.model_flag` enables data-driven `-m` / `--model` flag dispatch per step. ([ORB-00053])
- **Ship command unified to async-by-default**: `orbit run ship` empty-task-IDs → auto mode, explicit IDs → gated; waiting-reason fields surfaced through `orbit run history` / `show`. ([ORB-00074], [ORB-00075])
- **Backlog dependency gating**: `list_backlog_tasks` filters by `task_dependencies_ready`, fixing out-of-order auto-pipeline execution. ([ORB-00057])
- **Skill quality nudges**: `orbit-create-task` now teaches optional `complexity`, `dependencies`, `parent_id`, and cross-artifact `relations`; `orbit.friction.add` description enumerates the tag taxonomy from YAML. ([ORB-00064], [ORB-00070], [ORB-00104])

## 0.5.4

### Features

- **Project-learnings push-injection (L1/L2/L3)**: relevant learning summaries are now injected into agent context at three layers — engine pre-prompt before runtime spawn, MCP sidecar on path-bearing tools (`orbit.graph.show`, `orbit.graph.refs`, `orbit.task.show`), and a Claude Code `PreToolUse` hook on `Edit | Write | Read`. Summary-only payloads with per-session dedup, per-call caps, and an `ORBIT_SESSION_ID` envelope for cross-process dedup. ([ORB-00009])
- **First-class design-docs surface (`orbit.design.*` + `orbit design check` CLI)**: four MCP tools (`init`, `list`, `show`, `check`) plus a Rust port of the design-doc decay checker, with `orbit workspace init --design` seeding `docs/design/CONVENTIONS.md` when absent. `make check-design-docs` and `scripts/check_design_doc_decay.py` now wrap the Rust path. ([ORB-00019])
- **Default-seeded skills aligned across asset, registry, plugin, and router catalogs**: `orbit-learning` and `orbit-design` onboarded; `orbit-review-task` and the semantic-search skill brought into the default seed and the plugin's `skills/` symlinks; three drift-detection unit tests guard the four catalogs against recurrence. Default seed bumped 7 → 11. ([ORB-00020], [ORB-00022])
- **`orbit workspace init --inject-agent-rules`**: opt-in flag writes an idempotent Orbit-rules block into `CLAUDE.md` and `AGENTS.md` at the workspace root, delimited by `<!-- orbit-managed:start/end -->` markers. Block content sourced from an editable asset; malformed marker pairs refuse to write. ([ORB-00023])
- **Inline task status transitions in the dashboard**: per-task actions row gains a status selector wired through the existing `PATCH /tasks/:id` backend, ordered by `STATUS_ORDER` with `done` last and excluding `rejected`/`archived`/`friction`. Surfaces a "no longer shown in dashboard list" notice when transitioning to `done`. ([ORB-00025])
- **`orbit.learning.*` exposed over MCP**: the full eight-tool learning surface (`add`, `list`, `search`, `show`, `update`, `supersede`, `prune`, `reindex`) is now reachable from every MCP client, not just `orbit tool run`. Restores parity with the `orbit-learning` skill instructions. ([ORB-00039])

## 0.5.3

### Features

- **Claude Code plugin SessionStart hook**: the Orbit plugin now ships a `SessionStart` hook (`plugin/hooks/check-workspace.sh`) that detects an uninitialized workspace via a pure filesystem walk and surfaces a `systemMessage` instructing the user to run `orbit init` / `orbit workspace init`. Closes the silent-no-op gap where `orbit mcp serve` would attach with zero tools and no in-session signal. ([ORB-00018])

### Fixes

- **No-diff PR handoffs**: `pr_open` now treats branches with zero commits ahead of the configured base as successful repository-noop handoffs, moves completed tasks to `review`, returns `pr_created: false`, and avoids creating an empty GitHub PR. ([ORB-00016])

## 0.5.1

### Fixes

- **Deprecation warning routes through `tracing`**: the `knowledge.task_id_pattern` deprecation notice was emitted via `writeln!` directly to stderr from `orbit-core`, contradicting the `CLAUDE.md` "`tracing` for diagnostics; `eprintln!` only in `orbit-cli`" rule and leaking into `cargo test` output. Now goes through `tracing::warn!` with a structured `config` field.

## 0.5.0

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

## 0.4.0

### Breaking Changes

- **Friction reports relocated**: friction is no longer a task type or status. Records live as append-only markdown under `.orbit/frictions/{yyyy}-{mm}/F{nnn}.md` and are managed through `orbit.friction.add/list/show/stats`. `orbit.task.add` rejects `type: friction` / `status: friction`; web API and scoreboard JSON drop `friction_bounty`. ([T20260510-13])
- **Task type taxonomy reduced**: the `task | feature | epic | issue | bug | chore | refactor | friction` enum collapses to `feature | bug | refactor | chore`. `orbit.task.add` and `orbit.task.update` reject the removed values; existing tasks were migrated. ([T20260510-14])
- **Attribution narrowed to `model`**: the `agent` field is removed from `Actor`; `orbit.task.add` rejects an `agent` parameter and Orbit infers the agent family from `model` via `agent_from_model`. MCP `orbit_task_list`, `orbit_task_search`, and `orbit_task_review_thread_list` responses are now object-shaped (previously top-level arrays) so Cursor and VS Code accept them. ([T20260510-15])
- **Knowledge-graph leaf IDs unified across extractors**: Python, Rust, Java, and TypeScript leaf selectors now use a single canonical form so SQL and JSON paths return set-equivalent results. `GRAPH_SQLITE_INDEX_SCHEMA_VERSION` bumps; consumers caching selectors must rebuild. ([T20260510-7])
- **Semantic search requires a companion binary**: `orbit-embed-companion` is installed separately via `orbit semantic install`; `orbit semantic *` and the matching MCP tools fail until it is present. ([T20260510-9], [T20260510-10])
- **`JobV2Step` rejects multi-body shapes**: YAML steps that previously parsed silently with both `target` and `parallel` (or any other body combination) now fail at load. ([T20260509-31])
- **`orbit-locks` skill removed**: the seeded `orbit-locks/SKILL.md` and the ad-hoc `orbit.task.locks*` instructions in the seeded `orbit` skill are gone — the gate pipeline still owns reservations. External agent prompts referencing the skill must be updated. ([T20260510-17])

### Features

- **Knowledge graph latency wins**: lazy source hydration via `GraphReadOptions`, a bounded default-ranking work cap on search, and a `BinaryHeap` top-K in `overview.top_files`. ([T20260509-65], [T20260509-67], [T20260509-68])
- **Knowledge command surface**: graph business logic — ranking, classification, fast-path orchestration — relocated into `orbit_knowledge::commands::*` so non-tool consumers share canonical behavior. ([T20260510-5])
- **Semantic search subsystem**: `orbit-embed` client, `orbit-embed-companion` binary, `embeddings` and `tasks_fts` SQLite schema, paragraph chunker, BLAKE3 dedup, task-mutation index hooks, and `orbit semantic install/uninstall/reindex/stats/search/related` CLI plus MCP surface. ([T20260510-3], [T20260510-9], [T20260510-10], [T20260510-20])
- **Task tags**: first-class `tags: Vec<String>` field with normalized SQLite index and `--tag` filtering on `orbit task list/search`. ([T20260510-12])
- **Activity/job runtime polish**: wildcard-aware tool allowlists honored at dispatch and HTTP-loop schema advertisement, asset-load-time allowlist validation, agent-loop `on_denial: continue`, literal-boolean condition atoms, and exclusive locking on duel scoreboard appends. ([T20260509-15], [T20260509-22], [T20260509-23], [T20260509-25], [T20260509-32])
- **Recovery role configurability**: seeded step-failure recovery activity uses `role: reviewer` and resolves agent/model from `[agent.reviewer]` config instead of hardcoded Codex. ([T20260509-14])
- **Done-task sync cap**: website task sync caps generated pages to the 100 most recent `done` tasks. ([T20260509-20])
- **Debug-job-failure skill**: seeded `orbit-debug-job-failure` SKILL.md teaching agents how to investigate failed/stuck/cancelled job runs across run state, audit events, blobs, and live processes. ([T20260509-79])

## 0.3.1

### Features

- **Pipeline dispatch reliability**: hardened parallel, gate, and epic pipelines with failed-child completion handling, longer task lock coverage, epic timeout/convergence fixes, resolved workspace subprocess cwd, and per-step agent log/error surfacing. ([T20260427-34], [T20260427-36], [T20260427-38], [T20260427-40], [T20260508-8], [T20260508-14])
- **Metrics and public docs**: split the public metrics surface into Operations and Scoreboard views, added done-task sync pages for orbit-cli.com, refreshed positioning/reference docs, and refined the website UI. ([T20260508-4], [T20260508-16], [T20260508-19], [T20260508-20], [T20260507-21])
- **Registry and benchmark tooling**: added the `orbit-registry` crate and identity-key benchmark harness for exercising knowledge graph selector stability. ([T20260507-12], [T20260508-2])

### Fixes

- **macOS sandbox and CLI execution**: allowed Claude's `$HOME/.claude.json` lock/tmp siblings, re-allowed the active job-run worktree after global deny rules, and demoted successful CLI exits when the inner Orbit envelope reported failure. ([T20260508-13], [T20260508-17])
- **Workflow defaults and links**: made workflow base branches resolve from `[workflow] base_branch` when CLI flags are omitted, and fixed task-ID links in generated PR bodies with an opt-in URL template. ([T20260508-11], [T20260508-12])
- **CI clippy guardrails**: grouped macOS sandbox spawn inputs into a request struct so strict workspace clippy passes under `-D warnings`. ([T20260508-21])

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

## 0.2.0

### Features

- **Parallel batch execution**: dispatch and execute multiple tasks in parallel with file-level conflict detection and shared worktrees
- **Auto-cleanup on merge**: ship workflow now deletes the remote branch after a successful PR merge

### Fixes

- **`--parallelism` flag**: serialized as JSON integer instead of string, fixing schema validation failure on `orbit run ship --parallelism N`
- **Stale default artifacts**: `orbit workspace init` now always refreshes default skills, activities, and jobs to their latest embedded versions (custom artifacts are preserved)
- **Clippy warning**: resolved unused-mut warning and removed clippy from CI

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
