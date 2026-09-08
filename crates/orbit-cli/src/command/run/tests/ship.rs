use chrono::Utc;
use orbit_registry::workspace_registry::{registry_path_for, save_registry_to};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
use tempfile::tempdir;

use crate::command::Execute;
use crate::tests::env_isolation::EnvGuard;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde_json::json;

use super::super::auto::AutoCommand;
use super::super::ship::*;

fn ship_args(task_ids: &[&str], mode: ShipMode, base: Option<&str>) -> ShipCommand {
    ShipCommand {
        task_ids: task_ids.iter().map(|value| value.to_string()).collect(),
        mode: Some(mode),
        base: base.map(str::to_string),
        complete: false,
        allow_crew: Vec::new(),
        json: false,
        claim_token: None,
    }
}

/// The same args with the operator's explicit completion authorization.
fn completing_ship_args(task_ids: &[&str], mode: ShipMode, base: Option<&str>) -> ShipCommand {
    ShipCommand {
        complete: true,
        ..ship_args(task_ids, mode, base)
    }
}

fn restricted_ship_args(
    task_ids: &[&str],
    mode: ShipMode,
    base: Option<&str>,
    crews: &[&str],
) -> ShipCommand {
    ShipCommand {
        allow_crew: crews.iter().map(|crew| (*crew).to_string()).collect(),
        ..ship_args(task_ids, mode, base)
    }
}

#[test]
fn ship_mode_uses_selected_root_registry_when_home_registry_is_empty() {
    let fixture = tempdir().expect("fixture tempdir");
    let selected_root = fixture.path().join("selected-root");
    let workspace_root = fixture.path().join("workspace-orbit");
    let repo_root = fixture.path().join("repo");
    let home = fixture.path().join("empty-home");
    let home_registry_root = home.join(".orbit");
    for directory in [
        &selected_root,
        &workspace_root,
        &repo_root,
        &home_registry_root,
    ] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }

    let runtime = OrbitRuntime::from_roots(&selected_root, &workspace_root)
        .expect("build selected-root runtime");
    let workspace = Workspace {
        id: "ws_ship_mode".to_string(),
        name: "ship-mode-test".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: Some("local".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout =
        WorkspaceCheckout::owner(workspace.id.clone(), repo_root, workspace_root.clone());
    let mut registry = WorkspaceRegistry::default();
    orbit_registry::workspace_registry::register_workspace(&mut registry, workspace)
        .expect("register workspace");
    orbit_registry::workspace_registry::register_checkout(&mut registry, checkout)
        .expect("register checkout");
    save_registry_to(&registry, &registry_path_for(&selected_root))
        .expect("save selected-root registry");
    save_registry_to(
        &WorkspaceRegistry::default(),
        &registry_path_for(&home_registry_root),
    )
    .expect("save empty home registry");

    let _env = EnvGuard::acquire().home(&home);
    assert_eq!(
        resolve_ship_mode(
            &ShipCommand {
                task_ids: Vec::new(),
                mode: None,
                base: None,
                complete: false,
                allow_crew: Vec::new(),
                json: false,
                claim_token: None,
            },
            &runtime,
        )
        .expect("resolve ship mode"),
        orbit_core::ShipMode::Local
    );
}

/// Build a ship plan from test args, threading the args' explicit mode through
/// the resolved-mode parameter (production resolves this from the registry).
fn build_plan(args: &ShipCommand, config_base_branch: &str) -> Result<WorkflowRunPlan, OrbitError> {
    let mode = args.mode.expect("test args set an explicit mode").to_core();
    build_ship_run_plan(args, config_base_branch, mode)
}

#[test]
fn ship_auto_mode_omits_task_ids_and_uses_pr_mode_by_default() {
    let plan = build_plan(&ship_args(&[], ShipMode::Pr, None), "agent-main").expect("build plan");

    assert_eq!(plan.workflow_alias, SHIP_WORKFLOW);
    assert_eq!(
        plan.input,
        json!({
            "mode": "pr",
            "base_branch": "agent-main",
        })
    );
}

#[test]
fn ship_auto_mode_preserves_local_mode_and_base_override() {
    let plan = build_plan(&ship_args(&[], ShipMode::Local, Some("main")), "agent-main")
        .expect("build plan");

    assert_eq!(plan.workflow_alias, SHIP_WORKFLOW);
    assert_eq!(
        plan.input,
        json!({
            "mode": "local",
            "base_branch": "main",
            "base_sync": "local",
        })
    );
}

#[test]
fn explicit_ship_uses_unified_gated_workflow_with_pr_mode() {
    let plan = build_plan(
        &ship_args(&["T20260425-2010", "T20260425-2011"], ShipMode::Pr, None),
        "agent-main",
    )
    .expect("build plan");

    assert_eq!(plan.workflow_alias, SHIP_WORKFLOW);
    assert_eq!(
        plan.input,
        json!({
            "mode": "pr",
            "base_branch": "agent-main",
            "task_ids": ["T20260425-2010", "T20260425-2011"],
        })
    );
}

#[test]
fn explicit_ship_preserves_local_mode_and_base_override() {
    let plan = build_plan(
        &ship_args(&["T20260425-2010"], ShipMode::Local, Some("main")),
        "agent-main",
    )
    .expect("build plan");

    assert_eq!(plan.workflow_alias, SHIP_WORKFLOW);
    assert_eq!(
        plan.input,
        json!({
            "mode": "local",
            "base_branch": "main",
            "base_sync": "local",
            "task_ids": ["T20260425-2010"],
        })
    );
}

#[test]
fn explicit_ship_persists_its_crew_restriction() {
    let plan = build_plan(
        &restricted_ship_args(&["T20260425-2010"], ShipMode::Pr, None, &["sol"]),
        "agent-main",
    )
    .expect("build plan");

    assert_eq!(plan.input["allowed_crews"], json!(["sol"]));
}

#[test]
fn ship_local_deprecation_returns_legacy_error() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let err = LegacyShipLocalCommand {
        task_ids: vec!["T20260425-2010".to_string()],
        base: None,
        json: false,
    }
    .execute(&runtime)
    .expect_err("deprecated command should fail");
    assert!(
        err.to_string().contains("orbit run ship --mode local"),
        "unexpected error: {err}"
    );
}

#[test]
fn ship_rejects_removed_history_forms() {
    let err = build_plan(&ship_args(&["list"], ShipMode::Pr, None), "agent-main")
        .expect_err("legacy history form should fail");
    assert!(
        err.to_string().contains("orbit run history"),
        "unexpected error: {err}"
    );

    let err = build_plan(&ship_args(&["show"], ShipMode::Pr, None), "agent-main")
        .expect_err("legacy history form should fail");
    assert!(
        err.to_string().contains("orbit run history"),
        "unexpected error: {err}"
    );
}

#[test]
fn ship_rejects_removed_local_subcommand_form() {
    let err = build_plan(&ship_args(&["local"], ShipMode::Pr, None), "agent-main")
        .expect_err("legacy local form should fail");
    assert!(
        err.to_string().contains("--mode local"),
        "unexpected error: {err}"
    );
}

#[test]
fn ship_rejects_removed_auto_positional_form() {
    let err = build_plan(&ship_args(&["auto"], ShipMode::Pr, None), "agent-main")
        .expect_err("legacy auto form should fail");
    assert!(
        err.to_string().contains("orbit run auto"),
        "unexpected error: {err}"
    );
}

#[test]
fn ship_rejects_duplicate_task_ids() {
    let err = build_plan(
        &ship_args(&["T20260425-2010", "T20260425-2010"], ShipMode::Pr, None),
        "agent-main",
    )
    .expect_err("duplicate task IDs should fail");
    assert!(
        err.to_string().contains("duplicate task id"),
        "unexpected error: {err}"
    );
}

fn write_ship_job_asset(runtime: &OrbitRuntime) {
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs directory");
    std::fs::write(
        jobs_dir.join("task_auto_pipeline.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: task_auto_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: wait
      spec:
        type: deterministic
        action: sleep
        config:
          seconds: 30
"#,
    )
    .expect("write ship job fixture");
}

/// The interactive command must enter the same runtime submission path as the
/// dashboard, MCP tool, and routine action. A generic workflow dispatch would
/// insert a second run here instead of returning this typed shared conflict.
#[test]
fn interactive_ship_inherits_the_shared_in_flight_guard() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    write_ship_job_asset(&runtime);
    let task = runtime
        .add_task(TaskAddParams {
            title: "CLI ship admission fixture".to_string(),
            description: "Synthetic task for the shared in-flight guard".to_string(),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("create task fixture");
    let task_id = task.id.to_string();
    ship_args(&[&task_id], ShipMode::Local, Some("main"))
        .execute(&runtime)
        .expect("first interactive ship dispatch");
    let first_run = runtime
        .list_job_runs(Default::default())
        .expect("list first CLI run")
        .into_iter()
        .next()
        .expect("first CLI dispatch persists a run");
    // [ORB-11333] Every delivery submission captures the effective review
    // policy once; the caller-shaped input is otherwise unchanged.
    let mut persisted = first_run.input.clone().expect("persisted run input");
    let review = persisted
        .as_object_mut()
        .expect("run input object")
        .remove("review")
        .expect("captured review admission");
    assert_eq!(review["timing"], "none");
    assert_eq!(review["timing_source"], "built-in");
    assert_eq!(
        persisted,
        json!({
            "mode": "local",
            "base_branch": "main",
            "base_sync": "local",
            "task_ids": [task_id],
        })
    );
    let audits = runtime
        .list_audit_events(None, None, None, None, 20)
        .expect("list CLI submission audits");
    assert!(audits.iter().any(|audit| {
        audit.tool_name.as_deref() == Some("pipeline.invoke")
            && audit.target_id.as_deref() == Some(first_run.run_id.as_str())
    }));

    let error = ship_args(&[&task_id], ShipMode::Local, Some("main"))
        .execute(&runtime)
        .expect_err("an interactive ship must refuse a task already in flight");

    let OrbitError::ShipRunInFlight { task_id, run_id } = &error else {
        panic!("expected ShipRunInFlight, got {error:?}");
    };
    assert_eq!(task_id, &task.id.to_string());
    assert!(run_id.starts_with("jrun-"), "unexpected run id: {run_id}");

    let runs = runtime
        .list_job_runs(Default::default())
        .expect("list job runs");
    assert_eq!(
        runs.len(),
        1,
        "a refused CLI submission must not insert a run"
    );
}

/// [ORB-11187] `--complete` is the operator's per-invocation authorization for
/// the run to finish delivery and complete the tasks it ships.
#[test]
fn complete_flag_persists_the_completion_policy_in_the_submitted_input() {
    let plan = build_plan(
        &completing_ship_args(&["T20260425-2010"], ShipMode::Pr, None),
        "agent-main",
    )
    .expect("build plan");

    assert_eq!(
        plan.input,
        json!({
            "mode": "pr",
            "base_branch": "agent-main",
            "task_ids": ["T20260425-2010"],
            "completion": "done",
        })
    );
}

/// The default is unchanged in the strongest sense available: the submitted run
/// input is byte-identical to what it was before the flag existed, so nothing
/// downstream can read a completion policy out of an ordinary submission.
#[test]
fn omitting_the_complete_flag_leaves_the_submitted_input_untouched() {
    for mode in [ShipMode::Pr, ShipMode::Local] {
        let plan = build_plan(&ship_args(&["T20260425-2010"], mode, None), "agent-main")
            .expect("build plan");

        assert!(
            plan.input.get("completion").is_none(),
            "{mode:?} default submission must not carry a completion policy"
        );
    }
}

/// Both entrypoints must parse the flag; `run auto` states the wider scope.
#[test]
fn complete_flag_parses_on_ship_and_auto_with_documented_scope() {
    use clap::{Args as _, FromArgMatches};

    let matches = ShipCommand::augment_args(clap::Command::new("ship"))
        .no_binary_name(true)
        .try_get_matches_from(["T20260425-2010", "--complete"])
        .expect("`orbit run ship <TASK_ID> --complete` parses");
    let ship = ShipCommand::from_arg_matches(&matches).expect("build ship command");
    assert!(ship.complete);

    let matches = AutoCommand::augment_args(clap::Command::new("auto"))
        .no_binary_name(true)
        .try_get_matches_from(["--for", "4h", "--complete"])
        .expect("`orbit run auto --for 4h --complete` parses");
    let auto = AutoCommand::from_arg_matches(&matches).expect("build auto command");
    assert!(auto.complete);

    let auto_help = AutoCommand::augment_args(clap::Command::new("auto"))
        .render_long_help()
        .to_string();
    assert!(
        auto_help.contains("blanket authorization"),
        "`run auto --complete` must document its window-wide authorization scope"
    );
    assert!(
        auto_help.contains("Off by default"),
        "`run auto --complete` must document that it is off by default"
    );
}

#[test]
fn allow_crew_parses_on_explicit_ship_and_is_documented() {
    use clap::{Args as _, FromArgMatches};

    let matches = ShipCommand::augment_args(clap::Command::new("ship"))
        .no_binary_name(true)
        .try_get_matches_from(["T20260425-2010", "--allow-crew", "sol,terra"])
        .expect("explicit ship crew restriction parses");
    let ship = ShipCommand::from_arg_matches(&matches).expect("build ship command");
    assert_eq!(ship.allow_crew, vec!["sol", "terra"]);

    let help = ShipCommand::augment_args(clap::Command::new("ship"))
        .render_long_help()
        .to_string();
    assert!(help.contains("--allow-crew"));
    assert!(help.contains("before a run is created"));
}
