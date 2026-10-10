//! `orbit plugin test <dir>` (§5): the goldens a plugin ships never reach
//! the host secret store or an unconsented write root.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::plugin::PLUGIN_DIR_NAME;

use super::fixture::PluginFixture;
use crate::OrbitRuntime;
use crate::application::plugin::{
    PluginAddOptions, PluginTestOptions, PluginTestReport, install_plugin, test_plugin_dir,
};

/// A plugin whose one tool answers deterministically, with a golden file
/// that either matches it or deliberately does not.
fn write_tested_plugin(
    fixture: &PluginFixture,
    namespace: &str,
    expected_subject: &str,
) -> PathBuf {
    let root = fixture.sources.join(namespace).join(PLUGIN_DIR_NAME);
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

/// A backend that overwrites `<dir>/sentinel` and fails its case when the
/// sandbox does not let it.
#[cfg(unix)]
fn write_sentinel_backend(root: &Path, dir: &Path) {
    std::fs::write(
        root.join("bin/backend.sh"),
        format!(
            r#"#!/bin/sh
cat > /dev/null
echo overwritten > "{}/sentinel" 2>/dev/null || {{ printf '{{"ok":false,"error":{{"code":"unwritable","message":"sentinel"}}}}\n'; exit 0; }}
printf '{{"ok":true,"output":{{"subject":"world"}}}}\n'
"#,
            dir.display()
        ),
    )
    .expect("write sentinel backend");
}

/// An existing host directory holding a `sentinel` file the plugin must not
/// reach without consent.
#[cfg(unix)]
fn external_dir_with_sentinel(fixture: &PluginFixture, name: &str) -> PathBuf {
    let dir = fixture.sources.join(name);
    std::fs::create_dir_all(&dir).expect("create external directory");
    std::fs::write(dir.join("sentinel"), "original").expect("write sentinel");
    dir
}

#[cfg(unix)]
fn sentinel(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("sentinel")).expect("read sentinel")
}

#[cfg(unix)]
#[test]
fn a_config_templated_write_root_outside_scratch_needs_fs_consent() {
    if !orbit_exec::macos_sandbox_test_guard(
        "a_config_templated_write_root_outside_scratch_needs_fs_consent",
    ) {
        return;
    }
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_config_templated_write_root_outside_scratch_needs_fs_consent",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let root = write_tested_plugin(&fixture, "configured", "world");
    let external = external_dir_with_sentinel(&fixture, "configured-output");
    write_sentinel_backend(&root, &external);
    patch_manifest(
        &root,
        "",
        &format!(
            "  config:\n    defaults: {{ output: \"{}\" }}\n  permissions:\n    fs:\n      \
             write: [\"{{{{config.output}}}}\"]\n",
            external.display()
        ),
    );

    let error = run(&fixture.runtime, &root)
        .expect_err("a template rendering to a host directory is not scratch-contained");
    assert_refusal_prints_requested_set(&error, &["{{config.output}}", "--grant fs"]);
    assert!(
        error.to_string().contains(&external.display().to_string()),
        "the refusal names where the root resolved: {error}"
    );
    assert_eq!(
        sentinel(&external),
        "original",
        "the refusal comes before the backend runs"
    );

    let report = run_with(
        &fixture.runtime,
        &root,
        PluginTestOptions {
            grants: vec!["fs".to_string()],
            ..PluginTestOptions::default()
        },
    )
    .expect("`--grant fs` consents to the resolved external root");
    assert!(report.passed(), "{:?}", report.results);
    assert_eq!(
        sentinel(&external),
        "overwritten\n",
        "the consented root is opened"
    );
}

/// A plugin declaring `api_token`, whose backend reports which of two known
/// values its request carried — the golden's fixture (at version `fixture`)
/// or the host's stored one — without echoing either.
fn write_secret_plugin(fixture: &PluginFixture, namespace: &str, golden: &str) -> PathBuf {
    let root = fixture.sources.join(namespace).join(PLUGIN_DIR_NAME);
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin dir");
    std::fs::create_dir_all(root.join("tests/conformance")).expect("create conformance dir");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ninput=$(cat)\nfixture=false\nhost=false\ncase \"$input\" in \
         *'\"api_token\":{\"value\":\"fixture-token-5a\",\"version\":\"fixture\"}'*) \
         fixture=true;; esac\ncase \"$input\" in *host-token-9f*) host=true;; esac\n\
         printf '{\"ok\":true,\"output\":{\"fixture\":%s,\"host\":%s}}\\n' \"$fixture\" \"$host\"\n",
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
             description: Secret conformance fixture.\nspec:\n  backend:\n    type: exec\n    \
             command: bin/backend.sh\n  tools:\n    - name: probe\n      description: Probe.\n      \
             execution_kind: read_only\n      mcp_scope: workspace\n  secrets:\n    - name: \
             api_token\n  tests: [tests/conformance/*.yaml]\n"
        ),
    )
    .expect("write manifest");
    std::fs::write(root.join("tests/conformance/probe.yaml"), golden).expect("write golden");
    root
}

/// Goldens supply fixture secrets through the test file, and a conformance
/// run delivers those — and never the host's stored value, even for the
/// installed plugin of the same name.
#[cfg(unix)]
#[test]
fn goldens_supply_fixture_secrets_and_the_host_store_is_never_read() {
    if !orbit_exec::macos_sandbox_test_guard(
        "goldens_supply_fixture_secrets_and_the_host_store_is_never_read",
    ) {
        return;
    }
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "goldens_supply_fixture_secrets_and_the_host_store_is_never_read",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let root = write_secret_plugin(
        &fixture,
        "conformsecret",
        "schemaVersion: 1\nkind: PluginTest\ntests:\n  - name: with_fixture\n    tool: probe\n    \
         secrets:\n      api_token: fixture-token-5a\n    expect:\n      output: { fixture: true, \
         host: false }\n  - name: without_fixture\n    tool: probe\n    expect:\n      output: { \
         fixture: false, host: false }\n",
    );
    install_plugin(
        &fixture.runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install the fixture plugin");
    crate::runtime::plugin::secrets::PluginSecretStore::new(&fixture.global_root)
        .put(
            "conformsecret",
            "api_token",
            &crate::runtime::plugin::secrets::PluginSecretValue::new("host-token-9f".to_string())
                .expect("valid value"),
        )
        .expect("the host holds a real value for the installed plugin");

    let report = run(&fixture.reopen(), &root).expect("run the conformance suite");

    assert!(
        report.passed(),
        "the fixture reaches the case that supplies it, the next case gets none, and the host \
         value reaches neither: {:?}",
        report.results
    );
}

/// A fixture for a name the manifest does not declare could never be
/// delivered, so the suite refuses to load rather than certify a case that
/// did not test what it says.
#[test]
fn a_golden_supplying_an_undeclared_secret_is_refused() {
    if !super::fixture::enter_isolated_child(
        module_path!(),
        "a_golden_supplying_an_undeclared_secret_is_refused",
    ) {
        return;
    }
    let fixture = PluginFixture::new();
    let root = write_secret_plugin(
        &fixture,
        "undeclaredsecret",
        "schemaVersion: 1\nkind: PluginTest\ntests:\n  - name: stray\n    tool: probe\n    \
         secrets:\n      other_token: x\n    expect:\n      output: {}\n",
    );

    let error = run(&fixture.runtime, &root).expect_err("an undeclared fixture name");
    assert!(
        error.to_string().contains("supplies secret 'other_token'")
            && error.to_string().contains("does not declare"),
        "{error}"
    );
}
