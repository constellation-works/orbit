//  Public review and machine config admission, provenance and migration.
//  `review.timing` [ORB-16775] is the one review layer before landing; the
//  deprecated `review.before_pr` and `review.before_landing` booleans still
//  load into it with the per-key layering they had.

fn review_layers_write_config(root: &Path, body: &str) {
    std::fs::write(root.join("config.toml"), body).expect("write config");
}

#[test]
fn review_timing_resolves_built_in_then_global_then_workspace_with_its_layer() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let load = || {
        ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
            .expect("config loads")
            .operation
    };

    review_layers_write_config(workspace.path(), "[workflow]\nauto_ship = true\n");
    let built_in = load();
    assert!(!built_in.review_before_landing.value && !built_in.review_before_pr.value);
    assert_eq!(
        built_in.review_before_landing.source,
        OperationLayerSource::BuiltIn
    );

    review_layers_write_config(global.path(), "[review]\ntiming = \"before_landing\"\n");
    let from_global = load();
    assert!(from_global.review_before_landing.value && !from_global.review_before_pr.value);
    assert_eq!(
        from_global.review_before_landing.source,
        OperationLayerSource::Global
    );

    // A workspace timing replaces the global one whole: `before_pr` here
    // never meets the global `before_landing`.
    review_layers_write_config(workspace.path(), "[review]\ntiming = \"before_pr\"\n");
    let from_workspace = load();
    assert!(from_workspace.review_before_pr.value && !from_workspace.review_before_landing.value);
    assert_eq!(
        from_workspace.review_before_pr.source,
        OperationLayerSource::Workspace
    );
    admit_settable_config_key("review.timing").expect("live key");
}

/// The deprecated booleans keep their per-key layering through the alias: a
/// workspace that states one inherits the other from the global file, so
/// two switches on across the layers still fail the load.
#[test]
fn deprecated_review_booleans_layer_per_key_and_both_on_fail_the_load() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    review_layers_write_config(global.path(), "[review]\nbefore_landing = true\n");
    review_layers_write_config(workspace.path(), "[review]\nbefore_pr = false\n");
    let policy = ResolvedConfig::load(&roots)
        .expect("config loads")
        .operation;
    assert!(policy.review_before_landing.value && !policy.review_before_pr.value);

    review_layers_write_config(global.path(), "[review]\nbefore_pr = true\n");
    review_layers_write_config(workspace.path(), "[review]\nbefore_landing = true\n");
    let error = ResolvedConfig::load(&roots)
        .expect_err("two review layers before landing are refused")
        .to_string();
    assert!(
        error.contains("review.before_pr")
            && error.contains("review.before_landing")
            && error.contains("review.timing"),
        "{error}"
    );

    // In one file, as `orbit config set` validates an edit.
    review_layers_write_config(global.path(), "");
    review_layers_write_config(
        workspace.path(),
        "[review]\nbefore_pr = true\nbefore_landing = true\n",
    );
    let error = ResolvedConfig::load(&roots)
        .expect_err("refused in one file too")
        .to_string();
    assert!(
        error.contains("review.before_pr") && error.contains("review.before_landing"),
        "{error}"
    );
}

/// [ORB-15192] `review.before_landing_hosts` resolves through the layers
/// trimmed and deduplicated, refuses a label that is not a machine id, and
/// fails the load beside `review.timing = before_pr`, naming both keys: a
/// listed host's claims would otherwise get two review layers before landing.
#[test]
fn before_landing_hosts_resolve_validated_and_are_refused_beside_before_pr() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    review_layers_write_config(
        workspace.path(),
        "[review]\nbefore_landing_hosts = [\" hm_mac \", \"hm_mac\", \"hm_linux-2\"]\n",
    );
    let policy = ResolvedConfig::load(&roots)
        .expect("config loads")
        .operation;
    let hosts = &policy.review_before_landing_hosts;
    assert_eq!(hosts.value, ["hm_mac", "hm_linux-2"]);
    assert_eq!(hosts.source, OperationLayerSource::Workspace);
    assert!(policy.reviews_before_landing_for(Some("hm_mac")));
    assert!(!policy.reviews_before_landing_for(Some("hm_other")));
    assert!(!policy.reviews_before_landing_for(None));
    admit_settable_config_key("review.before_landing_hosts").expect("live key");

    review_layers_write_config(
        workspace.path(),
        "[review]\nbefore_landing_hosts = [\"mac\"]\n",
    );
    let error = ResolvedConfig::load(&roots)
        .expect_err("a label that is not a machine id is refused")
        .to_string();
    assert!(error.contains("review.before_landing_hosts[0]"), "{error}");

    review_layers_write_config(global.path(), "[review]\ntiming = \"before_pr\"\n");
    review_layers_write_config(
        workspace.path(),
        "[review]\nbefore_landing_hosts = [\"hm_mac\"]\n",
    );
    let error = ResolvedConfig::load(&roots)
        .expect_err("before-PR review beside listed hosts is refused")
        .to_string();
    assert!(
        error.contains("review.timing is before_pr (global)")
            && error.contains("review.before_landing_hosts (workspace)"),
        "{error}"
    );

    // An empty list states nothing to conflict with.
    review_layers_write_config(workspace.path(), "[review]\nbefore_landing_hosts = []\n");
    ResolvedConfig::load(&roots).expect("before-PR review with no listed hosts loads");
}

/// A global `review.timing` edit that a workspace's deprecated boolean would
/// combine into two layers is refused before it is saved, so the workspace
/// keeps loading.
#[test]
fn global_set_that_would_turn_on_both_review_layers_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");

    // Each pairing: the workspace layer already has one switch on, and the
    // global edit turns on the other.
    for (workspace_key, global_timing) in [
        ("before_pr", "before_landing"),
        ("before_landing", "before_pr"),
    ] {
        review_layers_write_config(
            workspace.path(),
            &format!("[review]\n{workspace_key} = true\n"),
        );
        review_layers_write_config(global.path(), "");

        let mut store =
            ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
        store
            .set_value("review.timing", global_timing)
            .expect("stage global set");
        let error = store
            .validate_global_for_set("review.timing", workspace.path())
            .expect_err("a global set that conflicts with the workspace layer is refused")
            .to_string();
        assert!(
            error.contains("review.before_pr") && error.contains("review.before_landing"),
            "{error}"
        );

        // Refused before `save`, so the workspace still loads.
        assert_eq!(
            std::fs::read_to_string(&global_path).expect("read global"),
            ""
        );
        ResolvedConfig::load(&roots).expect("workspace config stays loadable");

        // A workspace that states the other switch off is not a conflict.
        review_layers_write_config(
            workspace.path(),
            "[review]\nbefore_pr = false\nbefore_landing = false\n",
        );
        store
            .validate_global_for_set("review.timing", workspace.path())
            .expect("no workspace switch is on");
    }
}

#[test]
fn global_set_that_breaks_a_cross_layer_throttle_rule_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");
    let original = "[workflow.resource_throttle]\ncpu_resume_percent = 70\n";
    review_layers_write_config(global.path(), original);
    review_layers_write_config(
        workspace.path(),
        "[workflow.resource_throttle]\ncpu_high_percent = 80\n",
    );
    ResolvedConfig::load(&roots).expect("the layers load before the edit");

    // Valid in the global file alone, but not beside the workspace's high.
    let key = "workflow.resource_throttle.cpu_resume_percent";
    let mut store =
        ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
    store.set_value(key, "85").expect("stage global set");
    store
        .validate_for_set(key)
        .expect("the global file alone admits the value");
    let error = store
        .validate_global_for_set(key, workspace.path())
        .expect_err("a global set that breaks the workspace's layered load is refused")
        .to_string();
    assert!(
        error.contains("cpu_resume_percent must be less than cpu_high_percent"),
        "{error}"
    );
    store
        .validate_global_with_workspace(workspace.path())
        .expect_err("the layered check alone refuses it too");

    // Refused before `save`: the global file is untouched and the workspace loads.
    assert_eq!(
        std::fs::read_to_string(&global_path).expect("read global"),
        original
    );
    ResolvedConfig::load(&roots).expect("workspace config stays loadable");

    // A value below the workspace's high is admitted and saves.
    store.set_value(key, "60").expect("stage a lower value");
    store
        .validate_global_for_set(key, workspace.path())
        .expect("resume below the workspace high is admitted");
    store.save().expect("save");
    ResolvedConfig::load(&roots).expect("still loadable after the admitted write");
}

#[test]
fn global_edit_that_removes_a_crew_a_workspace_names_is_refused_before_saving() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let global_path = global.path().join("config.toml");
    let original = "[crews.terra]\nmodel = \"m\"\nprovider = \"codex\"\n";
    review_layers_write_config(global.path(), original);
    review_layers_write_config(workspace.path(), "[workflow]\ndefault_crew = \"terra\"\n");
    ResolvedConfig::load(&roots).expect("the workspace names a global crew");

    let mut store =
        ConfigStore::open(ConfigScope::Global, global_path.clone()).expect("open global store");
    assert!(
        store
            .remove_crew_table("terra")
            .expect("stage crew removal")
    );
    store
        .validate()
        .expect("the global file alone has no crew to miss");
    store
        .validate_global_with_workspace(workspace.path())
        .expect_err("removing the crew the workspace default names is refused");

    assert_eq!(
        std::fs::read_to_string(&global_path).expect("read global"),
        original
    );
    ResolvedConfig::load(&roots).expect("workspace config stays loadable");
}

#[test]
fn global_validation_with_one_pinned_root_has_no_workspace_layer_to_check() {
    let root = tempfile::tempdir().expect("root tempdir");
    let global_path = root.path().join("config.toml");
    review_layers_write_config(root.path(), "");
    let mut store = ConfigStore::open(ConfigScope::Global, global_path).expect("open global store");
    store
        .set_value("workflow.resource_throttle.cpu_resume_percent", "85")
        .expect("stage global set");
    store
        .validate_global_with_workspace(root.path())
        .expect("one pinned root has no second layer");
}

/// [ORB-15195] `operation.review_crew` is one crew or a pool in the
/// complexity-pool grammar. A string loads as a one-member pool, a pool loads
/// canonical (trimmed, sorted), an empty array unsets the key even over a global value, and a
/// crew the registry does not define fails the load naming the entry.
#[test]
fn review_crew_loads_a_crew_or_a_pool_and_fails_on_an_unknown_member() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let load = || ResolvedConfig::load(&roots);

    review_layers_write_config(workspace.path(), "[operation]\nreview_crew = \"grok\"\n");
    let single = load().expect("a single crew still loads");
    assert_eq!(single.operation.review_crew.value, vec!["grok".to_string()]);
    assert_eq!(
        single.operation.review_crew.source,
        OperationLayerSource::Workspace
    );

    review_layers_write_config(
        workspace.path(),
        "[workflow.purpose_crews]\nreview = [\" sol \", \"grok\"]\n",
    );
    let pool = load().expect("a bare pool loads");
    assert_eq!(
        pool.operation.review_crew.value,
        vec!["grok".to_string(), "sol".to_string()]
    );
    assert_eq!(
        pool.snapshot.workflow_purpose_crews_review.as_ref(),
        Some(&pool.operation.review_crew.value)
    );

    review_layers_write_config(
        workspace.path(),
        "[workflow.purpose_crews]\nreview = [\"sol:3\", \"grok:1\"]\n",
    );
    assert_eq!(
        load()
            .expect("a weighted pool loads")
            .operation
            .review_crew
            .value,
        vec!["grok:1".to_string(), "sol:3".to_string()]
    );

    review_layers_write_config(
        workspace.path(),
        "[workflow.purpose_crews]\nreview = [\"sol:3\", \"grok\"]\n",
    );
    load().expect_err("bare and weighted entries do not mix");

    review_layers_write_config(
        global.path(),
        "[workflow.purpose_crews]\nreview = \"sol\"\n",
    );
    review_layers_write_config(workspace.path(), "[workflow.purpose_crews]\nreview = []\n");
    let unset = load().expect("an empty pool loads");
    assert!(unset.operation.review_crew.value.is_empty());
    assert_eq!(
        unset.operation.review_crew.source,
        OperationLayerSource::Workspace
    );

    review_layers_write_config(
        workspace.path(),
        "[workflow.purpose_crews]\nreview = [\"sol\", \"no-such-crew\"]\n",
    );
    let error = load()
        .expect_err("an unknown pool member fails the load")
        .to_string();
    assert!(
        error.contains("workflow.purpose_crews.review") && error.contains("no-such-crew"),
        "{error}"
    );
    review_layers_write_config(
        workspace.path(),
        "[workflow.purpose_crews]\nreview = \"no-such-crew\"\n",
    );
    let error = load()
        .expect_err("an unknown single crew fails the load")
        .to_string();
    assert!(error.contains("no-such-crew"), "{error}");
}

#[test]
fn machine_auto_concurrency_global_values_are_admitted() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    review_layers_write_config(
        global.path(),
        "[workflow.resource_throttle]\ncpu_high_percent = 2\ncpu_resume_percent = 1\nmemory_high_percent = 80\nmemory_resume_percent = 70\n\
         [machine.auto_concurrency]\nmode = \"shadow\"\nadmit_cpu_percent = 100\nadmit_memory_percent = 79\nprofile_window = 20\nmin_profile_deliveries = 10\nhorizon_cap_minutes = 90\nhead_max_wait_minutes = 5\nhead_stuck_passes = 7\n",
    );
    let configured = ResolvedConfig::load(&roots)
        .expect("global settings load with CPU utilization independent of load-based throttle");
    assert_eq!(
        configured.auto_concurrency,
        AutoConcurrencySettings {
            mode: "shadow".to_string(),
            admit_cpu_percent: 100,
            admit_memory_percent: 79,
            profile_window: 20,
            min_profile_deliveries: 10,
            horizon_cap_minutes: 90,
            head_max_wait_minutes: 5,
            head_stuck_passes: 7,
        }
    );
}

#[test]
fn machine_auto_concurrency_rejects_invalid_values_with_the_key_named() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    for (config, expected_key) in [
        (
            "[machine.auto_concurrency]\nadmit_cpu_percent = 0\n",
            "machine.auto_concurrency.admit_cpu_percent",
        ),
        (
            "[machine.auto_concurrency]\nadmit_memory_percent = 101\n",
            "machine.auto_concurrency.admit_memory_percent",
        ),
        (
            "[workflow.resource_throttle]\nmemory_high_percent = 80\nmemory_resume_percent = 70\n\
             [machine.auto_concurrency]\nadmit_memory_percent = 80\n",
            "machine.auto_concurrency.admit_memory_percent",
        ),
        (
            "[machine.auto_concurrency]\nprofile_window = 9\nmin_profile_deliveries = 10\n",
            "machine.auto_concurrency.profile_window",
        ),
        (
            "[machine.auto_concurrency]\nmode = \"automatic\"\n",
            "machine.auto_concurrency.mode",
        ),
    ] {
        review_layers_write_config(global.path(), config);
        let error = ResolvedConfig::load(&roots)
            .expect_err("invalid host auto-concurrency settings are refused")
            .to_string();
        assert!(error.contains(expected_key), "{expected_key}: {error}");
    }
}

#[test]
fn machine_auto_concurrency_compares_memory_with_global_throttle_threshold() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());

    review_layers_write_config(
        global.path(),
        "[workflow.resource_throttle]\nmemory_high_percent = 80\nmemory_resume_percent = 70\n\
         [machine.auto_concurrency]\nadmit_memory_percent = 75\n",
    );
    review_layers_write_config(
        workspace.path(),
        "[workflow.resource_throttle]\nmemory_high_percent = 70\nmemory_resume_percent = 60\n",
    );
    ResolvedConfig::load(&roots)
        .expect("a workspace throttle override does not replace the host threshold");

    review_layers_write_config(
        global.path(),
        "[workflow.resource_throttle]\nmemory_high_percent = 70\nmemory_resume_percent = 60\n\
         [machine.auto_concurrency]\nadmit_memory_percent = 75\n",
    );
    review_layers_write_config(
        workspace.path(),
        "[workflow.resource_throttle]\nmemory_high_percent = 90\nmemory_resume_percent = 80\n",
    );
    let error = ResolvedConfig::load(&roots)
        .expect_err("a workspace cannot raise the global memory admission ceiling")
        .to_string();
    assert!(
        error.contains("machine.auto_concurrency.admit_memory_percent"),
        "{error}"
    );
}

#[path = "review_layers/operation.rs"]
mod operation;

#[path = "review_layers/machine.rs"]
mod machine;

#[path = "review_layers/deprecated_aliases.rs"]
mod deprecated_aliases;

#[test]
fn build_output_bindings_default_empty_and_workspace_replaces_global() {
    let global = tempfile::tempdir().expect("global");
    let workspace = tempfile::tempdir().expect("workspace");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let load = || {
        ResolvedConfig::load(&roots)
            .expect("config loads")
            .snapshot
            .workflow_validation_env_build_output_env
    };
    assert!(load().is_empty());
    review_layers_write_config(
        global.path(),
        "[workflow.validation_env]\nbuild_output_env = ['GLOBAL_OUTPUT']\n",
    );
    assert_eq!(load(), ["GLOBAL_OUTPUT"]);
    review_layers_write_config(
        workspace.path(),
        "[workflow.validation_env]\nbuild_output_env = [' DIST_DIR ', 'DIST_DIR']\n",
    );
    assert_eq!(load(), ["DIST_DIR"]);
    review_layers_write_config(
        workspace.path(),
        "[workflow.validation_env]\nbuild_output_env = []\n",
    );
    assert!(load().is_empty(), "explicit empty overrides global");
    admit_settable_config_key("workflow.validation_env.build_output_env").expect("live key");
    for name in [
        "BAD-NAME",
        "PATH",
        "Path",
        "HOME",
        "ORBIT_VALIDATION_SUMMARY",
        "orbit_summary",
        "",
    ] {
        review_layers_write_config(
            workspace.path(),
            &format!("[workflow.validation_env]\nbuild_output_env = ['{name}']\n"),
        );
        assert!(
            ResolvedConfig::load(&roots).is_err(),
            "invalid locator {name}"
        );
    }
}

#[test]
fn cargo_incremental_defaults_off_and_a_workspace_can_opt_in() {
    let global = tempfile::tempdir().expect("global");
    let workspace = tempfile::tempdir().expect("workspace");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let load = || {
        ResolvedConfig::load(&roots)
            .expect("config loads")
            .snapshot
            .workflow_validation_env_cargo_incremental
    };
    assert!(!load());
    review_layers_write_config(
        global.path(),
        "[workflow.validation_env]\ncargo_incremental = true\n",
    );
    assert!(load());
    review_layers_write_config(
        workspace.path(),
        "[workflow.validation_env]\ncargo_incremental = false\n",
    );
    assert!(
        !load(),
        "an explicit workspace false replaces a global opt-in"
    );
    admit_settable_config_key("workflow.validation_env.cargo_incremental").expect("live key");
}
