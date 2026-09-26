// Legacy CLI binary surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// The CLI binary prints genuine user-facing command output.
#![allow(clippy::print_stderr, clippy::print_stdout)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]

//! CLI entry point for Orbit: command parsing, dispatch, and output formatting.
//!
//! Parses command-line arguments with `clap`, initializes the [`OrbitRuntime`],
//! dispatches to the appropriate command handler, and formats results as JSON
//! or human-readable table output. Wraps every command in an audit middleware
//! that records success, failure, or policy-denial events.
//!
//! # Role
//! The outermost crate in the dependency graph. Depends on `orbit-core` and
//! `orbit-types`. All other crates are consumed transitively via `orbit-core`.
//! This binary is the `orbit` executable.
//!
//! # Key responsibilities
//! - Parse the top-level CLI surface and route subcommands to their handlers
//! - Bootstrap the runtime, including optional `--root` data-dir overrides and
//!   `--workspace` selectors (registered name, logical id, or checkout path)
//! - Emit machine-readable JSON or human-readable table output
//! - Wrap command execution in audit logging so human and agent actions are recorded
//!
//! # Dependency direction
//! orbit-core, orbit-types → `orbit-cli` (binary crate, no dependents)

mod audit_middleware;
mod command;
mod output;
mod parse;
mod plugin_cli;

use clap::{Arg, ArgMatches, Command, CommandFactory, FromArgMatches};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::ActorIdentity;
use orbit_core::composition::pin_executable_generation;

#[cfg(test)]
use crate::command::init::InitCommand;
use crate::command::operation::{CommandOperation, DispatchContext, RuntimeNeed};
use crate::output::sink::{FormatArg, OutputMode, OutputSink};

/// Clap id and long name of the global output-format argument.
const FORMAT_ARG_ID: &str = "format";

/// The global `--format`, declared exactly once for the whole CLI.
///
/// It is built here and grafted onto the parsed command rather than added as a
/// field on [`command::Cli`] because the staged terminal-interface migration
/// [ORB-10569] owns `main.rs` while concurrent work owns the `command/` tree.
/// Either declaration site yields the same surface: one declaration, rendered
/// under `Options:` in `orbit --help` and accepted after a subcommand.
fn format_arg() -> Arg {
    Arg::new(FORMAT_ARG_ID)
        .long(FORMAT_ARG_ID)
        .value_name("MODE")
        .value_parser(clap::value_parser!(FormatArg))
        .help("Output format (default: auto — a table on a terminal, plain text when piped)")
}

/// Whether this command already declares a `--format` of its own.
///
/// `orbit audit export` does, naming its export file's serialization with its
/// own value type. It keeps that meaning; the global flag is simply not
/// offered there, and its help says so. `crate::tests::cli_format` pins the
/// list of such commands.
fn declares_format(command: &Command) -> bool {
    command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(FORMAT_ARG_ID))
}

/// Add [`format_arg`] to the root and to every subcommand that does not
/// declare its own `--format`.
///
/// This walks the tree instead of using `Arg::global`, which would be the
/// obvious spelling but panics here. A global arg is keyed by *id*: clap
/// declines to propagate it into a subcommand that already defines the same id
/// (so `audit export` keeps its own `--format`), but it then propagates the
/// *values* of every global id up and down the whole match tree regardless of
/// type. `orbit audit export --format csv` therefore lands an `ExportFormat`
/// under the root's `format` id, and `orbit --format json audit export` lands a
/// `FormatArg` under the subcommand's — each one a downcast panic in the other
/// reader. Declaring the argument per level keeps every value at the level it
/// was parsed at, where its type is the one that level expects.
fn install_format_arg(command: Command) -> Command {
    let subcommands: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();

    let mut command = if declares_format(&command) {
        command
    } else {
        command.arg(format_arg())
    };
    for name in subcommands {
        command = command.mut_subcommand(name, install_format_arg);
    }
    command
}

/// The `--format` value, taken from the deepest level that parsed one.
///
/// A level that owns an unrelated `--format` yields a downcast error rather
/// than a value, which reads here as "no global format was requested".
fn requested_format(matches: &ArgMatches) -> Option<FormatArg> {
    let mut level = matches;
    let mut requested = None;
    loop {
        if let Ok(Some(format)) = level.try_get_one::<FormatArg>(FORMAT_ARG_ID) {
            requested = Some(*format);
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return requested,
        }
    }
}

/// Clap ids of the per-command boolean flags that have always meant "emit the
/// machine-readable form".
///
/// `--ops` is here alongside `--json` because it is the same rung wearing a
/// different name: on `task list` and `job list` it selects a narrower record
/// shape and has always forced JSON. Leaving it out would make
/// `orbit task list --ops` render a table on a terminal.
const LEGACY_JSON_ARG_IDS: [&str; 2] = ["json", "ops"];

/// Whether the invoked subcommand's own `--json`/`--ops` boolean was set.
///
/// Mode precedence rung 2 (spec §2), read the same way `--format` is: from the
/// parsed matches rather than from 86 individual argument structs. The flags
/// stay declared and accepted where they are [ADR-0306]; this is what makes
/// them route through the resolver instead of each branching for itself.
fn legacy_json(matches: &ArgMatches) -> bool {
    let mut level = matches;
    loop {
        for id in LEGACY_JSON_ARG_IDS {
            if matches!(level.try_get_one::<bool>(id), Ok(Some(true))) {
                return true;
            }
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return false,
        }
    }
}

/// Repoint clap's default "pass it after `--`" tip when the rejected flag is
/// `--crew` on `orbit run job` / `orbit job run` (matched by the `<JOB_ID>`
/// usage, unique to [`command::run::JobRunArgs`]).
///
/// That default tip is actively wrong here: a job run has no positional slot
/// a trailing `--crew` could land in, so following it just produces a second,
/// more confusing error. Crew selection for a job run goes through the
/// existing `--input` contract (`resolve_crew_for_run_input` reads `crew`
/// from the run input), so the repair only rewrites the tip text — it does
/// not change what argv the parser accepts or make `--crew` appear to work.
fn repair_crew_flag_suggestion(mut err: clap::error::Error) -> clap::error::Error {
    use clap::error::{ContextKind, ContextValue};

    let is_crew_flag = matches!(
        err.get(ContextKind::InvalidArg),
        Some(ContextValue::String(arg)) if arg == "--crew"
    );
    let is_job_run = matches!(
        err.get(ContextKind::Usage),
        Some(ContextValue::StyledStr(usage)) if usage.to_string().contains("<JOB_ID>")
    );
    if is_crew_flag && is_job_run {
        err.insert(
            ContextKind::Suggested,
            ContextValue::StyledStrs(vec![clap::builder::StyledStr::from(
                "crew selection is `--input crew=<name>`, e.g. `--input crew=luna` (see `orbit run job --help`)",
            )]),
        );
    }
    err
}

/// Long-lived commands that must reap JSONL archives even when the active
/// file is within budget. `--help` never reaches here: clap exits in
/// [`parse_cli`]. Short-lived commands rotate only if a later JSONL write
/// finds an oversized active file (one `metadata()` check, no directory walk).
fn command_rotates_jsonl_on_start(command: &command::Commands) -> bool {
    match command {
        command::Commands::Mcp(mcp) => matches!(
            mcp.command,
            command::mcp::McpSubcommand::Serve(_) | command::mcp::McpSubcommand::Listen(_)
        ),
        command::Commands::Web(web) => {
            matches!(web.command, command::web::WebSubcommand::Serve(_))
        }
        command::Commands::Sweep(_) => true,
        command::Commands::Clock(clock) => {
            matches!(clock.command, command::clock::ClockSubcommand::Tick(_))
        }
        _ => false,
    }
}

fn is_clock_tick(command: &command::Commands) -> bool {
    matches!(command, command::Commands::Sweep(_))
        || matches!(command, command::Commands::Clock(clock)
            if matches!(clock.command, command::clock::ClockSubcommand::Tick(_)))
}

/// The Orbit root this invocation will use, read from argv before clap runs.
///
/// The derived CLI cannot answer this yet: the tree it would parse against
/// is the one the plugin groups still have to be added to. `--root` is the
/// only argument that changes *which* plugins those are, so it is the only
/// one read here, with the same precedence `resolve_generation_root` applies
/// afterwards (`--root`, then `ORBIT_ROOT`, then the host-global root).
fn plugin_root_override() -> Option<std::path::PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy().into_owned();
        if let Some(value) = arg.strip_prefix("--root=") {
            return Some(std::path::PathBuf::from(value));
        }
        if arg == "--root" {
            return args.next().map(std::path::PathBuf::from);
        }
    }
    None
}

/// The `orbit <ns>` groups this host's enabled plugins contribute (§4.6).
///
/// A host with no plugin install directory answers with one `stat` and no
/// store or config read, so the common case pays nothing for a surface it
/// does not use. Any failure below that is this host's plugin problem, not
/// this command's: the built-in CLI must stay usable, so it is logged and
/// the groups are simply absent (§4.9).
fn plugin_cli_groups() -> Vec<orbit_core::adapter::command::PluginCliGroup> {
    let Ok(root) = orbit_core::runtime::resolve_generation_root(plugin_root_override().as_deref())
    else {
        return Vec::new();
    };
    if !root.join("plugins").is_dir() {
        return Vec::new();
    }
    match orbit_core::adapter::command::host_plugin_cli_groups(&root) {
        Ok(groups) => groups,
        Err(error) => {
            tracing::warn!(
                target: "orbit.cli.plugin",
                error = %error,
                "omitting plugin command groups from the CLI surface"
            );
            Vec::new()
        }
    }
}

/// Parse argv into the derived CLI plus the two inputs to mode resolution.
fn parse_cli() -> (command::Cli, Option<FormatArg>, bool) {
    let groups = plugin_cli_groups();
    let root = plugin_cli::augment(command::Cli::command(), &groups);
    let root = if groups.is_empty() {
        root
    } else {
        // The hand-rolled top-level template lists commands by section, so a
        // plugin group is listed in its own section rather than left out of
        // `orbit --help` entirely.
        root.help_template(command::ROOT_HELP_TEMPLATE.replace(
            "\nOptions:",
            &format!("{}\nOptions:", plugin_cli::help_section(&groups)),
        ))
    };
    let matches = install_format_arg(root)
        .try_get_matches_from(std::env::args_os())
        .unwrap_or_else(|err| repair_crew_flag_suggestion(err).exit());
    let requested = requested_format(&matches);
    let legacy = legacy_json(&matches);
    let cli = match plugin_cli::invocation_from_matches(&groups, &matches) {
        // A plugin group is not a `Commands` variant clap can build, so the
        // two global arguments are read here and the rest of the invocation
        // is the tool call the group reduced to.
        Some(invocation) => command::Cli {
            root: matches.get_one::<std::path::PathBuf>("root").cloned(),
            workspace: matches.get_one::<String>("workspace").cloned(),
            command: command::Commands::PluginGroup(Box::new(invocation)),
        },
        None => command::Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit()),
    };
    (cli, requested, legacy)
}

fn main() {
    orbit_common::observability::logging::init_default_subscriber("warn");
    output::pipe::install_handler();

    let (cli, requested_format, legacy_json) = parse_cli();
    if command_rotates_jsonl_on_start(&cli.command) {
        orbit_common::observability::logging::rotate_global_jsonl_best_effort();
    }
    // Resolved once per invocation, before dispatch, and passed to the one
    // renderer that consumes it. Nothing downstream re-derives these answers.
    let sink = OutputSink::from_process(requested_format, legacy_json);
    sink.apply_color_policy();
    tracing::debug!(
        mode = ?sink.mode(),
        is_tty = sink.is_tty(),
        width = sink.width(),
        color_allowed = sink.color_allowed(),
        progress_allowed = sink.progress_allowed(),
        "resolved output sink"
    );
    // Update owns exclusive admission and pins its candidate before convergence.
    // Read-only commands may join a live generation without rewriting the
    // record when store schema matches. Everything else pins the exact running
    // inode before any bootstrap. This also covers all MCP transports, managed
    // workers and automatic migrations. `--root` / `ORBIT_ROOT` isolate that
    // pin so a read-only unpinned `~/.orbit` cannot block scratch init. A
    // managed macOS child joins its parent's host registry pin even when
    // ORBIT_ROOT selects workspace data; update still checks both roots.
    let inspection =
        matches!(&cli.command, command::Commands::Migrate(command) if !command.confirm);
    let root_override = cli.root.clone();
    let workspace_selector = cli.workspace.clone();
    let actor = ActorIdentity::from_env();
    let CommandOperation {
        runtime_need,
        task_owner_id,
        audit_meta,
        json_error_preference,
        suppress_errors,
        dispatch,
        governed,
        plugin_callback_entry_point,
    } = cli.command.operation().attribute_to(&actor);
    // ORB-12876: a recognized plugin backend reaches Orbit only through a tool
    // call, which the callback allowlist gates against the plugin's
    // `permissions.orbit_tools`. Refuse it the rest of the CLI here — before
    // generation pinning, runtime bootstrap and dispatch — so no plain command
    // reads governed data around that allowlist.
    if !plugin_callback_entry_point
        && let Err(error) = refuse_plugin_child_cli(&audit_meta, root_override.as_deref())
    {
        print_error(&error, &sink, json_error_preference);
        std::process::exit(1);
    }
    let clock_tick = is_clock_tick(&cli.command);
    let quiet_clock_tick =
        clock_tick && !matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
    let _generation = if matches!(&cli.command, command::Commands::Update(_)) || inspection {
        None
    } else {
        let root =
            match orbit_core::runtime::resolve_process_generation_root(root_override.as_deref()) {
                Ok(root) => root,
                Err(error) => {
                    print_error(&error, &sink, None);
                    std::process::exit(1);
                }
            };
        match pin_executable_generation(
            &root,
            matches!(
                runtime_need,
                RuntimeNeed::ReadOnly | RuntimeNeed::PluginReadOnly
            ),
        ) {
            Ok(guard) => {
                if clock_tick
                    && let Ok(digest) = orbit_common::fs::generation::process_generation()
                    && let Ok(Some(summary)) =
                        orbit_common::fs::generation::finish_clock_generation_hold(
                            &root,
                            digest,
                            chrono::Utc::now(),
                        )
                {
                    eprintln!("{summary}");
                }
                Some(guard)
            }
            Err(error) => {
                if clock_tick
                    && orbit_common::fs::generation::is_clock_generation_hold(&error)
                    && let Ok(digest) = orbit_common::fs::generation::process_generation()
                    && orbit_common::fs::generation::record_clock_generation_hold(
                        &root,
                        digest,
                        chrono::Utc::now(),
                    )
                    .is_ok()
                    && quiet_clock_tick
                {
                    std::process::exit(1);
                }
                print_error(&error, &sink, None);
                std::process::exit(1);
            }
        }
    };

    let bootstrapped = match &runtime_need {
        RuntimeNeed::Forbidden => {
            // A runtime-forbidden command has no store to authorize or audit
            // against. None is governed; `Commands::operation` is exhaustive, so
            // a future one that is would have to resolve this first.
            debug_assert!(
                governed.is_none(),
                "a governed operation must be able to reach the authorization chokepoint"
            );
            let result = dispatch(
                cli.command,
                DispatchContext::without_runtime(
                    root_override.as_deref(),
                    workspace_selector.as_deref(),
                ),
            );
            finish_command(result, &sink, suppress_errors, json_error_preference);
            return;
        }
        RuntimeNeed::Required => RegisteredRuntimeFactory::initialize_with_overrides(
            root_override.as_deref(),
            workspace_selector.as_deref(),
        ),
        RuntimeNeed::SelectedWorkspace { selector } => {
            RegisteredRuntimeFactory::initialize_with_overrides(
                root_override.as_deref(),
                Some(selector),
            )
        }
        RuntimeNeed::PipelineWorker => {
            RegisteredRuntimeFactory::initialize_pipeline_worker_with_overrides(
                root_override.as_deref(),
                workspace_selector.as_deref(),
            )
        }
        RuntimeNeed::ReadOnly => match task_owner_id.as_deref() {
            Some(task_id) => orbit_cmd::task_owner::initialize_for_task_show(
                root_override.as_deref(),
                workspace_selector.as_deref(),
                task_id,
            ),
            None => RegisteredRuntimeFactory::initialize_read_only_with_overrides(
                root_override.as_deref(),
                workspace_selector.as_deref(),
            ),
        },
        RuntimeNeed::PluginReadOnly => {
            RegisteredRuntimeFactory::initialize_plugin_read_only_with_overrides(
                root_override.as_deref(),
                workspace_selector.as_deref(),
            )
        }
        RuntimeNeed::TaskOwner { task_id } => orbit_cmd::task_owner::initialize_for_task_show(
            root_override.as_deref(),
            workspace_selector.as_deref(),
            task_id,
        ),
    };

    let runtime = match bootstrapped {
        Ok(runtime) => runtime,
        Err(err) => {
            if suppress_errors {
                return;
            }
            print_error(&err, &sink, json_error_preference);
            std::process::exit(1);
        }
    }
    .with_actor(actor);

    let context = DispatchContext::with_runtime(
        &runtime,
        root_override.as_deref(),
        workspace_selector.as_deref(),
    );
    // ORB-10453: the CLI's single authorization chokepoint. Every command
    // traverses it before dispatch, so a governed operation cannot be reached
    // by adding a subcommand that forgets its own guard.
    let authorize = |runtime: &orbit_core::OrbitRuntime| match governed {
        Some(operation) => {
            runtime.authorize_command_operation(operation.command, operation.subcommand)
        }
        None => Ok(()),
    };
    let skip_audit = _generation
        .as_ref()
        .is_some_and(orbit_common::fs::generation::GenerationGuard::joined_foreign_generation);
    let result = match audit_meta {
        Some(meta) if !skip_audit => {
            let mut guard = audit_middleware::AuditGuard::new(&runtime, meta);
            let result = authorize(&runtime).and_then(|()| dispatch(cli.command, context));
            guard.mark_result(&result);
            result
        }
        _ => authorize(&runtime).and_then(|()| dispatch(cli.command, context)),
    };

    finish_command(result, &sink, suppress_errors, json_error_preference);
}

/// Refuse this invocation if a plugin backend is the caller [ORB-12876].
///
/// The root is the one the command itself will use, so `--root` cannot pick a
/// different Orbit to be judged against than the one about to be read. A root
/// that does not resolve is not skipping the gate: the command has no Orbit
/// state to read either, and `main` fails it a few lines below.
///
/// The decision is `orbit_core`'s, taken from the host-issued callback session
/// rather than from anything the child controls.
fn refuse_plugin_child_cli(
    audit_meta: &Option<command::operation::CommandMeta>,
    root_override: Option<&std::path::Path>,
) -> Result<(), orbit_core::OrbitError> {
    let Ok(root) = orbit_core::runtime::resolve_generation_root(root_override) else {
        return Ok(());
    };
    let invocation = audit_meta
        .as_ref()
        .map(|meta| match meta.subcommand.as_deref() {
            Some(subcommand) => format!("{} {subcommand}", meta.command),
            None => meta.command.clone(),
        });
    orbit_core::adapter::command::refuse_plugin_child_cli_command(&root, invocation.as_deref())
}

/// Render what the command returned, or report why it failed.
///
/// This is the only place a command's records reach stdout: `dispatch` hands
/// back a payload and `output::render` projects it into the mode the sink
/// resolved (spec §3). A rendering failure is a command failure — a payload
/// that could not be serialized must not exit `0`.
fn finish_command(
    result: command::CommandOut,
    sink: &OutputSink,
    suppress_errors: bool,
    json_error_preference: Option<bool>,
) {
    let exit_code = result
        .as_ref()
        .map(command::CommandOutput::exit_code)
        .unwrap_or(0);
    let rendered = result.and_then(|output| output::render::emit(output, sink));
    if let Err(err) = rendered {
        if suppress_errors {
            return;
        }
        print_error(&err, sink, json_error_preference);
        std::process::exit(1);
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// Report a failed command on **stderr**, in every mode.
///
/// The JSON error payload used to go to stdout, which meant a `--json` caller
/// piping stdout into a parser received an error object where a result was
/// expected, and had to distinguish the two by shape. Spec §5 puts the payload
/// on stderr and leaves stdout carrying the payload and nothing else.
///
/// **Breaking change**: a script parsing `orbit ... --json` errors off stdout
/// reads them from stderr now (`2>&1`, or check the exit code, which was
/// already `1`).
///
/// Whether the report is JSON is the command's declared preference when it has
/// one, and otherwise the sink's mode — `--format json` on a command with no
/// `--json` flag of its own still gets a machine-readable failure.
fn print_error(
    error: &orbit_core::OrbitError,
    sink: &OutputSink,
    tool_run_json_output: Option<bool>,
) {
    if let Some(pretty) = json_error_format(sink, tool_run_json_output) {
        let payload = crate::output::json::error_payload(error);
        if let Ok(rendered) = crate::output::json::render(&payload, pretty) {
            eprintln!("{rendered}");
            return;
        }
    }

    eprintln!("error: {error}");
}

/// Whether to report an error as JSON, and whether to pretty-print it.
fn json_error_format(sink: &OutputSink, tool_run_json_output: Option<bool>) -> Option<bool> {
    if let Some(pretty) = tool_run_json_output {
        return Some(pretty);
    }
    matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson).then(|| sink.pretty_json())
}

#[cfg(test)]
mod tests;
