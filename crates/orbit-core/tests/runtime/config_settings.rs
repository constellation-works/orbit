//! The Settings config view and crew writes do not depend on the auto-task
//! listing: with every auto-task definition malformed, they still render and
//! commit, and the "Used by" column simply omits auto-task references. Each
//! row also carries its built-in default, removal fallback, equality and,
//! for the global file, its reach into the bound workspace.

use orbit_core::OrbitRuntime;
use orbit_core::application::config::{self, ConfigScope, ConfigWriteInit};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

fn reclaim_config_defaults_replaces_layers_and_refuses_unconfined_patterns() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let roots = orbit_config::ConfigRoots::new(&global, &workspace);
    let load = || orbit_config::ResolvedConfig::load(&roots);
    // [ORB-15613] The default also covers the scratch build directories and
    // base checkouts that leaked 33 GB of kept worktrees on one host.
    assert_eq!(
        load().unwrap().snapshot.worktree_reclaim,
        ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]
    );
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

/// Mid-run steering is off with two rounds unless a layer says otherwise; each
/// key resolves through the layers on its own, and a value outside its range
/// or of the wrong type fails the load naming the key.
fn mid_run_steering_defaults_off_layers_per_key_and_refuses_invalid_values() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let roots = orbit_config::ConfigRoots::new(&global, &workspace);
    let load = || orbit_config::ResolvedConfig::load(&roots);
    let steering = |config: orbit_config::ResolvedConfig| config.snapshot.mid_run_steering();
    assert_eq!(
        steering(load().unwrap()),
        orbit_config::MidRunSteeringSettings {
            enabled: false,
            max_rounds: 2
        }
    );
    std::fs::write(
        global.join("config.toml"),
        "[workflow.mid_run_steering]\nenabled = true\nmax_rounds = 4\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow.mid_run_steering]\nmax_rounds = 1\n",
    )
    .unwrap();
    assert_eq!(
        steering(load().unwrap()),
        orbit_config::MidRunSteeringSettings {
            enabled: true,
            max_rounds: 1
        },
        "the workspace's round budget wins; the global enable still applies"
    );
    for (body, key) in [
        ("max_rounds = 0", "workflow.mid_run_steering.max_rounds"),
        ("max_rounds = 11", "workflow.mid_run_steering.max_rounds"),
        (
            "max_rounds = \"two\"",
            "workflow.mid_run_steering.max_rounds",
        ),
        ("enabled = \"yes\"", "workflow.mid_run_steering.enabled"),
    ] {
        std::fs::write(
            workspace.join("config.toml"),
            format!("[workflow.mid_run_steering]\n{body}\n"),
        )
        .unwrap();
        let error = load().unwrap_err().to_string();
        assert!(error.contains(key), "{body}: {error}");
    }
}

fn settings_and_crew_writes_survive_a_fully_malformed_auto_task_listing() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn global_writes_that_break_the_workspace_layered_load_are_refused_unchanged() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn settings_rows_report_built_in_fallback_equality_and_global_reach() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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
        "[review]\nminutes = 120\ntiming = \"off\"\n\n\
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
    let timing = row(&effective, "review.timing");
    assert_eq!(timing["equals"], json!({"global": null, "built_in": true}));
    assert_eq!(
        timing["fallback"],
        json!({"layer": "built-in", "value": "off", "state": "default"})
    );
    // Removing a workspace security key falls to the built-in value, never the
    // global one, while the workspace file exists.
    let sandbox = row(&effective, "execution.codex.sandbox");
    assert_eq!(sandbox["source"]["layer"], json!("workspace"));
    assert_eq!(sandbox["fallback"]["layer"], json!("built-in"));
    assert_eq!(sandbox["fallback"]["state"], json!("default"));
    assert_ne!(
        sandbox["fallback"]["value"],
        json!("read-only"),
        "a workspace security key falls to the built-in, never the global sandbox"
    );
    assert_ne!(
        sandbox["fallback"]["value"],
        json!("danger-full-access"),
        "the fallback is the built-in, not the workspace override"
    );
    // A global security key the workspace does not restate is not inherited:
    // the built-in value is in force and there is nothing to remove.
    let pass = row(&effective, "execution.env.pass");
    assert_eq!(pass["source"]["layer"], json!("built-in"));
    assert_eq!(pass["fallback"], Value::Null);
    // A global-only key falls back to its built-in default.
    let hours = row(&effective, "ci_failure.operator_suppression_hours");
    assert_eq!(hours["source"]["layer"], json!("global"));
    assert_eq!(hours["fallback"]["layer"], json!("built-in"));
    assert_eq!(hours["fallback"]["state"], json!("default"));
    assert_ne!(
        hours["fallback"]["value"],
        json!(12),
        "the fallback is the built-in, not the global hours"
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
        row(&workspace_view, "review.timing")["equals"],
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

fn settings_fallback_and_equality_use_admitted_global_values() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let crews = "[crews.sol]\nprovider = 'codex'\nmodel = 'sol-model'\n\n\
                 [crews.opus]\nprovider = 'claude'\nmodel = 'opus-model'\n";
    // Keep a policy file after key removal, including a workspace-only crew.
    let remaining_workspace = "[crews.terra]\nprovider = 'codex'\nmodel = 'terra-model'\n";
    for (key, section, global_literal, workspace_literal, raw_global, equal, fallback_layer) in [
        (
            "workflow.purpose_crews.review",
            "workflow.purpose_crews",
            "'sol'",
            "'sol'",
            json!("sol"),
            true,
            "global",
        ),
        (
            "workflow.low_complexity_crews",
            "workflow",
            "['sol', 'opus']",
            "['sol', 'opus']",
            json!(["sol", "opus"]),
            true,
            "global",
        ),
        (
            "workflow.low_complexity_crews",
            "workflow",
            "['sol', 'opus']",
            "['opus', ' sol ', 'sol']",
            json!(["sol", "opus"]),
            true,
            "global",
        ),
        (
            "workflow.low_complexity_crews",
            "workflow",
            "['sol', 'opus']",
            "['sol']",
            json!(["sol", "opus"]),
            false,
            "global",
        ),
        (
            "workflow.low_complexity_crews",
            "workflow",
            "['sol:2', 'opus:3']",
            "['opus:3', 'sol:2']",
            json!(["sol:2", "opus:3"]),
            true,
            "global",
        ),
        (
            "workflow.purpose_crews.review",
            "workflow.purpose_crews",
            "'sol'",
            "'opus'",
            json!("sol"),
            false,
            "global",
        ),
        (
            "execution.env.pass",
            "execution.env",
            "[' B ', 'A', 'A']",
            "['A', 'B']",
            json!([" B ", "A", "A"]),
            true,
            "built-in",
        ),
        (
            "execution.codex.sandbox",
            "execution.codex",
            "' read-only '",
            "'read-only'",
            json!(" read-only "),
            true,
            "built-in",
        ),
    ] {
        let field = key.rsplit('.').next().unwrap();
        std::fs::write(
            global.join("config.toml"),
            format!("[{section}]\n{field} = {global_literal}\n\n{crews}"),
        )
        .unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            format!("[{section}]\n{field} = {workspace_literal}\n\n{remaining_workspace}"),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
        let effective = config::effective_view(&runtime).expect("effective view");
        let effective_row = row(&effective, key);
        let workspace_view =
            config::file_view(&runtime, ConfigScope::Workspace).expect("workspace view");
        let workspace_row = row(&workspace_view, key);
        assert_eq!(
            effective_row["equals"]["global"],
            json!(equal),
            "{key}: {workspace_literal}"
        );
        assert_eq!(
            workspace_row["equals"]["global"],
            json!(equal),
            "{key}: {workspace_literal}"
        );
        assert_eq!(
            effective_row["shadowed_by"][0]["value"], raw_global,
            "raw provenance stays compatible"
        );
        assert_eq!(
            effective_row["shadowed_by"][0]["reason"],
            json!("overridden")
        );
        assert_eq!(workspace_row["fallback"], effective_row["fallback"]);
        assert_eq!(effective_row["fallback"]["layer"], json!(fallback_layer));
        let global_view = config::file_view(&runtime, ConfigScope::Global).expect("global view");
        let reach = &row(&global_view, key)["reach"];
        assert_eq!(reach["state"], json!("overridden"));
        assert_eq!(
            reach["equals_global"],
            json!(equal),
            "{key}: {workspace_literal}"
        );
        assert_eq!(reach["workspace_value"], effective_row["value"]);

        let predicted = effective_row["fallback"]["value"].clone();
        std::fs::write(workspace.join("config.toml"), remaining_workspace).unwrap();
        let reloaded =
            OrbitRuntime::from_roots(&global, &workspace).expect("reload after key removal");
        let after_removal = config::effective_view(&reloaded).expect("view after key removal");
        assert_eq!(
            predicted,
            row(&after_removal, key)["value"],
            "fallback must predict the admitted JSON value and type for {key}"
        );
    }
}

fn settings_global_facts_keep_workspace_admission_context() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[workflow.purpose_crews]\nreview = 'terra'\n\n\
         [workflow]\nbase_branch = ''\n\n\
         [workflow.resource_throttle]\ncpu_resume_percent = 95\n",
    )
    .unwrap();
    let remaining = "[workflow]\nbase_branch = 'agent-main'\ndefault_crew = 'terra'\n\n\
                     [workflow.resource_throttle]\ncpu_high_percent = 99\n\n\
                     [crews.terra]\nprovider = 'codex'\nmodel = 'terra-model'\n";
    std::fs::write(
        workspace.join("config.toml"),
        format!("[workflow.purpose_crews]\nreview = ['terra']\n\n{remaining}"),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    let effective = config::effective_view(&runtime).expect("effective view");
    let review = row(&effective, "workflow.purpose_crews.review");
    assert_eq!(review["equals"]["global"], json!(true));
    assert_eq!(review["fallback"]["value"], json!(["terra"]));
    let workspace_view =
        config::file_view(&runtime, ConfigScope::Workspace).expect("workspace view");
    assert_eq!(
        row(&workspace_view, "workflow.purpose_crews.review")["equals"]["global"],
        json!(true)
    );
    // The overwritten global branch cannot be admitted. Do not claim a
    // successful removal or compare its raw value with an admitted one.
    let branch = row(&effective, "workflow.base_branch");
    assert_eq!(branch["fallback"], Value::Null);
    assert_eq!(branch["equals"]["global"], Value::Null);
    std::fs::write(workspace.join("config.toml"), remaining).unwrap();
    let reloaded = OrbitRuntime::from_roots(&global, &workspace)
        .expect("reload with workspace context retained");
    let after_removal = config::effective_view(&reloaded).expect("view after key removal");
    assert_eq!(
        review["fallback"]["value"],
        row(&after_removal, "workflow.purpose_crews.review")["value"]
    );
}

fn a_plugin_only_workspace_file_leaves_global_security_keys_in_effect() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

fn literal_and_json_key_writes_preserve_their_input_contracts() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let path = workspace.join("config.toml");
    let before = "# retained\n[workflow]\nbase_branch = 'before'\n";
    std::fs::write(&path, before).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let typed = config::set_key(
        &runtime,
        "workflow.base_branch",
        &json!("true"),
        ConfigScope::Workspace,
        ConfigWriteInit::RequireExisting,
    )
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, before).unwrap();
    let literal = config::set_key_literal(
        &runtime,
        "workflow.base_branch",
        "\"true\"",
        ConfigScope::Workspace,
        ConfigWriteInit::RequireExisting,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(literal.old_value, typed.old_value);
    assert_eq!(literal.new_value, typed.new_value);
    assert_eq!(literal.rows, typed.rows);
    // A JSON string stays a string; raw CLI syntax infers a boolean.
    config::set_key(
        &runtime,
        "workflow.auto_ship",
        &json!("true"),
        ConfigScope::Workspace,
        ConfigWriteInit::RequireExisting,
    )
    .unwrap_err();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let written = config::set_key_literal(
        &runtime,
        "workflow.auto_ship",
        "true",
        ConfigScope::Workspace,
        ConfigWriteInit::RequireExisting,
    )
    .unwrap();
    assert_eq!(written.new_value, json!(true));
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    reclaim_config_defaults_replaces_layers_and_refuses_unconfined_patterns_scenarios,
    reclaim_config_defaults_replaces_layers_and_refuses_unconfined_patterns,
    mid_run_steering_defaults_off_layers_per_key_and_refuses_invalid_values,
    settings_and_crew_writes_survive_a_fully_malformed_auto_task_listing,
    global_writes_that_break_the_workspace_layered_load_are_refused_unchanged,
    settings_rows_report_built_in_fallback_equality_and_global_reach,
    settings_fallback_and_equality_use_admitted_global_values,
    settings_global_facts_keep_workspace_admission_context,
    a_plugin_only_workspace_file_leaves_global_security_keys_in_effect,
    literal_and_json_key_writes_preserve_their_input_contracts
);
