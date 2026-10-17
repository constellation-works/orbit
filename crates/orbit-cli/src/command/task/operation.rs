//! The task verbs derived from the task operation registry.
//!
//! Task reads, writes, artifacts, locks and the review verbs are declared
//! once in `orbit_common::governance::task`; the clap tree is the
//! noun-agnostic adapter in [`super::super::operation_args`], and every verb
//! runs through the tool. The task subcommands with no tool stay hand-wired
//! in [`super::command`].
//!
//! What stays here is CLI-specific: [`prepare`] consumes the CLI-only flags,
//! and [`render`] builds the text output from the tool's answer. Both are
//! exhaustive matches on [`TaskVerb`].

use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::governance::task::{TASK_CLI_OPTIONS_PARAM, TaskVerb};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Map, Value, json};

use super::super::operation_args::Invocation;
use crate::command::{CommandOut, Payload};

/// Explicit owner selection is resolved before opening the CLI runtime.
pub(crate) fn retry_owner_workspace(command: &super::command::TaskSubcommand) -> Option<&str> {
    let super::command::TaskSubcommand::Operation(super::command::TaskInvocation::Registry(
        invocation,
    )) = command
    else {
        return None;
    };
    if invocation.spec.verb != TaskVerb::RetryHandoff {
        return None;
    }
    invocation
        .input
        .get("owner_workspace")
        .and_then(Value::as_str)
}

/// CLI-only response options: list presentation and the artifact destination.
#[derive(Clone, Default)]
pub(super) struct ListView {
    /// Signal-tier records (`--ops`).
    pub(super) ops: bool,
    /// Every table column (`--full`).
    pub(super) full: bool,
    /// Destination consumed from `artifact get --out`.
    pub(super) artifact_out: Option<String>,
}

pub(super) fn execute(invocation: Invocation<TaskVerb>, runtime: &OrbitRuntime) -> CommandOut {
    let Invocation { spec, mut input } = invocation;
    let view = prepare(spec.verb, runtime, &mut input)?;
    let value = runtime.run_tool(spec.tool_name, Value::Object(input.clone()))?;
    render(spec.verb, runtime, &input, value, view)
}

/// `orbit task add`'s CLI-only flags, carried to the handler as its CLI
/// options under the names it reads.
const ADD_CLI_OPTIONS: [(&str, &str); 6] = [
    ("parent_id", "parent_id"),
    ("dependencies", "dependencies"),
    ("plan", "plan"),
    ("external_refs", "external_refs"),
    ("status", "status"),
    ("source_task", "source_task_id"),
];

/// `orbit task update`'s CLI-only switches, carried as its CLI options.
const UPDATE_CLI_SWITCHES: [&str; 3] = ["approve", "force", "discard_candidate"];

/// Update input that changes nothing on its own.
const UPDATE_MODIFIERS: [&str; 4] = [
    "id",
    "model",
    "allow_missing_context",
    "allow_drop_system_tags",
];

/// Shape a task request for the CLI before it reaches the tool. Each
/// CLI-only parameter is consumed here, so none reaches the tool as a field
/// of its own: the write verbs carry theirs as the handler's CLI options,
/// which also mark the request as the operator command's.
fn prepare(
    verb: TaskVerb,
    runtime: &OrbitRuntime,
    input: &mut Map<String, Value>,
) -> Result<ListView, OrbitError> {
    match verb {
        TaskVerb::Add => {
            let mut options = Map::new();
            for (flag, option) in ADD_CLI_OPTIONS {
                if let Some(value) = input.shift_remove(flag) {
                    options.insert(option.to_string(), value);
                }
            }
            input.insert(TASK_CLI_OPTIONS_PARAM.to_string(), Value::Object(options));
            input.insert("workspace".to_string(), json!(runtime.paths().repo_root));
            insert_env_model(input);
            Ok(ListView::default())
        }
        TaskVerb::Update => {
            // `--host` was resolved by the host preflight before dispatch.
            input.shift_remove("host");
            if let Some(pr_status) = input.get("pr_status").and_then(Value::as_str) {
                validate_pr_status(pr_status)?;
            }
            let mut options = Map::new();
            for switch in UPDATE_CLI_SWITCHES {
                if take_switch(input, switch) {
                    options.insert(switch.to_string(), Value::Bool(true));
                }
            }
            if let Some(artifacts) = input.shift_remove("artifacts") {
                options.insert("artifacts".to_string(), artifact_entries(&artifacts)?);
            }
            if options.is_empty()
                && input
                    .keys()
                    .all(|field| UPDATE_MODIFIERS.contains(&field.as_str()))
            {
                return Err(OrbitError::InvalidInput(
                    "nothing to update: pass at least one field flag, e.g. `--status` or `--title` (see `orbit task update --help`)"
                        .to_string(),
                ));
            }
            input.insert(TASK_CLI_OPTIONS_PARAM.to_string(), Value::Object(options));
            insert_env_model(input);
            Ok(ListView::default())
        }
        TaskVerb::List => {
            // `--all` is a deprecated no-op.
            input.shift_remove("all");
            Ok(ListView {
                ops: take_switch(input, "ops"),
                full: take_switch(input, "full"),
                ..Default::default()
            })
        }
        // `--host` was resolved by the host preflight before dispatch.
        TaskVerb::Show => {
            input.shift_remove("host");
            Ok(ListView::default())
        }
        TaskVerb::ArtifactPut => {
            input.shift_remove("host");
            insert_env_model(input);
            Ok(ListView::default())
        }
        TaskVerb::ArtifactGet => {
            input.shift_remove("host");
            Ok(ListView {
                artifact_out: input
                    .shift_remove("out")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned)),
                ..Default::default()
            })
        }
        TaskVerb::LocksReserve => {
            let raw = input
                .shift_remove("ttl")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "30m".into());
            let seconds = crate::command::locks::reservation_ttl_seconds(&raw)?;
            input.insert("ttl_seconds".into(), json!(seconds));
            Ok(ListView::default())
        }
        TaskVerb::LocksRelease => {
            crate::command::require_confirmation(
                take_switch(input, "confirm"),
                "task lock release",
            )?;
            Ok(ListView::default())
        }
        // The review verbs act on this checkout's workspace unless `--host`
        // routed them, which the preflight did before dispatch.
        TaskVerb::RetryHandoff => {
            if let Some(Value::String(selector)) = input.shift_remove("owner_workspace") {
                let workspace =
                    RegisteredRuntimeFactory::resolve_cli_owner_workspace(runtime, &selector)?;
                input.insert("workspace".into(), json!(workspace));
            }
            Ok(ListView::default())
        }
        TaskVerb::ReviewReset | TaskVerb::ReconcileReview | TaskVerb::Requeue => {
            input.shift_remove("host");
            input.insert("workspace".to_string(), json!(runtime.paths().repo_root));
            Ok(ListView::default())
        }
        // Reject and delete have no command line.
        TaskVerb::LocksList
        | TaskVerb::Eligible
        | TaskVerb::Lint
        | TaskVerb::Reject
        | TaskVerb::Delete => Ok(ListView::default()),
    }
}

fn take_switch(input: &mut Map<String, Value>, name: &str) -> bool {
    input.shift_remove(name) == Some(Value::Bool(true))
}

/// Attribute a write without `--model` to the agent the environment names,
/// as every other CLI write does.
fn insert_env_model(input: &mut Map<String, Value>) {
    if input.contains_key("model") {
        return;
    }
    let (agent, model) = super::mutation_identity(None);
    if let Some(model) = model.or(agent) {
        input.insert("model".to_string(), Value::String(model));
    }
}

/// Validate `--pr-status`: the value is stored verbatim and read back as a
/// merge gate, so a typo must be refused here rather than persisted as a
/// status no reader recognizes. An empty value is kept: it clears the field.
pub(crate) fn validate_pr_status(value: &str) -> Result<(), OrbitError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "approve" | "approved" | "request-changes" | "request_changes"
        | "changes-requested" | "changes_requested" => Ok(()),
        _ => Err(OrbitError::InvalidInput(format!(
            "unknown PR status '{value}': expected `approve` or `request-changes` (empty string clears)"
        ))),
    }
}

/// `--artifact path=content` values as the handler's artifact entries.
fn artifact_entries(raw_values: &Value) -> Result<Value, OrbitError> {
    raw_values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|raw| {
            let Some((path, content)) = raw.split_once('=') else {
                return Err(OrbitError::InvalidInput(format!(
                    "task artifact must use `path=content` form, got `{raw}`"
                )));
            };
            let path = path.trim();
            if path.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "task artifact path must not be empty".to_string(),
                ));
            }
            Ok(json!({ "path": path, "content": content }))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// The `orbit.task.update` input for a write delivered to the host that
/// holds the task [ORB-14449].
///
/// Flags with no tool field are refused rather than dropped, each with the
/// remedy that fits it: `--artifact` has its own routed command
/// (`orbit task artifact put`), `--approve` is a human lifecycle action for
/// the owning host, and `--force` and `--discard-candidate` run there too.
pub(crate) fn remote_update_input(
    input: &Map<String, Value>,
    host_ssh: &str,
) -> Result<Value, OrbitError> {
    let id = input.get("id").and_then(Value::as_str).unwrap_or_default();
    let refusals = unroutable_update_refusals(input, id, host_ssh);
    if !refusals.is_empty() {
        return Err(OrbitError::InvalidInput(refusals.join("; ")));
    }
    if input.contains_key("note") {
        return Err(OrbitError::InvalidInput(
            "`note` is recorded on an approval; pass it with `approve`".to_string(),
        ));
    }
    if let Some(pr_status) = input.get("pr_status").and_then(Value::as_str) {
        validate_pr_status(pr_status)?;
    }
    let mut remote = input
        .iter()
        .filter(|(field, _)| !matches!(field.as_str(), "host" | "model"))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect::<Map<_, _>>();
    if remote.len() == 1 {
        return Err(OrbitError::InvalidInput(
            "nothing to update: pass at least one field flag, e.g. `--status` or `--title` (see `orbit task update --help`)"
                .to_string(),
        ));
    }
    let explicit = input
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let (_, Some(model)) = super::mutation_identity(explicit) {
        remote.insert("model".to_string(), Value::String(model));
    }
    Ok(Value::Object(remote))
}

/// One refusal sentence per routed-unsupported flag group, in flag order.
fn unroutable_update_refusals(input: &Map<String, Value>, id: &str, host_ssh: &str) -> Vec<String> {
    let mut refusals = Vec::new();
    let artifacts = input
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if !artifacts.is_empty() {
        let puts = artifacts
            .iter()
            .map(|raw| {
                let path = raw.split_once('=').map_or(*raw, |(path, _)| path);
                format!(
                    "`orbit task artifact put {id} <source-file> --path {}`",
                    orbit_common::process::shell::quote_posix_arg(path.trim())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        refusals.push(format!(
            "--artifact cannot be routed to the host that holds task {id}; store the artifact \
             with the command that routes, writing its content to a file first: {puts}"
        ));
    }
    let set = |name: &str| input.get(name) == Some(&Value::Bool(true));
    if set("approve") {
        let mut remote = format!("orbit task update {id} --approve");
        if let Some(note) = input.get("note").and_then(Value::as_str) {
            // SSH hands the remote shell one string: quote the note for it,
            // then the whole command for the local shell.
            let note = orbit_common::process::shell::quote_posix_arg(note);
            remote =
                orbit_common::process::shell::quote_posix_arg(&format!("{remote} --note {note}"));
        }
        refusals.push(format!(
            "--approve cannot be routed to the host that holds task {id}: approval is a human \
             lifecycle action that runs on the owning host: `ssh {host_ssh} {remote}`"
        ));
    }
    let ssh_only = [
        ("--force", set("force")),
        ("--discard-candidate", set("discard_candidate")),
    ]
    .into_iter()
    .filter_map(|(flag, set)| set.then_some(flag))
    .collect::<Vec<_>>();
    if !ssh_only.is_empty() {
        refusals.push(format!(
            "{} cannot be routed to the host that holds task {id}; run it there: `ssh {host_ssh} \
             orbit task update {id} …`",
            ssh_only.join(", ")
        ));
    }
    refusals
}

/// Build a task response's text payload.
fn render(
    verb: TaskVerb,
    runtime: &OrbitRuntime,
    input: &Map<String, Value>,
    value: Value,
    view: ListView,
) -> CommandOut {
    match verb {
        TaskVerb::Add => {
            let (document, task) = super::output::task_write_json(runtime, &value)?;
            Ok(Payload::detail(document, task.id).into())
        }
        TaskVerb::Update => {
            let (document, task) = super::output::task_write_json(runtime, &value)?;
            let approved = input
                .get(TASK_CLI_OPTIONS_PARAM)
                .and_then(|options| options.get("approve"))
                == Some(&Value::Bool(true));
            let text = if approved {
                format!("Approved task '{}' -> {}", task.id, task.status)
            } else {
                format!("Updated task '{}'", task.id)
            };
            Ok(Payload::detail(document, text).into())
        }
        TaskVerb::ArtifactPut => {
            let (document, task) = super::output::task_write_json(runtime, &value)?;
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .or_else(|| {
                    input
                        .get("source_path")
                        .and_then(Value::as_str)
                        .and_then(|path| std::path::Path::new(path).file_name())
                        .and_then(|name| name.to_str())
                })
                .unwrap_or_default();
            Ok(Payload::detail(
                document,
                format!("Stored artifact '{path}' on task '{}'", task.id),
            )
            .into())
        }
        TaskVerb::ArtifactGet => super::artifact::render_get(value, view.artifact_out.as_deref()),
        TaskVerb::LocksList | TaskVerb::LocksReserve | TaskVerb::LocksRelease => {
            crate::command::locks::render(verb, value)
        }
        TaskVerb::List => super::list::render(input, value, view),
        TaskVerb::Eligible => super::eligible::render(input, value),
        TaskVerb::Show => super::show::render(runtime, input, value),
        TaskVerb::Lint => super::lint::render(input, value),
        TaskVerb::ReviewReset => {
            Ok(Payload::detail(value, super::review_reset::REVIEW_RESET_TEXT).into())
        }
        TaskVerb::Requeue => {
            let text = requeue_text(&value);
            Ok(Payload::detail(value, text).into())
        }
        TaskVerb::ReconcileReview => {
            let text = super::reconcile_review::reconciliation_text(&value);
            Ok(Payload::detail(value, text).into())
        }
        TaskVerb::RetryHandoff | TaskVerb::Reject | TaskVerb::Delete => {
            Ok(Payload::document(value).into())
        }
    }
}

/// `orbit task requeue`'s text: the task, what its next run resumes, and any
/// pull request the discard closed or could not close.
pub(crate) fn requeue_text(value: &Value) -> String {
    let id = value.get("id").and_then(Value::as_str).unwrap_or("-");
    let mut lines = vec![format!("Requeued {id} to backlog.")];
    if let Some(resumes) = value.get("resumes").filter(|v| v.is_object()) {
        let field = |key: &str| resumes.get(key).and_then(Value::as_str).unwrap_or("-");
        lines.push(format!(
            "Resumes pull request #{} on {} at {}.",
            field("pull_request"),
            field("branch"),
            field("head_sha")
        ));
    }
    match value.get("discard_closure").and_then(Value::as_str) {
        Some("settled") => lines.push(
            "Closed the task's undelivered Orbit pull requests, if it had any; their branches are \
             kept."
                .into(),
        ),
        Some("disabled") => lines.push(
            "pr.close_on_terminal is off, so no pull request was closed; close the discarded one \
             by hand."
                .into(),
        ),
        Some(_) => lines.push(
            "Could not close every undelivered Orbit pull request of the task; close the rest by \
             hand."
                .into(),
        ),
        None => {}
    }
    if let Some(next) = value.get("next_step").and_then(Value::as_str) {
        lines.push(next.to_string());
    }
    lines.join("\n")
}
