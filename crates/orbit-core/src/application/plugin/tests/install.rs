use std::path::Path;

use orbit_types::plugin::PluginStatus;
use orbit_types::telemetry::AuditEventStatus;

use super::super::{PluginAddOptions, PluginUpgradeOptions, install_plugin, upgrade_plugin};
use super::fixture::{PluginFixture, PluginSpecFixture};

#[cfg(unix)]
fn enter_fake_git_install_child(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_PLUGIN_GIT_INSTALL_CHILD")
        .ok()
        .as_deref()
        == Some(&exact_test)
    {
        return true;
    }

    let temp = tempfile::tempdir().expect("fake git directory");
    let git = temp.path().join("git");
    std::fs::write(
        &git,
        r#"#!/bin/sh
set -eu
checkout=''
for arg in "$@"; do checkout=$arg; done
mkdir -p "$checkout/.git" "$checkout/.orbit-plugin/bin"
printf '[remote "origin"]\n\turl = https://example.test/demo.git\n' > "$checkout/.git/config"
printf '# product code\n' > "$checkout/Cargo.lock"
cat > "$checkout/.orbit-plugin/plugin.yaml" <<'EOF'
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.2.3
  description: Git fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
EOF
printf '#!/bin/sh\n' > "$checkout/.orbit-plugin/bin/backend.sh"
chmod +x "$checkout/.orbit-plugin/bin/backend.sh"
"#,
    )
    .expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");
    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    super::fixture::clear_child_authority(&mut child);
    let output = child
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_INSTALL_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("fake git PATH"))
        .output()
        .expect("run isolated fake-git install test");
    super::fixture::assert_child_passed(&output, &exact_test);
    false
}

/// A refusal before the backend starts is audited as `Denied`, still naming
/// the plugin.
#[cfg(unix)]
#[test]
fn an_ungranted_plugin_call_is_audited_as_denied_with_plugin_provenance() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "an_ungranted_plugin_call_is_audited_as_denied_with_plugin_provenance",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo").requesting_fs_write());
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let runtime = fixture.reopen();
    fixture
        .call(&runtime, "demo.hello")
        .expect_err("the grant is missing");
    let events = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
        .expect("audit events");
    let event = events.first().expect("the refusal was audited");
    assert_eq!(event.status, AuditEventStatus::Denied);
    assert_eq!(
        event.plugin.as_ref().map(|plugin| plugin.name.as_str()),
        Some("demo")
    );
}

/// [ORB-12794] `git clone` writes the source URL (credentials included, when
/// present) into `.git/config`, and that root is the first entry in the
/// plugin backend's unconditional read set. The install must not carry the
/// clone's VCS metadata into the tree the backend can always read.
#[cfg(unix)]
#[test]
fn add_from_a_git_source_excludes_the_clone_metadata() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "add_from_a_git_source_excludes_the_clone_metadata",
    ) {
        return;
    }
    if !enter_fake_git_install_child("add_from_a_git_source_excludes_the_clone_metadata") {
        return;
    }
    let fixture = PluginFixture::new();

    let summary = install_plugin(
        &fixture.runtime,
        "git+https://example.test/demo.git",
        &PluginAddOptions::default(),
    )
    .expect("install from an HTTPS Git source");

    let installed = Path::new(&summary.install_path);
    assert!(
        installed.join("plugin.yaml").is_file(),
        "installed tree must contain the plugin files"
    );
    assert!(
        installed.join("bin/backend.sh").is_file(),
        "installed tree must contain the plugin files"
    );
    assert!(
        !installed.join(".git").exists(),
        "installed tree must not contain the clone's .git directory"
    );
    assert!(
        !installed.join("Cargo.lock").exists() && !installed.join(".orbit-plugin").exists(),
        "only the checkout's .orbit-plugin/ contents are installed"
    );
}

/// A default-only change to a `{{config.*}}` fs root is judged on the directory
/// the sandbox will open. The template text stays the same, so a raw-string
/// diff would carry the `fs` grant onto a broader root [ORB-14341].
#[cfg(unix)]
#[test]
fn upgrade_broadens_a_templated_fs_default_only_when_the_resolved_root_grows() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "upgrade_broadens_a_templated_fs_default_only_when_the_resolved_root_grows",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let parent = fixture.sources.join("roots");
    let child = parent.join("cache");
    std::fs::create_dir_all(&child).expect("create fs roots");
    let parent = parent.canonicalize().expect("parent root");
    let child = child.canonicalize().expect("child root");

    install_enabled(&fixture, "demo", "1.0.0", "v1-broad", &parent);
    let narrowed = upgrade_enabled(&fixture, "demo", "1.0.1", "v1-narrow", &child);
    assert!(
        !narrowed.grants_reset,
        "a default that resolves inside the previous root keeps the grant: {narrowed:?}"
    );
    assert_eq!(narrowed.summary.status, PluginStatus::Active);
    assert_eq!(narrowed.summary.granted, ["fs".to_string()]);

    let broadened = upgrade_enabled(&fixture, "demo", "1.0.2", "v2-broad", &parent);
    assert!(
        broadened.grants_reset,
        "a broader resolved root must clear grants: {broadened:?}"
    );
    assert!(
        broadened
            .permission_changes
            .iter()
            .any(|change| change.grant.as_str() == "fs" && change.widened),
        "the fs change is classified as widening: {:?}",
        broadened.permission_changes
    );
    assert_eq!(broadened.summary.status, PluginStatus::Disabled);
    assert!(broadened.summary.granted.is_empty());
    assert!(!broadened.summary.host_enabled);
    let diagnostic = broadened.summary.diagnostic.unwrap_or_default();
    assert!(
        diagnostic.contains(&parent.display().to_string()),
        "the widening report names the broader root: {diagnostic}"
    );
    let row = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read row")
        .expect("installed");
    assert!(!row.enabled);
    assert!(row.grants.is_empty());

    // Global config pins the key, so the manifest default is not what a call
    // opens. Changing that default must not demand re-consent.
    install_enabled(&fixture, "held", "1.0.0", "held-v1", &child);
    std::fs::write(
        fixture.global_root.join("config.toml"),
        format!("[plugins.held]\nout = \"{}\"\n", child.display()),
    )
    .expect("write global plugin config");
    let pinned = upgrade_enabled(&fixture, "held", "1.1.0", "held-v2", &parent);
    assert!(
        !pinned.grants_reset,
        "a global override keeps the rendered root, so the default change is not widening: {pinned:?}"
    );
    assert_eq!(pinned.summary.granted, ["fs".to_string()]);
    assert_eq!(pinned.summary.status, PluginStatus::Active);
}

#[cfg(unix)]
fn templated_write_block(root: &Path) -> String {
    format!(
        "  config:\n    defaults:\n      out: \"{}\"\n  permissions:\n    fs:\n      write: [\"{{{{config.out}}}}\"]\n",
        root.display()
    )
}

#[cfg(unix)]
fn install_enabled(fixture: &PluginFixture, name: &str, version: &str, dir: &str, root: &Path) {
    let block = templated_write_block(root);
    let mut spec = PluginSpecFixture::new(dir, name);
    spec.version = version;
    spec.permissions = Some(&block);
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .unwrap_or_else(|error| panic!("install {name} v{version}: {error}"));
}

#[cfg(unix)]
fn upgrade_enabled(
    fixture: &PluginFixture,
    name: &str,
    version: &str,
    dir: &str,
    root: &Path,
) -> super::super::PluginUpgradeResult {
    let block = templated_write_block(root);
    let mut spec = PluginSpecFixture::new(dir, name);
    spec.version = version;
    spec.permissions = Some(&block);
    let source = fixture.write_plugin(spec);
    upgrade_plugin(
        &fixture.runtime,
        name,
        Some(source.to_str().expect("utf8 path")),
        &PluginUpgradeOptions::default(),
    )
    .unwrap_or_else(|error| panic!("upgrade {name} to v{version}: {error}"))
}
