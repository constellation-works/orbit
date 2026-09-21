//! `orbit plugin test <dir>` (§5): the goldens a plugin ships, run through
//! the real protocol, and the certification that records what they passed on.

use std::path::{Path, PathBuf};

use super::fixture::PluginFixture;
use crate::application::plugin::{PluginAddOptions, install_plugin, test_plugin_dir};
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
    let report = test_plugin_dir(&runtime, &root).expect("run the conformance suite");

    assert!(report.passed(), "{:?}", report.results);
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
    let report = test_plugin_dir(&runtime, &root).expect("run the conformance suite");

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
    let report = test_plugin_dir(&runtime, &root).expect("run the conformance suite");

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

    let error = test_plugin_dir(&fixture.runtime, &root)
        .expect_err("a plugin with no goldens has nothing to certify");
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

    let error = test_plugin_dir(&fixture.runtime, Path::new(&root))
        .expect_err("a golden must name a declared tool");
    assert!(
        error.to_string().contains("greetings"),
        "the refusal names the tool: {error}"
    );
}
