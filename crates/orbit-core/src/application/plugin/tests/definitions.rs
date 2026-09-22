//! The §4.5 definition rules and the `plugin:<ns>` catalog layer.

use orbit_types::plugin::PluginStatus;

use super::super::{
    PluginAddOptions, PluginEnableOptions, install_plugin, show_plugin, validate_plugin_dir,
};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::OrbitRuntime;

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
