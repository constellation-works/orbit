//! The Settings config view and crew writes do not depend on the auto-task
//! listing: with every auto-task definition malformed, they still render and
//! commit, and the "Used by" column simply omits auto-task references. Each
//! row also carries its built-in default, removal fallback, equality and,
//! for the global file, its reach into the bound workspace.

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

#[test]
fn settings_rows_report_built_in_fallback_equality_and_global_reach() {
    if !isolated(
        "config_settings::settings_rows_report_built_in_fallback_equality_and_global_reach",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[review]\nminutes = 45\n\n\
         [workflow]\nauto_ship = true\n\n\
         [ci_failure]\noperator_suppression_hours = 12\n\n\
         [execution.codex]\nsandbox = \"read-only\"\n\n\
         [execution.env]\npass = [\"GLOBAL_TOKEN\"]\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[review]\nminutes = 120\nbefore_pr = false\n\n\
         [workflow]\nauto_ship = true\ndefault_crew = \"sol\"\n\n\
         [execution.codex]\nsandbox = \"danger-full-access\"\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let effective = config::effective_view(&runtime).expect("effective view");
    let built_in_minutes = orbit_config::OperationPolicy::built_in()
        .review_minutes
        .value;
    // A workspace value overriding a different global one falls back to it.
    let minutes = row(&effective, "review.minutes");
    assert_eq!(minutes["source"]["layer"], json!("workspace"));
    assert_eq!(
        minutes["built_in"],
        json!({"state": "default", "value": built_in_minutes}),
        "the row's built-in default agrees with the review policy's"
    );
    assert_eq!(
        minutes["fallback"],
        json!({"layer": "global", "value": 45, "state": "set"})
    );
    assert_eq!(
        minutes["equals"],
        json!({"global": false, "built_in": false})
    );
    // A workspace value that restates global, and one that restates the
    // built-in default the global file never sets.
    assert_eq!(
        row(&effective, "workflow.auto_ship")["equals"],
        json!({"global": true, "built_in": false})
    );
    let before_pr = row(&effective, "review.before_pr");
    assert_eq!(
        before_pr["equals"],
        json!({"global": null, "built_in": true})
    );
    assert_eq!(
        before_pr["fallback"],
        json!({"layer": "built-in", "value": false, "state": "default"})
    );
    // Removing a workspace security key falls to the built-in value, never the
    // global one, while the workspace file exists.
    let sandbox = row(&effective, "execution.codex.sandbox");
    assert_eq!(sandbox["source"]["layer"], json!("workspace"));
    assert_eq!(
        sandbox["fallback"],
        json!({"layer": "built-in", "value": "workspace-write", "state": "default"})
    );
    // A global security key the workspace does not restate is not inherited:
    // the built-in value is in force and there is nothing to remove.
    let pass = row(&effective, "execution.env.pass");
    assert_eq!(pass["source"]["layer"], json!("built-in"));
    assert_eq!(pass["fallback"], Value::Null);
    // A global-only key falls back to its built-in default.
    let hours = row(&effective, "ci_failure.operator_suppression_hours");
    assert_eq!(hours["source"]["layer"], json!("global"));
    assert_eq!(
        hours["fallback"],
        json!({"layer": "built-in", "value": 6, "state": "default"})
    );
    assert_eq!(hours["equals"], json!({"global": true, "built_in": false}));
    // A key with no built-in value says so rather than reporting a value.
    let approval = row(&effective, "execution.codex.approval_policy");
    assert_eq!(approval["state"], json!("unset"));
    assert_eq!(
        approval["built_in"],
        json!({"state": "unset", "value": null})
    );
    assert_eq!(approval["fallback"], Value::Null);
    assert_eq!(effective["crews_enumerated"], json!(true));
    // A row says why `orbit config set` refuses it and whether only the
    // global file may carry it, matching what a write would answer.
    let machine_id = row(&effective, "machine.id");
    assert_eq!(machine_id["settable"], json!(false));
    assert!(
        machine_id["read_only_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit init")),
        "a read-only key carries its refusal note: {machine_id}"
    );
    let machine_name = row(&effective, "machine.name");
    assert_eq!(machine_name["global_only"], json!(true));
    assert_eq!(machine_name["read_only_reason"], Value::Null);
    let workspace_write = config::set_key(
        &runtime,
        "machine.name",
        &json!("renamed"),
        ConfigScope::Workspace,
        ConfigWriteInit::RequireExisting,
    );
    assert!(
        workspace_write.is_err(),
        "a global-only row refuses a workspace write"
    );
    assert_eq!(minutes["global_only"], json!(false));
    assert_eq!(minutes["read_only_reason"], Value::Null);

    let global_view = config::file_view(&runtime, ConfigScope::Global).expect("global view");
    assert_eq!(
        row(&global_view, "review.minutes")["reach"],
        json!({"state": "overridden", "workspace_value": 120, "equals_global": false})
    );
    assert_eq!(
        row(&global_view, "workflow.auto_ship")["reach"],
        json!({"state": "overridden", "workspace_value": true, "equals_global": true})
    );
    assert_eq!(
        row(&global_view, "ci_failure.operator_suppression_hours")["reach"],
        json!({"state": "in-effect"})
    );
    assert_eq!(
        row(&global_view, "execution.env.pass")["reach"],
        json!({"state": "not-inherited"})
    );
    let unset = row(&global_view, "execution.codex.approval_policy");
    assert_eq!(
        unset["reach"],
        Value::Null,
        "a key the file does not set reaches nothing"
    );
    assert_eq!(unset["built_in"], json!({"state": "unset", "value": null}));
    assert_eq!(global_view["crews_enumerated"], json!(false));
    assert_eq!(global_view["crew_tables"], json!([]));

    let workspace_view =
        config::file_view(&runtime, ConfigScope::Workspace).expect("workspace view");
    let minutes = row(&workspace_view, "review.minutes");
    assert_eq!(
        minutes["fallback"],
        json!({"layer": "global", "value": 45, "state": "set"})
    );
    assert_eq!(
        minutes["equals"],
        json!({"global": false, "built_in": false})
    );
    assert_eq!(
        row(&workspace_view, "execution.codex.sandbox")["fallback"]["layer"],
        json!("built-in")
    );
    assert_eq!(
        row(&workspace_view, "review.before_pr")["equals"],
        json!({"global": null, "built_in": true})
    );
    assert_eq!(
        row(&workspace_view, "workflow.auto_ship")["equals"],
        json!({"global": true, "built_in": false})
    );
    assert_eq!(workspace_view["crews_enumerated"], json!(false));
    assert_eq!(
        workspace_view["crew_tables"],
        json!(["sol"]),
        "an empty `crews` list must not read as no crew defined"
    );
    assert_eq!(
        workspace_view["layers"]["execution_not_inherited"],
        json!(true)
    );
}

#[test]
fn a_plugin_only_workspace_file_leaves_global_security_keys_in_effect() {
    if !isolated(
        "config_settings::a_plugin_only_workspace_file_leaves_global_security_keys_in_effect",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[execution.env]\npass = [\"GLOBAL_TOKEN\"]\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[plugin_enablement]\nexample = false\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let pass = row(
        &config::effective_view(&runtime).expect("effective view"),
        "execution.env.pass",
    )
    .clone();
    assert_eq!(pass["source"]["layer"], json!("global"));
    assert_eq!(pass["fallback"]["layer"], json!("built-in"));
    let global_view = config::file_view(&runtime, ConfigScope::Global).expect("global view");
    assert_eq!(
        row(&global_view, "execution.env.pass")["reach"],
        json!({"state": "in-effect"})
    );
    let workspace_view =
        config::file_view(&runtime, ConfigScope::Workspace).expect("workspace view");
    assert_eq!(
        workspace_view["layers"]["execution_not_inherited"],
        json!(false),
        "a file holding only plugin toggles is not a policy layer"
    );
}

/// One key's row from a Settings view's sections.
fn row<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| section["keys"].as_array().unwrap())
        .find(|row| row["key"] == json!(key))
        .unwrap_or_else(|| panic!("'{key}' missing from the Settings view"))
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
