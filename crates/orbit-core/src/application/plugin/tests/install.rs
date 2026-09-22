use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_types::plugin::PluginStatus;
use orbit_types::telemetry::AuditEventStatus;

use super::super::{
    PluginAddOptions, PluginRemoveOptions, PluginUpgradeOptions, install_plugin, list_plugins,
    plugin_doctor, remove_plugin, show_plugin, upgrade_plugin, validate_plugin_dir,
};
use super::fixture::{PluginFixture, PluginSpecFixture, write_plugin_at};

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
mkdir -p "$checkout/.git" "$checkout/bin"
printf '[remote "origin"]\n\turl = https://example.test/demo.git\n' > "$checkout/.git/config"
cat > "$checkout/plugin.yaml" <<'EOF'
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
printf '#!/bin/sh\n' > "$checkout/bin/backend.sh"
chmod +x "$checkout/bin/backend.sh"
"#,
    )
    .expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");
    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_INSTALL_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("fake git PATH"))
        .output()
        .expect("run isolated fake-git install test");
    assert!(
        output.status.success(),
        "fake-git install child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[cfg(unix)]
#[test]
fn add_installs_a_tagged_https_git_source() {
    if !enter_fake_git_install_child("add_installs_a_tagged_https_git_source") {
        return;
    }
    let fixture = PluginFixture::new();
    let summary = install_plugin(
        &fixture.runtime,
        "git+https://example.com/demo.git#v1.2.3",
        &PluginAddOptions::default(),
    )
    .expect("install tagged HTTPS Git source");
    assert_eq!(summary.name, "demo");
    assert_eq!(summary.version, "1.2.3");
    assert_eq!(summary.status, PluginStatus::Disabled);
}

#[test]
fn add_refuses_unsafe_git_source_entries() {
    let fixture = PluginFixture::new();
    for source in [
        "git+ext::sh -c 'exit 0' %S",
        "git+file:///tmp/plugin",
        "git+-uplugin",
        "git+https://example.com/demo.git#-upload-pack=payload",
    ] {
        let error = install_plugin(&fixture.runtime, source, &PluginAddOptions::default())
            .expect_err("unsafe Git source must be refused")
            .to_string();
        assert!(
            error.contains(source),
            "add refusal must name source entry {source:?}: {error}"
        );
    }
}

#[test]
fn add_refuses_a_source_inside_the_repository() {
    let fixture = PluginFixture::new();
    let inside = fixture.repo_root.join("plugins/demo");
    write_plugin_at(&inside, PluginSpecFixture::new("demo", "demo"));

    let error = install_plugin(
        &fixture.runtime,
        inside.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect_err("an in-repository source must be refused");
    let message = error.to_string();
    assert!(message.contains("global-install-only"), "{message}");
    assert!(message.contains(".orbit/plugins.yaml"), "{message}");
    assert!(
        list_plugins(&fixture.runtime).expect("list").is_empty(),
        "the refusal must not record an install"
    );
}

#[test]
fn add_then_enable_puts_the_tool_on_the_surface() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));

    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    assert_eq!(summary.status, PluginStatus::Disabled);
    assert!(
        summary.install_path.starts_with(
            fixture
                .global_root
                .join("plugins")
                .to_str()
                .expect("utf8 install root")
        ),
        "installed globally: {}",
        summary.install_path
    );
    assert_eq!(summary.manifest_digest.len(), 64);

    // Disabled: the tool is not registered anywhere yet.
    let runtime = fixture.reopen();
    assert!(runtime.show_tool("demo.hello").is_err());
    let doctor = plugin_doctor(&runtime).expect("doctor");
    assert_eq!(doctor.len(), 1);
    assert!(
        doctor[0].message.contains("orbit plugin enable demo"),
        "{doctor:?}"
    );

    super::super::enable_plugin(
        &runtime,
        "demo",
        &super::super::PluginEnableOptions {
            grants: vec!["fs".to_string()],
            force: false,
        },
    )
    .expect("enable");

    let runtime = fixture.reopen();
    let tool = runtime
        .show_tool("demo.hello")
        .expect("plugin tool is registered");
    assert!(tool.active && tool.enabled);
    assert!(!tool.builtin);
    assert!(
        tool.parameters.iter().any(|param| param.name == "subject"),
        "input schema reached the registry: {:?}",
        tool.parameters
    );
    assert!(
        runtime
            .list_mcp_tool_definitions()
            .expect("mcp definitions")
            .iter()
            .any(|definition| definition.schema.name == "demo.hello"),
        "an enabled plugin tool is advertised"
    );

    let shown = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(shown.status, PluginStatus::Active);
    assert_eq!(shown.granted, ["fs"]);
    assert_eq!(shown.tools.len(), 1);
    assert_eq!(
        shown.tools[0].advertised_name.as_deref(),
        Some("demo_hello")
    );
    assert!(
        plugin_doctor(&runtime).expect("doctor")[0]
            .message
            .is_empty()
    );
}

#[test]
fn adding_a_version_with_wider_requests_revokes_carried_grants() {
    let fixture = PluginFixture::new();
    let mut v1 = PluginSpecFixture::new("demo-v1", "demo");
    v1.permissions = Some("  permissions:\n    fs:\n      write: [\"{{plugin_state}}\"]\n");
    let v1 = fixture.write_plugin(v1);
    install_plugin(
        &fixture.runtime,
        v1.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect("install v1 with fs authorized");

    let mut v2 = PluginSpecFixture::new("demo-v2", "demo");
    v2.version = "2.0.0";
    v2.permissions = Some(
        "  permissions:\n    fs:\n      write: [\"{{workspace}}\"]\n    orbit_tools: [orbit.task.list]\n",
    );
    let v2 = fixture.write_plugin(v2);
    let summary = install_plugin(
        &fixture.runtime,
        v2.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install widened v2");

    assert_eq!(summary.status, PluginStatus::Disabled);
    assert!(summary.granted.is_empty(), "{summary:?}");
    let output = summary.diagnostic.as_deref().unwrap_or_default();
    assert!(
        output.contains("fs")
            && output.contains("write={{workspace}}")
            && output.contains("orbit_tools")
            && output.contains("orbit plugin enable demo --grant fs,orbit_tools"),
        "the output must name each widened request and the re-consent command: {output}"
    );
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert!(!stored.enabled);
    assert!(stored.grants.is_empty());

    // The old witness must be revoked as well as the row. Otherwise a backend
    // able to rewrite orbit.db could restore the old grant names and make them
    // authorize the new manifest's wider paths.
    fixture
        .runtime
        .stores()
        .plugins()
        .set_plugin_enabled("demo", true, &["fs".to_string()])
        .expect("attempt to replay the old row authority");
    let replayed = show_plugin(&fixture.reopen(), "demo").expect("show replayed row");
    assert_eq!(replayed.status, PluginStatus::Inactive);
    assert!(replayed.granted.is_empty(), "{replayed:?}");
    assert!(
        replayed
            .diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("do not match")),
        "the revoked witness must refuse the replayed old grant: {replayed:?}"
    );
}

#[test]
fn adding_a_version_without_wider_requests_preserves_authority() {
    let fixture = PluginFixture::new();
    let v1 = fixture.write_plugin(PluginSpecFixture::new("demo-v1", "demo").requesting_fs_write());
    install_plugin(
        &fixture.runtime,
        v1.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect("install v1 with fs authorized");

    let mut v2 = PluginSpecFixture::new("demo-v2", "demo").requesting_fs_write();
    v2.version = "2.0.0";
    let v2 = fixture.write_plugin(v2);
    let summary = install_plugin(
        &fixture.runtime,
        v2.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install equivalent v2");

    assert_eq!(summary.status, PluginStatus::Active, "{summary:?}");
    assert_eq!(summary.granted, ["fs"]);
    let stored = fixture
        .runtime
        .stores()
        .plugins()
        .get_plugin("demo")
        .expect("read plugin row")
        .expect("installed plugin");
    assert!(stored.enabled);
    assert_eq!(stored.grants, ["fs"]);
}

#[test]
fn upgrade_reports_the_permission_diff_and_accepts_explicit_reconsent() {
    let fixture = PluginFixture::new();
    let v1 = fixture.write_plugin(PluginSpecFixture::new("demo-v1", "demo").requesting_fs_write());
    install_plugin(
        &fixture.runtime,
        v1.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect("install v1");

    let mut v2 = PluginSpecFixture::new("demo-v2", "demo");
    v2.version = "2.0.0";
    v2.permissions = Some("  permissions:\n    fs:\n      write: [\"{{workspace}}/.cache\"]\n");
    let v2 = fixture.write_plugin(v2);
    let result = upgrade_plugin(
        &fixture.runtime,
        "demo",
        Some(v2.to_str().expect("utf8 path")),
        &PluginUpgradeOptions {
            digest: None,
            grants: vec!["fs".to_string()],
        },
    )
    .expect("upgrade with explicit re-consent");

    assert!(!result.grants_reset);
    assert_eq!(result.summary.status, PluginStatus::Active);
    assert_eq!(result.summary.granted, ["fs"]);
    assert_eq!(result.permission_changes.len(), 1);
    let change = &result.permission_changes[0];
    assert_eq!(change.grant.as_str(), "fs");
    assert_eq!(
        change.requested.as_deref(),
        Some("write={{workspace}}/.cache")
    );
    assert!(change.widened);
}

#[test]
fn add_rejects_grants_without_enable() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let error = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            grants: vec!["fs".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect_err("grant without enable must be rejected")
    .to_string();
    assert!(error.contains("--grant requires --enable"), "{error}");
}

#[cfg(unix)]
#[test]
fn an_enabled_plugin_tool_executes_through_audited_dispatch() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
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
    let output = fixture
        .call_with_input(
            &runtime,
            "demo.hello",
            serde_json::json!({ "subject": "orbit" }),
        )
        .expect("run the plugin tool");
    assert_eq!(output["plugin"], "demo");
    assert_eq!(output["envelope"]["tool"], "demo.hello");
    assert_eq!(output["envelope"]["input"]["subject"], "orbit");

    let events = runtime
        .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
        .expect("audit events");
    let event = events.first().expect("the call was audited");
    let plugin = event
        .plugin
        .as_ref()
        .expect("the audit row names the plugin");
    assert_eq!(plugin.name, "demo");
    assert_eq!(plugin.version, "1.0.0");
    let install_path = show_plugin(&runtime, "demo").expect("show").install_path;
    let loaded = orbit_tools::plugin::manifest_digest(
        &std::fs::read(Path::new(&install_path).join("plugin.yaml")).expect("manifest bytes"),
    );
    assert_eq!(
        plugin.manifest_digest, loaded,
        "audit provenance must name the digest of the bytes that ran"
    );
}

/// Rewriting `plugin.yaml` after install is not a silent grant expansion: the
/// next load registers the plugin inactive and names both digests plus the
/// re-consent commands.
#[test]
fn a_rewritten_manifest_after_install_is_refused_until_reconsent() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");
    let stored = summary.manifest_digest.clone();

    let manifest = Path::new(&summary.install_path).join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace("Fixture plugin.", "Fixture plugin (tampered)."),
    )
    .expect("rewrite the installed manifest");
    let loaded = orbit_tools::plugin::manifest_digest(&std::fs::read(&manifest).expect("bytes"));
    assert_ne!(stored, loaded);

    let runtime = fixture.reopen();
    let shown = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(shown.status, PluginStatus::Inactive);
    let diagnostic = shown.diagnostic.as_deref().unwrap_or_default();
    assert!(
        diagnostic.contains(&stored)
            && diagnostic.contains(&loaded)
            && diagnostic.contains("orbit plugin add --force")
            && diagnostic.contains("orbit plugin enable demo"),
        "{diagnostic}"
    );
}

/// `orbit plugin validate` refuses a write tree that contains the plugin
/// directory or reaches protected host-global paths. Registration and calls
/// apply the same shared rule.
#[test]
fn validate_refuses_fs_write_roots_that_cover_the_plugin_or_global_root() {
    let fixture = PluginFixture::new();
    let mut covering_plugin = PluginSpecFixture::new("cover", "cover");
    covering_plugin.permissions =
        Some("  permissions:\n    fs:\n      write: [\"{{plugin_root}}\"]\n");
    let covering = fixture.write_plugin(covering_plugin);
    let error = validate_plugin_dir(&fixture.runtime, &covering, false)
        .expect_err("plugin-root write must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]") && error.contains("plugin install root"),
        "{error}"
    );

    let mut slash = PluginSpecFixture::new("slash", "slash");
    slash.permissions = Some("  permissions:\n    fs:\n      write: [\"/\"]\n");
    let slash_dir = fixture.write_plugin(slash);
    let error = validate_plugin_dir(&fixture.runtime, &slash_dir, false)
        .expect_err("write of / must be refused")
        .to_string();
    assert!(error.contains("spec.permissions.fs.write[0]"), "{error}");

    let global_dir = fixture.write_plugin(PluginSpecFixture::new("hostroot", "hostroot"));
    let manifest = global_dir.join("plugin.yaml");
    let body = std::fs::read_to_string(&manifest).expect("read manifest");
    std::fs::write(
        &manifest,
        body.replace(
            "  backend:\n",
            &format!(
                "  permissions:\n    fs:\n      write: [\"{}\"]\n  backend:\n",
                fixture.global_root.display()
            ),
        ),
    )
    .expect("request a global-root write");
    let error = validate_plugin_dir(&fixture.runtime, &global_dir, false)
        .expect_err("write of the global root must be refused")
        .to_string();
    assert!(
        error.contains("spec.permissions.fs.write[0]") && error.contains("Orbit global root"),
        "{error}"
    );

    for (name, write) in [
        (
            "hostbin",
            fixture
                .global_root
                .join("bin")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "grants",
            fixture
                .global_root
                .join("plugins/.grants")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "other",
            fixture
                .global_root
                .join("plugins/victim/1.0.0")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "traversal",
            "{{plugin_state}}/../../../plugins/.grants".to_string(),
        ),
    ] {
        let permissions = format!("  permissions:\n    fs:\n      write: [\"{write}\"]\n");
        let mut protected = PluginSpecFixture::new(name, name);
        protected.permissions = Some(&permissions);
        let protected_dir = fixture.write_plugin(protected);
        let error = validate_plugin_dir(&fixture.runtime, &protected_dir, false)
            .expect_err("a protected global-root descendant must be refused")
            .to_string();
        assert!(
            error.contains("spec.permissions.fs.write[0]")
                && error.contains("protected path beneath Orbit global root"),
            "{name}: {error}"
        );
    }

    let allowed = fixture.write_plugin(PluginSpecFixture::new("ok", "ok").requesting_fs_write());
    validate_plugin_dir(&fixture.runtime, &allowed, false)
        .expect("the current {{plugin_state}} tree remains writable");
}

/// Every way a backend can fail short of a valid response is a tool error
/// carrying an audit row with the plugin's provenance and grants, and none of
/// them returns part of the backend's output (design §4.2, §4.4).
#[cfg(unix)]
#[test]
fn a_failing_plugin_call_is_audited_with_plugin_provenance_and_no_partial_output() {
    for (backend, expected, status) in [
        (
            "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 7\n",
            "exited with 7",
            AuditEventStatus::Failure,
        ),
        (
            "#!/bin/sh\ncat >/dev/null\nprintf 'almost {\"ok\":true}'\n",
            "invalid JSON output",
            AuditEventStatus::Failure,
        ),
        (
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":\"three\"}}\\n'\n",
            "violates its output_schema",
            AuditEventStatus::Failure,
        ),
    ] {
        let fixture = PluginFixture::new();
        let source = fixture.write_plugin(
            PluginSpecFixture::new("demo", "demo")
                .with_backend(backend)
                .with_output_schema(
                    "        type: object\n        required: [count]\n        properties:\n          count: { type: integer }\n",
                ),
        );
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
        // The call returning `Err` *is* the no-partial-success property:
        // there is no value for the caller to act on, whichever way the
        // backend failed. The diagnostic may quote the offending value.
        let error = fixture
            .call(&runtime, "demo.hello")
            .expect_err("the backend failed")
            .to_string();
        assert!(error.contains(expected), "{error}");

        let events = runtime
            .list_audit_events(None, Some("demo.hello".to_string()), None, None, 10)
            .expect("audit events");
        let event = events.first().expect("the failed call was audited");
        assert_eq!(event.status, status);
        let plugin = event
            .plugin
            .as_ref()
            .expect("the audit row names the plugin");
        assert_eq!(plugin.name, "demo");
        assert_eq!(plugin.manifest_digest.len(), 64);
    }
}

/// A refusal before the backend starts is audited as `Denied`, still naming
/// the plugin.
#[cfg(unix)]
#[test]
fn an_ungranted_plugin_call_is_audited_as_denied_with_plugin_provenance() {
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

#[test]
fn a_pinned_but_uninstalled_plugin_is_reported_without_breaking_the_runtime() {
    let fixture = PluginFixture::new();
    fixture.write_pin_file(
        "schemaVersion: 1\nplugins:\n  - name: absent\n    source: /nowhere/absent\n    enabled: true\n",
    );

    let runtime = fixture.reopen();
    // Built-ins are untouched.
    runtime
        .show_tool("orbit.task.show")
        .expect("builtins still register");

    let summaries = list_plugins(&runtime).expect("list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].status, PluginStatus::Missing);
    assert!(summaries[0].pinned);
    let diagnostic = summaries[0].diagnostic.as_deref().unwrap_or_default();
    assert!(diagnostic.contains("orbit plugin sync"), "{diagnostic}");
    let doctor = plugin_doctor(&runtime).expect("doctor");
    assert!(
        doctor[0].message.contains("orbit plugin sync"),
        "{doctor:?}"
    );
}

#[test]
fn doctor_reports_an_unparseable_pin_file() {
    let fixture = PluginFixture::new();
    fixture.write_pin_file("schemaVersion: 1\nplugins:\n  - name: graph\n    version: invalid\n");

    let findings = plugin_doctor(&fixture.reopen()).expect("doctor");
    assert!(
        findings.iter().any(|finding| {
            finding.plugin == "pin file"
                && finding.status == PluginStatus::Inactive
                && finding.message.contains("invalid")
        }),
        "doctor must report the invalid pin file: {findings:?}"
    );
}

fn relative_inventory(root: &Path) -> BTreeSet<String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read inventory") {
            let entry = entry.expect("entry");
            let file_type = entry.file_type().expect("file type");
            let relative = entry
                .path()
                .strip_prefix(root)
                .expect("inventory path is under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(relative);
            if file_type.is_dir() {
                walk(root, &entry.path(), out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

/// [ORB-12794] `git clone` writes the source URL (credentials included, when
/// present) into `.git/config`, and that root is the first entry in the
/// plugin backend's unconditional read set. The install must not carry the
/// clone's VCS metadata into the tree the backend can always read.
#[cfg(unix)]
#[test]
fn add_from_a_git_source_excludes_the_clone_metadata() {
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
}

#[test]
fn install_inventory_matches_the_source_tree() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let source_inventory = relative_inventory(&source);

    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    let installed = Path::new(&summary.install_path);
    assert_eq!(
        relative_inventory(installed),
        source_inventory,
        "install must copy the source tree and nothing else"
    );
}

#[cfg(unix)]
#[test]
fn add_refuses_a_source_with_a_symlink_to_a_file_outside_the_tree() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let secret = fixture.sources.join("outside-secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, source.join("leaked")).expect("symlink");

    let error = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect_err("an outside-tree symlink must be refused");
    let message = error.to_string();
    assert!(
        message.contains("leaked"),
        "refusal must name the offending entry: {message}"
    );
    assert!(
        message.contains("symbolic link"),
        "refusal must say why: {message}"
    );
    assert!(
        list_plugins(&fixture.runtime).expect("list").is_empty(),
        "the refusal must not record an install"
    );
    let plugins_root = fixture.global_root.join("plugins");
    if plugins_root.exists() {
        let listing = relative_inventory(&plugins_root);
        assert!(
            listing.iter().all(|path| {
                let bytes = std::fs::read(plugins_root.join(path)).unwrap_or_default();
                !bytes
                    .windows(b"SECRET-CONTENT-OUTSIDE-PLUGIN-TREE".len())
                    .any(|window| window == b"SECRET-CONTENT-OUTSIDE-PLUGIN-TREE")
            }),
            "no installed file may hold the outside-tree target: {listing:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_hand_edited_install_with_a_symlink_cannot_become_active() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    let summary = install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("install");

    let secret = fixture.sources.join("outside-secret");
    std::fs::write(&secret, "SECRET-CONTENT-OUTSIDE-PLUGIN-TREE").expect("secret");
    std::os::unix::fs::symlink(&secret, Path::new(&summary.install_path).join("env"))
        .expect("plant symlink in the install tree");

    let runtime = fixture.reopen();
    let shown = show_plugin(&runtime, "demo").expect("show");
    assert_eq!(shown.status, PluginStatus::Inactive);
    let diagnostic = shown.diagnostic.as_deref().unwrap_or_default();
    assert!(
        diagnostic.contains("symbolic link") || diagnostic.contains("env"),
        "load must name the planted link: {diagnostic}"
    );
    assert!(
        runtime.show_tool("demo.hello").is_err(),
        "a tree with a planted symlink must not register tools"
    );
}

/// Everything sitting in `~/.orbit/plugins/<ns>/`, by name.
fn namespace_entries(namespace_dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(namespace_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Two plugin sources at the same version, distinguishable by their backend,
/// each padded with enough files that a file-by-file copy into a live install
/// directory would be wide open to the observer in the test below.
#[cfg(unix)]
fn same_version_sources(fixture: &PluginFixture, first: &str, second: &str) -> (PathBuf, PathBuf) {
    let write = |dir: &str, backend: &str| {
        let root = fixture.write_plugin(PluginSpecFixture::new(dir, "demo").with_backend(backend));
        // Padding beside the backend, so one `read_dir` of `bin/` tells the
        // observer whether the whole tree is there.
        for index in 0..COPY_PADDING_FILES {
            std::fs::write(root.join(format!("bin/pad-{index}")), "x".repeat(4096))
                .expect("write padding file");
        }
        root
    };
    (write("first", first), write("second", second))
}

#[cfg(unix)]
const COPY_PADDING_FILES: usize = 300;

/// A forced same-version replace used to delete the live `<version>/` and copy
/// the new tree back into it file by file, so a concurrent `orbit` — a clock
/// tick, an MCP server, a dashboard panel — could load a `plugin.yaml` that was
/// already in place while `bin/backend.sh` was still being written, and execute
/// truncated bytes. The copy now lands in a staging directory and is published
/// with one rename, so a reader sees the whole old tree, the whole new one, or
/// — between renaming the old tree aside and renaming the new one in — no
/// install directory at all [ORB-12823].
///
/// The observer discards any sample that straddles the swap by bracketing it
/// with the install directory's inode, so what it reports is a tree that was
/// genuinely torn rather than one that was replaced mid-read.
#[cfg(unix)]
#[test]
fn a_forced_reinstall_never_exposes_a_half_copied_tree() {
    use std::os::unix::fs::MetadataExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const FIRST: &str = "#!/bin/sh\nprintf 'first\\n'\n";
    const SECOND: &str = "#!/bin/sh\nprintf 'second\\n'\n";
    // `bin/` holds the backend plus the padding files.
    let complete_bin_entries = COPY_PADDING_FILES + 1;

    let fixture = PluginFixture::new();
    let (first, second) = same_version_sources(&fixture, FIRST, SECOND);
    install_plugin(
        &fixture.runtime,
        first.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install the first tree");

    let install_path = fixture.global_root.join("plugins/demo/1.0.0");
    let stop = Arc::new(AtomicBool::new(false));
    let observer = std::thread::spawn({
        let install_path = install_path.clone();
        let stop = Arc::clone(&stop);
        move || {
            let inode = |path: &Path| std::fs::metadata(path).ok().map(|meta| meta.ino());
            let mut sampled = 0_u64;
            // A torn tree stays torn for as long as the copy runs, so a
            // handful of observations says everything the failure needs to.
            let mut torn: Vec<String> = Vec::new();
            let mut report = |what: String| {
                if torn.len() < 8 && !torn.contains(&what) {
                    torn.push(what);
                }
            };
            while !stop.load(Ordering::Relaxed) {
                // No install directory is a clean "not installed", not a tree
                // a reader could load half of.
                let Some(before) = inode(&install_path) else {
                    continue;
                };
                let manifest = install_path.join("plugin.yaml").exists();
                let backend = std::fs::read_to_string(install_path.join("bin/backend.sh"));
                let entries = std::fs::read_dir(install_path.join("bin"))
                    .map(|entries| entries.count())
                    .unwrap_or_default();
                if inode(&install_path) != Some(before) {
                    continue;
                }
                sampled += 1;
                if !manifest {
                    continue;
                }
                match backend {
                    Ok(bytes) if bytes == FIRST || bytes == SECOND => {}
                    Ok(bytes) => report(format!(
                        "a manifest beside a backend of {} bytes",
                        bytes.len()
                    )),
                    Err(error) => report(format!("a manifest with no readable backend: {error}")),
                }
                if entries != complete_bin_entries {
                    report(format!(
                        "a manifest beside {entries} of {complete_bin_entries} backend files"
                    ));
                }
            }
            (sampled, torn)
        }
    });

    install_plugin(
        &fixture.runtime,
        second.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            force: true,
            ..PluginAddOptions::default()
        },
    )
    .expect("replace the installed tree with the same version");
    stop.store(true, Ordering::Relaxed);
    let (sampled, torn) = observer.join().expect("observer thread");

    assert!(sampled > 0, "the observer never read the install directory");
    assert!(
        torn.is_empty(),
        "a concurrent reader saw a partly copied install tree: {torn:?}"
    );
    assert_eq!(
        std::fs::read_to_string(install_path.join("bin/backend.sh")).expect("read the backend"),
        SECOND,
        "the forced install replaced the tree"
    );
    assert_eq!(
        namespace_entries(&fixture.global_root.join("plugins/demo")),
        BTreeSet::from(["1.0.0".to_string()]),
        "neither the staged nor the replaced directory survives the swap"
    );
}

/// Each upgrade used to leave `plugins/<ns>/<oldversion>/` behind: readable to
/// every plugin backend, and enough to make a later `add` of that version
/// demand `--force`. `remove` then deleted only the recorded version and its
/// `remove_dir(parent)` silently failed, so the namespace family outlived the
/// plugin [ORB-12823].
#[test]
fn upgrades_prune_the_namespace_and_remove_takes_the_whole_family() {
    let fixture = PluginFixture::new();
    let namespace_dir = fixture.global_root.join("plugins/demo");
    for (dir, version) in [("v1", "1.0.0"), ("v2", "2.0.0"), ("v3", "3.0.0")] {
        let mut spec = PluginSpecFixture::new(dir, "demo");
        spec.version = version;
        let source = fixture.write_plugin(spec);
        install_plugin(
            &fixture.runtime,
            source.to_str().expect("utf8 path"),
            &PluginAddOptions::default(),
        )
        .expect("install");
        assert_eq!(
            namespace_entries(&namespace_dir),
            BTreeSet::from([version.to_string()]),
            "only the version the row names stays installed"
        );
    }

    // The version two upgrades ago is installable again without `--force`,
    // because nothing of it was left behind.
    let mut spec = PluginSpecFixture::new("v1-again", "demo");
    spec.version = "1.0.0";
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("a version an upgrade replaced installs again without --force");

    remove_plugin(&fixture.runtime, "demo", &PluginRemoveOptions::default()).expect("remove");
    assert!(
        !namespace_dir.exists(),
        "remove takes the namespace family, not just the recorded version"
    );
}

/// `current` was a second name for the install that nothing ever read, so it is
/// gone from the design: the `plugins` row's `install_path` is the only
/// authority (§3). A link an Orbit that still wrote one left behind is pruned
/// by the next `add` [ORB-12823].
#[cfg(unix)]
#[test]
fn add_writes_no_current_link_and_prunes_one_an_earlier_orbit_left() {
    let fixture = PluginFixture::new();
    let namespace_dir = fixture.global_root.join("plugins/demo");
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install");
    assert_eq!(
        namespace_entries(&namespace_dir),
        BTreeSet::from(["1.0.0".to_string()]),
        "the install writes the version directory and nothing beside it"
    );

    std::os::unix::fs::symlink(Path::new("1.0.0"), namespace_dir.join("current"))
        .expect("write the link an earlier Orbit kept");
    let mut spec = PluginSpecFixture::new("demo-next", "demo");
    spec.version = "1.1.0";
    let source = fixture.write_plugin(spec);
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("upgrade");
    assert_eq!(
        namespace_entries(&namespace_dir),
        BTreeSet::from(["1.1.0".to_string()]),
        "the stale link is pruned with the version it named"
    );
}

/// An install that failed after the copy used to leave the tree at
/// `<version>/` with no `plugins` row, so the next `add` of that version
/// demanded `--force` for a plugin this host never recorded [ORB-12823].
#[test]
fn an_install_that_fails_after_the_copy_leaves_no_tree_and_no_row() {
    let fixture = PluginFixture::new();
    let source = fixture.write_plugin(PluginSpecFixture::new("demo", "demo"));
    // Refused where it is refused today: after the tree is staged and
    // published, and before the row is written.
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            enable: true,
            grants: vec!["not-a-grant".to_string()],
            ..PluginAddOptions::default()
        },
    )
    .expect_err("an unparseable grant refuses the install");

    assert!(
        list_plugins(&fixture.runtime).expect("list").is_empty(),
        "the refusal recorded no plugin"
    );
    assert!(
        namespace_entries(&fixture.global_root.join("plugins/demo")).is_empty(),
        "the refusal left neither a tree nor staging behind"
    );
    install_plugin(
        &fixture.runtime,
        source.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("a plain `add` still installs, with no --force to clear first");
}

/// The same rollback under `--force`: the tree the operator still has a row
/// for has to survive an install that does not land [ORB-12823].
#[cfg(unix)]
#[test]
fn a_forced_replace_that_fails_puts_the_previous_tree_back() {
    const FIRST: &str = "#!/bin/sh\nprintf 'first\\n'\n";
    const SECOND: &str = "#!/bin/sh\nprintf 'second\\n'\n";

    let fixture = PluginFixture::new();
    let (first, second) = same_version_sources(&fixture, FIRST, SECOND);
    let installed = install_plugin(
        &fixture.runtime,
        first.to_str().expect("utf8 path"),
        &PluginAddOptions::default(),
    )
    .expect("install the first tree");

    install_plugin(
        &fixture.runtime,
        second.to_str().expect("utf8 path"),
        &PluginAddOptions {
            digest: None,
            force: true,
            enable: true,
            grants: vec!["not-a-grant".to_string()],
        },
    )
    .expect_err("an unparseable grant refuses the forced replace");

    let install_path = Path::new(&installed.install_path);
    assert_eq!(
        std::fs::read_to_string(install_path.join("bin/backend.sh")).expect("read the backend"),
        FIRST,
        "the tree the row still names is the one the operator installed"
    );
    assert_eq!(
        std::fs::read_dir(install_path.join("bin"))
            .expect("read the backend directory")
            .count(),
        COPY_PADDING_FILES + 1,
        "the restored tree is whole"
    );
    assert_eq!(
        namespace_entries(&fixture.global_root.join("plugins/demo")),
        BTreeSet::from(["1.0.0".to_string()]),
        "the rolled-back install left no staging or replaced directory"
    );
}
