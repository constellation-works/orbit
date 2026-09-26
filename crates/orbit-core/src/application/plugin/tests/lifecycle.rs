use std::path::{Path, PathBuf};

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
fn remove_retains_state_by_default_and_purges_only_its_own_state_when_requested() {
    for purge_state in [false, true] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        let own_state = fixture.global_root.join("state/plugins/demo");
        let other_state = fixture.global_root.join("state/plugins/other");
        let outside = fixture.repo_root.join("operator-data");
        for dir in [&own_state, &other_state, &outside] {
            std::fs::create_dir_all(dir).expect("create state or outside tree");
            std::fs::write(dir.join("keep.txt"), "keep").expect("write sentinel");
        }

        remove_plugin(
            &fixture.runtime,
            "demo",
            &PluginRemoveOptions {
                purge_state,
                ..PluginRemoveOptions::default()
            },
        )
        .expect("remove");

        assert_eq!(own_state.exists(), !purge_state);
        for dir in [&other_state, &outside] {
            assert_eq!(
                std::fs::read_to_string(dir.join("keep.txt")).expect("sentinel survives"),
                "keep"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn purge_refuses_a_symlinked_state_prefix_before_changing_the_install() {
    use std::os::unix::fs::symlink;

    for prefix in ["state/plugins", "state/plugins/demo"] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
        let summary = install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        let outside = fixture.repo_root.join("operator-data");
        std::fs::create_dir_all(&outside).expect("outside tree");
        let keep = outside.join("keep.txt");
        std::fs::write(&keep, "keep").expect("outside sentinel");
        let link = fixture.global_root.join(prefix);
        std::fs::create_dir_all(link.parent().expect("state prefix parent"))
            .expect("state prefix parent");
        symlink(&outside, &link).expect("link state prefix outside");

        let error = remove_plugin(
            &fixture.runtime,
            "demo",
            &PluginRemoveOptions {
                purge_state: true,
                ..PluginRemoveOptions::default()
            },
        )
        .expect_err("symlinked state prefix must refuse removal");
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && error.to_string().contains(&link.display().to_string()),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&keep).expect("sentinel"), "keep");
        assert!(Path::new(&summary.install_path).is_dir());
        assert_eq!(list_plugins(&fixture.runtime).expect("list").len(), 1);
    }
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
    remove_plugin(
        &runtime,
        "demo",
        &PluginRemoveOptions {
            record_only: true,
            ..PluginRemoveOptions::default()
        },
    )
    .expect("record-only removal clears a row it cannot verify");
    assert!(list_plugins(&runtime).expect("list").is_empty());
    assert!(
        !crate::runtime::plugin::grants::plugin_grant_witness_path(&runtime.global_root(), "demo")
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

// ---------------------------------------------------------------------------
// Digest-pinned HTTPS archive sources.
// ---------------------------------------------------------------------------

const ARCHIVE_URL: &str = "https://example.test/demo-1.0.0.tar.gz";
const UNINSTALLED_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// Re-enter this test with a fetch shim on `PATH` that serves whatever the
/// child writes to `ORBIT_TEST_PLUGIN_ARCHIVE`, so an `https://` source runs
/// the real install path without a network or a TLS server.
#[cfg(unix)]
fn enter_fake_fetch_child(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_PLUGIN_FETCH_CHILD")
        .ok()
        .as_deref()
        == Some(&exact_test)
    {
        return true;
    }

    let temp = tempfile::tempdir().expect("fake fetch directory");
    let archive = temp.path().join("archive.tar.gz");
    let quote = |value: &Path| format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"));
    let curl = temp.path().join("curl");
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nset -eu\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '--output' ]; then out=$arg; fi\n  prev=$arg\ndone\n[ -n \"$out\" ] || exit 2\n[ -f {archive} ] || exit 22\ncat {archive} > \"$out\"\n",
            archive = quote(&archive),
        ),
    )
    .expect("write fake curl");
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))
        .expect("make fake curl executable");

    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_FETCH_CHILD", &exact_test)
        .env("ORBIT_TEST_PLUGIN_ARCHIVE", &archive)
        .env(
            "PATH",
            std::env::join_paths(paths).expect("fake fetch PATH"),
        )
        .output()
        .expect("run isolated fake-fetch test");
    assert!(
        output.status.success(),
        "fake-fetch child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// Pack `source` as the archive the fetch shim will serve, and return the
/// `sha256:` digest a pin has to name for it.
#[cfg(unix)]
fn publish_archive(source: &Path) -> String {
    let archive =
        PathBuf::from(std::env::var_os("ORBIT_TEST_PLUGIN_ARCHIVE").expect("archive path"));
    let status = std::process::Command::new("tar")
        .args([
            "czf".as_ref(),
            archive.as_os_str(),
            "-C".as_ref(),
            source.as_os_str(),
            ".".as_ref(),
        ])
        .status()
        .expect("run tar");
    assert!(status.success(), "tar must pack the fixture plugin");
    let bytes = std::fs::read(&archive).expect("read published archive");
    format!(
        "sha256:{}",
        orbit_common::security::release::sha256_hex(&bytes)
    )
}

#[cfg(unix)]
#[test]
fn sync_installs_a_digest_pinned_https_archive() {
    if !enter_fake_fetch_child("sync_installs_a_digest_pinned_https_archive") {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let digest = publish_archive(&source);
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {ARCHIVE_URL}\n    digest: {digest}\n    enabled: true\n"
    ));

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, PluginStatus::Active, "{outcomes:?}");
    assert!(
        outcomes[0].message.contains("installed v1.0.0"),
        "{outcomes:?}"
    );

    let installed = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read record")
        .expect("demo is installed");
    assert_eq!(
        installed.archive_digest.as_deref(),
        Some(digest.trim_start_matches("sha256:")),
        "the verified archive digest is recorded so doctor can compare it later"
    );
    fixture
        .reopen()
        .show_tool("demo.hello")
        .expect("the fetched plugin registers its tool");
}

#[cfg(unix)]
#[test]
fn sync_refuses_a_pinned_archive_whose_digest_does_not_match() {
    if !enter_fake_fetch_child("sync_refuses_a_pinned_archive_whose_digest_does_not_match") {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let served = publish_archive(&source);
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {ARCHIVE_URL}\n    digest: {UNINSTALLED_DIGEST}\n    enabled: true\n"
    ));

    let outcomes = sync_plugins(&fixture.runtime, false, &[]).expect("sync continues");
    assert_eq!(outcomes[0].status, PluginStatus::Missing, "{outcomes:?}");
    assert!(
        outcomes[0].message.contains("pin for 'demo'"),
        "the refusal must name the pin entry: {outcomes:?}"
    );
    assert!(
        outcomes[0].message.contains(&served) && outcomes[0].message.contains(UNINSTALLED_DIGEST),
        "the refusal must report both digests: {outcomes:?}"
    );
    assert!(
        fixture
            .runtime
            .stores()
            .plugins()
            .get_plugin("demo")
            .expect("read record")
            .is_none(),
        "nothing may be installed from an archive that is not the pinned one"
    );
}

/// No trust on first use: an archive pin with no digest is refused before any
/// fetch, and the diagnostic names the entry to fix.
#[test]
fn sync_refuses_an_archive_pin_without_a_digest() {
    let fixture = PluginFixture::new();
    fixture.write_pin_file(&format!(
        "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {ARCHIVE_URL}\n    enabled: true\n"
    ));

    let error = sync_plugins(&fixture.runtime, false, &[])
        .expect_err("an unpinned archive source must be refused")
        .to_string();
    assert!(
        error.contains("plugins[0].digest") && error.contains(ARCHIVE_URL),
        "the refusal must name the pin entry and its source: {error}"
    );
    assert!(
        error.contains("first use"),
        "the refusal must say why a digest is mandatory: {error}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_reports_a_pinned_archive_whose_digest_no_longer_matches() {
    if !enter_fake_fetch_child("doctor_reports_a_pinned_archive_whose_digest_no_longer_matches") {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let installed_digest = publish_archive(&source);
    let pin = |digest: &str| {
        format!(
            "schemaVersion: 1\nplugins:\n  - name: demo\n    source: {ARCHIVE_URL}\n    digest: {digest}\n    enabled: true\n"
        )
    };
    fixture.write_pin_file(&pin(&installed_digest));
    sync_plugins(&fixture.runtime, false, &[]).expect("sync");

    assert!(
        plugin_doctor(&fixture.runtime)
            .expect("doctor")
            .iter()
            .all(|row| !row.message.contains("archive")),
        "a pin that matches what is installed is not a finding"
    );

    // The workspace moves the pin to a release this host has never installed.
    fixture.write_pin_file(&pin(UNINSTALLED_DIGEST));
    let rows = plugin_doctor(&fixture.runtime).expect("doctor");
    let finding = rows
        .iter()
        .find(|row| row.message.contains(UNINSTALLED_DIGEST))
        .unwrap_or_else(|| panic!("doctor must report the moved pin: {rows:?}"));
    assert_eq!(finding.plugin, "demo");
    assert!(
        finding
            .message
            .contains(installed_digest.trim_start_matches("sha256:")),
        "the finding must name what is actually installed: {finding:?}"
    );
    assert!(
        finding.message.contains("orbit plugin upgrade demo"),
        "the finding must name the recovery: {finding:?}"
    );
}
