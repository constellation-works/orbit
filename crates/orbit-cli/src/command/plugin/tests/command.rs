//! Plugin consent and secret-handling guards at the CLI layer.

use std::path::PathBuf;

use clap::Parser;
use orbit_core::OrbitRuntime;

use super::super::PluginSubcommand;
use super::super::scaffold::PluginScaffoldArgs;
use super::super::test::PluginTestArgs;
use crate::command::{Cli, CommandOutput, Commands, Execute};

#[cfg(unix)]
#[test]
fn plugin_test_refuses_an_unconfined_manifest_until_the_operator_accepts_it() {
    let fixture = PluginTestFixture::new();
    let plugin_dir = fixture.root.join("open");
    PluginScaffoldArgs {
        namespace: "open".to_string(),
        dir: Some(plugin_dir.clone()),
        force: false,
    }
    .execute(&fixture.runtime)
    .expect("scaffold a plugin");
    let manifest_path = plugin_dir.join(".orbit-plugin/plugin.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read scaffold manifest");
    let patched = manifest
        .replacen(
            "    timeout_ms: 30000\n",
            "    timeout_ms: 30000\n    sandbox: none\n",
            1,
        )
        .replacen(
            "  permissions:\n    network: none\n",
            "  permissions:\n    fs:\n      write: [\"/Users/daniel\"]\n    network: any\n    \
             env_pass: [\"HOME\"]\n",
            1,
        );
    assert_ne!(patched, manifest, "the scaffold manifest shape changed");
    std::fs::write(&manifest_path, patched).expect("write manifest");

    let refused = PluginTestArgs {
        dir: plugin_dir.clone(),
        first_party: false,
        grants: Vec::new(),
        accept_requested: false,
        case: None,
        update_goldens: false,
    }
    .execute(&fixture.runtime)
    .expect_err("an unconfined absolute-write manifest needs consent");
    let message = refused.to_string();
    for needle in [
        "Requested grants:",
        "unsandboxed",
        "/Users/daniel",
        "network (any)",
        "env_pass (HOME)",
        "--accept-requested",
        "--grant",
    ] {
        assert!(message.contains(needle), "missing {needle}: {message}");
    }

    // The refusal above names `/Users/daniel` and never starts the backend.
    // Consent has to use a directory this test owns. Absolute write roots
    // outside Orbit's materialization roots must already exist.
    let consented_write = fixture.root.join("consented-write");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read patched manifest");
    let consented = manifest.replace("/Users/daniel", &consented_write.display().to_string());
    std::fs::write(&manifest_path, consented).expect("point the write root at the fixture");

    let missing = PluginTestArgs {
        dir: plugin_dir.clone(),
        first_party: false,
        grants: Vec::new(),
        accept_requested: true,
        case: None,
        update_goldens: false,
    }
    .execute(&fixture.runtime)
    .expect("the suite reports the missing consented directory");
    assert_eq!(missing.exit_code(), 1);
    let CommandOutput::Payload(payload) = missing else {
        panic!("plugin test must return a report payload");
    };
    let (document, _) = payload.into_view();
    let detail = document["results"][0]["detail"]
        .as_str()
        .expect("the failed call has a diagnostic");
    assert!(detail.contains("does not exist"), "{detail}");
    assert!(
        detail.contains("create this consented directory before running the plugin"),
        "{detail}"
    );
    assert!(
        !consented_write.exists(),
        "the host must not create the manifest-named absolute write root"
    );

    std::fs::create_dir_all(&consented_write).expect("create the consented write root");
    let accepted = PluginTestArgs {
        dir: plugin_dir,
        first_party: false,
        grants: Vec::new(),
        accept_requested: true,
        case: None,
        update_goldens: false,
    }
    .execute(&fixture.runtime)
    .expect("accept-requested runs the requested profile");
    assert_eq!(accepted.exit_code(), 0);
    assert!(
        consented_write.is_dir(),
        "the consented absolute write root is opened"
    );
}

#[cfg(unix)]
struct PluginTestFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    runtime: OrbitRuntime,
}

#[cfg(unix)]
impl PluginTestFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let global = temp.path().join("global");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&global).expect("create global root");
        std::fs::create_dir_all(&workspace).expect("create workspace root");
        let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
        Self {
            root: temp.path().to_path_buf(),
            _temp: temp,
            runtime,
        }
    }
}

/// A value typed after the secret name must reach the command's own refusal,
/// never clap's "unexpected argument '<value>'" error, which would print it.
#[test]
fn a_secret_value_in_argv_is_captured_for_refusal_rather_than_echoed() {
    use super::super::PluginSecretSubcommand;

    for argv in [
        &[
            "orbit", "plugin", "secret", "set", "demo", "token", "hunter2",
        ][..],
        &[
            "orbit",
            "plugin",
            "secret",
            "set",
            "demo",
            "token",
            "--value=hunter2",
        ][..],
        &[
            "orbit", "plugin", "secret", "set", "demo", "token", "-v", "hunter2",
        ][..],
    ] {
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|error| {
            panic!("{argv:?} must parse so the command can refuse it: {error}")
        });
        let Commands::Plugin(command) = cli.command else {
            panic!("expected the plugin command");
        };
        let PluginSubcommand::Secret(secret) = command.command else {
            panic!("expected plugin secret");
        };
        let PluginSecretSubcommand::Set(args) = secret.command else {
            panic!("expected plugin secret set");
        };
        assert_eq!(
            (args.plugin.as_str(), args.name.as_str()),
            ("demo", "token")
        );
        assert!(!args.rejected.is_empty(), "{argv:?}");
    }
}
