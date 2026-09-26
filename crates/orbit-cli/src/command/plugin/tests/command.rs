//! The CLI half of the plugin namespace rules: the reserved-command list in
//! `orbit-types` has to equal the real clap tree, which only this crate sees.

use std::path::PathBuf;

use clap::{CommandFactory, Parser, error::ErrorKind};
use orbit_core::{OrbitRuntime, adapter::command::PluginPermissionChange};
use orbit_types::plugin::{PluginGrant, RESERVED_CLI_COMMANDS};

use super::super::PluginSubcommand;
use super::super::scaffold::PluginScaffoldArgs;
use super::super::test::PluginTestArgs;
use crate::command::{Cli, CommandOutput, Commands, Execute};

#[test]
fn reserved_cli_commands_match_the_shipped_command_tree() {
    let mut shipped: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_string())
        .collect();
    shipped.sort();
    let mut reserved: Vec<String> = RESERVED_CLI_COMMANDS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    reserved.sort();

    // `help` is reserved but is not a `Commands` variant, so compare the
    // shipped tree against the reserved list minus that one entry.
    reserved.retain(|name| name != "help");
    assert_eq!(
        shipped, reserved,
        "orbit_types::plugin::RESERVED_CLI_COMMANDS drifted from the clap tree. A plugin \
         namespace equal to a command name would shadow it, so add or remove the name there \
         in the same change that adds or removes the command"
    );
}

#[test]
fn cli_parses_the_plugin_lifecycle() {
    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "add",
        "./demo",
        "--enable",
        "--grant",
        "fs,network",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Add(args) => {
                assert_eq!(args.source, "./demo");
                assert!(args.enable && !args.force);
                assert_eq!(args.grants, ["fs", "network"]);
            }
            _ => panic!("expected plugin add"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "upgrade",
        "demo",
        "git+https://example.test/demo#v2",
        "--grant",
        "fs,orbit_tools",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Upgrade(args) => {
                assert_eq!(args.name, "demo");
                assert_eq!(
                    args.source.as_deref(),
                    Some("git+https://example.test/demo#v2")
                );
                assert_eq!(args.grants, ["fs", "orbit_tools"]);
            }
            _ => panic!("expected plugin upgrade"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "enable",
        "demo",
        "--grant",
        "network,orbit_tools",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Enable(args) => {
                assert_eq!(args.name, "demo");
                assert_eq!(args.grants, ["network", "orbit_tools"]);
            }
            _ => panic!("expected plugin enable"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from(["orbit", "plugin", "sync", "--grant", "fs,orbit_tools"]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Sync(args) => {
                assert!(!args.dry_run);
                assert_eq!(args.grants, ["fs", "orbit_tools"]);
            }
            _ => panic!("expected plugin sync"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from(["orbit", "plugin", "remove", "demo", "--yes"]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Remove(args) => {
                assert_eq!(args.name, "demo");
                assert!(args.yes);
                assert!(
                    !args.record_only,
                    "ordinary removal deletes the install it recorded"
                );
            }
            _ => panic!("expected plugin remove"),
        },
        _ => panic!("expected the plugin command"),
    }

    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "migrate",
        "./bin/tool",
        "--name",
        "graph",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Migrate(args) => {
                assert_eq!(args.binary, "./bin/tool");
                assert_eq!(args.name.as_deref(), Some("graph"));
                assert_eq!(args.version, "0.1.0");
            }
            _ => panic!("expected plugin migrate"),
        },
        _ => panic!("expected the plugin command"),
    }
}

#[test]
fn plugin_add_rejects_grants_without_enable() {
    let error = match Cli::try_parse_from(["orbit", "plugin", "add", "./demo", "--grant", "fs"]) {
        Ok(_) => panic!("--grant without --enable must fail in clap"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    assert!(error.to_string().contains("--enable"), "{error}");
}

#[test]
fn plugin_upgrade_human_output_prints_the_requested_permission_diff() {
    let change = PluginPermissionChange {
        grant: PluginGrant::Fs,
        previous: Some("write={{plugin_state}}".to_string()),
        requested: Some("write={{workspace}}".to_string()),
        widened: true,
    };

    let text = super::super::upgrade::format_permission_change(&change);
    assert!(
        text.contains("fs: write={{plugin_state}} -> write={{workspace}} (widened)"),
        "{text}"
    );
}

#[test]
fn plugin_enable_help_says_an_explicit_grant_list_replaces() {
    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("plugin")
        .expect("plugin command")
        .find_subcommand_mut("enable")
        .expect("plugin enable command")
        .render_long_help()
        .to_string();
    assert!(
        help.contains("Replaces the recorded set when present")
            && help.contains("omitting --grant preserves it"),
        "plugin enable help must distinguish replacement from omission:\n{help}"
    );

    let mut command = Cli::command();
    let help = command
        .find_subcommand_mut("plugin")
        .expect("plugin command")
        .find_subcommand_mut("add")
        .expect("plugin add command")
        .render_long_help()
        .to_string();
    assert!(
        help.contains("Complete permission grant set")
            && help.contains("Replaces any recorded set"),
        "plugin add help must state the same replacement semantics:\n{help}"
    );
}

#[test]
fn cli_parses_plugin_test_consent_flags() {
    let cli = Cli::parse_from([
        "orbit",
        "plugin",
        "test",
        "./demo",
        "--first-party",
        "--grant",
        "fs,unsandboxed",
        "--accept-requested",
        "--case",
        "status_reports_ready",
        "--update-goldens",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Test(args) => {
                assert_eq!(args.dir, PathBuf::from("./demo"));
                assert!(args.first_party);
                assert_eq!(args.grants, ["fs", "unsandboxed"]);
                assert!(args.accept_requested);
                assert_eq!(args.case.as_deref(), Some("status_reports_ready"));
                assert!(args.update_goldens);
            }
            _ => panic!("expected plugin test"),
        },
        _ => panic!("expected the plugin command"),
    }
}

#[cfg(unix)]
#[test]
fn scaffolded_plugin_passes_plugin_test_with_no_flags() {
    let fixture = PluginTestFixture::new();
    let plugin_dir = fixture.root.join("demo");
    PluginScaffoldArgs {
        namespace: "demo".to_string(),
        dir: Some(plugin_dir.clone()),
        force: false,
    }
    .execute(&fixture.runtime)
    .expect("scaffold a plugin");

    let output = PluginTestArgs {
        dir: plugin_dir,
        first_party: false,
        grants: Vec::new(),
        accept_requested: false,
        case: None,
        update_goldens: false,
    }
    .execute(&fixture.runtime)
    .expect("the scaffolded plugin passes with no flags");
    assert_eq!(output.exit_code(), 0);
}

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
    let manifest_path = plugin_dir.join("plugin.yaml");
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
