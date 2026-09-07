//! Operation-mode precedence, preset reset, independent fields, provenance,
//! and invalid-setting rejection [ORB-11332].

use tempfile::tempdir;

use crate::operation::{
    CompletionPreference, DeliveryCap, OperationLayer, OperationLayerSource, OperationPolicy,
    OperationPreset, PreparationPreference, PromotionPreference, RecoveryPreference, ReviewPolicy,
};
use crate::{ResolvedConfig, load_effective_config};

use super::{roots, write_config};

fn load(global: &str, workspace: &str) -> ResolvedConfig {
    let global_dir = tempdir().expect("global");
    let workspace_dir = tempdir().expect("workspace");
    write_config(global_dir.path(), global);
    write_config(workspace_dir.path(), workspace);
    ResolvedConfig::load(&roots(global_dir.path(), workspace_dir.path())).expect("load config")
}

fn load_error(global: &str, workspace: &str) -> String {
    let global_dir = tempdir().expect("global");
    let workspace_dir = tempdir().expect("workspace");
    write_config(global_dir.path(), global);
    write_config(workspace_dir.path(), workspace);
    ResolvedConfig::load(&roots(global_dir.path(), workspace_dir.path()))
        .expect_err("invalid operation config must fail")
        .to_string()
}

#[test]
fn built_in_policy_is_supervised_with_no_review_and_review_delivery_cap() {
    let policy = OperationPolicy::built_in();

    assert_eq!(policy.preset.value, OperationPreset::Supervised);
    assert_eq!(policy.preset.source.label(), "built-in");
    assert_eq!(policy.preparation.value, PreparationPreference::Manual);
    assert_eq!(policy.leaf_ceiling.value, 5);
    assert_eq!(
        policy.promotion.value,
        PromotionPreference::SeparateApproval
    );
    assert_eq!(policy.completion.value, CompletionPreference::Review);
    assert_eq!(policy.recovery.value, RecoveryPreference::Existing);
    assert_eq!(policy.recovery_episodes_per_task.value, 2);
    assert_eq!(policy.recovery_minutes_per_task.value, 30);
    assert_eq!(policy.review_policy.value, ReviewPolicy::None);
    assert_eq!(policy.review_crew.value, None);
    assert_eq!(policy.delivery_cap.value, DeliveryCap::Review);
    assert_eq!(
        policy.leaf_ceiling.source.label(),
        "preset:supervised@built-in"
    );
}

#[test]
fn no_config_files_yield_the_built_in_policy() {
    let global_dir = tempdir().expect("global");
    let workspace_dir = tempdir().expect("workspace");
    let config = ResolvedConfig::load(&roots(global_dir.path(), workspace_dir.path()))
        .expect("load without files");

    assert_eq!(config.operation, OperationPolicy::built_in());
}

#[test]
fn global_autonomous_preset_installs_its_defaults_with_preset_provenance() {
    let config = load("[operation]\npreset = \"autonomous\"\n", "");

    let policy = &config.operation;
    assert_eq!(policy.preset.value, OperationPreset::Autonomous);
    assert_eq!(policy.preset.source.label(), "global");
    assert_eq!(policy.leaf_ceiling.value, 10);
    assert_eq!(
        policy.leaf_ceiling.source.label(),
        "preset:autonomous@global"
    );
    assert_eq!(policy.promotion.value, PromotionPreference::Automatic);
    assert_eq!(policy.completion.value, CompletionPreference::Done);
    assert_eq!(policy.recovery.value, RecoveryPreference::Scheduled);
    assert_eq!(policy.preparation.value, PreparationPreference::Automatic);
    // Independent fields keep their own defaults.
    assert_eq!(policy.review_policy.value, ReviewPolicy::None);
    assert_eq!(policy.delivery_cap.value, DeliveryCap::Review);
}

#[test]
fn workspace_preset_selection_resets_global_preset_managed_fields() {
    let config = load(
        "[operation]\npreset = \"autonomous\"\nleaf_ceiling = 12\ncompletion = \"done\"\n",
        "[operation]\npreset = \"supervised\"\nleaf_ceiling = 3\n",
    );

    let policy = &config.operation;
    assert_eq!(policy.preset.value, OperationPreset::Supervised);
    assert_eq!(policy.preset.source.label(), "workspace");
    // Restated at the workspace: explicit workspace value wins.
    assert_eq!(policy.leaf_ceiling.value, 3);
    assert_eq!(policy.leaf_ceiling.source.label(), "workspace");
    // Not restated: the global explicit `done` must not leak through.
    assert_eq!(policy.completion.value, CompletionPreference::Review);
    assert_eq!(
        policy.completion.source.label(),
        "preset:supervised@workspace"
    );
    assert_eq!(
        policy.promotion.value,
        PromotionPreference::SeparateApproval
    );
}

#[test]
fn omitted_workspace_preset_preserves_inherited_fields() {
    let config = load(
        "[operation]\npreset = \"autonomous\"\nleaf_ceiling = 12\n",
        "[operation]\nrecovery_minutes_per_task = 45\n",
    );

    let policy = &config.operation;
    assert_eq!(policy.preset.value, OperationPreset::Autonomous);
    assert_eq!(policy.preset.source.label(), "global");
    assert_eq!(policy.leaf_ceiling.value, 12);
    assert_eq!(policy.leaf_ceiling.source.label(), "global");
    assert_eq!(policy.completion.value, CompletionPreference::Done);
    assert_eq!(policy.completion.source.label(), "preset:autonomous@global");
    assert_eq!(policy.recovery_minutes_per_task.value, 45);
    assert_eq!(policy.recovery_minutes_per_task.source.label(), "workspace");
}

#[test]
fn review_fields_and_delivery_cap_survive_a_preset_reset() {
    let config = load(
        "[operation]\nreview_policy = \"after-landing\"\nreview_crew = \"reviewers\"\ndelivery_cap = \"done\"\n",
        "[operation]\npreset = \"supervised\"\n",
    );

    let policy = &config.operation;
    assert_eq!(policy.review_policy.value, ReviewPolicy::AfterLanding);
    assert_eq!(policy.review_policy.source.label(), "global");
    assert_eq!(policy.review_crew.value.as_deref(), Some("reviewers"));
    assert_eq!(policy.delivery_cap.value, DeliveryCap::Done);
    assert_eq!(policy.delivery_cap.source.label(), "global");
}

#[test]
fn run_layer_preset_resets_config_fields_and_run_fields_win() {
    let config = load(
        "[operation]\npreset = \"autonomous\"\nleaf_ceiling = 12\n",
        "",
    );
    let run = OperationLayer {
        preset: Some(OperationPreset::Supervised),
        recovery_episodes_per_task: Some(1),
        review_policy: Some(ReviewPolicy::AfterLanding),
        ..OperationLayer::default()
    };

    let policy = config.operation.with_run_layer(&run);
    assert_eq!(policy.preset.value, OperationPreset::Supervised);
    assert_eq!(policy.preset.source.layer, OperationLayerSource::Run);
    assert_eq!(policy.leaf_ceiling.value, 5);
    assert_eq!(policy.leaf_ceiling.source.label(), "preset:supervised@run");
    assert_eq!(policy.recovery_episodes_per_task.value, 1);
    assert_eq!(policy.recovery_episodes_per_task.source.label(), "run");
    assert_eq!(policy.review_policy.value, ReviewPolicy::AfterLanding);
    assert_eq!(policy.review_policy.source.label(), "run");
}

#[test]
fn delivery_cap_reduces_a_done_preference_and_discloses_it() {
    let config = load("[operation]\npreset = \"autonomous\"\n", "");
    let (completion, cap) = config.operation.capped_completion();
    assert_eq!(completion, CompletionPreference::Review);
    assert_eq!(cap, Some("delivery_cap_review"));

    let raised = load(
        "[operation]\npreset = \"autonomous\"\ndelivery_cap = \"done\"\n",
        "",
    );
    let (completion, cap) = raised.operation.capped_completion();
    assert_eq!(completion, CompletionPreference::Done);
    assert_eq!(cap, None);
}

#[test]
fn explanation_names_each_winning_source_and_the_cap() {
    let config = load(
        "[operation]\npreset = \"autonomous\"\nreview_policy = \"before-pr\"\n",
        "[operation]\nleaf_ceiling = 8\n",
    );

    let explanation = config.operation.explain();
    assert_eq!(explanation["preset"]["value"], "autonomous");
    assert_eq!(explanation["preset"]["source"], "global");
    assert_eq!(explanation["leaf_ceiling"]["value"], 8);
    assert_eq!(explanation["leaf_ceiling"]["source"], "workspace");
    assert_eq!(
        explanation["completion"]["source"],
        "preset:autonomous@global"
    );
    assert_eq!(explanation["review_policy"]["value"], "before-pr");
    assert_eq!(explanation["review_policy"]["source"], "global");
    assert_eq!(explanation["review_reviewer_starts"]["value"], 2);
    assert_eq!(explanation["review_reviewer_starts"]["source"], "built-in");
    assert_eq!(explanation["effective_completion"]["value"], "review");
    assert_eq!(
        explanation["effective_completion"]["cap"],
        "delivery_cap_review"
    );
    assert_eq!(explanation["version"], 2);
}

#[test]
fn review_budgets_are_independent_bounded_fields() {
    let config = load(
        "[operation]\nreview_reviewer_starts = 3\nreview_minutes = 45\n",
        "[operation]\npreset = \"autonomous\"\nreview_repair_cycles = 0\n",
    );

    let budget = config.operation.review_budget();
    assert_eq!(budget.reviewer_starts, 3);
    assert_eq!(budget.repair_cycles, 0);
    assert_eq!(budget.minutes, 45);
    assert_eq!(
        config.operation.review_reviewer_starts.source.label(),
        "global"
    );
    assert_eq!(
        config.operation.review_repair_cycles.source.label(),
        "workspace"
    );

    let error = load_error("[operation]\nreview_reviewer_starts = 0\n", "");
    assert!(
        error.contains("operation.review_reviewer_starts has invalid value 0"),
        "{error}"
    );
}

#[test]
fn unknown_operation_key_fails_clearly() {
    let error = load_error(
        "[operation]\npreset = \"autonomous\"\nspeed = \"fast\"\n",
        "",
    );
    assert!(
        error.contains("[operation] has unknown key 'speed'"),
        "{error}"
    );
}

#[test]
fn unknown_preset_value_fails_clearly() {
    let error = load_error("", "[operation]\npreset = \"fast\"\n");
    assert!(
        error.contains(
            "operation.preset has invalid value 'fast'; expected one of: supervised, autonomous"
        ),
        "{error}"
    );
}

#[test]
fn out_of_range_numeric_settings_fail_clearly() {
    let error = load_error("[operation]\nleaf_ceiling = 0\n", "");
    assert!(
        error.contains("operation.leaf_ceiling has invalid value 0"),
        "{error}"
    );

    let error = load_error("", "[operation]\nrecovery_minutes_per_task = 5000\n");
    assert!(
        error.contains("operation.recovery_minutes_per_task has invalid value 5000"),
        "{error}"
    );

    let error = load_error("", "[operation]\nreview_crew = \"  \"\n");
    assert!(
        error.contains("operation.review_crew must not be empty"),
        "{error}"
    );
}

#[test]
fn a_config_without_an_operation_section_keeps_existing_behavior() {
    let config = load(
        "[workflow]\nbase_branch = \"agent-main\"\n",
        "[scoring]\nenabled = false\n",
    );

    assert_eq!(config.operation, OperationPolicy::built_in());
    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(!config.scoring_enabled);
}

#[test]
fn effective_config_provenance_mirrors_the_preset_reset() {
    let global_dir = tempdir().expect("global");
    let workspace_dir = tempdir().expect("workspace");
    write_config(
        global_dir.path(),
        "[operation]\npreset = \"autonomous\"\nleaf_ceiling = 12\nreview_crew = \"reviewers\"\n",
    );
    write_config(
        workspace_dir.path(),
        "[operation]\npreset = \"supervised\"\ncompletion = \"review\"\n",
    );

    let effective = load_effective_config(&roots(global_dir.path(), workspace_dir.path()))
        .expect("effective config");
    let entry = |key: &str| {
        effective
            .values()
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("{key} missing from effective config"))
    };
    let source = |key: &str| entry(key).source.kind().label().to_string();
    let value = |key: &str| entry(key).value.clone();

    assert_eq!(source("operation.preset"), "workspace");
    assert_eq!(source("operation.completion"), "workspace");
    // Reset by the workspace preset: no explicit value survives the merge.
    assert_eq!(source("operation.leaf_ceiling"), "built-in");
    assert_eq!(value("operation.leaf_ceiling"), serde_json::Value::Null);
    // Independent field inherits from global untouched.
    assert_eq!(source("operation.review_crew"), "global");
    assert_eq!(
        value("operation.review_crew"),
        serde_json::json!("reviewers")
    );
}
