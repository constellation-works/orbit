//! The CLI half of the plugin namespace rules: the reserved-command list in
//! `orbit-types` has to equal the real clap tree, which only this crate sees.

use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use orbit_core::OrbitRuntime;
use orbit_types::plugin::RESERVED_CLI_COMMANDS;

use super::super::PluginSubcommand;
use super::super::scaffold::PluginScaffoldArgs;
use super::super::test::PluginTestArgs;
use crate::command::{Cli, Commands, Execute};

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

    let cli = Cli::parse_from(["orbit", "plugin", "sync", "--dry-run"]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Sync(args) => assert!(args.dry_run),
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
        "--grant",
        "fs,unsandboxed",
        "--accept-requested",
    ]);
    match cli.command {
        Commands::Plugin(command) => match command.command {
            PluginSubcommand::Test(args) => {
                assert_eq!(args.dir, PathBuf::from("./demo"));
                assert_eq!(args.grants, ["fs", "unsandboxed"]);
                assert!(args.accept_requested);
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
        grants: Vec::new(),
        accept_requested: false,
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
        grants: Vec::new(),
        accept_requested: false,
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
    // Consent has to use a directory this test owns: a granted absolute write
    // root is created before the child runs.
    let consented_write = fixture.root.join("consented-write");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read patched manifest");
    let consented = manifest.replace("/Users/daniel", &consented_write.display().to_string());
    std::fs::write(&manifest_path, consented).expect("point the write root at the fixture");

    let accepted = PluginTestArgs {
        dir: plugin_dir,
        grants: Vec::new(),
        accept_requested: true,
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
