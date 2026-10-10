//! The Settings config view and crew writes do not depend on the auto-task
//! listing: with every auto-task definition malformed, they still render and
//! commit, and the "Used by" column simply omits auto-task references.

use orbit_core::OrbitRuntime;
use orbit_core::application::config::{self, ConfigScope, ConfigWriteInit};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

#[test]
fn reclaim_config_defaults_replaces_layers_and_refuses_unconfined_patterns() {
    if !isolated(
        "config_settings::reclaim_config_defaults_replaces_layers_and_refuses_unconfined_patterns",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let roots = orbit_config::ConfigRoots::new(&global, &workspace);
    let load = || orbit_config::ResolvedConfig::load(&roots);
    assert_eq!(load().unwrap().snapshot.worktree_reclaim, ["target"]);
    std::fs::write(
        global.join("config.toml"),
        "[worktree]\nreclaim = ['target', 'node_modules']\nreclaim_below_free_mib = 100\n",
    )
    .unwrap();
    assert_eq!(
        load().unwrap().snapshot.worktree_reclaim,
        ["target", "node_modules"]
    );
    std::fs::write(
        workspace.join("config.toml"),
        "[worktree]\nreclaim = ['.orbit/tmp/*target*', 'website/**/node_modules']\n",
    )
    .unwrap();
    let config = load().unwrap();
    assert_eq!(
        config.snapshot.worktree_reclaim,
        [".orbit/tmp/*target*", "website/**/node_modules"]
    );
    assert_eq!(config.snapshot.worktree_reclaim_below_free_mib, Some(100));
    std::fs::write(workspace.join("config.toml"), "[worktree]\nreclaim = []\n").unwrap();
    assert!(load().unwrap().snapshot.worktree_reclaim.is_empty());
    for pattern in [
        "/home",
        "../target",
        "target/../output",
        ".",
        "",
        "**",
        "**/**",
        "C:/home",
        "\\\\server\\share",
        "**/.",
    ] {
        let config = format!(
            "[worktree]\nreclaim = [{}]\n",
            serde_json::to_string(pattern).unwrap()
        );
        std::fs::write(workspace.join("config.toml"), config).unwrap();
        let error = load().unwrap_err().to_string();
        assert!(error.contains("worktree.reclaim"), "{pattern}: {error}");
    }
    // Invalid declarations in an earlier layer fail even if replaced later.
    std::fs::write(
        global.join("config.toml"),
        "[worktree]\nreclaim = ['/home']\n",
    )
    .unwrap();
    std::fs::write(workspace.join("config.toml"), "[worktree]\nreclaim = []\n").unwrap();
    assert!(load().is_err());
}

#[test]
fn settings_and_crew_writes_survive_a_fully_malformed_auto_task_listing() {
    if !isolated(
        "config_settings::settings_and_crew_writes_survive_a_fully_malformed_auto_task_listing",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\ndefault_crew = \"opus\"\n\
         [crews.opus]\nprovider = \"claude\"\nmodel = \"opus-model\"\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    // Every definition fails to load, so the listing fails closed.
    let definitions = runtime.paths().local_dir.join("auto_tasks");
    std::fs::create_dir_all(&definitions).unwrap();
    std::fs::write(definitions.join("broken.yaml"), "name: [\n").unwrap();
    assert!(
        runtime.auto_task_listing(false).is_err(),
        "the fixture must trip the fail-closed auto-task listing"
    );

    let view = config::effective_view(&runtime)
        .expect("the Settings view renders with every auto-task definition malformed");
    let crews = view["crews"].as_array().expect("crew rows");
    // The folded config references survive; only auto-task references drop.
    let opus_references = crew_named(crews, "opus")["referenced_by"]
        .as_array()
        .unwrap();
    assert!(opus_references.contains(&json!("workflow.default_crew")));
    assert!(
        opus_references
            .iter()
            .all(|reference| !reference.as_str().unwrap().starts_with("auto-task ")),
        "no auto-task reference may appear while the listing fails: {opus_references:?}"
    );

    let fields = fields(json!({"provider": "codex", "model": "sol-model-2"}));
    let outcome = config::set_crew(
        &runtime,
        "sol",
        &fields,
        ConfigScope::Workspace,
        ConfigWriteInit::default(),
    )
    .expect("a multi-field crew write commits and returns its outcome");
    assert_eq!(outcome.new_value["model"], json!("sol-model-2"));

    config::delete_crew(&runtime, "sol", ConfigScope::Workspace)
        .expect("an unreferenced crew deletes");
    let view = config::effective_view(&runtime).expect("the Settings view still renders");
    assert!(
        view["crews"]
            .as_array()
            .unwrap()
            .iter()
            .all(|crew| crew["name"] != json!("sol")),
        "the deleted crew must leave the Settings view"
    );
}

#[test]
fn global_writes_that_break_the_workspace_layered_load_are_refused_unchanged() {
    if !isolated(
        "config_settings::global_writes_that_break_the_workspace_layered_load_are_refused_unchanged",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let global_config = "[workflow]\ndefault_crew = \"sol\"\n\n\
                         [workflow.resource_throttle]\ncpu_resume_percent = 70\n\n\
                         [crews.sol]\nprovider = \"claude\"\nmodel = \"sol-model\"\n\n\
                         [crews.terra]\nprovider = \"codex\"\nmodel = \"terra-model\"\n";
    std::fs::write(global.join("config.toml"), global_config).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\nmedium_complexity_crews = [\"terra\"]\n\n\
         [workflow.resource_throttle]\ncpu_high_percent = 80\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    let roots = orbit_config::ConfigRoots::new(&global, &workspace);
    orbit_config::ResolvedConfig::load(&roots).expect("the layers load before any write");
    let unchanged = || {
        assert_eq!(
            std::fs::read_to_string(global.join("config.toml")).unwrap(),
            global_config,
            "a refused global write leaves the file byte-identical"
        );
        orbit_config::ResolvedConfig::load(&roots).expect("workspace config stays loadable");
    };

    // The resume value is valid in the global file alone, not beside the
    // workspace's high.
    let error = config::set_key(
        &runtime,
        "workflow.resource_throttle.cpu_resume_percent",
        &json!(85),
        ConfigScope::Global,
        ConfigWriteInit::default(),
    )
    .expect_err("a cross-layer throttle conflict is refused")
    .to_string();
    assert!(
        error.contains("cpu_resume_percent must be less than cpu_high_percent"),
        "{error}"
    );
    unchanged();

    // Removing or breaking a crew the workspace pool names.
    config::delete_crew(&runtime, "terra", ConfigScope::Global)
        .expect_err("a global crew the workspace default names cannot be deleted");
    unchanged();
    config::set_crew(
        &runtime,
        "terra",
        &fields(json!({"provider": null})),
        ConfigScope::Global,
        ConfigWriteInit::default(),
    )
    .expect_err("clearing the provider breaks the crew the workspace names");
    unchanged();

    // A write that keeps the layers loadable still lands.
    config::set_key(
        &runtime,
        "workflow.resource_throttle.cpu_resume_percent",
        &json!(60),
        ConfigScope::Global,
        ConfigWriteInit::default(),
    )
    .expect("a compatible global write is admitted");
    orbit_config::ResolvedConfig::load(&roots).expect("still loadable");
}

fn crew_named<'a>(crews: &'a [Value], name: &str) -> &'a Value {
    crews
        .iter()
        .find(|crew| crew["name"] == json!(name))
        .unwrap_or_else(|| panic!("crew '{name}' missing from the Settings view"))
}

fn fields(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}
