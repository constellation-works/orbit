//! Workspace-scoped enable toggles: `[plugin_enablement]` narrows the host
//! enable state for one workspace and never touches another.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginDisabledLayer, PluginStatus};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

use super::super::{
    InactivePluginScope, PluginAddOptions, PluginEnableOptions, disable_plugin,
    disable_plugin_in_workspace, enable_plugin, enable_plugin_in_workspace, install_plugin,
    plugin_doctor, show_plugin, sync_plugins,
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
        Self::with_fixture(PluginFixture::new())
    }

    fn with_fixture(fixture: PluginFixture) -> Self {
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

/// Auto-task names a default listing (or, with `include`, an opt-in one) shows.
fn listed_auto_tasks(runtime: &OrbitRuntime, include: bool) -> Vec<String> {
    runtime
        .auto_task_listing(include)
        .expect("list auto-tasks")
        .into_iter()
        .map(|listed| listed.definition.name)
        .collect()
}

/// Routine status as `routine list` reads it for A and B in one pass.
fn routine_report(ws: &TwoWorkspaces) -> crate::application::routines::RoutineStatusReport {
    let collection = crate::application::routines::collect_routines(&[
        (workspace_record("alpha"), ws.runtime_a()),
        (workspace_record("beta"), ws.runtime_b()),
    ]);
    crate::application::routines::RoutineStatusReport {
        machine_name: "host".to_string(),
        machine_id: "hm_test".to_string(),
        statuses: Vec::new(),
        retired: collection.retired,
        load_errors: collection.errors,
    }
}

/// Which workspaces' copies of the seeded routine a listing shows as
/// inactive, and which load as live routines.
fn routine_sources(ws: &TwoWorkspaces, include: bool) -> (Vec<String>, Vec<String>) {
    let collection = crate::application::routines::collect_routines(&[
        (workspace_record("alpha"), ws.runtime_a()),
        (workspace_record("beta"), ws.runtime_b()),
    ]);
    let live = collection
        .routines
        .iter()
        .filter(|routine| routine.definition.name == "graph-refresh")
        .map(|routine| routine.source_workspace.clone())
        .collect();
    let inactive = routine_report(ws)
        .listed_retired(include)
        .filter(|routine| routine.name == "graph-refresh" && routine.skipped)
        .map(|routine| routine.source_workspace.clone())
        .collect();
    (inactive, live)
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "disabling_in_one_workspace_takes_its_whole_surface_off_there_and_leaves_the_other_active",
    ) {
        return;
    }
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

/// A workspace behind a symlinked ancestor is an ordinary setup. Discovery
/// lists resolved paths, so the per-workspace toggle has to match them against
/// a resolved routines directory: otherwise a workspace disable is silently
/// ignored for that workspace's routines and its seeded schedules keep firing.
#[cfg(unix)]
#[test]
fn a_workspace_disable_holds_when_the_workspace_sits_behind_a_symlink() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_workspace_disable_holds_when_the_workspace_sits_behind_a_symlink",
    ) {
        return;
    }
    let ws = TwoWorkspaces::with_fixture(PluginFixture::new_behind_symlink());
    assert!(
        ws.fixture
            .workspace_root
            .starts_with(ws.fixture._root.path().join("linked")),
        "the fixture must keep the symlinked spelling"
    );
    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable in A");
    let a = ws.runtime_a();
    let b = ws.runtime_b();

    let reason = skip_reason(&a).expect("A skips the seeded auto-task");
    assert!(
        reason.contains("switched off in this workspace"),
        "{reason}"
    );
    assert_eq!(skip_reason(&b), None);

    let collection = crate::application::routines::collect_routines(&[
        (workspace_record("alpha"), a),
        (workspace_record("beta"), b),
    ]);
    let skipped = collection
        .retired
        .iter()
        .filter(|routine| routine.name == "graph-refresh")
        .collect::<Vec<_>>();
    assert_eq!(skipped.len(), 1, "only A's copy is skipped: {skipped:?}");
    assert_eq!(skipped[0].source_workspace, "alpha");
}

#[test]
fn listings_hide_a_workspace_disabled_plugins_definitions_there_until_it_is_re_enabled() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "listings_hide_a_workspace_disabled_plugins_definitions_there_until_it_is_re_enabled",
    ) {
        return;
    }
    let ws = TwoWorkspaces::new();
    disable_plugin_in_workspace(&ws.fixture.runtime, "graph").expect("disable in A");
    let a = ws.runtime_a();

    assert!(!listed_auto_tasks(&a, false).contains(&"graph-reindex".to_string()));
    assert!(listed_auto_tasks(&ws.runtime_b(), false).contains(&"graph-reindex".to_string()));
    let shown = a
        .auto_task_listing(true)
        .expect("opt-in listing")
        .into_iter()
        .find(|listed| listed.definition.name == "graph-reindex")
        .expect("the opt-in lists the hidden definition");
    assert_eq!(
        shown.inactive_plugin.map(|inactive| inactive.scope),
        Some(InactivePluginScope::Workspace)
    );
    assert_eq!(
        shown.skipped_reason,
        skip_reason(&a),
        "one reason everywhere"
    );

    let (inactive, live) = routine_sources(&ws, false);
    assert!(inactive.is_empty(), "hidden by default: {inactive:?}");
    assert_eq!(live, vec!["beta".to_string()]);
    let (inactive, _) = routine_sources(&ws, true);
    assert_eq!(inactive, vec!["alpha".to_string()]);

    // Re-enabling needs no re-seed: the files never moved.
    enable_plugin_in_workspace(&ws.runtime_a(), "graph", false).expect("enable in A");
    assert!(listed_auto_tasks(&ws.runtime_a(), false).contains(&"graph-reindex".to_string()));
    assert_no_routine_parked(&ws);
}

/// Neither copy of the seeded routine is parked for its plugin any more. (Two
/// live copies of one name then collide, which is the loader's ordinary
/// host-wide uniqueness rule, not a plugin state.)
fn assert_no_routine_parked(ws: &TwoWorkspaces) {
    let parked = routine_report(ws)
        .inactive_plugin_routines()
        .map(|routine| routine.source_workspace.clone())
        .collect::<Vec<_>>();
    assert!(parked.is_empty(), "{parked:?}");
}

#[test]
fn a_host_disable_hides_the_definitions_in_every_workspace_until_re_enabled() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_host_disable_hides_the_definitions_in_every_workspace_until_re_enabled",
    ) {
        return;
    }
    let ws = TwoWorkspaces::new();
    let seeded = crate::application::auto_tasks::definition_path(
        &ws.fixture.runtime.paths().local_dir,
        "graph-reindex",
    );
    let before = std::fs::read_to_string(&seeded).expect("seeded auto-task");
    disable_plugin(&ws.fixture.runtime, "graph").expect("host disable");

    for runtime in [ws.runtime_a(), ws.runtime_b()] {
        assert!(!listed_auto_tasks(&runtime, false).contains(&"graph-reindex".to_string()));
        let shown = runtime
            .auto_task_listing(true)
            .expect("opt-in listing")
            .into_iter()
            .find(|listed| listed.definition.name == "graph-reindex")
            .expect("listed on request");
        assert_eq!(
            shown.inactive_plugin.map(|inactive| inactive.scope),
            Some(InactivePluginScope::Host)
        );
    }
    let (inactive, live) = routine_sources(&ws, false);
    assert!(
        inactive.is_empty() && live.is_empty(),
        "{inactive:?} {live:?}"
    );
    let (inactive, _) = routine_sources(&ws, true);
    assert_eq!(
        inactive.len(),
        2,
        "both copies listed on request: {inactive:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&seeded).expect("file kept"),
        before,
        "a disable leaves the seeded file exactly where it is"
    );

    enable_plugin(
        &ws.fixture.reopen(),
        "graph",
        &PluginEnableOptions::default(),
    )
    .expect("host enable");
    for runtime in [ws.runtime_a(), ws.runtime_b()] {
        assert!(listed_auto_tasks(&runtime, false).contains(&"graph-reindex".to_string()));
    }
    assert_no_routine_parked(&ws);
}

#[test]
fn an_unset_toggle_inherits_the_host_state_and_writes_nothing() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "an_unset_toggle_inherits_the_host_state_and_writes_nothing",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_workspace_enable_while_the_host_is_disabled_is_refused_and_changes_nothing",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_workspace_enable_turns_the_plugin_back_on_there",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "workspace_toggles_admit_a_pool_member_defined_only_on_the_host",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "workspace_toggles_reject_a_pool_member_missing_from_both_layers_without_writing",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_toggle_change_marks_the_cached_runtime_stale",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "sync_applies_a_disabled_pin_to_this_workspace_only",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "sync_reopens_a_disabled_host_and_workspace_to_the_final_effective_status",
    ) {
        return;
    }
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
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "show_and_doctor_report_the_host_state_and_the_workspace_reason",
    ) {
        return;
    }
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
