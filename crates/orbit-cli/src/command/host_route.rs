//! Host routing before any runtime opens [ORB-14449].
//!
//! A call that addresses one task by id, made without a selector, goes to the
//! host the id's prefix names. `--host` names the host for `--workspace` and
//! `--pull` and becomes the selector that host lists. A call that lands on
//! another host is delivered over the federated client route and rendered
//! here; nothing local opens for it. Routing and resolution live in
//! `orbit_cmd::hosts`; this module only maps commands to tool calls and
//! renders the answer.

use std::path::{Path, PathBuf};

use orbit_cmd::hosts::{self, HostWorkspaceRoute, TaskIdRoute};
use orbit_common::governance::authorization::{CallerCapabilities, CallerEnvelope};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{MachineQualifiedSelector, is_id_routed_tool};
use orbit_registry::hosts::HostEntry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Map, Value, json};

use super::run::RunSubcommand;
use super::task::TaskSubcommand;
use super::task::artifact::TaskArtifactSubcommand;
use super::tool::ToolSubcommand;
use super::{Cli, CommandOut, Commands, Payload};

/// What the preflight decided.
pub(crate) enum Preflight {
    /// Run here, as before. `--host` may have rewritten the selector.
    Local,
    /// The call went to another host; this is its outcome.
    Remote(CommandOut),
}

/// Route `cli` before bootstrap: resolve `--host`, then deliver a call that
/// belongs to another host there.
pub(crate) fn preflight(cli: &mut Cli) -> Result<Preflight, OrbitError> {
    let host = requested_host(&cli.command);
    if host.is_none() && !may_route(cli) {
        return Ok(Preflight::Local);
    }
    let global_root = orbit_core::runtime::resolve_global_root()?;
    // A claimed worker's calls follow its owner binding, never a prefix.
    if OrbitRuntime::current_worker_invocation(&global_root)?.is_some() {
        if host.is_some() {
            return Err(OrbitError::InvalidInput(
                "--host is not available to a claimed worker; its calls follow the owner \
                 binding"
                    .into(),
            ));
        }
        return Ok(Preflight::Local);
    }
    if let Commands::Task(task) = &cli.command
        && routed_command_id(&task.command).is_none()
        && let Some(selector) = workspace_value(cli)?
        && let Ok(qualified) = selector.parse::<MachineQualifiedSelector>()
        && !matches!(
            inspect_machine_identity(&global_root)?,
            MachineIdentityState::Present(identity) if identity.id == qualified.machine_id()
        )
    {
        return Err(OrbitError::InvalidInput(format!(
            "--workspace '{selector}' names another host; this task command runs on this \
             machine and takes no --host. Run it on that host with `ssh <target> orbit task …`, \
             or use `orbit tool run <tool> --workspace {selector}` for a supported task tool \
             (see `orbit host list`)"
        )));
    }
    if let Commands::Run(run) = &mut cli.command
        && let RunSubcommand::Auto(auto) = &mut run.command
    {
        if let Some(pull) = auto.pull.as_mut() {
            *pull = pull_selector(&global_root, auto.host.as_deref(), pull)?;
        }
        return Ok(Preflight::Local);
    }
    if let Some(host) = host {
        let value = workspace_value(cli)?.ok_or_else(|| {
            OrbitError::InvalidInput(
                "--host names the host for --workspace; pass --workspace <name or ws_*> as that \
                 host lists it"
                    .into(),
            )
        })?;
        let resolved = match hosts::resolve_host_workspace(&global_root, &host, &value)? {
            HostWorkspaceRoute::Local { workspace_id } => workspace_id,
            HostWorkspaceRoute::Remote { selector, .. } => selector,
        };
        set_workspace_value(cli, resolved)?;
    }
    let Some(task_id) = routed_task_id(&cli.command)? else {
        return deliver_by_selector(cli, &global_root);
    };
    if let Some(selector) = workspace_value(cli)? {
        let Some(holder) = hosts::selector_remote_host(&global_root, &selector)? else {
            return Ok(Preflight::Local);
        };
        return deliver(cli, &global_root, &holder, Some(selector));
    }
    // `--root` / `ORBIT_ROOT` names the store explicitly, as a selector does.
    if explicit_root(cli) {
        return Ok(Preflight::Local);
    }
    match hosts::route_task_id(&global_root, &task_id)? {
        TaskIdRoute::Local => Ok(Preflight::Local),
        TaskIdRoute::Remote(holder) => deliver(cli, &global_root, &holder, None),
    }
}

/// A non-id call (`orbit tool run` of any tool) whose selector names a
/// registered remote host goes there too, as the raw token does.
fn deliver_by_selector(cli: &Cli, global_root: &Path) -> Result<Preflight, OrbitError> {
    let Commands::Tool(tool) = &cli.command else {
        return Ok(Preflight::Local);
    };
    if !matches!(tool.command, ToolSubcommand::Run(_)) {
        return Ok(Preflight::Local);
    }
    let Some(selector) = workspace_value(cli)? else {
        return Ok(Preflight::Local);
    };
    let Some(holder) = hosts::selector_remote_host(global_root, &selector)? else {
        return Ok(Preflight::Local);
    };
    deliver(cli, global_root, &holder, Some(selector))
}

/// Whether this command needs host preflight, so commands without routing
/// or a qualified task selector pay no host-file read.
fn may_route(cli: &Cli) -> bool {
    match &cli.command {
        Commands::Task(task) => {
            routed_command_id(&task.command).is_some()
                || cli.workspace.as_deref().is_some_and(|selector| {
                    selector.trim().parse::<MachineQualifiedSelector>().is_ok()
                })
        }
        Commands::Tool(tool) => matches!(tool.command, ToolSubcommand::Run(_)),
        Commands::Run(run) => {
            matches!(&run.command, RunSubcommand::Auto(auto) if auto.pull.is_some())
        }
        _ => false,
    }
}

fn requested_host(command: &Commands) -> Option<String> {
    let host = match command {
        Commands::Task(task) => match &task.command {
            TaskSubcommand::Show(args) => args.routing.host.as_deref(),
            TaskSubcommand::Update(args) => args.routing.host.as_deref(),
            TaskSubcommand::Artifact(command) => command.routing.host.as_deref(),
            TaskSubcommand::ReviewReset(args) => args.routing.host.as_deref(),
            TaskSubcommand::ReconcileReview(command) => command.routing.host.as_deref(),
            _ => None,
        },
        Commands::Tool(tool) => match &tool.command {
            ToolSubcommand::Run(args) => args.host.as_deref(),
            _ => None,
        },
        Commands::Run(run) => match &run.command {
            RunSubcommand::Auto(auto) => auto.host.as_deref(),
            _ => None,
        },
        _ => None,
    };
    host.map(str::trim)
        .filter(|host| !host.is_empty())
        .map(ToOwned::to_owned)
}

fn explicit_root(cli: &Cli) -> bool {
    cli.root.is_some() || std::env::var("ORBIT_ROOT").is_ok_and(|value| !value.trim().is_empty())
}

/// The id an id-routed `orbit task` subcommand addresses.
fn routed_command_id(command: &TaskSubcommand) -> Option<&str> {
    match command {
        TaskSubcommand::Show(args) => Some(&args.id),
        TaskSubcommand::Update(args) => Some(&args.id),
        TaskSubcommand::Artifact(artifact) => Some(match &artifact.command {
            TaskArtifactSubcommand::Put(args) => &args.id,
            TaskArtifactSubcommand::Get(args) => &args.id,
        }),
        TaskSubcommand::ReviewReset(args) => Some(&args.id),
        TaskSubcommand::ReconcileReview(command) => Some(command.command.task_id()),
        _ => None,
    }
}

/// The task id this command addresses, when it is an id-routed call.
fn routed_task_id(command: &Commands) -> Result<Option<String>, OrbitError> {
    Ok(match command {
        Commands::Task(task) => routed_command_id(&task.command).map(ToOwned::to_owned),
        Commands::Tool(tool) => match &tool.command {
            ToolSubcommand::Run(args) if is_id_routed_tool(&args.name) => args
                .parsed_input()?
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned),
            _ => None,
        },
        _ => None,
    })
}

/// The workspace value the call names: a `tool run` input's own `workspace`
/// first, else the global `--workspace`.
fn workspace_value(cli: &Cli) -> Result<Option<String>, OrbitError> {
    if let Commands::Tool(tool) = &cli.command
        && let ToolSubcommand::Run(args) = &tool.command
        && let Some(selector) = args
            .parsed_input()?
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
    {
        return Ok(Some(selector.to_string()));
    }
    Ok(cli
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
        .map(ToOwned::to_owned))
}

fn set_workspace_value(cli: &mut Cli, selector: String) -> Result<(), OrbitError> {
    if let Commands::Tool(tool) = &mut cli.command
        && let ToolSubcommand::Run(args) = &mut tool.command
    {
        let mut input = args.parsed_input()?;
        if input.get("workspace").is_some() || cli.workspace.is_none() {
            let object = input.as_object_mut().ok_or_else(|| {
                OrbitError::InvalidInput("tool input must be a JSON object".into())
            })?;
            object.insert("workspace".into(), Value::String(selector));
            args.replace_input(input);
            return Ok(());
        }
    }
    cli.workspace = Some(selector);
    Ok(())
}

/// `--pull` with or without `--host`. Orbit never picks the owner host itself.
fn pull_selector(global_root: &Path, host: Option<&str>, pull: &str) -> Result<String, OrbitError> {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        if pull.trim().parse::<MachineQualifiedSelector>().is_err() {
            return Err(OrbitError::UnknownSelector(format!(
                "'{pull}' is not a host-qualified selector; name the owner with `--host <host>` \
                 (see `orbit host list`), or pass the full selector federated discovery lists \
                 (`orbit_workspace_list`)"
            )));
        }
        return Ok(pull.trim().to_string());
    };
    match hosts::resolve_host_workspace(global_root, host, pull)? {
        HostWorkspaceRoute::Remote { selector, .. } => Ok(selector),
        HostWorkspaceRoute::Local { .. } => Err(OrbitError::CapabilityRefused(format!(
            "`--pull` pulls from another host's workspace, and '{host}' is this machine; run \
             plain `orbit run auto` to drain a local backlog"
        ))),
    }
}

/// The authority a routed call asks for: an operator only when this process
/// resolves as one and declares no agent. An agent never propagates
/// `--operator`; the destination's own rules decide the rest.
fn caller_authority() -> McpSessionAuthority {
    let envelope = CallerEnvelope::from_process_env(&ToolSessionContext::default());
    let caller = CallerCapabilities::resolve(&envelope);
    if caller.grants().contains(&McpCapability::Operator) && !envelope.agent_declared {
        McpSessionAuthority::Operator
    } else {
        McpSessionAuthority::Agent
    }
}

/// One tool call a command reduces to, and how its answer renders.
struct RoutedCall {
    name: String,
    input: Value,
    render: Render,
}

enum Render {
    TaskShow { fields: Vec<String> },
    TaskWrite { verb: &'static str },
    ArtifactGet { out: Option<PathBuf> },
    ReviewReset,
    Reconcile,
    ToolRun { full: bool, fields: Vec<String> },
}

fn routed_call(cli: &Cli, holder: &HostEntry) -> Result<RoutedCall, OrbitError> {
    match &cli.command {
        Commands::Task(task) => task_call(&task.command, holder),
        Commands::Tool(tool) => match &tool.command {
            ToolSubcommand::Run(args) => {
                if args.dry_run {
                    return Err(OrbitError::InvalidInput(format!(
                        "--dry-run checks this host's tool admission; run it on '{}': `ssh {} \
                         orbit tool run {} --dry-run …`",
                        holder.name, holder.ssh, args.name
                    )));
                }
                let mut input = args.parsed_input()?;
                super::tool::request_write_sidecars_from_cli_fields(
                    &args.name,
                    &mut input,
                    &args.fields,
                )?;
                Ok(RoutedCall {
                    name: args.name.clone(),
                    input,
                    render: Render::ToolRun {
                        full: args.full,
                        fields: args.fields.clone(),
                    },
                })
            }
            _ => Err(not_routable()),
        },
        _ => Err(not_routable()),
    }
}

fn task_call(command: &TaskSubcommand, holder: &HostEntry) -> Result<RoutedCall, OrbitError> {
    Ok(match command {
        TaskSubcommand::Show(args) => {
            let fields = args
                .fields
                .iter()
                .flat_map(|field| field.split(','))
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let mut input = json!({ "id": args.id });
            if !fields.is_empty() {
                input["fields"] = json!(fields);
            }
            RoutedCall {
                name: "orbit.task.show".into(),
                input,
                render: Render::TaskShow { fields },
            }
        }
        TaskSubcommand::Update(args) => RoutedCall {
            name: "orbit.task.update".into(),
            input: args.remote_tool_input(&holder.ssh)?,
            render: Render::TaskWrite { verb: "Updated" },
        },
        TaskSubcommand::Artifact(artifact) => match &artifact.command {
            TaskArtifactSubcommand::Put(args) => {
                let mut input = json!({
                    "id": args.id,
                    "source_path": args.source_path,
                });
                if let Some(path) = &args.artifact_path {
                    input["path"] = json!(path);
                }
                if let (_, Some(model)) = super::task::mutation_identity(args.model.clone()) {
                    input["model"] = json!(model);
                }
                // The bytes cross, never the path: the source is read here,
                // confined to the current directory.
                let cwd = std::env::current_dir()?;
                RoutedCall {
                    name: "orbit.task.artifact.put".into(),
                    input: orbit_cmd::prepare_remote_task_artifact_put(
                        input,
                        Some(&cwd),
                        Some(&cwd),
                    )?,
                    render: Render::TaskWrite {
                        verb: "Stored an artifact on",
                    },
                }
            }
            TaskArtifactSubcommand::Get(args) => RoutedCall {
                name: "orbit.task.artifact.get".into(),
                input: json!({ "id": args.id, "path": args.path }),
                render: Render::ArtifactGet {
                    out: args.out.clone(),
                },
            },
        },
        TaskSubcommand::ReviewReset(args) => RoutedCall {
            name: "orbit.task.review_reset".into(),
            input: args.tool_input(),
            render: Render::ReviewReset,
        },
        TaskSubcommand::ReconcileReview(command) => RoutedCall {
            name: "orbit.task.reconcile_review".into(),
            input: command.command.tool_input(),
            render: Render::Reconcile,
        },
        _ => return Err(not_routable()),
    })
}

fn not_routable() -> OrbitError {
    OrbitError::InvalidInput("this command has no tool call to deliver to another host".into())
}

fn deliver(
    cli: &Cli,
    global_root: &Path,
    holder: &HostEntry,
    selector: Option<String>,
) -> Result<Preflight, OrbitError> {
    let call = routed_call(cli, holder)?;
    let caller = match inspect_machine_identity(global_root)? {
        MachineIdentityState::Present(identity) => identity.id,
        MachineIdentityState::Absent => {
            return Err(OrbitError::InvalidInput(
                "this machine has no [machine] identity yet; run `orbit init` first".into(),
            ));
        }
    };
    let client = hosts::routed_client(global_root, &caller, caller_authority())?;
    let mut input = call.input;
    if let Some(selector) = selector
        && let Some(object) = input.as_object_mut()
    {
        object.insert("workspace".into(), Value::String(selector));
    }
    let answer =
        orbit_mcp::McpHost::call_tool(&client, &call.name, input, ToolSessionContext::default());
    Ok(Preflight::Remote(answer.and_then(|value| {
        render(&call.name, call.render, value, holder)
    })))
}

fn host_json(holder: &HostEntry) -> Value {
    json!({ "name": holder.name, "machine_id": holder.machine_id })
}

/// Name the answering host on an object answer.
fn with_host(mut value: Value, holder: &HostEntry) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("host".into(), host_json(holder));
    }
    value
}

fn render(name: &str, render: Render, value: Value, holder: &HostEntry) -> CommandOut {
    match render {
        Render::TaskShow { fields } => {
            let value = match fields.as_slice() {
                [field] if !value.is_object() => json!({ field.as_str(): value }),
                _ => value,
            };
            let text = if fields.is_empty() {
                task_summary(&value, holder)
            } else {
                field_lines(&value, &fields)
            };
            let doc = if fields.is_empty() {
                with_host(value, holder)
            } else {
                value
            };
            Ok(Payload::detail(doc, text).into())
        }
        Render::TaskWrite { verb } => {
            let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
            let text = format!("{verb} task '{id}' on host '{}'", holder.name);
            Ok(Payload::detail(with_host(value, holder), text).into())
        }
        Render::ArtifactGet { out } => artifact_output(value, out, holder),
        Render::ReviewReset => Ok(Payload::detail(
            with_host(value, holder),
            super::task::review_reset::REVIEW_RESET_TEXT,
        )
        .into()),
        Render::Reconcile => {
            let text = super::task::reconcile_review::reconciliation_text(&value);
            Ok(Payload::detail(with_host(value, holder), text).into())
        }
        Render::ToolRun { full, fields } => {
            let shaped = super::tool::shape_tool_output(name, value, full, &fields)?;
            Ok(Payload::document(shaped).into())
        }
    }
}

/// The human view of a routed `task show`: the task's own fields and where
/// they were read, without the local-only sidecars a remote answer lacks.
fn task_summary(task: &Value, holder: &HostEntry) -> String {
    let text = |key: &str| task.get(key).and_then(Value::as_str).unwrap_or_default();
    let mut lines = vec![
        format!("ID: {}", text("id")),
        format!("Host: {} ({})", holder.name, holder.machine_id),
    ];
    if let Some(workspace) = task.get("workspace").and_then(Value::as_object) {
        let field = |key: &str| {
            workspace
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        lines.push(format!("Workspace: {} ({})", field("name"), field("id")));
    }
    for (label, key) in [
        ("Title", "title"),
        ("Status", "status"),
        ("Priority", "priority"),
        ("Complexity", "complexity"),
        ("Type", "type"),
        ("Description", "description"),
    ] {
        if !text(key).is_empty() {
            lines.push(format!("{label}: {}", text(key)));
        }
    }
    let list = |key: &str| {
        task.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let criteria = list("acceptance_criteria");
    if !criteria.is_empty() {
        lines.push("Acceptance Criteria:".into());
        lines.extend(criteria.iter().map(|criterion| format!("  - {criterion}")));
    }
    let tags = list("tags");
    if !tags.is_empty() {
        lines.push(format!("Tags: {}", tags.join(", ")));
    }
    lines.join("\n")
}

fn field_lines(value: &Value, fields: &[String]) -> String {
    let object = value.as_object().cloned().unwrap_or_else(Map::new);
    fields
        .iter()
        .map(|field| match object.get(field) {
            Some(Value::String(text)) => format!("{field}: {text}"),
            Some(other) => format!("{field}: {other}"),
            None => format!("{field}:"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A routed `task artifact get`. Text crosses as text; any other payload
/// arrives as base64, which this view does not decode, so it is read on the
/// host that holds it.
fn artifact_output(value: Value, out: Option<PathBuf>, holder: &HostEntry) -> CommandOut {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(content) = value
        .get("content")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return Err(OrbitError::InvalidInput(format!(
            "artifact '{path}' on task '{id}' is not text; read it on host '{}': `ssh {} orbit \
             task artifact get {id} {path} --out <FILE>`",
            holder.name, holder.ssh
        )));
    };
    let mut doc = json!({
        "id": id,
        "path": path,
        "media_type": value.get("media_type").cloned().unwrap_or(Value::Null),
        "size": content.len(),
        "presentation": "text",
        "written_to": out.as_ref().map(|out| out.display().to_string()),
    });
    doc["host"] = host_json(holder);
    if let Some(out) = out {
        #[allow(
            clippy::disallowed_methods,
            reason = "remote artifact exports must match local exports to user-selected files, symlinks or devices"
        )]
        std::fs::write(&out, content.as_bytes()).map_err(|error| {
            OrbitError::Io(format!("write artifact to '{}': {error}", out.display()))
        })?;
        let text = format!(
            "Wrote {} bytes of '{path}' from host '{}' to {}",
            content.len(),
            holder.name,
            out.display()
        );
        return Ok(Payload::detail(doc, text).into());
    }
    Ok(Payload::detail(doc, content).into())
}
