//! Workspace-scoped enable toggles: `[plugin_enablement]` narrows the host
//! enable state for one workspace and never touches another.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginDisabledLayer, PluginStatus};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

use super::super::{
    PluginAddOptions, disable_plugin, disable_plugin_in_workspace, enable_plugin_in_workspace,
    install_plugin, plugin_doctor, show_plugin, sync_plugins,
};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::PluginFixture;
use crate::OrbitRuntime;

/// Workspace A is the fixture's own; B is a second checkout on the same host.
struct TwoWorkspaces {
    fixture: PluginFixture,
    source: PathBuf,
    workspace_b: PathBuf,
}

impl TwoWorkspaces {
    /// Install `graph` enabled on the host, seeded into A by the enable and
    /// into B by a sync of B's pin, with no toggle written anywhere.
    fn new() -> Self {
        let fixture = PluginFixture::new();
        let source = DefinitionPlugin::new("graph").with_panel().write(&fixture);
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions {
                enable: true,
                ..PluginAddOptions::default()
            },
        )
        .expect("install enabled plugin");
        let workspace_b = fixture.repo_root.join("workspace-b/.orbit");
        std::fs::create_dir_all(&workspace_b).expect("create workspace B");
        std::fs::write(workspace_b.join("plugins.yaml"), pin(&source, true)).expect("pin in B");
        sync_plugins(&runtime_at(&fixture, &workspace_b), false, &[]).expect("seed workspace B");
        Self {
            fixture,
            source,
            workspace_b,
        }
    }

    fn runtime_a(&self) -> OrbitRuntime {
        self.fixture.reopen()
    }

    fn runtime_b(&self) -> OrbitRuntime {
        runtime_at(&self.fixture, &self.workspace_b)
    }

    fn host_enabled(&self) -> bool {
        self.fixture
            .runtime
            .stores()
            .plugins()
            .get_plugin("graph")
            .expect("read plugin row")
            .expect("plugin row")
            .enabled
    }
}

fn runtime_at(fixture: &PluginFixture, workspace: &Path) -> OrbitRuntime {
    OrbitRuntime::from_roots(&fixture.global_root, workspace).expect("build runtime")
}

fn pin(source: &Path, enabled: bool) -> String {
    format!(
        "schemaVersion: 1\nplugins:\n  - name: graph\n    source: {}\n    enabled: {enabled}\n",
        source.display()
    )
}

fn layer_of(runtime: &OrbitRuntime, reference: &str) -> String {
    runtime
        .catalog_reference_layers("graph_refresh_pipeline")
        .expect("layers")
        .into_iter()
        .find(|row| row.reference == reference)
        .map(|row| row.layer)
        .unwrap_or_else(|| "absent".to_string())
}

fn skip_reason(runtime: &OrbitRuntime) -> Option<String> {
    let definition = runtime
        .auto_task_show("graph-reindex")
        .expect("show auto-task")
        .expect("the seeded auto-task");
    runtime.auto_task_skip_reason(&definition)
}

fn workspace_record(name: &str) -> Workspace {
    let now = chrono::Utc::now();
    Workspace {
        id: format!("ws_{name}"),
        name: name.to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    }
}

fn workspace_toggle(workspace: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(workspace.join("config.toml")).ok()?;
    let document = toml::from_str::<toml::Value>(&raw).expect("parse workspace config");
    document.get("plugin_enablement")?.get("graph")?.as_bool()
}

#[test]
fn disabling_in_one_workspace_takes_its_whole_surface_off_there_and_leaves_the_other_active() {
    let ws = TwoWorkspaces::new();

    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable in A");
    assert!(
        ws.host_enabled(),
        "a workspace disable never touches the host row"
    );

    let a = ws.runtime_a();
    let b = ws.runtime_b();

    // Tools: gone from A's registry, and a call is refused with the typed
    // error rather than reported as an unknown tool.
    assert!(a.show_tool("graph.hello").is_err());
    match ws.fixture.call(&a, "graph.hello") {
        Err(OrbitError::PluginDisabledInWorkspace { plugin, .. }) => assert_eq!(plugin, "graph"),
        other => panic!("expected the workspace refusal, got {other:?}"),
    }
    ws.fixture
        .call(&b, "graph.hello")
        .expect("B still serves the tool");

    // Job and activity catalogs.
    assert_eq!(layer_of(&a, "job:graph_refresh_pipeline"), "unresolved");
    assert_eq!(layer_of(&b, "job:graph_refresh_pipeline"), "plugin:graph");
    assert_eq!(layer_of(&b, "activity:graph_refresh"), "plugin:graph");

    // Auto-tasks.
    let reason = skip_reason(&a).expect("A skips the seeded auto-task");
    assert!(
        reason.contains("switched off in this workspace") && reason.contains("--scope workspace"),
        "{reason}"
    );
    assert_eq!(skip_reason(&b), None);

    // Routines, judged per source workspace in one discovery pass.
    let collection = crate::application::routines::collect_routines(&[
        (workspace_record("alpha"), a.clone()),
        (workspace_record("beta"), b.clone()),
    ]);
    let skipped = collection
        .retired
        .iter()
        .filter(|routine| routine.name == "graph-refresh")
        .collect::<Vec<_>>();
    assert_eq!(skipped.len(), 1, "only A's copy is skipped: {skipped:?}");
    assert_eq!(skipped[0].source_workspace, "alpha");
    assert!(
        skipped[0]
            .reason
            .contains("switched off in workspace 'alpha'"),
        "{}",
        skipped[0].reason
    );
    assert!(
        collection
            .routines
            .iter()
            .any(|routine| routine.definition.name == "graph-refresh"),
        "B's copy still loads"
    );

    // Dashboard panels.
    assert!(matches!(
        a.plugin_panel_refresh_ms("graph", "status"),
        Err(OrbitError::NotFound { .. })
    ));
    b.plugin_panel_refresh_ms("graph", "status")
        .expect("B still serves the panel");
}

#[test]
fn an_unset_toggle_inherits_the_host_state_and_writes_nothing() {
    let ws = TwoWorkspaces::new();

    for runtime in [ws.runtime_a(), ws.runtime_b()] {
        let summary = show_plugin(&runtime, "graph").expect("show");
        assert_eq!(summary.status, PluginStatus::Active);
        assert_eq!(summary.workspace_toggle, None);
        assert_eq!(summary.disabled_by, None);
    }
    for workspace in [&ws.fixture.workspace_root, &ws.workspace_b] {
        assert!(
            !workspace.join("config.toml").exists(),
            "neither a host enable nor a sync of an enabled pin writes a toggle"
        );
    }
}

#[test]
fn a_workspace_enable_while_the_host_is_disabled_is_refused_and_changes_nothing() {
    let ws = TwoWorkspaces::new();
    disable_plugin(&ws.fixture.runtime, "graph").expect("host disable");

    match enable_plugin_in_workspace(&ws.fixture.runtime, "graph", false) {
        Err(OrbitError::PluginDisabledOnHost { plugin }) => assert_eq!(plugin, "graph"),
        other => panic!("expected the host refusal, got {:?}", other.map(|_| ())),
    }
    assert!(!ws.host_enabled(), "the refusal leaves the host row alone");
    assert!(
        !ws.fixture.workspace_root.join("config.toml").exists(),
        "the refusal writes no toggle"
    );

    let summary = show_plugin(&ws.runtime_a(), "graph").expect("show");
    assert_eq!(summary.disabled_by, Some(PluginDisabledLayer::Host));
    assert!(!summary.host_enabled);
}

#[test]
fn a_workspace_enable_turns_the_plugin_back_on_there() {
    let ws = TwoWorkspaces::new();
    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable in A");

    let result = enable_plugin_in_workspace(&ws.runtime_a(), "graph", false).expect("enable in A");
    assert_eq!(result.summary.status, PluginStatus::Active);
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(true));
    ws.fixture
        .call(&ws.runtime_a(), "graph.hello")
        .expect("A serves the tool again");
}

#[test]
fn workspace_toggles_admit_a_pool_member_defined_only_on_the_host() {
    let ws = TwoWorkspaces::new();
    std::fs::write(
        ws.fixture.global_root.join("config.toml"),
        "[workflow]\ndefault_crew = \"host-only\"\n[crews.host-only]\nmodel = \"gpt-6-sol\"\nprovider = \"codex\"\n",
    )
    .expect("write host crew");
    let config_path = ws.fixture.workspace_root.join("config.toml");
    std::fs::write(
        &config_path,
        "# keep this comment\n[workflow]\nlow_complexity_crews = [\"host-only:100\"]\n",
    )
    .expect("write workspace pool");

    disable_plugin_in_workspace(&ws.runtime_a(), "graph").expect("disable with host-only crew");
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(false));

    enable_plugin_in_workspace(&ws.runtime_a(), "graph", false)
        .expect("enable with host-only crew");
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(true));
    let saved = std::fs::read_to_string(&config_path).expect("read workspace config");
    assert!(saved.contains("# keep this comment"), "{saved}");
    let effective = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        &ws.fixture.global_root,
        &ws.fixture.workspace_root,
    ))
    .expect("saved config remains valid when layered");
    assert_eq!(effective.plugin_enablement.get("graph"), Some(&true));
}

#[test]
fn workspace_toggles_reject_a_pool_member_missing_from_both_layers_without_writing() {
    let ws = TwoWorkspaces::new();
    let config_path = ws.fixture.workspace_root.join("config.toml");
    let original = "[workflow]\nlow_complexity_crews = [\"missing-crew:100\"]\n";
    std::fs::write(&config_path, original).expect("write invalid workspace pool");

    // Use the fixture's already-open runtime: reopening must reject the
    // invalid config before the toggle writer can be reached.
    for result in [
        disable_plugin_in_workspace(&ws.fixture.runtime, "graph").map(|_| ()),
        enable_plugin_in_workspace(&ws.fixture.runtime, "graph", false).map(|_| ()),
    ] {
        let error = result.expect_err("missing crew must be refused");
        assert!(
            error
                .to_string()
                .contains("crew 'missing-crew' is not defined in [crews.*]"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("read unchanged config"),
            original
        );
    }
}

#[test]
fn a_toggle_change_marks_the_cached_runtime_stale() {
    let ws = TwoWorkspaces::new();
    let a = ws.runtime_a();
    assert!(!a.plugin_state_changed().expect("state"));

    disable_plugin_in_workspace(&a, "graph").expect("disable in A");
    assert!(
        a.plugin_state_changed().expect("state"),
        "a long-lived host must rebuild after a toggle write"
    );
    assert!(!ws.runtime_b().plugin_state_changed().expect("state"));
}

#[test]
fn sync_applies_a_disabled_pin_to_this_workspace_only() {
    let ws = TwoWorkspaces::new();
    ws.fixture.write_pin_file(&pin(&ws.source, false));

    let planned = sync_plugins(&ws.fixture.runtime, true, &[]).expect("dry run");
    assert_eq!(planned[0].status, PluginStatus::Active);
    assert!(
        planned[0]
            .message
            .contains("would switch off in this workspace"),
        "{planned:?}"
    );
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), None);

    let outcomes = sync_plugins(&ws.fixture.runtime, false, &[]).expect("sync A");
    assert_eq!(outcomes[0].status, PluginStatus::Disabled);
    assert!(ws.host_enabled(), "a pin never disables the host row");
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(false));
    assert_eq!(workspace_toggle(&ws.workspace_b), None, "B is untouched");
    assert!(ws.runtime_a().show_tool("graph.hello").is_err());
    ws.runtime_b()
        .show_tool("graph.hello")
        .expect("B keeps the tool");

    // Flipping the pin back clears the toggle again.
    ws.fixture.write_pin_file(&pin(&ws.source, true));
    let outcomes = sync_plugins(&ws.runtime_a(), false, &[]).expect("sync A again");
    assert_eq!(outcomes[0].status, PluginStatus::Active, "{outcomes:?}");
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(true));
    assert_eq!(workspace_toggle(&ws.workspace_b), None, "B is untouched");
    ws.runtime_a()
        .show_tool("graph.hello")
        .expect("A serves the tool again");
}

#[test]
fn sync_reopens_a_disabled_host_and_workspace_to_the_final_effective_status() {
    let ws = TwoWorkspaces::new();
    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable A workspace");
    disable_plugin(&ws.fixture.runtime, "graph").expect("disable host");
    ws.fixture.write_pin_file(&pin(&ws.source, true));

    let planned = sync_plugins(&ws.fixture.runtime, true, &[]).expect("dry run");
    assert_eq!(planned[0].status, PluginStatus::Disabled);
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(false));
    assert_eq!(workspace_toggle(&ws.workspace_b), None);

    let outcomes = sync_plugins(&ws.fixture.runtime, false, &[]).expect("sync A");
    let fresh = show_plugin(&ws.runtime_a(), "graph").expect("fresh A status");
    assert_eq!(fresh.status, PluginStatus::Active);
    assert_eq!(outcomes[0].status, fresh.status, "{outcomes:?}");
    assert!(
        outcomes[0]
            .message
            .contains("switched back on in this workspace")
    );
    assert!(ws.host_enabled());
    assert_eq!(workspace_toggle(&ws.fixture.workspace_root), Some(true));
    assert_eq!(
        workspace_toggle(&ws.workspace_b),
        None,
        "B toggle is untouched"
    );
    assert_eq!(
        show_plugin(&ws.runtime_b(), "graph")
            .expect("fresh B status")
            .status,
        PluginStatus::Active
    );
}

#[test]
fn show_and_doctor_report_the_host_state_and_the_workspace_reason() {
    let ws = TwoWorkspaces::new();
    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable in A");

    let a = ws.runtime_a();
    let summary = show_plugin(&a, "graph").expect("show in A");
    assert_eq!(summary.status, PluginStatus::Disabled);
    assert!(summary.host_enabled);
    assert_eq!(summary.workspace_toggle, Some(false));
    assert_eq!(summary.disabled_by, Some(PluginDisabledLayer::Workspace));
    assert!(
        !summary.tools.is_empty() && summary.tools.iter().all(|tool| !tool.active),
        "the tools that are off stay named: {:?}",
        summary.tools
    );

    let row = plugin_doctor(&a)
        .expect("doctor")
        .into_iter()
        .find(|row| row.plugin == "graph" && row.status == PluginStatus::Disabled)
        .expect("a doctor row for graph");
    assert!(
        row.intentional,
        "a workspace toggle is a choice, not a finding"
    );

    let summary_b = show_plugin(&ws.runtime_b(), "graph").expect("show in B");
    assert_eq!(summary_b.status, PluginStatus::Active);
    assert!(
        plugin_doctor(&ws.runtime_b())
            .expect("doctor B")
            .iter()
            .all(|row| !row.intentional)
    );
}
