//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, `os:` and `ui` tag routing, retired deterministic stubs, final
//! recovery, repeated rebase recovery, relation auto-close, PR closure on
//! terminal decisions and requeues, settlement from a delivery PR merged
//! outside Orbit, orphaned delivery adoption, plugin inspection, the sandbox
//! opt-out, the agent sandbox mask and cold lexical search hydration.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`). Declare every top-level file
//! (`<name>.rs`, or a `<name>/mod.rs` directory) below; `every_top_level_module_is_declared`
//! fails and names a missing one. A build script is not used: it would rebuild
//! `orbit-core` and every dependent crate on any edit under this directory.

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../support/admission.rs"]
mod admission_test_support;
#[path = "../support/external_merge_fixture.rs"]
mod external_merge_fixture;
#[path = "../support/git.rs"]
mod git_test_support;
#[path = "../support/task_params.rs"]
mod task_params_test_support;

orbit_common::isolate_test_process!();

/// Keep related permutations in one table with isolated fixture state.
macro_rules! scenario_table {
    ($table:ident, $($row:ident),+ $(,)?) => {
        #[test]
        fn $table() {
            crate::scenarios::run(module_path!(), stringify!($table), &[
                $((stringify!($row), $row as fn() -> _)),+
            ]);
        }
    };
}
pub(crate) use scenario_table;

// Every top-level module in this directory, declared by hand. The guard test below
// fails when a top-level file or `<name>/mod.rs` is missing from this list. They follow
// `scenario_table`, so each module can use the macro.
mod actor_identity;
mod admission_audit;
mod admission_forecast;
mod admitted_crew_write_back;
mod agent_sandbox_mask;
mod application_authority;
mod artifact_tools;
mod attention_observation;
mod attention_provider;
mod attention_wait;
mod audit_commit_report;
mod auto_task_job_steps;
mod auto_task_update;
mod backlog_resource_fit;
mod backlog_review_modes;
mod baseline_hold_refresh;
mod baseline_hold_tick;
mod baseline_red_repair;
mod blocked_recheck;
mod bundle_validation;
mod cascade_outcomes;
mod child_wait_audit;
mod ci_retry_journal_wait;
mod claimed_model_access;
mod claude_worker_credential;
mod codebase_regressions;
mod codebase_related;
mod config_settings;
mod crew_accounts;
mod crew_memory;
mod decision_channel;
mod decision_precedent;
mod decision_raise;
mod decision_requested;
mod default_catalog;
mod delivery_holds;
mod dispatch_admission;
mod distributed_drain;
mod drain_approval;
mod drain_cancel;
mod drain_crew_pools;
mod drain_extend;
mod drain_live_agents;
mod escalation_triage_crews;
mod experiment_lifecycle;
mod experiment_readout;
mod final_recovery;
mod final_recovery_cards;
mod final_recovery_mechanical;
mod fixture_template;
mod flaky_validation;
mod footprint_hold;
mod forecast_accuracy;
mod forge_hold_resume;
mod friction_stats_observation;
mod git_protection;
mod granted_mutation_proposals;
mod granted_tools;
mod granted_tools_window;
mod held_child_guard;
mod host_labels;
mod host_os_executor;
mod host_os_routing;
mod host_resources;
mod host_ui_routing;
mod human_crew_work;
mod implement_blocker;
mod invocation_metrics;
mod job_failure_streak;
mod job_finalization;
mod light_delivery;
mod local_provider_holds;
mod local_route_before_pr;
mod mid_run_steering;
mod muse_state_grant;
mod named_accepted_history;
mod notice_authority;
mod operator_ship_override;
mod origin_fetch_hold;
mod orphan_adoption;
mod pilot_warning_cards;
mod plugin_inspection;
mod pr_forge_admission;
mod pr_infrastructure;
mod pre_family_tool_names;
mod process_isolation;
mod promotion_commit;
mod provider_failure_hold;
mod provider_limit_admission;
mod provider_limit_budget;
mod provider_limit_hold;
mod provider_roots;
mod purpose_crews;
mod rebase_recovery_attempts;
mod recovery_fixture;
mod recovery_provider_auth;
mod relation_auto_close;
mod resident_authority;
mod resident_cycle;
mod resident_digest;
mod resident_metrics;
mod resource_profiles;
mod resume_ownership;
mod retired_stubs;
mod review_admission_shape;
mod review_baseline_hold;
mod review_before_landing;
mod review_budget_hold;
mod review_continuation;
mod review_evidence_fulfilment;
mod review_evidence_paths;
mod review_evidence_writers;
mod review_failed_reviewer_hold;
mod review_gate_audit;
mod review_held_resume;
mod review_owner_host_evidence;
mod review_record_ids;
mod review_report_revisions;
mod review_run_option;
mod reviewable_lines;
mod run_cli_invocations;
mod run_failure_diagnostic;
mod run_history;
mod run_outcome_contract;
mod sandbox_off;
mod scenarios;
mod scheduled_tasks;
mod script_env_pass;
mod script_file_inputs;
mod script_grant_request;
mod script_roots;
mod search_hydration;
mod security_alert_sweep;
mod session_events;
mod shared_root_identity;
mod standing_drain_keeper;
mod step_recovery;
mod store_gc;
mod system_work_crews;
mod tag_instructions;
mod task_context_comment_budget;
mod task_delivery;
mod task_external_merge;
mod task_lint_paths;
mod task_pilot;
mod task_pr_closure;
mod task_selector_hints;
mod task_update;
mod task_workspace_projection;
mod upgrade_pending_requeue;
mod upgrade_resume;
mod usage_pressure_draws;
mod vision_probe;
mod worker_exit_diagnostic;
mod worker_fixture;
mod workspace_seed;
mod worktree_reclaim;

// `readiness/` has no `mod.rs`; its one file is mounted here by path.
#[path = "readiness/population.rs"]
mod readiness_population;

/// A top-level file that no `mod` line declares is never compiled, so its tests
/// would silently not run. Fail and name it instead.
#[test]
fn every_top_level_module_is_declared() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime");
    let main_source = include_str!("main.rs");
    let mut sources = vec![main_source.to_owned()];
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .expect("read tests/runtime")
        .flatten()
    {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if path.join("mod.rs").is_file() {
                names.push(file_name);
            }
        } else if let Some(stem) = file_name.strip_suffix(".rs").filter(|s| *s != "main") {
            names.push(stem.to_owned());
            sources.push(std::fs::read_to_string(&path).expect("read runtime test file"));
        }
    }
    names.sort();
    let missing: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| {
            let declared = main_source
                .lines()
                .any(|line| line.trim() == format!("mod {name};"));
            // A sibling may mount a top-level file by path, so it compiles once there.
            let mounted = sources
                .iter()
                .any(|text| text.contains(&format!("#[path = \"{name}.rs\"]")));
            !declared && !mounted
        })
        .collect();
    assert!(
        missing.is_empty(),
        "add `mod <name>;` to crates/orbit-core/tests/runtime/main.rs for: {missing:?}"
    );
}
