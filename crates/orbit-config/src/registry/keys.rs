use super::*;

/// The choices an enum key accepts, from its declared allowed values; empty
/// for every other key.
pub fn config_key_options(key: &str) -> Vec<&'static str> {
    describe(key)
        .and_then(|descriptor| descriptor.allowed.choices())
        .map(<[&str]>::to_vec)
        .unwrap_or_default()
}

/// Look up one registry key's metadata.
pub fn describe(key: &str) -> Option<&'static ConfigKeyDescriptor> {
    CONFIG_KEY_REGISTRY.iter().find(|entry| entry.key == key)
}

/// Fixed keys retired from the registry that an existing `config.toml` may
/// still carry. Loading warns and ignores each one for one release (see
/// `resolved::warn_compatibility_keys`); `orbit config get`/`set` refuse it
/// with the migration note instead of a did-you-mean, so the operator learns
/// the key is gone rather than misspelled. Delete an entry together with its
/// load warning once the release window has passed.
pub(crate) const REMOVED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "plugin.legacy_callback_identity",
        "plugin callbacks require the host-issued session descriptor; the environment token and \
         process ancestry no longer authorize callbacks. Delete this key and keep file descriptor \
         3 open across backend wrappers, exec and setsid",
    ),
    (
        "workflow.pilot_max_complexity",
        "the task pilot applies its assessed complexity as-is; route a tier with \
     workflow.<tier>_complexity_crews or pin `crew` on the task instead",
    ),
    (
        "semantic",
        "semantic search was removed; delete the table and use lexical search",
    ),
    (
        "search.model",
        "search uses SQLite FTS5 and no longer selects a model; delete this key",
    ),
    // Operation mode was removed on 2026-09-21; the `[operation]` table keeps
    // only the deprecated `review_crew`. See docs/design/orbit-core/4_decisions.md.
    ("operation.preset", OPERATION_MODE_REMOVED_NOTE),
    ("operation.completion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.preparation", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.preparation_due_seconds",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.promotion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.leaf_ceiling", OPERATION_MODE_REMOVED_NOTE),
    ("operation.recovery", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.recovery_episodes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    (
        "operation.recovery_minutes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.delivery_cap", OPERATION_MODE_REMOVED_NOTE),
    // [ORB-13989] [ORB-13992] Each candidate gets one review, and its
    // reviewer fixes its findings in one commit; no repair cycle or extra
    // reviewer start is counted.
    (
        "operation.review_repair_cycles",
        "each candidate gets one review whose reviewer fixes its findings in one commit, so \
     no repair cycle is counted; review.minutes bounds that review",
    ),
    (
        "operation.review_reviewer_starts",
        "each candidate gets one review, so reviewer starts are no longer counted; \
     review.minutes bounds that review",
    ),
    // [ORB-15922] The agent-family scoreboard was retired on 2026-10-13 and
    // nothing records scoreboard metrics any more. Drop this entry after
    // 2026-11-12, when the key becomes an ordinary unknown setting.
    (
        "scoring.enabled",
        "deprecated and ignored: the agent-family scoreboard was retired and nothing records \
     scoreboard metrics; delete the [scoring] table. This key stops being recognised after \
     2026-11-12",
    ),
];

const OPERATION_MODE_REMOVED_NOTE: &str = "operation mode was removed; review settings live in \
     [review] (timing, minutes) and workflow.purpose_crews.review";

/// Keys retired from the registry that still load: loading translates each
/// into its replacement ([`crate::aliases`]) or, where its note says so,
/// ignores it, and warns that it is deprecated
/// (`resolved::warn_compatibility_keys`), and
/// `orbit config get`/`set` refuse it with this note. A later release makes
/// them errors; move each to its replacement before then [ORB-13992].
pub(crate) const DEPRECATED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "workflow.explore.enabled",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "workflow.explore.max_answer_bytes",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "workflow.explore.max_calls_per_run",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "workflow.explore.max_concurrent_per_run",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "workflow.explore.timeout_seconds",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "workflow.purpose_crews.explorer",
        "ignored: explorer delegation was removed; remove this key",
    ),
    (
        "commit.planned_by_trailer",
        "ignored: Planned-By is no longer written; read the implementer from Co-authored-by or task attribution",
    ),
    (
        "commit.implemented_by_trailer",
        "ignored: Implemented-By is no longer written; read the implementer from Co-authored-by or task attribution",
    ),
    (
        "operation.review_minutes",
        "translated to review.minutes, now the limit for one candidate's before-PR review; \
     move the value to [review] minutes",
    ),
    (
        "operation.review_policy",
        "translated: before-pr sets review.timing = \"before_pr\"; after-landing enables the \
     delivery-code-review auto-task until its own enabled flag is set (`orbit auto-task \
     toggle delivery-code-review on|off`); none turns neither on. Set review.timing \
     and the auto-task flag instead",
    ),
    // [ORB-16775] Consolidated key families. Each loads through its
    // replacement and `orbit doctor --fix-retired-config-keys` rewrites it.
    (
        "operation.review_crew",
        "translated to workflow.purpose_crews.review, the same crew or pool; move the value there",
    ),
    (
        "workflow.final_recovery_crews",
        "translated to workflow.purpose_crews.final_recovery, the same pool; move the value there",
    ),
    ("workflow.purpose_crews.ci_failure_sweep", SWEEP_POOL_NOTE),
    ("workflow.purpose_crews.dependabot", SWEEP_POOL_NOTE),
    ("workflow.purpose_crews.merged_head_repair", SWEEP_POOL_NOTE),
    ("review.before_pr", REVIEW_TIMING_NOTE),
    ("review.before_landing", REVIEW_TIMING_NOTE),
    (
        "machine.worker_containment_strict",
        "translated: true sets machine.worker_containment = \"strict\"; false keeps the \
     boolean's mode. Set machine.worker_containment to off, warn or strict instead",
    ),
    ("workflow.validation_env.login_shell", SHELL_PROBE_NOTE),
    ("workflow.validation_env.interactive", SHELL_PROBE_NOTE),
    // [ORB-15663] System work names no lane of its own: auto-task templates,
    // the task pilot and step-failure / PR conflict recovery draw from the
    // complexity pools. The key loads and warns, but no longer routes.
    (
        "workflow.system_crew",
        "ignored: system work (auto-task templates, the task pilot, step-failure and PR \
     conflict recovery) draws its crew from workflow.<tier>_complexity_crews for its \
     complexity. Delete the key and set those pools instead",
    ),
    (
        "workflow.regression_risk.record_sites",
        "ignored: regression.sites on regression_from links are always recorded; delete the key",
    ),
    (
        "workflow.auto_ship",
        "ignored: the ship sweep it opted into was removed; dispatch with `orbit run auto` \
     drains or `orbit run ship`, and delete the key",
    ),
    // [ORB-22803] The crew ladder was removed on 2026-11-07. Each key loads,
    // warns and is ignored; code-health-sweep deletes these entries after
    // 2026-12-07, when they become ordinary unknown settings.
    ("workflow.crew_ladder.mode", CREW_LADDER_REMOVED_NOTE),
    ("workflow.crew_ladder.rungs", CREW_LADDER_REMOVED_NOTE),
    ("workflow.crew_ladder.tiers", CREW_LADDER_REMOVED_NOTE),
    (
        "workflow.crew_ladder.explore_rate",
        CREW_LADDER_REMOVED_NOTE,
    ),
    ("workflow.crew_ladder.margin", CREW_LADDER_REMOVED_NOTE),
    ("workflow.crew_ladder.look_every", CREW_LADDER_REMOVED_NOTE),
    ("workflow.crew_ladder.max_sample", CREW_LADDER_REMOVED_NOTE),
    (
        "workflow.crew_ladder.exclude_types",
        CREW_LADDER_REMOVED_NOTE,
    ),
    (
        "workflow.crew_ladder.watch_window",
        CREW_LADDER_REMOVED_NOTE,
    ),
    (
        "workflow.crew_ladder.retry_after_days",
        CREW_LADDER_REMOVED_NOTE,
    ),
    (
        "workflow.crew_ladder.delivery_quality",
        CREW_LADDER_REMOVED_NOTE,
    ),
    (
        "workflow.crew_ladder.quality_grade_margin",
        CREW_LADDER_REMOVED_NOTE,
    ),
    (
        "workflow.crew_ladder.min_grade_coverage",
        CREW_LADDER_REMOVED_NOTE,
    ),
    // [ORB-22806] Regression-risk prompt marks were removed on 2026-11-07.
    // Each key loads, warns and is ignored; code-health-sweep deletes these
    // entries after 2026-12-07, when they become ordinary unknown settings.
    (
        "workflow.regression_risk.enabled",
        REGRESSION_RISK_REMOVED_NOTE,
    ),
    (
        "workflow.regression_risk.window_days",
        REGRESSION_RISK_REMOVED_NOTE,
    ),
    (
        "workflow.regression_risk.min_support",
        REGRESSION_RISK_REMOVED_NOTE,
    ),
    (
        "workflow.regression_risk.threshold_percent",
        REGRESSION_RISK_REMOVED_NOTE,
    ),
    (
        "workflow.regression_risk.max_files",
        REGRESSION_RISK_REMOVED_NOTE,
    ),
    // [ORB-22804] The task-type list keeps routing type pools under its new
    // name; the performance weighting and its scorecard snapshots are gone.
    (
        "workflow.crew_performance_types",
        "translated to workflow.task_types, the same ordered entries \
     workflow.type_complexity_crews routes by; rename the tables to [[workflow.task_types]]. \
     This spelling stops being recognised after 2026-12-07",
    ),
    ("workflow.crew_performance", CREW_PERFORMANCE_REMOVED_NOTE),
    (
        "workflow.crew_performance_strength",
        CREW_PERFORMANCE_REMOVED_NOTE,
    ),
    (
        "workflow.crew_performance_cap",
        CREW_PERFORMANCE_REMOVED_NOTE,
    ),
    (
        "workflow.crew_performance_window_days",
        CREW_PERFORMANCE_REMOVED_NOTE,
    ),
    // [ORB-22805] The explore brief was removed on 2026-11-07. Each key loads,
    // warns and is ignored; code-health-sweep deletes these entries after
    // 2026-12-07, when they become ordinary unknown settings.
    ("workflow.explore_brief.enabled", EXPLORE_BRIEF_REMOVED_NOTE),
    ("workflow.explore_brief.tags", EXPLORE_BRIEF_REMOVED_NOTE),
    ("workflow.explore_brief.deliver", EXPLORE_BRIEF_REMOVED_NOTE),
    // [ORB-22807] Visual review was removed on 2026-11-07. Each key loads,
    // warns and is ignored; code-health-sweep deletes these entries after
    // 2026-12-07, when they become ordinary unknown settings.
    ("visual_review.manifests", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.default_width", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.mode", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.max_mocks", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.reviewer", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.max_images", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.max_image_tokens", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.max_findings", VISUAL_REVIEW_REMOVED_NOTE),
    (
        "visual_review.require_established",
        VISUAL_REVIEW_REMOVED_NOTE,
    ),
    ("visual_review.block_at", VISUAL_REVIEW_REMOVED_NOTE),
    ("visual_review.file_at", VISUAL_REVIEW_REMOVED_NOTE),
    (
        "workflow.purpose_crews.visual_review",
        VISUAL_REVIEW_REMOVED_NOTE,
    ),
    // [ORB-22844] The evolution-pass, ux-rules and finding-verification
    // routines were removed on 2026-11-07. Each key loads, warns and is
    // ignored; code-health-sweep deletes these entries after 2026-12-07, when
    // they become ordinary unknown settings.
    (
        "resident.evolution.min_occurrences",
        EVOLUTION_PASS_REMOVED_NOTE,
    ),
    (
        "resident.evolution.min_distinct_cycles",
        EVOLUTION_PASS_REMOVED_NOTE,
    ),
    (
        "resident.evolution.min_distinct_mandates",
        EVOLUTION_PASS_REMOVED_NOTE,
    ),
    (
        "resident.evolution.min_cost_minutes",
        EVOLUTION_PASS_REMOVED_NOTE,
    ),
    (
        "resident.evolution.max_proposals",
        EVOLUTION_PASS_REMOVED_NOTE,
    ),
    ("ux_rules.surfaces", UX_RULES_REMOVED_NOTE),
    ("ux_rules.runner", UX_RULES_REMOVED_NOTE),
    ("ux_rules.ui_base_url", UX_RULES_REMOVED_NOTE),
    ("ux_rules.max_new_tasks", UX_RULES_REMOVED_NOTE),
    ("ux_rules.file_at", UX_RULES_REMOVED_NOTE),
    ("ux_rules.disabled", UX_RULES_REMOVED_NOTE),
    ("review.verify_findings", VERIFY_FINDINGS_REMOVED_NOTE),
    // [ORB-22808] Tuning keys whose defaults every host kept became fixed
    // values on 2026-11-08. Each key loads, warns and is ignored;
    // code-health-sweep deletes these entries after 2026-12-08, when they
    // become ordinary unknown settings.
    ("attention.age_bump_hours", ATTENTION_FIXED_NOTE),
    ("attention.ci_cache_seconds", ATTENTION_FIXED_NOTE),
    ("attention.cursor_ttl_hours", ATTENTION_FIXED_NOTE),
    ("attention.fanout_budget_seconds", ATTENTION_FIXED_NOTE),
    ("attention.per_workspace_item_cap", ATTENTION_FIXED_NOTE),
    ("attention.run_stuck_after_minutes", ATTENTION_FIXED_NOTE),
    ("attention.run_window_hours", ATTENTION_FIXED_NOTE),
    ("attention.source_timeout_seconds", ATTENTION_FIXED_NOTE),
    (
        "automation.stall_window_minutes",
        AUTOMATION_STALL_FIXED_NOTE,
    ),
    (
        "ci_failure.operator_suppression_hours",
        CI_FAILURE_SUPPRESSION_FIXED_NOTE,
    ),
    ("execution.agent_stall_minutes", EXECUTION_FIXED_NOTE),
    (
        "execution.proc_spawn_max_timeout_minutes",
        EXECUTION_FIXED_NOTE,
    ),
    ("experiments.max_running", EXPERIMENTS_FIXED_NOTE),
    ("experiments.min_delivered_rate", EXPERIMENTS_FIXED_NOTE),
    ("jobs.failure_streak_warn", JOB_FAILURE_STREAK_FIXED_NOTE),
    ("machine.application_consent_max_days", MACHINE_FIXED_NOTE),
    ("machine.host_shutdown_hold_minutes", MACHINE_FIXED_NOTE),
    (
        "machine.auto_concurrency.admit_memory_percent",
        AUTO_CONCURRENCY_FIXED_NOTE,
    ),
    (
        "machine.auto_concurrency.head_max_wait_minutes",
        AUTO_CONCURRENCY_FIXED_NOTE,
    ),
    (
        "machine.auto_concurrency.head_stuck_passes",
        AUTO_CONCURRENCY_FIXED_NOTE,
    ),
    (
        "machine.auto_concurrency.horizon_cap_minutes",
        AUTO_CONCURRENCY_FIXED_NOTE,
    ),
    (
        "machine.auto_concurrency.profile_window",
        AUTO_CONCURRENCY_FIXED_NOTE,
    ),
    (
        "resident.evolution.decline_suppress_days",
        EVOLUTION_LEDGER_FIXED_NOTE,
    ),
    (
        "resident.evolution.window_days",
        EVOLUTION_LEDGER_FIXED_NOTE,
    ),
    ("retention.journal_days", RETENTION_FIXED_NOTE),
    ("retention.store_warn_bytes", RETENTION_FIXED_NOTE),
    ("runtime.log_max_file_mb", LOG_ROTATION_FIXED_NOTE),
    ("runtime.log_max_total_mb", LOG_ROTATION_FIXED_NOTE),
    ("runtime.log_retention_days", LOG_ROTATION_FIXED_NOTE),
    (
        "security_alert_sweep.min_severity",
        SECURITY_ALERT_SEVERITY_FIXED_NOTE,
    ),
    (
        "worktree.parked_reclaim_after_hours",
        PARKED_RECLAIM_FIXED_NOTE,
    ),
];

const ATTENTION_FIXED_NOTE: &str = "ignored: the attention queue's tuning is fixed: cursors \
     last 24 hours, failed runs are read back 48 hours, a run counts as stuck after 120 \
     minutes, CI reads are cached 60 seconds, each source has 10 seconds and a cross-host \
     fan-out 20, one workspace contributes at most 200 candidates, and severity rises at 24 and \
     72 hours. Delete the [attention] table. This key stops being recognised after 2026-12-08";

const AUTOMATION_STALL_FIXED_NOTE: &str = "ignored: a deferred delivery-automation reason is \
     logged and filed after a fixed 60 minutes; delete the key. This key stops being \
     recognised after 2026-12-08";

const CI_FAILURE_SUPPRESSION_FIXED_NOTE: &str = "ignored: an archived or rejected CI sweep \
     task suppresses re-filing for a fixed 6 hours; delete the key. This key stops being \
     recognised after 2026-12-08";

const EXECUTION_FIXED_NOTE: &str = "ignored: a provider process idle for 30 minutes fails its \
     step as stalled, and one proc.spawn call inside an activity may run at most 45 minutes; \
     both are fixed. Delete the key. This key stops being recognised after 2026-12-08";

const EXPERIMENTS_FIXED_NOTE: &str = "ignored: at most 3 experiments run at once in a \
     workspace, and a readout gives a verdict only when at least 0.9 of a treatment arm's \
     settled tasks received its treatment; both are fixed. Delete the key. This key stops \
     being recognised after 2026-12-08";

const JOB_FAILURE_STREAK_FIXED_NOTE: &str = "ignored: the dashboard warns that a job is \
     failing after a fixed 5 consecutive failed runs; delete the key. This key stops being \
     recognised after 2026-12-08";

const MACHINE_FIXED_NOTE: &str = "ignored: a constrained application consent lasts at most 30 \
     days, and unattended admissions hold 180 minutes before a scheduled shutdown; both are \
     fixed. Delete the key from the global config.toml. This key stops being recognised after \
     2026-12-08";

const AUTO_CONCURRENCY_FIXED_NOTE: &str = "ignored: forecast admission keeps a fixed 75% \
     memory ceiling, a 50-delivery profile window, a 120-minute horizon cap, and a starvation \
     guard of 30 minutes and 20 passes; machine.auto_concurrency.admit_cpu_percent, \
     min_profile_deliveries and mode stay settable. Delete the key from the global \
     config.toml. This key stops being recognised after 2026-12-08";

const EVOLUTION_LEDGER_FIXED_NOTE: &str = "ignored: the evolution ledger counts a fixed 14 \
     days of cost, and a declined proposal stays declined 30 days unless the decline names \
     its own days; delete the key. This key stops being recognised after 2026-12-08";

const RETENTION_FIXED_NOTE: &str = "ignored: an applied task-commit journal row may be deleted \
     after a fixed 14 days, and orbit doctor warns about a store file over 1 GiB; \
     retention.audit_days and retention.runs_days stay settable. Delete the key. This key \
     stops being recognised after 2026-12-08";

const LOG_ROTATION_FIXED_NOTE: &str = "ignored: the operational log rolls at a fixed 100 MiB, \
     keeps at most 500 MiB of archives, and deletes archives after 7 days; delete the key \
     from the [runtime] table. This key stops being recognised after 2026-12-08";

const SECURITY_ALERT_SEVERITY_FIXED_NOTE: &str = "ignored: the security sweep files alerts of \
     moderate severity and above unless its job input sets min_severity; delete the key. \
     This key stops being recognised after 2026-12-08";

const PARKED_RECLAIM_FIXED_NOTE: &str = "ignored: worktree GC may remove a someday task's \
     worktree after it has been parked a fixed 336 hours (14 days); delete the key. This key \
     stops being recognised after 2026-12-08";

const EVOLUTION_PASS_REMOVED_NOTE: &str = "ignored: the weekly evolution pass routine was \
     removed; the evolution ledger keeps its fixed 14-day window and 30-day decline. Delete \
     the key. This key stops being recognised after 2026-12-07";

const UX_RULES_REMOVED_NOTE: &str = "ignored: the ux-rules routine and its findings filer were \
     removed; run scripts/ux-rules/run.mjs by hand and delete the [ux_rules] table. This key \
     stops being recognised after 2026-12-07";

const VERIFY_FINDINGS_REMOVED_NOTE: &str = "ignored: finding verification was removed, so a \
     review-filed bug is approved and promoted like any other task; delete the key. This key \
     stops being recognised after 2026-12-07";

const REGRESSION_RISK_REMOVED_NOTE: &str = "ignored: regression-risk prompt marks were removed; \
     task riskiness scores use a fixed 30-day window and a minimum support of 5. Delete the \
     [workflow.regression_risk] table. This key stops being recognised after 2026-12-07";

const VISUAL_REVIEW_REMOVED_NOTE: &str = "ignored: visual review (mock comparison, `verify-mock-ui` \
     capture and review) was removed; delete the [visual_review] table and \
     workflow.purpose_crews.visual_review. This key stops being recognised after 2026-12-07";

const EXPLORE_BRIEF_REMOVED_NOTE: &str = "ignored: the task pilot's explore brief was removed; \
     delete the [workflow.explore_brief] table. This key stops being recognised after 2026-12-07";

const CREW_LADDER_REMOVED_NOTE: &str = "ignored: the crew ladder and its standing shadow trials \
     were removed; route each tier with workflow.<tier>_complexity_crews and delete the \
     [workflow.crew_ladder] table. This key stops being recognised after 2026-12-07";

const CREW_PERFORMANCE_REMOVED_NOTE: &str = "ignored: crew-performance weighting, its \
     scorecard snapshots and the crew_performance_refresh routine were removed; route by type \
     with workflow.type_complexity_crews and delete the key. This key stops being recognised \
     after 2026-12-07";

const SWEEP_POOL_NOTE: &str = "translated to workflow.purpose_crews.sweep_tasks, the one pool \
     every sweep-filed task (CI failure, Dependabot and code scanning, merged-head repair) draws \
     from; move the value there";

const REVIEW_TIMING_NOTE: &str = "translated to review.timing: before_pr = true is \"before_pr\", \
     before_landing = true is \"before_landing\", both off is \"off\"; set review.timing instead. \
     review.timing = before_pr itself is deprecated (use off or before_landing) but still honoured";

const SHELL_PROBE_NOTE: &str = "translated to workflow.validation_env.shell_probe: login_shell = \
     false is \"off\", interactive = false is \"login\", otherwise \"interactive\"; set \
     shell_probe instead";

/// A registry key whose deprecated value form still loads: the row's own
/// resolver reads it, load warns, and `orbit doctor --fix-retired-config-keys`
/// rewrites the value. The key itself stays settable in its current form.
pub(crate) const DEPRECATED_VALUE_FORMS: &[(&str, &str)] = &[(
    "machine.worker_containment",
    "a boolean is deprecated: true reads as \"warn\" and false as \"off\"; set off, warn or \
     strict instead",
)];

/// The deprecation note for a translated key, or `None` for any other key.
pub(crate) fn deprecated_key_note(key: &str) -> Option<&'static str> {
    DEPRECATED_CONFIG_KEYS
        .iter()
        .find(|(deprecated, _)| *deprecated == key)
        .map(|(_, note)| *note)
}

/// The migration note for a removed key, or `None` for any other key.
pub(crate) fn removed_key_note(key: &str) -> Option<&'static str> {
    let key = if key.starts_with("semantic.") {
        "semantic"
    } else {
        key
    };
    REMOVED_CONFIG_KEYS
        .iter()
        .find(|(removed, _)| *removed == key)
        .map(|(_, note)| *note)
}

/// Whether `key` is a removed or deprecated registry key that load still accepts.
///
/// `orbit config get` and `set` refuse it. `ConfigStore::unset_value` still
/// removes it, so a doctor repair can delete the key without accepting a new value.
pub(crate) fn is_compatibility_config_key(key: &str) -> bool {
    DEPRECATED_CONFIG_KEYS
        .iter()
        .any(|(candidate, _)| *candidate == key)
        || REMOVED_CONFIG_KEYS
            .iter()
            .any(|(candidate, _)| *candidate == key)
}

/// Registry keys `orbit config set` refuses to write, with the reason an
/// operator needs. Unlike [`REMOVED_CONFIG_KEYS`] these are live settings —
/// readable by `orbit config get`/`show` and admitted at load — they are
/// simply not the operator's to change after `orbit init` recorded them.
pub(crate) const IMMUTABLE_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "machine.id",
        "a machine identity is generated once by `orbit init` and never reused; \
         changing it would orphan every task, run, and workspace record minted under it",
    ),
    (
        "machine.task_prefix",
        "the task-id namespace is fixed for the life of this machine's task store; \
         ids already minted under it cannot be renumbered",
    ),
];

/// The refusal note for an unsettable key, or `None` for any other key.
pub fn immutable_key_note(key: &str) -> Option<&'static str> {
    IMMUTABLE_CONFIG_KEYS
        .iter()
        .find(|(immutable, _)| *immutable == key)
        .map(|(_, note)| *note)
}

/// Dotted prefix of the one table only the global `config.toml` may carry.
pub const GLOBAL_ONLY_KEY_PREFIX: &str = "machine.";

/// Whether `key` is one of the three `[machine]` identity keys, which are
/// written together by `orbit init` and never unset one at a time.
pub(crate) fn is_machine_identity_key(key: &str) -> bool {
    matches!(key, "machine.id" | "machine.name" | "machine.task_prefix")
}

/// The OS user Orbit Web's lease intake socket accepts. Read only from the
/// global `config.toml`: a checkout must not choose who may create leases.
pub const LEASE_INTAKE_PEER_USER_KEY: &str = "lease.intake.peer_user";

/// Dotted prefix of the lease listener's settings, read only from the
/// listener user's global `config.toml`.
pub const LEASE_LISTENER_KEY_PREFIX: &str = "lease.listener.";

/// Whether `key` names a setting a workspace `config.toml` may not supply:
/// the `[machine]` table, each crew's `env` list, which routes host
/// variables into that crew's agents, the lease intake peer user and the
/// lease listener's settings.
pub fn is_global_only_key(key: &str) -> bool {
    key == "assistance"
        || key.starts_with("assistance.")
        || key == GLOBAL_ONLY_KEY_PREFIX.trim_end_matches('.')
        || key.starts_with(GLOBAL_ONLY_KEY_PREFIX)
        || key == LEASE_INTAKE_PEER_USER_KEY
        || key == LEASE_LISTENER_KEY_PREFIX.trim_end_matches('.')
        || key.starts_with(LEASE_LISTENER_KEY_PREFIX)
        || matches!(
            parse_crew_field_key(key),
            Ok(Some(CrewFieldKey { field: "env", .. }))
        )
}

/// Admit a dotted key for a write through `orbit config set`.
///
/// Everything [`admit_config_key`] accepts, minus the keys that are read-only
/// after `orbit init` recorded them.
pub fn admit_settable_config_key(key: &str) -> Result<(), OrbitError> {
    admit_config_key(key)?;
    if let Some(note) = immutable_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' is read-only: {note}"
        )));
    }
    Ok(())
}

/// Admit a dotted key for `orbit config get`/`set`.
///
/// Fixed registry keys, live `crews.<name>.<field>` keys and
/// `plugins.<ns>.<key>` keys owned by an installed plugin succeed. A
/// removed key fails with its migration note. Unknown registry keys fail
/// with nearby-key suggestions when available; misspelled crew fields fail
/// with valid-field suggestions. All before any document mutation.
pub fn admit_config_key(key: &str) -> Result<(), OrbitError> {
    if describe(key).is_some() {
        return Ok(());
    }
    if let Some(note) = removed_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' was removed and is ignored: {note}"
        )));
    }
    if let Some(note) = deprecated_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' is deprecated and no longer settable: {note}"
        )));
    }
    if let Some(parsed) = crate::plugins::parse_plugin_field_key(key)? {
        return crate::plugins::admit_plugin_field_key(parsed);
    }
    match parse_crew_field_key(key)? {
        Some(_) => Ok(()),
        None => Err(OrbitError::invalid_input_with_suggestions(
            format!("unknown config key '{key}'"),
            nearby_config_key_names(key),
        )),
    }
}

/// Parse `crews.<name>.<field>` when `key` is a crew-table path.
///
/// `None` means this is not a crew key (including the bare `crews` table).
/// An ill-formed crew path or unknown field is an error, not a fallthrough
/// to the fixed-key registry, so `crews.sol.effrot` is not reported as an
/// unknown registry setting.
pub(crate) fn parse_crew_field_key(key: &str) -> Result<Option<CrewFieldKey<'_>>, OrbitError> {
    let mut parts = key.split('.');
    if parts.next() != Some("crews") {
        return Ok(None);
    }
    let Some(name) = parts.next() else {
        return Ok(None);
    };
    let Some(field) = parts.next() else {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    };
    if parts.next().is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' has extra segments"
        )));
    }
    if name.is_empty() {
        return Err(OrbitError::InvalidInput(
            "crew config keys require a non-empty crew name".to_string(),
        ));
    }
    crate::crew_pools::reject_unpoolable_crew_name(name, "crew config keys")?;
    if field.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    }
    if CREW_CONFIG_FIELDS.contains(&field) {
        return Ok(Some(CrewFieldKey { name, field }));
    }
    Err(OrbitError::invalid_input_with_suggestions(
        format!("unknown crew field '{field}' in '{key}'"),
        CREW_CONFIG_FIELDS
            .iter()
            .map(|known| format!("crews.{name}.{known}"))
            .collect(),
    ))
}

/// At most three nearby fixed keys, closest first with lexical tie-breaking.
fn nearby_config_key_names(key: &str) -> Vec<String> {
    let key_length = key.chars().count();
    let mut candidates: Vec<_> = CONFIG_KEY_REGISTRY
        .iter()
        .filter_map(|entry| {
            let threshold = key_length.max(entry.key.chars().count()).div_ceil(3);
            let distance = orbit_common::text::unicode_edit_distance(key, entry.key);
            (distance <= threshold).then_some((distance, entry.key))
        })
        .collect();
    candidates.sort_unstable();
    candidates
        .into_iter()
        .take(3)
        .map(|(_, candidate)| candidate.to_owned())
        .collect()
}
