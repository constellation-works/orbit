use crate::audit_middleware;
use crate::command;
use crate::output;
use crate::output::sink::{OutputMode, OutputSink};

/// Refuse this invocation if a plugin backend is the caller [ORB-12876].
///
/// The root is the one the command itself will use, so `--root` cannot pick a
/// different Orbit to be judged against than the one about to be read. A root
/// that does not resolve is not skipping the gate: the command has no Orbit
/// state to read either, and `main` fails it a few lines below.
///
/// The decision is `orbit_core`'s, taken from the host-issued callback session
/// rather than from anything the child controls.
pub(super) fn refuse_plugin_child_cli(
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
pub(super) fn finish_command(
    result: command::CommandOut,
    sink: &OutputSink,
    json_error_preference: Option<bool>,
    mut guard: Option<audit_middleware::AuditGuard<'_>>,
) {
    let exit_code = result
        .as_ref()
        .map(command::CommandOutput::exit_code)
        .unwrap_or(0);
    let audit_message = result
        .as_ref()
        .ok()
        .and_then(command::CommandOutput::audit_message)
        .map(str::to_owned);
    let rendered = result.and_then(|output| {
        output::pipe::catch_closed_stdout(|| output::render::emit(output, sink))
    });
    if let Some(guard) = guard.as_mut() {
        guard.mark_result(&rendered, exit_code, audit_message.as_deref());
    }
    // logging::exit uses process::exit, which does not run destructors. Persist
    // the complete render/stream outcome and duration before any exit below.
    drop(guard);
    if let Err(err) = rendered {
        print_error(&err, sink, json_error_preference);
        orbit_common::observability::logging::exit(1);
    }
    if exit_code != 0 {
        orbit_common::observability::logging::exit(exit_code);
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
pub(super) fn print_error(
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
