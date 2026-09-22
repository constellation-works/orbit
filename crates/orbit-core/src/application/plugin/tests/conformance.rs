//! `orbit plugin test <dir>` (§5): the goldens a plugin ships, run through
//! the real protocol, and the certification that records what they passed on.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::OrbitError;

use super::fixture::PluginFixture;
use crate::OrbitRuntime;
use crate::application::plugin::{
    PluginAddOptions, PluginTestOptions, PluginTestReport, install_plugin, test_plugin_dir,
};
use crate::runtime::plugin_host::host_version;

/// A plugin whose one tool answers deterministically, with a golden file
/// that either matches it or deliberately does not.
fn write_tested_plugin(
    fixture: &PluginFixture,
    namespace: &str,
    expected_subject: &str,
) -> PathBuf {
    let root = fixture.sources.join(namespace);
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin dir");
    std::fs::create_dir_all(root.join("tests/conformance")).expect("create conformance dir");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"ok\":true,\"output\":{\"subject\":\"world\"}}\\n'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 1.0.0\n  \
             description: Conformance fixture.\nspec:\n  backend:\n    type: exec\n    command: \
             bin/backend.sh\n  tools:\n    - name: greet\n      description: Greet.\n      \
             execution_kind: read_only\n      mcp_scope: workspace\n  tests: \
             [tests/conformance/*.yaml]\n"
        ),
    )
    .expect("write manifest");
    std::fs::write(
        root.join("tests/conformance/greet.yaml"),
        format!(
            "schemaVersion: 1\nkind: PluginTest\ntests:\n  - name: greet_answers\n    tool: \
             greet\n    input: {{}}\n    expect:\n      output:\n        subject: \
             {expected_subject}\n"
        ),
    )
    .expect("write golden");
    root
}

#[cfg(unix)]
#[test]
fn a_passing_suite_certifies_the_installed_plugin_for_this_orbit() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "conform", "world");
    install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install the fixture plugin");

    let runtime = fixture.reopen();
    let report = run(&runtime, &root).expect("run the conformance suite");

    assert!(report.passed(), "{:?}", report.results);
    assert_eq!(report.requested_grants, "none");
    assert_eq!(report.orbit_version, host_version().to_string());
    assert!(
        report.certified,
        "a passing suite records the version: {}",
        report.certification_note
    );

    let recorded = runtime
        .stores()
        .plugins()
        .get_plugin("conform")
        .expect("read the plugin record")
        .expect("the plugin is installed");
    assert_eq!(
        recorded.certified_orbit_version.as_deref(),
        Some(host_version().to_string().as_str()),
        "`orbit plugin show` reads the certification from the record"
    );
}

#[cfg(unix)]
#[test]
fn a_directory_with_a_first_party_origin_remote_is_refused() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "firstparty", "world");
    mark_first_party_source(&root);
    let refused = install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect_err("a directory origin remote cannot verify a first-party claim");
    let diagnostic = refused.to_string();
    assert!(
        diagnostic.contains("metadata.origin")
            && diagnostic.contains("claims the reserved `orbit.firstparty.*` namespace")
            && diagnostic.contains("plugin source is not a constellation-works repository"),
        "the existing first-party diagnostic remains visible: {diagnostic}"
    );
}

#[cfg(unix)]
#[test]
fn an_uninstalled_first_party_suite_requires_the_explicit_flag() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "uninstalled", "world");
    mark_first_party_manifest(&root);

    let refused = run(&fixture.runtime, &root).expect_err("unverified origin must refuse");
    assert!(
        refused.to_string().contains("claims the reserved"),
        "the existing origin diagnostic remains visible: {refused}"
    );

    let report = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: true,
            ..PluginTestOptions::default()
        },
    )
    .expect("the explicit first-party flag runs the goldens");
    assert!(report.passed(), "{:?}", report.results);
    assert!(!report.certified, "{}", report.certification_note);
    assert!(
        report.certification_note.contains("not installed"),
        "an uninstalled directory cannot record a certification: {}",
        report.certification_note
    );
}

#[cfg(unix)]
#[test]
fn a_wrong_expected_output_fails_and_names_the_test() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "wrong", "mars");
    install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install the fixture plugin");

    let runtime = fixture.reopen();
    let report = run(&runtime, &root).expect("run the conformance suite");

    assert!(!report.passed());
    assert_eq!(report.failures(), ["greet_answers"]);
    let failure = &report.results[0];
    assert!(
        failure.detail.contains("mars") && failure.detail.contains("world"),
        "the failure states what was expected and what came back: {}",
        failure.detail
    );
    assert!(
        !report.certified,
        "a failing suite certifies nothing: {}",
        report.certification_note
    );
    assert!(
        runtime
            .stores()
            .plugins()
            .get_plugin("wrong")
            .expect("read the plugin record")
            .expect("the plugin is installed")
            .certified_orbit_version
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn a_directory_that_is_not_the_installed_tree_is_not_certified() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "drifted", "world");
    install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install the fixture plugin");

    // Edit the source tree after installing: the suite still passes, but it
    // passed against bytes this host does not run.
    let manifest = std::fs::read_to_string(root.join("plugin.yaml")).expect("read manifest");
    std::fs::write(
        root.join("plugin.yaml"),
        manifest.replace("Conformance fixture.", "Edited after install."),
    )
    .expect("edit manifest");

    let runtime = fixture.reopen();
    let report = run(&runtime, &root).expect("run the conformance suite");

    assert!(report.passed());
    assert!(!report.certified);
    assert!(
        report.certification_note.contains("manifest digest"),
        "the note explains what to do: {}",
        report.certification_note
    );
}

#[cfg(unix)]
#[test]
fn a_plugin_with_no_goldens_cannot_be_certified() {
    let fixture = PluginFixture::new();
    let root = fixture.sources.join("bare");
    super::fixture::write_plugin_at(
        &root,
        super::fixture::PluginSpecFixture::new("bare", "bare"),
    );

    let error =
        run(&fixture.runtime, &root).expect_err("a plugin with no goldens has nothing to certify");
    assert!(
        error.to_string().contains("spec.tests"),
        "the refusal names the manifest key: {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_golden_naming_an_undeclared_tool_refuses_the_directory() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "typo", "world");
    let golden = root.join("tests/conformance/greet.yaml");
    let contents = std::fs::read_to_string(&golden).expect("read golden");
    std::fs::write(&golden, contents.replace("tool: greet", "tool: greetings"))
        .expect("write golden");

    let error =
        run(&fixture.runtime, Path::new(&root)).expect_err("a golden must name a declared tool");
    assert!(
        error.to_string().contains("greetings"),
        "the refusal names the tool: {error}"
    );
}

fn run(runtime: &OrbitRuntime, root: &Path) -> Result<PluginTestReport, OrbitError> {
    test_plugin_dir(runtime, root, &PluginTestOptions::default())
}

fn run_with(
    runtime: &OrbitRuntime,
    root: &Path,
    options: PluginTestOptions,
) -> Result<PluginTestReport, OrbitError> {
    test_plugin_dir(runtime, root, &options)
}

#[cfg(unix)]
fn mark_first_party_source(root: &Path) {
    mark_first_party_manifest(root);
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .expect("run git init");
    assert!(status.success(), "git init must succeed");
    let status = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://github.com/constellation-works/fixture.git",
        ])
        .current_dir(root)
        .status()
        .expect("add first-party origin");
    assert!(status.success(), "git remote add must succeed");
}

#[cfg(unix)]
fn mark_first_party_manifest(root: &Path) {
    let path = root.join("plugin.yaml");
    let manifest = std::fs::read_to_string(&path).expect("read manifest");
    let patched = manifest.replacen(
        "  version: 1.0.0\n",
        "  version: 1.0.0\n  publisher: constellation-works\n  origin: orbit\n",
        1,
    );
    assert_ne!(patched, manifest, "the fixture manifest shape changed");
    std::fs::write(path, patched).expect("mark first-party manifest");
}

/// Insert backend lines and a `permissions` block into a fixture manifest.
fn patch_manifest(root: &Path, backend_extra: &str, permissions: &str) {
    let path = root.join("plugin.yaml");
    let text = std::fs::read_to_string(&path).expect("read manifest");
    let patched = text.replacen(
        "    command: bin/backend.sh\n",
        &format!("    command: bin/backend.sh\n{backend_extra}{permissions}"),
        1,
    );
    assert_ne!(patched, text, "the fixture manifest shape changed");
    std::fs::write(&path, patched).expect("write manifest");
}

fn assert_refusal_prints_requested_set(error: &OrbitError, expected: &[&str]) {
    let message = error.to_string();
    assert!(
        message.contains("Requested grants:"),
        "the refusal prints the requested set: {message}"
    );
    for needle in expected {
        assert!(
            message.contains(needle),
            "the refusal should mention {needle}: {message}"
        );
    }
}

#[cfg(unix)]
#[test]
fn consent_required_requests_refuse_without_a_grant_or_accept_requested() {
    let fixture = PluginFixture::new();
    let cases = [
        (
            "nonebox",
            "    sandbox: none\n",
            "",
            &[
                "unsandboxed",
                "backend.sandbox: none",
                "--grant unsandboxed",
            ][..],
        ),
        (
            "absroot",
            "",
            "  permissions:\n    fs:\n      write: [\"/Users/daniel\"]\n",
            &["/Users/daniel", "fs (", "--grant fs"],
        ),
        (
            "wide",
            "",
            "  permissions:\n    network: any\n",
            &["network (any)", "--grant network"],
        ),
        (
            "passed",
            "",
            "  permissions:\n    env_pass: [\"HOME\"]\n",
            &["env_pass (HOME)", "--grant env_pass"],
        ),
    ];
    for (name, backend, permissions, needles) in cases {
        let root = write_tested_plugin(&fixture, name, "world");
        patch_manifest(&root, backend, permissions);
        let error = run(&fixture.runtime, &root).expect_err(name);
        assert_refusal_prints_requested_set(&error, needles);
        assert!(
            error.to_string().contains("--accept-requested"),
            "{name}: {error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_manifest_with_every_consent_required_request_prints_the_whole_set() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "allgrants", "world");
    patch_manifest(
        &root,
        "    sandbox: none\n",
        "  permissions:\n    fs:\n      write: [\"/Users/daniel\"]\n    network: any\n    \
         env_pass: [\"HOME\"]\n",
    );

    let error = run(&fixture.runtime, &root).expect_err("every dangerous request needs consent");
    assert_refusal_prints_requested_set(
        &error,
        &[
            "fs (",
            "/Users/daniel",
            "network (any)",
            "env_pass (HOME)",
            "unsandboxed",
            "--grant fs,network,env_pass,unsandboxed",
        ],
    );
}

#[cfg(unix)]
#[test]
fn a_grant_list_that_omits_a_dangerous_request_still_refuses() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "half", "world");
    patch_manifest(
        &root,
        "    sandbox: none\n",
        "  permissions:\n    network: any\n",
    );

    let error = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: false,
            grants: vec!["unsandboxed".to_string()],
            accept_requested: false,
        },
    )
    .expect_err("network: any was not named in --grant");
    assert_refusal_prints_requested_set(
        &error,
        &["network (any)", "unsandboxed", "--grant network"],
    );
    assert!(
        !error.to_string().contains("--grant unsandboxed"),
        "the hint names only the grants still missing: {error}"
    );
}

#[cfg(unix)]
#[test]
fn an_unknown_grant_name_is_refused_before_the_run() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "typoed", "world");

    let error = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: false,
            grants: vec!["wifi".to_string()],
            accept_requested: true,
        },
    )
    .expect_err("an unknown --grant name is not consent");
    let message = error.to_string();
    assert!(
        message.contains("wifi") && message.contains("unknown grant"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn template_writes_and_loopback_run_without_consent() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "bounded", "world");
    patch_manifest(
        &root,
        "",
        "  permissions:\n    fs:\n      write: [\"{{plugin_state}}\", \"{{workspace}}/out\"]\n    \
         network: loopback\n",
    );

    let report = run(&fixture.runtime, &root).expect("bounded requests need no extra consent");
    assert!(report.passed(), "{:?}", report.results);
    assert!(
        report.requested_grants.contains("{{plugin_state}}"),
        "{}",
        report.requested_grants
    );
    assert!(
        report.requested_grants.contains("loopback"),
        "{}",
        report.requested_grants
    );
    assert!(!report.requested_grants.contains("unsandboxed"));
}

#[cfg(unix)]
#[test]
fn accept_requested_keeps_the_certification_rule() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "open", "world");
    patch_manifest(&root, "    sandbox: none\n", "");
    install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install the fixture plugin");
    let runtime = fixture.reopen();

    let refused = run(&runtime, &root).expect_err("unsandboxed needs consent");
    assert_refusal_prints_requested_set(&refused, &["unsandboxed"]);
    assert!(
        runtime
            .stores()
            .plugins()
            .get_plugin("open")
            .expect("read the plugin record")
            .expect("the plugin is installed")
            .certified_orbit_version
            .is_none(),
        "a refusal writes no certification"
    );

    let accepted = PluginTestOptions {
        first_party: false,
        grants: Vec::new(),
        accept_requested: true,
    };
    let report = run_with(&runtime, &root, accepted.clone()).expect("the requested profile runs");
    assert!(report.passed(), "{:?}", report.results);
    assert!(
        report.certified,
        "a passing consented suite records the version: {}",
        report.certification_note
    );
    assert_eq!(
        runtime
            .stores()
            .plugins()
            .get_plugin("open")
            .expect("read the plugin record")
            .expect("the plugin is installed")
            .certified_orbit_version
            .as_deref(),
        Some(host_version().to_string().as_str())
    );

    let manifest = std::fs::read_to_string(root.join("plugin.yaml")).expect("read manifest");
    std::fs::write(
        root.join("plugin.yaml"),
        manifest.replace("Conformance fixture.", "Edited after install."),
    )
    .expect("edit manifest");
    let drifted = run_with(&runtime, &root, accepted).expect("the suite still passes");
    assert!(drifted.passed());
    assert!(!drifted.certified);
    assert!(
        drifted.certification_note.contains("manifest digest"),
        "the note explains what to do: {}",
        drifted.certification_note
    );
    assert_eq!(
        runtime
            .stores()
            .plugins()
            .get_plugin("open")
            .expect("read the plugin record")
            .expect("the plugin is installed")
            .certified_orbit_version
            .as_deref(),
        Some(host_version().to_string().as_str()),
        "a drifted directory does not replace the certification of the installed tree"
    );
}

#[cfg(unix)]
#[test]
fn grant_names_that_cover_the_request_run_the_requested_profile() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "named", "world");
    let write_root = fixture.sources.join("named-write");
    patch_manifest(
        &root,
        "    sandbox: none\n",
        &format!(
            "  permissions:\n    fs:\n      write: [\"{}\"]\n    network: any\n    env_pass: \
             [\"HOME\"]\n",
            write_root.display()
        ),
    );

    let refused = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: false,
            grants: vec![
                "fs".to_string(),
                "network".to_string(),
                "env_pass".to_string(),
                "unsandboxed".to_string(),
            ],
            accept_requested: false,
        },
    )
    .expect("the suite reports a refused call");
    assert!(!refused.passed(), "an absent host write root must fail");
    let message = &refused.results[0].detail;
    assert!(
        message.contains(&write_root.display().to_string()),
        "{message}"
    );
    assert!(message.contains("does not exist"), "{message}");
    assert!(
        message.contains("create this consented directory before running the plugin"),
        "{message}"
    );
    assert!(
        !write_root.exists(),
        "the host must not create a manifest-named absolute write root"
    );

    std::fs::create_dir_all(&write_root).expect("create the consented write root");
    let report = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: false,
            grants: vec![
                "fs".to_string(),
                "network".to_string(),
                "env_pass".to_string(),
                "unsandboxed".to_string(),
            ],
            accept_requested: false,
        },
    )
    .expect("a covering --grant list runs the requested profile");
    assert!(report.passed(), "{:?}", report.results);
    assert!(
        write_root.is_dir(),
        "the consented absolute write root is opened"
    );
    assert!(
        report.requested_grants.contains("unsandboxed"),
        "{}",
        report.requested_grants
    );
    assert!(
        report.requested_grants.contains("network (any)"),
        "{}",
        report.requested_grants
    );
    assert!(
        report.requested_grants.contains("env_pass (HOME)"),
        "{}",
        report.requested_grants
    );
}

#[cfg(unix)]
#[test]
fn accept_requested_still_refuses_a_write_root_that_covers_the_global_root() {
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "cover", "world");
    patch_manifest(&root, "", "  permissions:\n    fs:\n      write: [\"/\"]\n");

    let error = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            first_party: false,
            grants: Vec::new(),
            accept_requested: true,
        },
    )
    .expect_err("consent does not lift the global-root write refusal");
    assert!(error.to_string().contains("global root"), "{error}");
}
