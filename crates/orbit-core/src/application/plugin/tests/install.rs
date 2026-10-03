use std::path::Path;

use orbit_types::telemetry::AuditEventStatus;

use super::super::{PluginAddOptions, install_plugin};
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
