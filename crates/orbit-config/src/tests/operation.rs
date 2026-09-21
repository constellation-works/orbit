//! `[operation]` review precedence, provenance, invalid-setting rejection,
//! and the removed operation-mode keys warning instead of failing.

use tempfile::tempdir;

use crate::operation::{OperationLayerSource, OperationPolicy, ReviewPolicy};
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
fn built_in_policy_has_no_review_and_default_budget() {
    let policy = OperationPolicy::built_in();

    assert_eq!(policy.review_policy.value, ReviewPolicy::None);
    assert_eq!(policy.review_policy.source, OperationLayerSource::BuiltIn);
    assert_eq!(policy.review_policy.source.label(), "built-in");
    assert_eq!(policy.review_crew.value, None);
    assert_eq!(policy.review_reviewer_starts.value, 2);
    assert_eq!(policy.review_repair_cycles.value, 2);
    assert_eq!(policy.review_minutes.value, 30);
    assert_eq!(policy.version, 2);
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
fn workspace_review_fields_override_global_and_record_their_layer() {
    let config = load(
        "[operation]\nreview_policy = \"after-landing\"\nreview_crew = \"reviewers\"\n",
        "[operation]\nreview_policy = \"before-pr\"\n",
    );

    let policy = &config.operation;
    assert_eq!(policy.review_policy.value, ReviewPolicy::BeforePr);
    assert_eq!(policy.review_policy.source.label(), "workspace");
    assert_eq!(policy.review_crew.value.as_deref(), Some("reviewers"));
    assert_eq!(policy.review_crew.source.label(), "global");
}

#[test]
fn review_budgets_are_bounded_fields() {
    let config = load(
        "[operation]\nreview_reviewer_starts = 3\nreview_minutes = 45\n",
        "[operation]\nreview_repair_cycles = 0\n",
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

    let error = load_error("", "[operation]\nreview_minutes = 5000\n");
    assert!(
        error.contains("operation.review_minutes has invalid value 5000"),
        "{error}"
    );

    let error = load_error("", "[operation]\nreview_crew = \"  \"\n");
    assert!(
        error.contains("operation.review_crew must not be empty"),
        "{error}"
    );
}

#[test]
fn unknown_operation_key_fails_clearly() {
    let error = load_error(
        "[operation]\nreview_policy = \"none\"\nspeed = \"fast\"\n",
        "",
    );
    assert!(
        error.contains("[operation] has unknown key 'speed'"),
        "{error}"
    );
}

#[test]
fn unknown_review_policy_value_fails_clearly() {
    let error = load_error("", "[operation]\nreview_policy = \"fast\"\n");
    assert!(
        error.contains(
            "operation.review_policy has invalid value 'fast'; expected one of: none, before-pr, after-landing"
        ),
        "{error}"
    );
}

/// A `config.toml` written before operation mode was removed still loads:
/// its removed keys are warned and ignored, and the review keys beside them
/// resolve as before.
#[test]
fn removed_operation_mode_keys_are_ignored_not_refused() {
    let config = load(
        "[operation]\npreset = \"autonomous\"\nleaf_ceiling = 12\ndelivery_cap = \"done\"\n",
        "[operation]\ncompletion = \"done\"\nreview_policy = \"after-landing\"\n",
    );

    assert_eq!(
        config.operation.review_policy.value,
        ReviewPolicy::AfterLanding
    );
    assert_eq!(config.operation.review_policy.source.label(), "workspace");
    assert!(
        config.snapshot.value_for("operation.preset").is_none(),
        "removed keys are not admitted into the snapshot"
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
fn effective_config_lists_review_keys_with_their_layer() {
    let global_dir = tempdir().expect("global");
    let workspace_dir = tempdir().expect("workspace");
    write_config(
        global_dir.path(),
        "[operation]\nreview_crew = \"reviewers\"\nreview_minutes = 45\n",
    );
    write_config(
        workspace_dir.path(),
        "[operation]\nreview_policy = \"before-pr\"\nreview_minutes = 20\n",
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

    assert_eq!(source("operation.review_policy"), "workspace");
    assert_eq!(
        value("operation.review_policy"),
        serde_json::json!("before-pr")
    );
    assert_eq!(source("operation.review_crew"), "global");
    assert_eq!(
        value("operation.review_crew"),
        serde_json::json!("reviewers")
    );
    assert_eq!(source("operation.review_minutes"), "workspace");
    assert_eq!(value("operation.review_minutes"), serde_json::json!(20));
    assert!(
        !effective
            .values()
            .iter()
            .any(|entry| entry.key == "operation.preset"),
        "removed operation-mode keys are not effective values"
    );
}
