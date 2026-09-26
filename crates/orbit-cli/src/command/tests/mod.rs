// Content moved from inline #[cfg(test)] mod tests in command/mod.rs per ORB-00221.
// tests/mod.rs can directly contain tests for the declaring parent module (exempt from orphan rules).

mod auto_task;
mod doctor;
mod friction;
mod gc;
mod init;
mod locks;
mod operation;
mod operation_args;
mod search;
mod sweep;

use std::path::Path;

use clap::{Command, CommandFactory, Parser, error::ErrorKind};

use super::{Cli, Commands, mcp::McpSubcommand, search::SearchSubcommand, web::WebSubcommand};

const UPDATE_HELP_GOLDENS_ENV: &str = "ORBIT_UPDATE_HELP_GOLDENS";

fn assert_cli_rejects(args: &[&str], kind: ErrorKind, expected: &str) {
    let error = match Cli::try_parse_from(args.iter().copied()) {
        Ok(_) => panic!("form should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), kind, "{error}");
    let message = error.to_string();
    assert!(message.contains(expected), "{message}");
}

fn contains_concrete_artifact_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    let has_digits_after = |prefix: &[u8]| {
        bytes.windows(prefix.len() + 1).any(|window| {
            window[..prefix.len()] == *prefix && window[prefix.len()].is_ascii_digit()
        })
    };
    has_digits_after(b"ORB-")
        || has_digits_after(b"ADR-")
        || has_digits_after(b"L-")
        || bytes.windows(12).any(|window| {
            window[0] == b'F'
                && window[1..5].iter().all(u8::is_ascii_digit)
                && window[5] == b'-'
                && window[6..8].iter().all(u8::is_ascii_digit)
                && window[8] == b'-'
                && window[9..12].iter().all(u8::is_ascii_digit)
        })
}

fn assert_help_tree_has_no_concrete_artifact_ids(command: &Command) {
    let help = command.clone().render_long_help().to_string();
    assert!(
        !contains_concrete_artifact_id(&help),
        "help for `{}` contains a concrete workspace-local artifact ID:\n{help}",
        command.get_name()
    );
    for subcommand in command.get_subcommands() {
        assert_help_tree_has_no_concrete_artifact_ids(subcommand);
    }
}

/// Render `--help` for an argv prefix, exactly as the binary prints it.
fn help_for(args: &[&str]) -> String {
    let mut argv = args.to_vec();
    argv.push("--help");
    match Cli::try_parse_from(argv) {
        Ok(_) => panic!("--help exits before parsing"),
        Err(error) => error.to_string(),
    }
}

/// Compare rendered help against a checked-in golden, or overwrite it when
/// `ORBIT_UPDATE_HELP_GOLDENS=1`.
fn assert_help_matches_golden(args: &[&str], relative: &str, expected: &str) {
    let actual = help_for(args);
    if std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1") {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/command/tests")
            .join(relative);
        std::fs::write(&path, &actual)
            .unwrap_or_else(|err| panic!("write help golden {}: {err}", path.display()));
        return;
    }
    assert_eq!(
        actual,
        expected,
        "`{} --help` drifted from {relative}. If the new help is intentional, regenerate with \
         `{UPDATE_HELP_GOLDENS_ENV}=1 cargo test -p orbit-cli --bin orbit help_matches_the_shipped_surface` \
         or `make goldens UPDATE=1`, then review the diff.",
        args.join(" ")
    );
}

#[test]
fn plugin_help_matches_the_shipped_surface() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["orbit", "plugin"],
            "plugin_help/root.txt",
            include_str!("plugin_help/root.txt"),
        ),
        (
            &["orbit", "plugin", "upgrade"],
            "plugin_help/upgrade.txt",
            include_str!("plugin_help/upgrade.txt"),
        ),
        (
            &["orbit", "plugin", "remove"],
            "plugin_help/remove.txt",
            include_str!("plugin_help/remove.txt"),
        ),
        (
            &["orbit", "plugin", "validate"],
            "plugin_help/validate.txt",
            include_str!("plugin_help/validate.txt"),
        ),
        (
            &["orbit", "plugin", "scaffold"],
            "plugin_help/scaffold.txt",
            include_str!("plugin_help/scaffold.txt"),
        ),
        (
            &["orbit", "plugin", "test"],
            "plugin_help/test.txt",
            include_str!("plugin_help/test.txt"),
        ),
    ];
    for (args, relative, expected) in cases {
        assert_help_matches_golden(args, relative, expected);
    }
}

#[test]
fn mcp_listen_help_matches_the_shipped_surface() {
    assert_help_matches_golden(
        &["orbit", "mcp", "listen"],
        "mcp_help/listen.txt",
        include_str!("mcp_help/listen.txt"),
    );
}

/// `orbit --help`'s sections, in order, each with the commands it lists.
fn root_help_sections(help: &str) -> Vec<(String, Vec<String>)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in help.lines() {
        if let Some(heading) = line.strip_suffix(':').filter(|_| !line.starts_with(' ')) {
            sections.push((heading.to_string(), Vec::new()));
        } else if let (Some(name), Some((_, commands))) = (
            line.strip_prefix("  ")
                .and_then(|row| row.split_whitespace().next()),
            sections.last_mut(),
        ) {
            commands.push(name.to_string());
        }
    }
    sections.retain(|(heading, _)| heading != "Options");
    sections
}

#[test]
fn root_help_groups_every_visible_command_exactly_once() {
    let help = help_for(&["orbit"]);
    let sections = root_help_sections(&help);
    let expected: &[(&str, &[&str])] = &[
        (
            "Environment",
            &["init", "workspace", "config", "plugin", "migrate", "update"],
        ),
        ("Knowledge", &["task", "friction", "search"]),
        ("Operate", &["run", "job", "tool", "gc"]),
        ("Observe", &["audit", "log", "doctor"]),
        ("Scheduler", &["clock", "sweep", "routine", "auto-task"]),
        ("Services", &["mcp", "web"]),
    ];
    let expected: Vec<(String, Vec<String>)> = expected
        .iter()
        .map(|(heading, commands)| {
            (
                (*heading).to_string(),
                commands.iter().map(ToString::to_string).collect(),
            )
        })
        .collect();
    assert_eq!(sections, expected, "{help}");

    // The template is hand-rolled, so a visible command missing from it
    // would silently vanish from `orbit --help`.
    let listed: Vec<&String> = sections.iter().flat_map(|(_, names)| names).collect();
    for subcommand in Cli::command().get_subcommands() {
        if subcommand.is_hide_set() {
            continue;
        }
        let name = subcommand.get_name().to_string();
        assert_eq!(
            listed.iter().filter(|listed| ***listed == name).count(),
            1,
            "`{name}` must appear exactly once in the root help template:\n{help}"
        );
    }
}

#[test]
fn removed_definition_commands_are_unknown_subcommands() {
    for removed in ["activity", "executor", "policy"] {
        assert_cli_rejects(
            &["orbit", removed, "list"],
            ErrorKind::InvalidSubcommand,
            removed,
        );
    }
}

#[test]
fn recursive_cli_help_uses_only_placeholder_artifact_ids() {
    assert_help_tree_has_no_concrete_artifact_ids(&Cli::command());
}

#[test]
fn cli_command_tree_debug_assert_rejects_duplicate_long_flags() {
    // Clap only panics on colliding long names during command build. Keep an
    // explicit whole-tree check so a new global/subcommand overlap fails here
    // instead of in an unrelated parser or help snapshot [ORB-11765].
    Cli::command().debug_assert();
}

#[test]
fn cli_help_advertises_workspace_selector_distinct_from_root() {
    let help = match Cli::try_parse_from(["orbit", "--help"]) {
        Ok(_) => panic!("--help exits before parsing"),
        Err(error) => error.to_string(),
    };
    assert!(
        help.contains("--workspace <SELECTOR>"),
        "orbit --help must show a global --workspace selector: {help}"
    );
    assert!(
        help.contains("--root <ROOT>"),
        "orbit --help must keep --root as a data-dir override: {help}"
    );
    assert!(
        help.contains("logical ID") || help.contains("ws_*"),
        "orbit --help must describe the shared selector grammar: {help}"
    );
    assert!(
        !help.contains("--root <SELECTOR>"),
        "--root must not become a workspace selector: {help}"
    );
}

#[test]
fn cli_parses_top_level_workspace_selector_before_subcommand() {
    let cli = Cli::parse_from([
        "orbit",
        "--workspace",
        "orbit",
        "task",
        "list",
        "--limit",
        "1",
    ]);
    assert_eq!(cli.workspace.as_deref(), Some("orbit"));
    assert!(cli.root.is_none());
    match cli.command {
        Commands::Task(_) => {}
        _ => panic!("expected task command"),
    }
}

#[test]
fn cli_parses_doctor_stale_lock_cleanup() {
    let cli = Cli::parse_from([
        "orbit",
        "doctor",
        "--fix-stale-locks",
        "--remove-graph",
        "--json",
    ]);
    match cli.command {
        Commands::Doctor(command) => {
            assert!(command.fix_stale_locks);
            assert!(!command.fix_stale_task_locks);
            assert!(command.remove_graph);
            assert!(command.json);
            // [ORB-10501] Repairs are opt-in: an unflagged run only diagnoses.
        }
        _ => panic!("expected top-level doctor command"),
    }
}

#[test]
fn cli_parses_doctor_stale_task_lock_repair_without_blanket_fix() {
    let cli = Cli::parse_from(["orbit", "doctor", "--fix-stale-task-locks"]);
    match cli.command {
        Commands::Doctor(command) => {
            assert!(command.fix_stale_task_locks);
            assert!(!command.fix_stale_locks);
            assert!(!command.fix_retired_activity_backends);
        }
        _ => panic!("expected top-level doctor command"),
    }

    assert_cli_rejects(
        &["orbit", "doctor", "--fix"],
        ErrorKind::UnknownArgument,
        "unexpected argument '--fix'",
    );
}

#[test]
fn cli_parses_doctor_retired_activity_backend_repair() {
    let cli = Cli::parse_from([
        "orbit",
        "doctor",
        "--fix-retired-activity-backends",
        "--json",
    ]);
    match cli.command {
        Commands::Doctor(command) => {
            assert!(command.fix_retired_activity_backends);
            assert!(!command.fix_stale_artifacts);
            assert!(command.json);
        }
        _ => panic!("expected top-level doctor command"),
    }
}

#[test]
fn cli_parses_mcp_init() {
    let cli = Cli::parse_from(["orbit", "mcp", "init"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Init(_) => {}
            _ => panic!("expected mcp init"),
        },
        _ => panic!("expected top-level mcp command"),
    }
}

#[test]
fn cli_parses_mcp_serve() {
    let cli = Cli::parse_from(["orbit", "mcp", "serve"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Serve(_) => {}
            _ => panic!("expected mcp serve"),
        },
        _ => panic!("expected top-level mcp command"),
    }
}

#[test]
fn cli_parses_mcp_listen_with_a_loopback_default() {
    let cli = Cli::parse_from(["orbit", "mcp", "listen"]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Listen(args) => {
                assert!(args.addr.ip().is_loopback());
                assert!(!args.allow_non_loopback);
            }
            _ => panic!("expected mcp listen"),
        },
        _ => panic!("expected top-level mcp command"),
    }

    let cli = Cli::parse_from([
        "orbit",
        "mcp",
        "listen",
        "0.0.0.0:9123",
        "--allow-non-loopback",
    ]);
    match cli.command {
        Commands::Mcp(command) => match command.command {
            McpSubcommand::Listen(args) => {
                assert_eq!(args.addr.to_string(), "0.0.0.0:9123");
                assert!(args.allow_non_loopback);
            }
            _ => panic!("expected mcp listen"),
        },
        _ => panic!("expected top-level mcp command"),
    }
}

#[test]
fn cli_keeps_mcp_serve_stdio_only() {
    assert_cli_rejects(
        &["orbit", "mcp", "serve", "--listen", "127.0.0.1:7879"],
        ErrorKind::UnknownArgument,
        "--listen",
    );
}

#[test]
fn cli_rejects_removed_mcp_role_and_capability_flags() {
    assert_cli_rejects(
        &["orbit", "mcp", "serve", "--hub"],
        ErrorKind::UnknownArgument,
        "--hub",
    );
    assert_cli_rejects(
        &["orbit", "mcp", "serve", "--owner"],
        ErrorKind::UnknownArgument,
        "--owner",
    );
    assert_cli_rejects(
        &["orbit", "mcp", "serve", "--capabilities", "operator"],
        ErrorKind::UnknownArgument,
        "--capabilities",
    );
}

#[test]
fn cli_parses_web_serve() {
    let cli = Cli::parse_from(["orbit", "web", "serve"]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Serve(_) => {}
            WebSubcommand::Connect(_) => panic!("expected serve"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_serve_global_as_deprecated_noop() {
    // `--global` is a deprecated no-op (ORB-10029): `orbit web serve` always
    // serves in global mode now, but the flag must keep parsing since `orbit
    // web connect` forwards it to remote hosts that may run an older binary.
    let cli = Cli::parse_from(["orbit", "web", "serve", "--global"]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Serve(args) => assert!(args.global),
            WebSubcommand::Connect(_) => panic!("expected serve"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_serve_workspace_preselection() {
    // ORB-11388: `--root` is the data-directory override on `web serve` like
    // everywhere else; the dashboard's preselection hint is `--workspace`.
    let cli = Cli::parse_from([
        "orbit",
        "--root",
        "/tmp/scratch-root",
        "web",
        "serve",
        "--workspace",
        "ws_repo",
    ]);
    assert_eq!(cli.root.as_deref(), Some(Path::new("/tmp/scratch-root")));
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Serve(args) => assert_eq!(args.workspace.as_deref(), Some("ws_repo")),
            WebSubcommand::Connect(_) => panic!("expected serve"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_connect_workspace() {
    // ORB-11388: the remote workspace to preselect is `--workspace`; a
    // top-level `--root` reaches the global flag, which `orbit web connect`
    // rejects rather than silently ignores.
    let cli = Cli::parse_from([
        "orbit",
        "web",
        "connect",
        "my-host",
        "--workspace",
        "/srv/ws",
    ]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Connect(args) => {
                assert_eq!(args.workspace.as_deref(), Some("/srv/ws"));
            }
            WebSubcommand::Serve(_) => panic!("expected connect"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_connect() {
    let cli = Cli::parse_from(["orbit", "web", "connect", "my-host", "--no-open"]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Connect(args) => {
                assert_eq!(args.ssh_host, "my-host");
                assert!(args.no_open);
                assert!(!args.no_operator);
            }
            WebSubcommand::Serve(_) => panic!("expected connect"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_serve_operator() {
    let cli = Cli::parse_from(["orbit", "web", "serve", "--operator", "--no-open"]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Serve(args) => {
                assert!(args.operator);
                assert!(args.no_open);
            }
            WebSubcommand::Connect(_) => panic!("expected serve"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_parses_web_connect_no_operator() {
    let cli = Cli::parse_from(["orbit", "web", "connect", "my-host", "--no-operator"]);
    match cli.command {
        Commands::Web(command) => match command.command {
            WebSubcommand::Connect(args) => {
                assert!(args.no_operator);
            }
            WebSubcommand::Serve(_) => panic!("expected connect"),
        },
        _ => panic!("expected top-level web command"),
    }
}

#[test]
fn cli_rejects_removed_docs_command() {
    assert_cli_rejects(
        &["orbit", "docs", "list"],
        ErrorKind::InvalidSubcommand,
        "unrecognized subcommand 'docs'",
    );
}

#[test]
fn cli_rejects_learning_reindex() {
    assert_cli_rejects(
        &["orbit", "learning"],
        ErrorKind::InvalidSubcommand,
        "unrecognized subcommand 'learning'",
    );
}

#[test]
fn cli_parses_top_level_search() {
    let cli = Cli::parse_from([
        "orbit",
        "search",
        "semantic search design",
        "--kind",
        "task",
    ]);
    match cli.command {
        Commands::Search(args) => {
            assert_eq!(args.query.as_deref(), Some("semantic search design"));
            assert!(args.command.is_none());
        }
        _ => panic!("expected top-level search command"),
    }
}

#[test]
fn cli_rejects_removed_search_surfaces_and_accepts_reindex() {
    assert!(Cli::try_parse_from(["orbit", "semantic", "stats"]).is_err());
    assert!(Cli::try_parse_from(["orbit", "search", "query", "--hybrid"]).is_err());
    assert!(Cli::try_parse_from(["orbit", "search", "similar", "task-id"]).is_err());
    let cli = Cli::parse_from(["orbit", "search", "reindex"]);
    assert!(
        matches!(cli.command, Commands::Search(args) if args.command == Some(SearchSubcommand::Reindex))
    );
}

#[test]
fn cli_rejects_removed_search_path_lookup() {
    assert_cli_rejects(
        &["orbit", "search", "path", "crates/orbit-cli/"],
        ErrorKind::UnknownArgument,
        "unexpected argument 'crates/orbit-cli/'",
    );
}

#[test]
fn cli_rejects_retired_adr_search_kind() {
    assert_cli_rejects(
        &["orbit", "search", "perf", "--kind", "adr"],
        ErrorKind::InvalidValue,
        "invalid value 'adr'",
    );
}

#[test]
fn cli_rejects_removed_doc_search_kind() {
    assert_cli_rejects(
        &["orbit", "search", "perf", "--kind", "doc"],
        ErrorKind::InvalidValue,
        "invalid value 'doc'",
    );
}

#[test]
fn cli_rejects_search_query_with_semantic_neighbor() {
    assert_cli_rejects(
        &["orbit", "search", "query", "ORB-1"],
        ErrorKind::UnknownArgument,
        "unexpected argument 'ORB-1'",
    );
}

#[test]
fn cli_rejects_search_related_flag() {
    let legacy_flag = concat!("--", "related");
    assert_cli_rejects(
        &["orbit", "search", legacy_flag, "ORB-1"],
        ErrorKind::UnknownArgument,
        "unexpected argument '--related'",
    );
}

#[test]
fn cli_rejects_search_semantic_flag() {
    assert_cli_rejects(
        &["orbit", "search", "--semantic", "ORB-1"],
        ErrorKind::UnknownArgument,
        "unexpected argument '--semantic'",
    );
}

#[test]
fn cli_rejects_retired_search_field_and_model_flags() {
    for (args, retired_flag) in [
        (
            &["orbit", "search", "query", "--field", "title"][..],
            "--field",
        ),
        (
            &["orbit", "search", "query", "--model", "bge-small"][..],
            "--model",
        ),
    ] {
        assert_cli_rejects(
            args,
            ErrorKind::UnknownArgument,
            &format!("unexpected argument '{retired_flag}'"),
        );
    }
}

#[test]
fn cli_rejects_retired_search_path_flag() {
    assert_cli_rejects(
        &["orbit", "search", "--path", "crates/"],
        ErrorKind::UnknownArgument,
        "unexpected argument '--path'",
    );
}

#[test]
fn cli_rejects_top_level_serve() {
    assert_cli_rejects(
        &["orbit", "serve"],
        ErrorKind::InvalidSubcommand,
        "unrecognized subcommand 'serve'",
    );
}

#[test]
fn cli_rejects_down_alias() {
    assert_cli_rejects(
        &["orbit", "mcp", "down"],
        ErrorKind::InvalidSubcommand,
        "unrecognized subcommand 'down'",
    );
}
