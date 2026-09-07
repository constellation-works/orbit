use serde_json::Value;

use crate::OrbitError;

pub use orbit_types::workflow::{CompletionPolicy, ShipMode, resolved_ship_mode};

pub struct Workflow {
    pub alias: &'static str,
    pub job_id: &'static str,
}

pub const WORKFLOWS: &[Workflow] = &[
    Workflow {
        alias: "auto",
        job_id: "workspace_auto_pipeline",
    },
    Workflow {
        alias: "ship",
        job_id: "task_auto_pipeline",
    },
    Workflow {
        alias: "triage",
        job_id: "task_triage_pipeline",
    },
];

pub fn find_workflow(name: &str) -> Option<&'static Workflow> {
    WORKFLOWS.iter().find(|w| w.alias == name)
}

/// Canonical alias of the gated ship workflow (`task_auto_pipeline`).
pub const SHIP_WORKFLOW_ALIAS: &str = "ship";
pub const AUTO_WORKFLOW_ALIAS: &str = "auto";

/// Build the `task_auto_pipeline` input document for a ship run.
///
/// An empty `task_ids` slice selects auto mode (the pipeline discovers
/// backlog tasks itself). Explicit ids are validated for duplicates and
/// emptiness so every submission surface rejects the same malformed input.
///
/// [ORB-11187] `completion` is only written when it departs from the `review`
/// default, so an ordinary submission's persisted input is unchanged and the
/// presence of the key is itself the durable record that an operator granted
/// this run completion authority.
///
/// [ORB-11746] `base_sync` is only written for `--mode local`. Seeded shipping
/// jobs default `base_sync: remote` (fetch `origin/<base>`), which is correct
/// for PR delivery but fails `worktree_setup` on a disposable repo with no
/// remotes. Local mode opts into the local base so First Task can complete
/// without origin; PR submissions omit the key and keep the remote default.
pub fn build_ship_input(
    mode: ShipMode,
    base_branch: &str,
    task_ids: &[String],
    completion: CompletionPolicy,
    allowed_crews: &[String],
) -> Result<Value, OrbitError> {
    if base_branch.trim().is_empty() {
        return Err(OrbitError::InvalidInput(
            "ship base branch must not be empty".to_string(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in explicit task selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in explicit task selection"
            )));
        }
    }

    let mut map = serde_json::Map::new();
    map.insert(
        "mode".to_string(),
        Value::String(mode.as_input_value().to_string()),
    );
    map.insert(
        "base_branch".to_string(),
        Value::String(base_branch.to_string()),
    );
    if mode == ShipMode::Local {
        map.insert("base_sync".to_string(), Value::String("local".to_string()));
    }
    if !task_ids.is_empty() {
        map.insert(
            "task_ids".to_string(),
            Value::Array(task_ids.iter().cloned().map(Value::String).collect()),
        );
    }
    if completion.completes() {
        map.insert(
            "completion".to_string(),
            Value::String(completion.as_input_value().to_string()),
        );
    }
    if !allowed_crews.is_empty() {
        map.insert(
            "allowed_crews".to_string(),
            Value::Array(allowed_crews.iter().cloned().map(Value::String).collect()),
        );
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn triage_workflow_routes_to_triage_pipeline() {
        let workflow = find_workflow("triage").expect("triage workflow");

        assert_eq!(workflow.job_id, "task_triage_pipeline");
    }
}

#[cfg(test)]
mod ship_input_tests {
    use super::*;

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
        assert!(
            build_ship_input(ShipMode::Pr, "main", &dup, CompletionPolicy::Review, &[]).is_err()
        );

        let blank = vec!["  ".to_string()];
        assert!(
            build_ship_input(ShipMode::Pr, "main", &blank, CompletionPolicy::Review, &[]).is_err()
        );

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
}

#[cfg(test)]
mod ship_mode_resolution_tests {
    use super::*;
    use chrono::Utc;
    use orbit_types::workspace::{Workspace, WorkspaceStatus};

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
}
