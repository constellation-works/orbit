use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::plugin::PluginStatus;

use super::super::{
    PluginAddOptions, PluginEnableOptions, PluginMigrateRequest, PluginRemoveOptions,
    PluginSeedAction, disable_plugin, enable_plugin, install_plugin, list_plugins,
    migrate_plugin_sidecars, plugin_doctor, remove_plugin, show_plugin, sync_plugins,
    validate_plugin_dir,
};
use super::definition_fixture::DefinitionPlugin;
use super::fixture::{PluginFixture, PluginSpecFixture};

#[test]
fn sync_refuses_unsafe_git_pin_entries() {
    for source in [
        "git+ext::sh -c 'exit 0' %S",
        "git+file:///tmp/plugin",
        "git+-uplugin",
        "git+https://example.com/demo.git#-upload-pack=payload",
    ] {
        let fixture = PluginFixture::new();
        fixture.write_pin_file(&format!(
            "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {source:?}\n    enabled: true\n"
        ));

        let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync continues");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, PluginStatus::Missing);
        assert!(
            outcomes[0].message.contains(source),
            "sync refusal must name pin entry {source:?}: {outcomes:?}"
        );
    }
}

#[test]
fn sync_installs_what_the_pin_file_names_and_reports_what_it_cannot() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    version: 1.x\n    source: {}\n    enabled: true\n  - name: absent\n    enabled: true\n",
        source.display()
    ));

    let planned = sync_plugins(&fixture.runtime, true, &[]).expect("dry run");
    assert_eq!(planned.len(), 2);
    assert!(
        planned[0].message.starts_with("would install"),
        "{planned:?}"
    );

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync");
    assert_eq!(outcomes[0].status, PluginStatus::Active);
    assert!(
        outcomes[0].message.contains("installed v1.0.0"),
        "{outcomes:?}"
    );
    assert_eq!(outcomes[1].status, PluginStatus::Missing);
    assert!(
        outcomes[1].message.contains("names no `source`"),
        "{outcomes:?}"
    );

    let runtime = fixture.reopen();
    runtime
        .show_tool("demo.hello")
        .expect("synced plugin registers its tool");
}

#[test]
fn sync_disables_an_enabled_plugin_when_the_workspace_pin_is_disabled() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install enabled plugin");
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {}\n    enabled: false\n",
        source.display()
    ));

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync");
    assert_eq!(outcomes[0].status, PluginStatus::Disabled);
    assert!(outcomes[0].message.contains("disabled by workspace pin"));
    assert!(
        fixture.reopen().show_tool("demo.hello").is_err(),
        "the disabled pin must take the plugin off the next runtime's surface"
    );
}

#[test]
fn sync_refuses_a_source_namespace_that_differs_from_the_pin_on_every_run() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("source", "actual"));
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: pinned\n    source: {}\n    enabled: true\n",
        source.display()
    ));

    for attempt in 1..=2 {
        let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync continues");
        let message = &outcomes[0].message;
        assert!(
            outcomes[0].status == PluginStatus::Missing
                && message.contains("namespace 'actual'")
                && message.contains("requested name is 'pinned'"),
            "attempt {attempt} must refuse with both names: {outcomes:?}"
        );
        assert!(
            fixture
                .runtime
                .stores()
                .plugins()
                .get_plugin("actual")
                .expect("read actual row")
                .is_none(),
            "a namespace mismatch must not install under the manifest name"
        );
    }
}

#[test]
fn sync_refuses_an_out_of_range_source_without_side_effects_and_continues() {
    let fixture = PluginFixture::new();
    let mismatching = DefinitionPlugin::new("graph")
        .with_version("2.0.0")
        .write(&fixture);
    let matching = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: graph\n    version: 1.x\n    source: {}\n    enabled: true\n  - name: demo\n    version: ^1.0.0\n    source: {}\n    enabled: true\n",
        mismatching.display(),
        matching.display()
    ));

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync continues");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].status, PluginStatus::Missing);
    assert!(
        outcomes[0].message.contains("plugin 'graph' v2.0.0")
            && outcomes[0]
                .message
                .contains("pinned version requirement '1.x'"),
        "version refusal must name the source and requirement: {outcomes:?}"
    );
    assert_eq!(outcomes[1].status, PluginStatus::Active);

    assert!(
        fixture
            .runtime
            .stores()
            .plugins()
            .get_plugin("graph")
            .expect("read graph row")
            .is_none(),
        "a version mismatch must not create a host row"
    );
    assert!(
        !fixture.global_root.join("plugins/graph").exists(),
        "a version mismatch must not create an install tree"
    );
    assert!(
        !fixture
            .workspace_root
            .join("routines/graph-refresh.yaml")
            .exists()
            && !fixture
                .workspace_root
                .join("auto_tasks/graph-reindex.yaml")
                .exists(),
        "a version mismatch must not seed workspace definitions"
    );
    fixture
        .reopen()
        .show_tool("demo.hello")
        .expect("an unrelated matching pin still syncs");
}

#[test]
fn sync_reconciles_enabled_contributions_into_each_workspace() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: graph\n    source: {}\n    enabled: true\n",
        source.display()
    ));
    sync_plugins(&fixture.runtime, false, &[]).expect("sync workspace A");

    let workspace_b = fixture.repo_root.join("workspace-b/.orbit");
    std::fs::create_dir_all(&workspace_b).expect("create workspace B");
    std::fs::write(
        workspace_b.join("plugins.yaml"),
        format!(
            "schemaVersion: 1\nplugins:\n  - name: graph\n    source: {}\n    enabled: true\n",
            source.display()
        ),
    )
    .expect("pin plugin in workspace B");
    let runtime_b = crate::OrbitRuntime::from_roots(&fixture.global_root, &workspace_b)
        .expect("build workspace B runtime");

    let first = sync_plugins(&runtime_b, false, &[]).expect("sync workspace B");
    assert!(
        workspace_b.join("routines/graph-refresh.yaml").is_file()
            && workspace_b.join("auto_tasks/graph-reindex.yaml").is_file(),
        "an already-enabled host plugin must seed the current workspace"
    );
    assert!(first[0].message.contains("created"), "{first:?}");

    let second = sync_plugins(&runtime_b, false, &[]).expect("repeat workspace B sync");
    assert!(
        second[0]
            .message
            .contains("routine graph-refresh unchanged")
            && second[0]
                .message
                .contains("auto_task graph-reindex unchanged"),
        "unchanged contributions must remain visible: {second:?}"
    );
}

#[test]
fn sync_does_not_enable_a_grant_requesting_plugin_without_consent() {
    let fixture = PluginFixture::new();
    let source =
        fixture.write_plugin(PluginSpecFixture::new("guarded", "guarded").requesting_fs_write());
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: guarded\n    source: {}\n    enabled: true\n",
        source.display()
    ));

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync");
    assert_eq!(outcomes[0].status, PluginStatus::Disabled);
    assert!(
        outcomes[0].message.contains("requests fs")
            && outcomes[0].message.contains("plugin sync --grant fs"),
        "the refusal must name the requested consent: {outcomes:?}"
    );
    let installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("guarded")
        .expect("read plugin row")
        .expect("plugin was installed");
    assert!(!installed.enabled, "the committed pin is not grant consent");

    let consented = sync_plugins(&fixture.runtime, false, &["fs".to_string()])
        .expect("sync with explicit consent");
    assert_eq!(consented[0].status, PluginStatus::Active);
    let installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("guarded")
        .expect("read enabled row")
        .expect("plugin remains installed");
    assert!(installed.enabled);
    assert_eq!(installed.grants, ["fs"]);
}

#[test]
fn doctor_reports_seeded_definitions_older_than_the_installed_plugin() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install and seed");
    let routine = fixture.workspace_root.join("routines/graph-refresh.yaml");
    let raw = std::fs::read_to_string(&routine).expect("read seeded routine");
    std::fs::write(
        &routine,
        raw.replace("plugin:graph@1.0.0", "plugin:graph@0.9.0"),
    )
    .expect("make provenance stale");

    let findings = plugin_doctor(&fixture.reopen()).expect("doctor");
    assert!(
        findings.iter().any(|finding| {
            finding.plugin == "graph"
                && finding
                    .message
                    .contains("lag installed plugin 'graph' v1.0.0")
                && finding.message.contains("graph-refresh.yaml (v0.9.0)")
        }),
        "doctor must report stale workspace provenance: {findings:?}"
    );
}

#[test]
fn disable_and_remove_take_the_plugin_off_the_surface() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
    let install_path = summary.install_path.clone();

    let runtime = fixture.reopen();
    runtime
        .show_tool("demo.hello")
        .expect("registered while enabled");
    disable_plugin(&runtime, "demo").expect("disable");

    let runtime = fixture.reopen();
    assert!(
        runtime.show_tool("demo.hello").is_err(),
        "a disabled plugin registers nothing"
    );
    assert_eq!(
        list_plugins(&runtime).expect("list")[0].status,
        PluginStatus::Disabled
    );

    remove_plugin(&runtime, "demo", &PluginRemoveOptions::default()).expect("remove");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !Path::new(&install_path).exists(),
        "the install tree is gone"
    );
}

#[test]
fn validate_reports_an_unsatisfiable_requirement_as_a_warning() {
    let fixture = PluginFixture::new();
    let mut spec = PluginSpecFixture::new("future", "future");
    spec.requires_orbit = Some(">=99.0.0");
    let source = fixture.write_plugin(spec);

    let report = validate_plugin_dir(&fixture.runtime, &source, false).expect("validate");
    assert_eq!(report.name, "future");
    assert_eq!(report.tools, ["future.hello"]);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("requires orbit >=99.0.0")),
        "{report:?}"
    );
}

#[test]
fn failed_enabled_contributions_leave_the_installed_row_disabled() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("unsafe-default")
        .with_enabled_routine()
        .write(&fixture);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install disabled plugin");

    let error = enable_plugin(
        &fixture.runtime,
        "unsafe-default",
        &PluginEnableOptions::default(),
    )
    .expect_err("enabled shipped schedules are refused")
    .to_string();
    assert!(error.contains("enabled: true"), "{error}");
    let installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("unsafe-default")
        .expect("read plugin row")
        .expect("plugin remains installed");
    assert!(
        !installed.enabled,
        "contribution failure must not commit the enable row"
    );
    assert_eq!(
        show_plugin(&fixture.reopen(), "unsafe-default")
            .expect("show")
            .status,
        PluginStatus::Disabled
    );
}

/// `orbit plugin add --enable` used to run the same enable as `orbit plugin
/// enable` and then discard everything it produced beyond the install
/// summary: seeded routines and auto-tasks, linked skills, and warnings
/// (including a grant the manifest did not request) were all invisible on
/// this path [ORB-12807].
#[test]
fn add_enable_carries_the_seeded_skills_and_warnings_report_out_of_install() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);

    let result = fixture
        .runtime
        .add_plugin(
            source.to_str().expect("utf8 path"),
            &PluginAddOptions {
                enable: true,
                grants: vec!["fs".to_string()],
                ..PluginAddOptions::default()
            },
        )
        .expect("install and enable the fixture plugin");

    assert_eq!(result.summary.status, PluginStatus::Active, "{result:?}");

    let seeded_contains = |kind: &str, name: &str| {
        result
            .seeded
            .iter()
            .any(|outcome| outcome.kind == kind && outcome.name == name)
    };
    assert!(
        seeded_contains("routine", "graph-refresh"),
        "{:?}",
        result.seeded
    );
    assert!(
        seeded_contains("auto_task", "graph-reindex"),
        "{:?}",
        result.seeded
    );
    assert!(
        result
            .seeded
            .iter()
            .all(|outcome| outcome.action == PluginSeedAction::Created),
        "a fresh install must seed both definitions as created: {:?}",
        result.seeded
    );

    assert!(
        !result.skills.is_empty()
            && result
                .skills
                .iter()
                .all(|link| link.skill_id == "graph-graph"),
        "the shipped skill must be linked, not dropped, on the add --enable path: {:?}",
        result.skills
    );

    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("grant `fs`") && warning.contains("does not request")),
        "an unrequested grant must warn on add --enable the same way it does on enable: {:?}",
        result.warnings
    );
}

#[test]
fn validate_reports_the_namespaced_skill_discovery_id() {
    let fixture = PluginFixture::new();
    let source = DefinitionPlugin::new("graph").write(&fixture);

    let report = validate_plugin_dir(&fixture.runtime, &source, false).expect("validate");

    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("provider discovery as 'graph-graph'")),
        "validation must expose the skill id before install: {report:?}"
    );
}

#[test]
fn migrate_writes_a_v2_manifest_from_v1_sidecars() {
    let fixture = PluginFixture::new();
    let plugin_dir = fixture.sources.join("legacy");
    std::fs::create_dir_all(&plugin_dir).expect("create legacy dir");
    std::fs::write(plugin_dir.join("legacy-tool"), "#!/bin/sh\n").expect("write executable");
    std::fs::write(
        plugin_dir.join("legacy-recommend.orbit-tool.yaml"),
        "schemaVersion: 1\nname: legacy.recommend\ndescription: Recommend things.\nparameters:\n- name: repository\n  description: Repo path.\n  param_type: string\n  required: true\n",
    )
    .expect("write sidecar");
    std::fs::write(
        plugin_dir.join("legacy-status.orbit-tool.yaml"),
        "schemaVersion: 1\nname: legacy.status\ndescription: Report status.\nparameters: []\n",
    )
    .expect("write sidecar");

    let out_dir = fixture.sources.join("legacy-v2");
    let (yaml, path) = migrate_plugin_sidecars(&PluginMigrateRequest {
        backend_command: plugin_dir
            .join("legacy-tool")
            .to_string_lossy()
            .into_owned(),
        sidecars: Vec::new(),
        version: "0.1.0".to_string(),
        namespace: None,
        out_dir: Some(out_dir.clone()),
    })
    .expect("migrate");
    assert!(yaml.contains("kind: Plugin"), "{yaml}");
    assert_eq!(path.as_deref(), Some(out_dir.join("plugin.yaml").as_path()));
    assert!(
        !yaml.contains("null") && !yaml.contains("[]"),
        "migration must omit optional and empty fields: {yaml}"
    );
    assert!(yaml.contains("command: bin/legacy-tool"), "{yaml}");
    assert!(
        !yaml.contains("publisher:") && !yaml.contains("origin:"),
        "migration must not claim first-party provenance: {yaml}"
    );

    // The generated manifest is what `orbit plugin validate` accepts, and the
    // v1 tool names survive. Migration places the backend in the plugin root
    // even though its source and sidecars were elsewhere.
    assert!(out_dir.join("bin/legacy-tool").is_file());
    let report = validate_plugin_dir(&fixture.runtime, &out_dir, false).expect("validate migrated");
    assert_eq!(report.tools, ["legacy.recommend", "legacy.status"]);
}

/// The `plugins` row is writable by any backend holding `orbit_tools`, so a
/// lifecycle verb may not act on the path it records without checking it
/// first. `remove` is the dangerous one — the loader's own refusal used to
/// send the operator straight into `remove_dir_all` of whatever the row named
/// — but `enable` seeds from that tree and `disable` selects discovery links
/// by it, so all three refuse together [ORB-12800].
#[test]
fn a_relocated_row_is_refused_by_every_lifecycle_verb_and_leaves_that_tree_alone() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    // An operator directory Orbit never installed anything into.
    let sentinel = fixture.repo_root.join("unrelated");
    std::fs::create_dir_all(&sentinel).expect("create the sentinel tree");
    let keep = sentinel.join("keep.txt");
    std::fs::write(&keep, "operator data").expect("write the sentinel file");

    let runtime = fixture.reopen();
    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    let install_path = installed.install_path.clone();
    installed.install_path = sentinel.to_string_lossy().into_owned();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("the row write succeeds; the lifecycle verbs are what refuse it");

    let expected = runtime.global_root().join("plugins/demo");
    let refusals = [
        (
            "remove",
            remove_plugin(&runtime, "demo", &PluginRemoveOptions::default())
                .expect_err("remove must not delete a tree this host did not install"),
        ),
        (
            "enable",
            enable_plugin(&runtime, "demo", &PluginEnableOptions::default())
                .map(|_| ())
                .expect_err("enable must not seed definitions out of that tree"),
        ),
        (
            "disable",
            disable_plugin(&runtime, "demo")
                .map(|_| ())
                .expect_err("disable must not select discovery links by that tree"),
        ),
    ];
    for (verb, error) in refusals {
        let message = error.to_string();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && message.contains(&installed.install_path)
                && message.contains(&expected.display().to_string()),
            "{verb} must name the recorded and the expected path: {message}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&keep).expect("the sentinel file survives"),
        "operator data"
    );
    assert!(
        Path::new(&install_path).is_dir(),
        "a refusal touches neither tree"
    );

    // The refusal recommends the record-only removal, so that command has to
    // still have a row to clear: refusing must not discard the record first.
    assert_eq!(
        list_plugins(&runtime).expect("list").len(),
        1,
        "the refused row survives for the recovery the diagnostic names"
    );
    remove_plugin(&runtime, "demo", &PluginRemoveOptions { record_only: true })
        .expect("record-only removal clears a row it cannot verify");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !crate::runtime::plugin_grants::plugin_grant_witness_path(&runtime.global_root(), "demo")
            .exists(),
        "the grant witness goes with the record"
    );
    assert_eq!(
        std::fs::read_to_string(&keep).expect("the sentinel file survives the recovery"),
        "operator data"
    );
    assert!(
        Path::new(&install_path).is_dir(),
        "record-only leaves every installed file where it is"
    );
}
