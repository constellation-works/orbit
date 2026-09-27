//! The §4.5 definition rules and the `plugin:<ns>` catalog layer.

use orbit_types::plugin::PluginStatus;
use orbit_types::task::{TaskStatus, TaskType};
use orbit_types::workflow::JobRunState;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::{
    PluginAddOptions, PluginEnableOptions, install_plugin, show_plugin, validate_plugin_dir,
};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::OrbitRuntime;
use crate::application::job::seed_default_jobs;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::bootstrap::activity::seed_default_activities;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run git in fixture");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn plugin_delivery_fixture(case: &str) -> (PluginFixture, OrbitRuntime, String) {
    let fixture = PluginFixture::new();
    git(&fixture.repo_root, &["init"]);
    git(&fixture.repo_root, &["config", "user.name", "Orbit Test"]);
    git(
        &fixture.repo_root,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(fixture.repo_root.join(".gitignore"), ".orbit/\n").expect("ignore state");
    std::fs::write(fixture.repo_root.join("README.md"), "fixture\n").expect("write context");
    git(&fixture.repo_root, &["add", ".gitignore", "README.md"]);
    git(&fixture.repo_root, &["commit", "-m", "fixture base"]);
    git(&fixture.repo_root, &["checkout", "-b", "agent-main"]);
    seed_default_activities(&fixture.global_root.join("resources/activities"), true)
        .expect("seed shipped activities");
    seed_default_jobs(&fixture.global_root.join("resources/jobs"), true)
        .expect("seed shipped jobs");

    let plugin = DefinitionPlugin::new("delivery");
    let source = plugin.write(&fixture);
    let tail = match case {
        "success" => {
            "    - id: review\n      target: activity:update_task\n      default_input:\n        task_id: \"{{ input.task_id }}\"\n        status: review\n    - id: complete\n      target: activity:task_complete\n      default_input:\n        job_run_id: \"{{ steps.worktree.output.job_run_id }}\"\n        task_id: \"{{ input.task_id }}\"\n"
        }
        "dirty" => {
            "    - id: review\n      target: activity:update_task\n      default_input:\n        task_id: \"{{ input.task_id }}\"\n        status: review\n"
        }
        "failed" => {
            "    - id: fail\n      target: activity:update_task\n      default_input:\n        task_id: \"{{ input.task_id }}\"\n        status: not-a-status\n"
        }
        other => panic!("unknown case {other}"),
    };
    let job_path = source.join("definitions/jobs/pipeline.yaml");
    std::fs::write(
        &job_path,
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {}\nspec:\n  state: enabled\n  owns_task_worktree: true\n  steps:\n    - id: worktree\n      target: activity:worktree_setup\n      default_input:\n        task_ids: \"{{{{ input.task_ids }}}}\"\n        base: agent-main\n        base_sync: local\n{tail}",
            plugin.job
        ),
    )
    .expect("write plugin delivery job");
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 plugin source"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install delivery plugin");
    let runtime = fixture.reopen();
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("Plugin delivery {case}"),
            description: "Fixture delivery".to_string(),
            acceptance_criteria: vec!["The fixture delivers".to_string()],
            plan: "Run the fixture".to_string(),
            context_files: vec!["README.md".to_string()],
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                execution_summary: Some("Fixture work completed".to_string()),
                ..Default::default()
            },
        )
        .expect("record fixture summary");
    (fixture, runtime, task.id)
}

fn run_plugin_delivery_cases() {
    let (fixture, runtime, task_id) = plugin_delivery_fixture("success");
    let job = runtime
        .show_job_catalog_entry("delivery_refresh_pipeline")
        .expect("plugin job resolves from catalog");
    assert_eq!(
        layer_of(
            &runtime,
            "delivery_refresh_pipeline",
            "job:delivery_refresh_pipeline"
        )
        .0,
        "plugin:delivery"
    );
    let input = json!({"task_id": task_id, "task_ids": [task_id], "completion": "done"});
    let result = runtime
        .run_job_v2_from_yaml(&job.path, input)
        .expect("plugin delivery succeeds");
    let worktree = PathBuf::from(
        result.pipeline["worktree"]["workspace_path"]
            .as_str()
            .expect("worktree output path"),
    );
    assert!(
        !worktree.exists(),
        "successful plugin delivery reaps its worktree"
    );
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Done
    );
    let state = runtime
        .read_run_state(&result.run_id)
        .expect("run state")
        .expect("persisted run state");
    assert_eq!(
        state.pipeline["worktree_cleanup"]["reports"][0]["action"],
        "removed"
    );
    // The fixture holds a scoped HOME guard; release it before making the
    // next independent fixture in this child process.
    drop(runtime);
    drop(fixture);

    let (fixture, runtime, task_id) = plugin_delivery_fixture("failed");
    let failure = runtime
        .run_job_v2_from_yaml(
            &runtime
                .show_job_catalog_entry("delivery_refresh_pipeline")
                .expect("plugin job")
                .path,
            json!({"task_id": task_id, "task_ids": [task_id]}),
        )
        .expect_err("invalid task status fails the plugin job");
    assert!(failure.to_string().contains("not-a-status"));
    let run_id = runtime
        .get_task(&task_id)
        .expect("task after failure")
        .job_run_id
        .expect("worktree step admitted task");
    assert_eq!(
        runtime.show_job_run(&run_id).expect("failed run").state,
        JobRunState::Failed
    );
    let listed = Command::new("git")
        .current_dir(&fixture.repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("list registered worktrees");
    assert!(listed.status.success());
    assert!(
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(Path::new)
            .any(|path| path != fixture.repo_root.as_path() && path.exists()),
        "failed plugin run retains its registered worktree"
    );
    drop(runtime);
    drop(fixture);

    let (fixture, runtime, task_id) = plugin_delivery_fixture("dirty");
    let result = runtime
        .run_job_v2_from_yaml(
            &runtime
                .show_job_catalog_entry("delivery_refresh_pipeline")
                .expect("plugin job")
                .path,
            json!({"task_id": task_id, "task_ids": [task_id]}),
        )
        .expect("plugin review succeeds");
    let worktree = PathBuf::from(
        result.pipeline["worktree"]["workspace_path"]
            .as_str()
            .expect("worktree output path"),
    );
    assert!(
        worktree.exists(),
        "review-only delivery retains its worktree"
    );
    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("settle reviewed task after run stops");
    std::fs::write(worktree.join("untracked.txt"), "rescue me\n")
        .expect("dirty the plugin worktree");
    let report = runtime
        .cleanup_delivered_worktree(&result.run_id)
        .expect("collector runs")
        .expect("plugin opts in");
    assert_eq!(report.reports[0].action, "skipped:dirty_rescue_candidate");
    assert!(worktree.exists(), "dirty worktree remains for rescue");

    for coordinator in ["task_gate_pipeline", "workspace_auto_pipeline"] {
        let run = runtime
            .stores()
            .jobs()
            .insert_job_run(
                coordinator,
                1,
                chrono::Utc::now(),
                Some(json!({"task_ids": [task_id]})),
                None,
            )
            .expect("record coordinator run");
        assert!(
            runtime
                .cleanup_delivered_worktree(&run.run_id)
                .expect("coordinator lookup")
                .is_none()
        );
    }
    assert!(
        worktree.exists(),
        "coordinators do not reap the plugin worktree"
    );
    assert!(fixture.repo_root.exists());
}

#[test]
fn plugin_delivery_collects_only_safe_worktrees() {
    if std::env::var_os("ORBIT_PLUGIN_DELIVERY_CHILD").is_some() {
        run_plugin_delivery_cases();
        return;
    }
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .arg("plugin_delivery_collects_only_safe_worktrees")
        .arg("--nocapture")
        .env_remove("ORBIT_WORKTREE_ROOT")
        .env("ORBIT_PLUGIN_DELIVERY_CHILD", "1")
        .output()
        .expect("run isolated plugin fixture");
    assert!(
        output.status.success(),
        "isolated fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn install(fixture: &PluginFixture, plugin: &DefinitionPlugin<'_>) {
    let source = plugin.write(fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
}

fn layer_of(runtime: &OrbitRuntime, job: &str, reference: &str) -> (String, Vec<String>) {
    let rows = runtime.catalog_reference_layers(job).expect("layers");
    let row = rows
        .iter()
        .find(|row| row.reference == reference)
        .unwrap_or_else(|| panic!("a row for {reference} in {rows:?}"));
    (row.layer.clone(), row.shadows.clone())
}

#[test]
fn a_plugins_activities_and_jobs_resolve_from_its_own_catalog_layer() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "graph").expect("show").status,
        PluginStatus::Active
    );
    assert_eq!(
        layer_of(
            &runtime,
            "graph_refresh_pipeline",
            "job:graph_refresh_pipeline"
        ),
        ("plugin:graph".to_string(), Vec::new())
    );
    assert_eq!(
        layer_of(&runtime, "graph_refresh_pipeline", "activity:graph_refresh"),
        ("plugin:graph".to_string(), Vec::new())
    );
}

#[test]
fn a_later_plugin_with_the_same_activity_name_is_refused_and_the_first_still_serves() {
    let fixture = PluginFixture::new();
    let mut first = DefinitionPlugin::new("alpha");
    first.activity = "shared_index".to_string();
    install(&fixture, &first);
    let mut colliding = DefinitionPlugin::new("beta");
    colliding.activity = "shared_index".to_string();
    install(&fixture, &colliding);

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "alpha").expect("show alpha").status,
        PluginStatus::Active
    );
    let refused = show_plugin(&runtime, "beta").expect("show beta");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.expect("collision diagnostic");
    assert!(
        diagnostic.contains("activity 'shared_index'")
            && diagnostic.contains("plugin 'alpha'")
            && diagnostic.contains("plugin 'beta'"),
        "the diagnostic names the colliding activity and both plugins: {diagnostic}"
    );
    assert_eq!(
        layer_of(&runtime, "alpha_refresh_pipeline", "activity:shared_index").0,
        "plugin:alpha",
        "the first valid plugin keeps serving its catalog definitions"
    );
}

#[test]
fn a_later_plugin_with_the_same_job_name_is_refused() {
    let fixture = PluginFixture::new();
    let mut first = DefinitionPlugin::new("alpha");
    first.job = "shared_pipeline".to_string();
    install(&fixture, &first);
    let mut colliding = DefinitionPlugin::new("beta");
    colliding.job = "shared_pipeline".to_string();
    install(&fixture, &colliding);

    let runtime = fixture.reopen();
    assert_eq!(
        show_plugin(&runtime, "alpha").expect("show alpha").status,
        PluginStatus::Active
    );
    let refused = show_plugin(&runtime, "beta").expect("show beta");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.expect("collision diagnostic");
    assert!(
        diagnostic.contains("job 'shared_pipeline'")
            && diagnostic.contains("plugin 'alpha'")
            && diagnostic.contains("plugin 'beta'"),
        "the diagnostic names the colliding job and both plugins: {diagnostic}"
    );
}

#[test]
fn a_workspace_activity_shadows_the_plugins_and_the_layer_output_says_so() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));

    let activities = fixture.workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities).expect("create workspace activities");
    std::fs::write(
        activities.join("graph_refresh.yaml"),
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: graph_refresh\nspec:\n  \
         type: deterministic\n  description: The workspace's own refresh.\n  \
         input_schema_json:\n    type: object\n  action: sleep\n  config: {}\n",
    )
    .expect("write workspace activity");

    let runtime = fixture.reopen();
    let (layer, shadows) = layer_of(&runtime, "graph_refresh_pipeline", "activity:graph_refresh");
    assert_eq!(layer, "workspace");
    assert_eq!(shadows, vec!["plugin:graph".to_string()]);
}

#[test]
fn a_cross_plugin_routine_target_refuses_that_plugin_and_leaves_the_others_loading() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    install(
        &fixture,
        &DefinitionPlugin::new("atlas").targeting("job:graph_refresh_pipeline"),
    );

    let runtime = fixture.reopen();
    let refused = show_plugin(&runtime, "atlas").expect("show atlas");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("refresh.yaml")
            && diagnostic.contains("job:graph_refresh_pipeline")
            && diagnostic.contains("shipped default"),
        "the diagnostic names the file and the rule: {diagnostic}"
    );
    assert_eq!(
        show_plugin(&runtime, "graph").expect("show graph").status,
        PluginStatus::Active,
        "one plugin's refusal leaves the others untouched"
    );
}

#[test]
fn a_plugin_job_may_not_reference_another_plugins_activity() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("alpha"));

    let source = DefinitionPlugin::new("beta").write(&fixture);
    std::fs::write(
        source.join("definitions/jobs/pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: beta_refresh_pipeline\nspec:\n  \
         state: enabled\n  kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: \
         refresh\n      target: activity:alpha_refresh\n",
    )
    .expect("point beta's job at alpha's activity");
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install refused plugin for diagnostics");

    let runtime = fixture.reopen();
    let refused = show_plugin(&runtime, "beta").expect("show beta");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.expect("a diagnostic");
    assert!(
        diagnostic.contains("pipeline.yaml")
            && diagnostic.contains("alpha_refresh")
            && diagnostic.contains("only its own activity or a shipped default"),
        "the diagnostic names the job file, reference, and ownership rule: {diagnostic}"
    );
    assert_eq!(
        show_plugin(&runtime, "alpha").expect("show alpha").status,
        PluginStatus::Active,
        "the referenced plugin remains available"
    );
}

#[test]
fn a_routine_that_ships_enabled_refuses_the_plugin() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_enabled_routine(),
    );

    let runtime = fixture.reopen();
    let refused = show_plugin(&runtime, "graph").expect("show");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("refresh.yaml") && diagnostic.contains("enabled: true"),
        "{diagnostic}"
    );
}

#[test]
fn an_auto_task_that_ships_enabled_refuses_the_plugin() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_enabled_auto_task(),
    );

    let runtime = fixture.reopen();
    let refused = show_plugin(&runtime, "graph").expect("show");
    assert_eq!(refused.status, PluginStatus::Inactive);
    let diagnostic = refused.diagnostic.clone().expect("a diagnostic");
    assert!(
        diagnostic.contains("reindex.yaml") && diagnostic.contains("enabled: true"),
        "{diagnostic}"
    );
}

#[test]
fn validate_reports_what_enabling_would_seed() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);

    let report = validate_plugin_dir(&fixture.runtime, &source, false).expect("validate");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("seeds 1 routine(s) and 1 auto-task(s)")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn plugin_tool_call_reaches_a_plugin_tool_and_refuses_anything_else() {
    use orbit_engine::RuntimeHost;
    use orbit_tools::ToolContext;
    use serde_json::json;

    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();

    let output = runtime
        .run_deterministic(
            "plugin.tool_call",
            &json!({ "tool": "graph.hello", "input": {} }),
            &json!({}),
            ToolContext::default(),
        )
        .expect("the plugin tool answers");
    assert_eq!(output["plugin"], json!("graph"));

    // The call went through the ordinary audited dispatch, so the row names
    // the plugin behind it (§4.4).
    let events = runtime
        .list_audit_events(None, Some("graph.hello".to_string()), None, None, 10)
        .expect("audit events");
    let plugin = events
        .first()
        .expect("the call was audited")
        .plugin
        .as_ref()
        .expect("the audit row names the plugin");
    assert_eq!(plugin.name, "graph");
    assert_eq!(plugin.version, "1.0.0");

    let refusal = runtime
        .run_deterministic(
            "plugin.tool_call",
            &json!({ "tool": "orbit.search", "input": {} }),
            &json!({}),
            ToolContext::default(),
        )
        .expect_err("a built-in tool is not a plugin tool");
    assert!(
        refusal.to_string().contains("not a plugin tool"),
        "{refusal}"
    );
}

#[test]
fn enabling_records_grants_and_reports_the_definitions_it_seeded() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));

    let runtime = fixture.reopen();
    let result = super::super::enable_plugin(&runtime, "graph", &PluginEnableOptions::default())
        .expect("enable");
    assert_eq!(result.seeded.len(), 2);
    assert!(
        result
            .seeded
            .iter()
            .any(|seeded| seeded.kind == "routine" && seeded.name == "graph-refresh")
    );
    assert!(
        result
            .seeded
            .iter()
            .any(|seeded| seeded.kind == "auto_task" && seeded.name == "graph-reindex")
    );
}

/// The whole path an operator exercises with `orbit run job <plugin-job>`:
/// the plugin's job resolves from its own catalog layer, its step's activity
/// resolves too, and the `plugin.tool_call` inside it reaches the backend.
///
/// Run in-process rather than through a detached worker: this asserts the
/// catalog and dispatch wiring, which is what phase 3 adds, not the worker
/// handoff every other job already shares.
#[test]
fn a_plugin_job_runs_its_plugin_tool_call_end_to_end() {
    let fixture = PluginFixture::new();
    install(&fixture, &DefinitionPlugin::new("graph"));
    let runtime = fixture.reopen();

    let job_path = runtime
        .show_job_catalog_entry("graph_refresh_pipeline")
        .expect("the plugin job is in the catalog")
        .path;
    let result = runtime
        .run_job_v2_from_yaml(&job_path, serde_json::json!({}))
        .expect("run the plugin job");
    assert!(result.success, "{result:?}");

    let events = runtime
        .list_audit_events(None, Some("graph.hello".to_string()), None, None, 10)
        .expect("audit events");
    let plugin = events
        .first()
        .expect("the step's tool call was audited")
        .plugin
        .as_ref()
        .expect("the audit row names the plugin");
    assert_eq!(plugin.name, "graph");
}

/// [ORB-13270] A deterministic step has no agent in the loop, so the programs
/// a plugin's backend declares are bounded by what the operator granted at
/// enable, not by the empty `proc.spawn` list every activity context carries.
/// The granted `git` actually runs: the backend reports its output.
#[test]
fn a_plugin_job_step_spawns_the_program_its_operator_granted() {
    let fixture = PluginFixture::new();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_program("git"),
    );
    let runtime = fixture.reopen();

    let job_path = runtime
        .show_job_catalog_entry("graph_refresh_pipeline")
        .expect("the plugin job is in the catalog")
        .path;
    let result = runtime
        .run_job_v2_from_yaml(&job_path, json!({}))
        .expect("run the plugin job");
    assert!(result.success, "{result:?}");
    let pipeline = result.pipeline.to_string();
    assert!(
        pipeline.contains("git version"),
        "the backend ran the granted program: {pipeline}"
    );
}

/// [ORB-13270] The deterministic-step bound is the grant, so a declared
/// program that did not resolve at enable is still refused, by name.
#[test]
fn a_plugin_job_step_is_refused_a_program_that_was_never_granted() {
    let fixture = PluginFixture::new();
    let missing = fixture.sources.join("absent/orbit-fixture-missing-program");
    let missing = missing.to_str().expect("utf8 path").to_string();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_program(&missing),
    );
    let runtime = fixture.reopen();

    let job_path = runtime
        .show_job_catalog_entry("graph_refresh_pipeline")
        .expect("the plugin job is in the catalog")
        .path;
    let message = runtime
        .run_job_v2_from_yaml(&job_path, json!({}))
        .expect_err("the step is refused")
        .to_string();
    assert!(
        message.contains(&format!("program '{missing}'")) && message.contains("not granted"),
        "the refusal names the program: {message}"
    );
}

/// [ORB-13270] The grant is re-read at call time, not taken from the registry
/// built at load: a witness whose recorded program no longer matches the
/// loaded plugin refuses the step, and a disabled row grants nothing.
#[test]
fn a_plugin_job_step_rechecks_the_program_grant_at_call_time() {
    use std::collections::BTreeMap;

    use crate::runtime::plugin::grants::record_authorization;

    let fixture = PluginFixture::new();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_program("git"),
    );
    let runtime = fixture.reopen();
    let installed = runtime
        .stores()
        .plugins()
        .get_plugin("graph")
        .expect("read row")
        .expect("installed");

    // Rewritten after this runtime loaded the plugin, as a later consent
    // resolving `git` elsewhere would.
    record_authorization(
        &fixture.global_root,
        "graph",
        installed.enabled,
        &installed.grants,
        &BTreeMap::from([("git".to_string(), fixture.sources.join("elsewhere/git"))]),
    )
    .expect("rewrite the witness");

    let job_path = runtime
        .show_job_catalog_entry("graph_refresh_pipeline")
        .expect("the plugin job is in the catalog")
        .path;
    let message = runtime
        .run_job_v2_from_yaml(&job_path, json!({}))
        .expect_err("the step is refused")
        .to_string();
    assert!(
        message.contains("program 'git'") && message.contains("differs"),
        "{message}"
    );
}

/// [ORB-13270] Regression: an agent activity's context is still held to its
/// own `proc_allowed_programs`. With none declared, the same granted plugin
/// tool is refused, naming the program.
#[test]
fn an_agent_activity_without_the_program_is_still_refused_the_plugin_tool() {
    use orbit_engine::RuntimeHost;

    let fixture = PluginFixture::new();
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_program("git"),
    );
    let runtime = fixture.reopen();

    for allowlist in [None, Some(&[][..]), Some(&["uv".to_string()][..])] {
        let context = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            &runtime,
            Some("jrun-agent"),
            None,
            None,
            allowlist,
        );
        let refusal = runtime
            .run_deterministic(
                "plugin.tool_call",
                &json!({ "tool": "graph.hello", "input": {} }),
                &json!({}),
                context,
            )
            .expect_err("an agent context without git is refused");
        assert!(
            refusal
                .to_string()
                .contains("program 'git' is not in the allowed list"),
            "{allowlist:?}: {refusal}"
        );
    }

    // Allowing the program is what admits the agent, not the grant alone.
    let allowed = ["git".to_string()];
    let context = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
        &runtime,
        Some("jrun-agent"),
        None,
        None,
        Some(&allowed),
    );
    let output = runtime
        .run_deterministic(
            "plugin.tool_call",
            &json!({ "tool": "graph.hello", "input": {} }),
            &json!({}),
            context,
        )
        .expect("an agent allowed git reaches the tool");
    assert!(
        output["program_output"]
            .as_str()
            .is_some_and(|out| out.starts_with("git version")),
        "{output}"
    );
}

/// A plugin program granted at enable time reaches an agent in a shipped
/// disallow-mode activity. The fake executable keeps this independent of the
/// runner's installed development tools.
#[cfg(unix)]
#[test]
fn shipped_agent_activity_admits_granted_uv_plugin_program() {
    use orbit_engine::RuntimeHost;
    use orbit_engine::activity_job::load_activity_asset;
    use orbit_types::workflow::ActivityV2Spec;
    use std::os::unix::fs::PermissionsExt;

    let fixture = PluginFixture::new();
    let uv = fixture.sources.join("uv");
    std::fs::write(&uv, "#!/bin/sh\necho uv 1.0\n").expect("fake uv");
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).expect("executable uv");
    let uv_name = uv.to_str().expect("utf8 uv path");
    install(
        &fixture,
        &DefinitionPlugin::new("graph").with_program(uv_name),
    );
    let runtime = fixture.reopen();

    let asset = load_activity_asset(include_str!(
        "../../../../assets/activities/agent_invoke.yaml"
    ))
    .expect("shipped activity loads");
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("agent_invoke is an agent activity")
    };
    let mut context = <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
        &runtime,
        Some("jrun-agent"),
        None,
        None,
        spec.proc_allowed_programs.as_deref(),
    );
    context.proc_disallowed_programs = spec.proc_disallowed_programs;
    let output = runtime
        .run_deterministic(
            "plugin.tool_call",
            &json!({ "tool": "graph.hello", "input": {} }),
            &json!({}),
            context,
        )
        .expect("granted uv passes shipped agent program policy");
    assert!(
        output["program_output"]
            .as_str()
            .is_some_and(|s| s.starts_with("uv 1.0")),
        "{output}"
    );
}
