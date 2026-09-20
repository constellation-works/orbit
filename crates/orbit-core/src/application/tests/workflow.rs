use chrono::Utc;
use orbit_types::workspace::{Workspace, WorkspaceStatus};

use super::super::workflow::{
    CompletionPolicy, ShipMode, build_ship_input, find_workflow, resolved_ship_mode,
};

#[test]
fn ship_workflow_routes_to_auto_pipeline_only() {
    let workflow = find_workflow("ship").expect("ship workflow");

    assert_eq!(workflow.job_id, "task_auto_pipeline");
    assert!(find_workflow("ship-auto").is_none());
    assert!(find_workflow("ship-local").is_none());
    assert!(find_workflow("review-pr").is_none());
}

#[test]
fn auto_workflow_routes_to_workspace_sequencer() {
    let workflow = find_workflow("auto").expect("auto workflow");

    assert_eq!(workflow.job_id, "workspace_auto_pipeline");
}

#[test]
fn retired_triage_workflow_is_not_dispatchable() {
    assert!(find_workflow("triage").is_none());
}

#[test]
fn task_pilot_workflow_routes_to_task_pilot_pipeline() {
    let workflow = find_workflow("task-pilot").expect("task-pilot workflow");

    assert_eq!(workflow.job_id, "task_pilot_pipeline");
}

#[test]
fn build_ship_input_auto_mode_omits_task_ids() {
    let input = build_ship_input(ShipMode::Pr, "main", &[], CompletionPolicy::Review, &[])
        .expect("input builds");
    assert_eq!(input["mode"], "pr");
    assert_eq!(input["base_branch"], "main");
    assert!(input.get("task_ids").is_none());
    assert!(
        input.get("base_sync").is_none(),
        "PR mode must omit base_sync so seeded jobs keep fetching origin/<base>"
    );
}

#[test]
fn build_ship_input_explicit_tasks_and_local_mode() {
    let task_ids = vec!["T1".to_string(), "T2".to_string()];
    let input = build_ship_input(
        ShipMode::Local,
        "agent-main",
        &task_ids,
        CompletionPolicy::Review,
        &[],
    )
    .expect("builds");
    assert_eq!(input["mode"], "local");
    assert_eq!(input["base_branch"], "agent-main");
    assert_eq!(input["base_sync"], "local");
    assert_eq!(input["task_ids"], serde_json::json!(["T1", "T2"]));
}

#[test]
fn local_mode_opts_into_local_base_sync_without_origin() {
    // Disposable First Task repos have no remotes. Local ship must tell
    // worktree_setup to use the local base instead of fetching origin/<base>.
    let local = build_ship_input(ShipMode::Local, "main", &[], CompletionPolicy::Review, &[])
        .expect("local input builds");
    assert_eq!(local["base_sync"], "local");

    let pr = build_ship_input(ShipMode::Pr, "main", &[], CompletionPolicy::Review, &[])
        .expect("pr input builds");
    assert!(pr.get("base_sync").is_none());
}

#[test]
fn build_ship_input_rejects_duplicates_blank_ids_and_empty_base() {
    let dup = vec!["T1".to_string(), "T1".to_string()];
    assert!(build_ship_input(ShipMode::Pr, "main", &dup, CompletionPolicy::Review, &[]).is_err());

    let blank = vec!["  ".to_string()];
    assert!(build_ship_input(ShipMode::Pr, "main", &blank, CompletionPolicy::Review, &[]).is_err());

    assert!(build_ship_input(ShipMode::Pr, "  ", &[], CompletionPolicy::Review, &[]).is_err());
}

#[test]
fn completion_policy_is_written_only_when_authorized() {
    let default = build_ship_input(ShipMode::Pr, "main", &[], CompletionPolicy::Review, &[])
        .expect("default input builds");
    assert!(
        default.get("completion").is_none(),
        "an unauthorized submission must keep the pre-ORB-11187 input verbatim"
    );

    let completing = build_ship_input(ShipMode::Pr, "main", &[], CompletionPolicy::Done, &[])
        .expect("completing input builds");
    assert_eq!(completing["completion"], "done");
}

#[test]
fn completion_policy_parses_external_strings() {
    assert_eq!(
        CompletionPolicy::parse("review").expect("review"),
        CompletionPolicy::Review
    );
    assert_eq!(
        CompletionPolicy::parse("done").expect("done"),
        CompletionPolicy::Done
    );
    assert!(CompletionPolicy::parse("merged").is_err());
    assert!(!CompletionPolicy::default().completes());
    assert!(CompletionPolicy::Done.completes());
}

#[test]
fn allowed_crews_are_persisted_only_when_restricted() {
    let allowed = build_ship_input(
        ShipMode::Pr,
        "main",
        &["T1".to_string()],
        CompletionPolicy::Review,
        &["sol".to_string()],
    )
    .expect("input builds");

    assert_eq!(allowed["allowed_crews"], serde_json::json!(["sol"]));
}

#[test]
fn ship_mode_parses_external_strings() {
    assert_eq!(ShipMode::parse("pr").expect("pr"), ShipMode::Pr);
    assert_eq!(ShipMode::parse("local").expect("local"), ShipMode::Local);
    assert!(ShipMode::parse("yolo").is_err());
}

fn workspace(git_remote: Option<&str>, ship_mode: Option<&str>) -> Workspace {
    Workspace {
        id: "ws_test".to_string(),
        name: "test".to_string(),
        owner_machine_id: None,
        git_remote: git_remote.map(str::to_string),
        ship_mode: ship_mode.map(str::to_string),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn unset_ship_mode_defaults_to_pr_regardless_of_remote() {
    // The default is independent of the remote and preserves the review
    // boundary for every workspace configured without a ship mode.
    assert_eq!(
        resolved_ship_mode(&workspace(Some("https://github.com/acme/worker.git"), None)),
        ShipMode::Pr
    );
    assert_eq!(
        resolved_ship_mode(&workspace(Some("git@github.com:acme/bridge.git"), None)),
        ShipMode::Pr
    );
    assert_eq!(
        resolved_ship_mode(&workspace(Some("/home/daniel/git/polaris.git"), None)),
        ShipMode::Pr
    );
    assert_eq!(resolved_ship_mode(&workspace(None, None)), ShipMode::Pr);
}

#[test]
fn explicit_pr_wins_over_github_remote() {
    let ws = workspace(Some("https://github.com/acme/orbit.git"), Some("pr"));
    assert_eq!(resolved_ship_mode(&ws), ShipMode::Pr);
}

#[test]
fn explicit_local_wins() {
    let ws = workspace(Some("https://github.com/acme/orbit.git"), Some("local"));
    assert_eq!(resolved_ship_mode(&ws), ShipMode::Local);
}

#[test]
fn unparseable_explicit_mode_falls_back_to_pr() {
    let ws = workspace(Some("https://github.com/acme/orbit.git"), Some("bogus"));
    assert_eq!(resolved_ship_mode(&ws), ShipMode::Pr);
}
