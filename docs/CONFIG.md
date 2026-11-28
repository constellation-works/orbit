---
type: context
summary: Orbit Configuration
last_validated: 2026-11-22
---

# Orbit Configuration

See the [configuration reference index](config/README.md#orbit-configuration) for all topics, or follow an original section heading below.

## [Environment variables](config/environment.md#environment-variables)

## [Resident evolution ledger](config/README.md#resident-evolution-ledger)

## [Where config lives](config/README.md#where-config-lives)

## [Host-health auto-task](config/sandbox.md#host-health-auto-task)

## [Inspecting and editing](config/README.md#inspecting-and-editing)

## [`[machine]` — who this machine is](config/machine.md#machine--who-this-machine-is)
### [Dashboard over Tailscale Serve](config/machine.md#dashboard-over-tailscale-serve)
### [MCP roles and toolsets](config/machine.md#mcp-roles-and-toolsets)
### [`[machine.auto_concurrency]` — host forecast admission](config/machine.md#machineauto_concurrency--host-forecast-admission)
### [Worker containment](config/machine.md#worker-containment)

## [`[workflow]` — branch and crew defaults](config/workflow.md#workflow--branch-and-crew-defaults)
### [Task types](config/workflow.md#task-types)
### [Unlocked context paths](config/workflow.md#unlocked-context-paths)
### [Footprint lock modes](config/workflow.md#footprint-lock-modes)
### [Revalidating an unlocked-path repair](config/workflow-validation.md#revalidating-an-unlocked-path-repair)
### [Light delivery: `skip-review` and `skip-validation`](config/workflow-tags.md#light-delivery-skip-review-and-skip-validation)
### [Landed diff classification](config/workflow.md#landed-diff-classification)
### [Regression layers](config/workflow.md#regression-layers)
### [Review-skip tags: `workflow.review_skip_tags`](config/workflow-tags.md#review-skip-tags-workflowreview_skip_tags)
### [Generated paths are regenerated, not merged](config/workflow.md#generated-paths-are-regenerated-not-merged)
### [What `orbit init` seeds](config/workflow.md#what-orbit-init-seeds)
### [`[workflow.resource_throttle]` — host pressure](config/workflow.md#workflowresource_throttle--host-pressure)
### [`[workflow.task_pilot_freshness]` — when a task is piloted again](config/workflow.md#workflowtask_pilot_freshness--when-a-task-is-piloted-again)
### [`[workflow.activity_timeouts]` — CLI agent wall-clock budgets](config/workflow.md#workflowactivity_timeouts--cli-agent-wall-clock-budgets)
### [`[workflow.mid_run_steering]` — operator comments after dispatch](config/workflow.md#workflowmid_run_steering--operator-comments-after-dispatch)
### [`[workflow.standing_drain]` — the opt-in drain keeper](config/workflow.md#workflowstanding_drain--the-opt-in-drain-keeper)
### [Task riskiness scores](config/workflow.md#task-riskiness-scores)
### [Retired explorer configuration](config/workflow.md#retired-explorer-configuration)
### [`[workflow.agent_corpus]` — activity-scoped workspace knowledge](config/workflow.md#workflowagent_corpus--activity-scoped-workspace-knowledge)
### [`[workflow.tag_instructions.<tag>]` — extra instructions for tagged tasks](config/workflow-tags.md#workflowtag_instructionstag--extra-instructions-for-tagged-tasks)
### [`[workflow.validation_env]` — the toolchain required validation runs with](config/workflow-validation.md#workflowvalidation_env--the-toolchain-required-validation-runs-with)
#### [Workspace build output](config/workflow-validation.md#workspace-build-output)
#### [Incremental compilation](config/workflow-validation.md#incremental-compilation)
#### [Agent sessions share the configured PATH](config/workflow-validation.md#agent-sessions-share-the-configured-path)
#### [Required validation](config/workflow-validation.md#required-validation)
#### [Missing tools block the task](config/workflow-validation.md#missing-tools-block-the-task)
#### [Supervisor stops are inconclusive](config/workflow-validation.md#supervisor-stops-are-inconclusive)
#### [Network flakes are rerun](config/workflow-validation.md#network-flakes-are-rerun)
#### [Delivery refusals are held](config/workflow-validation.md#delivery-refusals-are-held)
#### [A red base is held, not blocked](config/workflow-validation.md#a-red-base-is-held-not-blocked)
#### [Agent-declared blockers stop the run](config/workflow-validation.md#agent-declared-blockers-stop-the-run)
#### [Owner sign-off holds the run (`owner_signoff_required`)](config/workflow-validation.md#owner-sign-off-holds-the-run-owner_signoff_required)

## [`[crews.<name>]` — which provider-model runs the task](config/crews.md#crewsname--which-provider-model-runs-the-task)
### [System work draws from the complexity pools](config/crews.md#system-work-draws-from-the-complexity-pools)
### [Purpose crew pools](config/crews.md#purpose-crew-pools)
### [Disabled crews](config/crews.md#disabled-crews)
### [Human crews](config/crews.md#human-crews)
### [The `no-drain` task tag](config/workflow-tags.md#the-no-drain-task-tag)
### [Per-crew credentials (`env`)](config/crews.md#per-crew-credentials-env)
### [Repairing a config that already names a crew with a colon](config/crews.md#repairing-a-config-that-already-names-a-crew-with-a-colon)

## [Provider identity and resolution](config/providers.md#provider-identity-and-resolution)
### [Canonical providers](config/providers.md#canonical-providers)
### [Resolution precedence](config/providers.md#resolution-precedence)
### [No silent fallback](config/providers.md#no-silent-fallback)

## [Provider CLI notes](config/providers.md#provider-cli-notes)
### [Retired providers](config/providers.md#retired-providers)
### [GitHub Copilot CLI](config/providers.md#github-copilot-cli)
### [Antigravity CLI](config/providers.md#antigravity-cli)
### [Muse Code CLI](config/providers.md#muse-code-cli)
### [MCP client registration](config/providers.md#mcp-client-registration)

## [Per-task crew override](config/crews.md#per-task-crew-override)
### [Automatic crew pools by complexity](config/crews.md#automatic-crew-pools-by-complexity)
#### [Weighting a pool](config/crews.md#weighting-a-pool)
#### [Type pools by task type and complexity](config/crews.md#type-pools-by-task-type-and-complexity)
### [Provider failure holds](config/crew-recovery.md#provider-failure-holds)
### [Provider usage limits](config/crews.md#provider-usage-limits)
#### [Usage-pressure draws](config/crews.md#usage-pressure-draws)
#### [Budgets for providers that report no usage](config/crews.md#budgets-for-providers-that-report-no-usage)
### [Final recovery pool](config/crew-recovery.md#final-recovery-pool)
### [PR auto-fix repairs](config/crew-recovery.md#pr-auto-fix-repairs)
### [Setting `task.crew`](config/crews.md#setting-taskcrew)
### [What "ran" vs what "was selected"](config/crews.md#what-ran-vs-what-was-selected)

## [Sandbox write grants for the shared Cargo caches](config/sandbox.md#sandbox-write-grants-for-the-shared-cargo-caches)

## [Sandbox pseudo-tty allocation (macOS)](config/sandbox.md#sandbox-pseudo-tty-allocation-macos)

## [Executor `host_os`](config/providers.md#executor-host_os)

## [Capability labels: `machine.labels` and `requires_labels`](config/machine.md#capability-labels-machinelabels-and-requires_labels)

## [Executor authentication recovery probes](config/providers.md#executor-authentication-recovery-probes)

## [Executor inspection tools (`inspection_args`)](config/providers.md#executor-inspection-tools-inspection_args)

## [Executor image input (`image_input`)](config/providers.md#executor-image-input-image_input)

## [`[execution.env]` — the agent subprocess environment](config/execution.md#executionenv--the-agent-subprocess-environment)
### [Provider credentials](config/execution.md#provider-credentials)

## [Other sections](config/operations.md#other-sections)
### [Execution](config/execution.md#execution)
### [`[review]` and `[operation]` — automatic review](config/review.md#review-and-operation--automatic-review)
#### [Owed host evidence](config/review.md#owed-host-evidence)
#### [Distributed PR delivery](config/review.md#distributed-pr-delivery)
### [`[[completion.hold]]` — keep risky completions in review](config/review.md#completionhold--keep-risky-completions-in-review)
### [`[pr]` — pull requests](config/delivery.md#pr--pull-requests)
### [`[commit]` and `[branch]` — commit messages and branch names](config/delivery.md#commit-and-branch--commit-messages-and-branch-names)
#### [The commit-message standard](config/delivery.md#the-commit-message-standard)
### [Delivery templates: `[pr]`, `[commit]` and `[branch]`](config/delivery.md#delivery-templates-pr-commit-and-branch)
### [Housekeeping](config/operations.md#housekeeping)
### [`[jobs]` — failing-job warning](config/operations.md#jobs--failing-job-warning)
### [Fleet notices — fixed limits](config/operations.md#fleet-notices--fixed-limits)
### [GitHub run-log cache — fixed limits](config/operations.md#github-run-log-cache--fixed-limits)
### [`[machine.github_quota]` — the host's GitHub API quota](config/machine.md#machinegithub_quota--the-hosts-github-api-quota)
### [`[security_alert_sweep]` — security finding severity](config/operations.md#security_alert_sweep--security-finding-severity)
### [Path leases](config/workflow.md#path-leases)
### [`[experiments]` — experiments over agent treatments](config/extensions.md#experiments--experiments-over-agent-treatments)
### [Crew memory — per-model memory (dormant, experiment-only)](config/extensions.md#crew-memory--per-model-memory-dormant-experiment-only)
### [`[seeding]` — which shipped defaults a workspace carries](config/extensions.md#seeding--which-shipped-defaults-a-workspace-carries)
#### [Workspace presets](config/extensions.md#workspace-presets)

## [Plugins — `.orbit/plugins.yaml` and `[plugins.<ns>]`](config/extensions.md#plugins--orbitpluginsyaml-and-pluginsns)

## [Validation and errors](config/validation.md#validation-and-errors)
