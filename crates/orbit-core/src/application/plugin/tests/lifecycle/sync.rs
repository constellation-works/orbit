//! `orbit plugin sync`, including digest-pinned HTTPS archive sources.

use std::path::{Path, PathBuf};

use orbit_types::plugin::PluginStatus;

use super::super::super::{
    PluginAddOptions, install_plugin, plugin_doctor, show_plugin, sync_plugins,
};
use super::super::definition_fixture::DefinitionPlugin;
use super::super::fixture::{PluginFixture, PluginSpecFixture};

#[test]
fn sync_refuses_unsafe_git_pin_entries() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_refuses_unsafe_git_pin_entries",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_installs_what_the_pin_file_names_and_reports_what_it_cannot",
    ) {
        return;
    }
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
fn sync_refuses_a_source_namespace_that_differs_from_the_pin_on_every_run() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_refuses_a_source_namespace_that_differs_from_the_pin_on_every_run",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_refuses_an_out_of_range_source_without_side_effects_and_continues",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_reconciles_enabled_contributions_into_each_workspace",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_does_not_enable_a_grant_requesting_plugin_without_consent",
    ) {
        return;
    }
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
fn sync_and_dry_run_report_the_loader_refusal_for_enabled_plugins() {
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_and_dry_run_report_the_loader_refusal_for_enabled_plugins",
    ) {
        return;
    }
    for (name, spec) in [
        (
            "ungranted",
            PluginSpecFixture::new("ungranted", "ungranted").requesting_fs_write(),
        ),
        (
            "future",
            PluginSpecFixture {
                requires_orbit: Some(">=99.0.0"),
                ..PluginSpecFixture::new("future", "future")
            },
        ),
    ] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(spec);
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
            "schemaVersion: 1\nplugins:\n  - name: {name}\n    enabled: true\n"
        ));
        let freshly_loaded = show_plugin(&fixture.reopen(), name).expect("fresh status");
        assert_eq!(freshly_loaded.status, PluginStatus::Inactive);
        let refusal = freshly_loaded.diagnostic.expect("loader refusal");

        for dry_run in [true, false] {
            let outcomes = sync_plugins(&fixture.runtime, dry_run, &[]).expect("sync outcome");
            assert_eq!(outcomes[0].status, PluginStatus::Inactive, "{outcomes:?}");
            assert!(
                outcomes[0].message.contains(&refusal),
                "sync must include the loader's refusal: {outcomes:?}"
            );
        }
    }
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
            "#!/bin/sh\nset -eu\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '--output' ]; then out=$arg; fi\n  prev=$arg\ndone\n[ -n \"$out\" ] || exit 2\n[ -f {archive} ] || exit 22\nif [ \"$out\" = '-' ]; then cat {archive}; else cat {archive} > \"$out\"; fi\n",
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
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    super::super::fixture::clear_child_authority(&mut child);
    let output = child
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_FETCH_CHILD", &exact_test)
        .env("ORBIT_TEST_PLUGIN_ARCHIVE", &archive)
        .env(
            "PATH",
            std::env::join_paths(paths).expect("fake fetch PATH"),
        )
        .output()
        .expect("run isolated fake-fetch test");
    super::super::fixture::assert_child_passed(&output, &exact_test);
    false
}

/// Pack the checkout holding the plugin root `source` as the archive the
/// fetch shim will serve, and return the `sha256:` digest a pin has to name
/// for it.
#[cfg(unix)]
fn publish_archive(source: &Path) -> String {
    let archive =
        PathBuf::from(std::env::var_os("ORBIT_TEST_PLUGIN_ARCHIVE").expect("archive path"));
    let source = source
        .parent()
        .expect("the plugin root sits in its checkout");
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_installs_a_digest_pinned_https_archive",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_refuses_a_pinned_archive_whose_digest_does_not_match",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "sync_refuses_an_archive_pin_without_a_digest",
    ) {
        return;
    }
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
    if !super::super::fixture::enter_isolated_child(
        module_path!(),
        "doctor_reports_a_pinned_archive_whose_digest_no_longer_matches",
    ) {
        return;
    }
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
