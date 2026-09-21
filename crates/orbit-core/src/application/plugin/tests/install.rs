use std::collections::BTreeSet;
use std::path::Path;

use orbit_types::plugin::PluginStatus;
use orbit_types::telemetry::AuditEventStatus;

use super::super::{
    PluginAddOptions, install_plugin, list_plugins, plugin_doctor, show_plugin, validate_plugin_dir,
};
use super::fixture::{PluginFixture, PluginSpecFixture, write_plugin_at};

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

#[cfg(unix)]
#[test]
fn an_enabled_plugin_tool_executes_through_audited_dispatch() {
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

/// A local git checkout, so `git+<url>` install exercises the real clone
/// path without a network fetch.
fn run_git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "orbit-test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.test")
        .env("GIT_COMMITTER_NAME", "orbit-test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.test")
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// [ORB-12794] `git clone` writes the source URL (credentials included, when
/// present) into `.git/config`, and that root is the first entry in the
/// plugin backend's unconditional read set. The install must not carry the
/// clone's VCS metadata into the tree the backend can always read.
#[test]
fn add_from_a_git_source_excludes_the_clone_metadata() {
    let fixture = PluginFixture::new();
    let repo = fixture.sources.join("git-origin");
    write_plugin_at(&repo, PluginSpecFixture::new("demo", "demo"));
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "fixture plugin"]);
    assert!(
        repo.join(".git").is_dir(),
        "fixture source must itself be a git checkout"
    );

    let summary = install_plugin(
        &fixture.runtime,
        &format!("git+{}", repo.display()),
        &PluginAddOptions::default(),
    )
    .expect("install from a local git source");

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
