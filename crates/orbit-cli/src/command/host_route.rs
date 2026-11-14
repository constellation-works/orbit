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
use orbit_common::governance::notice::NoticeVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_core::application::task::{TaskArtifactRead, TaskListResultPolicy};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{MachineQualifiedSelector, is_id_routed_tool};
use orbit_registry::hosts::HostEntry;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::tool::{McpCapability, ToolSessionContext, tool_family, tool_operation_name};
use serde_json::{Map, Value, json};

use super::run::RunSubcommand;
use super::task::TaskInvocation;
use super::task::TaskSubcommand;
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
    // Plugin callbacks cannot use the host's SSH credentials. Refuse before
    // discovery as well as delivery, using the same root that authenticates
    // local callback sessions. Resolving --host can itself attempt SSH.
    if let Commands::Tool(tool) = &cli.command
        && let ToolSubcommand::Run(args) = &tool.command
        && let Some(selector) = workspace_value(cli)?
    {
        let destination = if let Some(host) = host.as_deref()
            && hosts::host_ssh_target(&global_root, host)?.is_some()
        {
            Some(format!("--host {host} --workspace {selector}"))
        } else if hosts::selector_remote_host(&global_root, &selector)?.is_some() {
            Some(selector)
        } else {
            None
        };
        if let Some(destination) = destination {
            let callback_root = orbit_core::runtime::resolve_generation_root(cli.root.as_deref())?;
            orbit_core::adapter::command::refuse_plugin_child_remote_callback(
                &callback_root,
                &args.name,
                &destination,
            )?;
        }
    }
    if let Commands::Tool(tool) = &cli.command
        && let ToolSubcommand::Run(args) = &tool.command
        && let Some(selector) = workspace_value(cli)?
    {
        let callback_root = orbit_core::runtime::resolve_generation_root(cli.root.as_deref())?;
        orbit_cmd::registry_runtime::refuse_cli_callback_workspace(
            &callback_root,
            &args.name,
            &selector,
        )?;
    }
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
            *pull = hosts::pull_selector(&global_root, auto.host.as_deref(), pull)?;
        }
        return Ok(Preflight::Local);
    }
    if matches!(&cli.command, Commands::Notice(_)) {
        return notice_preflight(cli, &global_root);
    }
    if let Commands::Attention(attention) = &cli.command {
        return attention_preflight(cli, attention, &global_root);
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
        TaskIdRoute::Remote(holder) => {
            if let Commands::Tool(tool) = &cli.command
                && let ToolSubcommand::Run(args) = &tool.command
            {
                let callback_root =
                    orbit_core::runtime::resolve_generation_root(cli.root.as_deref())?;
                orbit_core::adapter::command::refuse_plugin_child_remote_callback(
                    &callback_root,
                    &args.name,
                    &task_id,
                )?;
            }
            deliver(cli, &global_root, &holder, None)
        }
    }
}

/// `orbit notice …` routing: a host-qualified `--workspace` selector names
/// the owner the call belongs to, exactly as for a task tool, and a
/// workspace-less `list` on a host with registered hosts aggregates every
/// owner through the federated client. Anything else runs locally, as
/// before. (`--host` is a notice flag of its own — the machines a
/// maintenance notice concerns — so the selector is the owner address.)
fn notice_preflight(cli: &mut Cli, global_root: &Path) -> Result<Preflight, OrbitError> {
    if let Some(selector) = workspace_value(cli)? {
        if let Some(holder) = hosts::selector_remote_host(global_root, &selector)? {
            return deliver(cli, global_root, &holder, Some(selector));
        }
        if let Ok(qualified) = selector.parse::<MachineQualifiedSelector>() {
            // The selector names a host, and no remote holder answered for
            // it: it is this machine (rewrite to the local `ws_*` the
            // bootstrap resolves) or a host this machine does not know.
            let local = matches!(
                inspect_machine_identity(global_root)?,
                MachineIdentityState::Present(identity) if identity.id == qualified.machine_id()
            );
            if !local {
                return Err(OrbitError::UnknownSelector(selector));
            }
            cli.workspace = Some(qualified.workspace_id().to_string());
        }
        return Ok(Preflight::Local);
    }
    let Commands::Notice(notice) = &cli.command else {
        return Ok(Preflight::Local);
    };
    if notice.command.spec.verb != NoticeVerb::List || !workspace_less(cli)? {
        return Ok(Preflight::Local);
    }
    if orbit_mcp::federated::load_destinations(global_root)?.is_empty() {
        return Ok(Preflight::Local);
    }
    let input = Value::Object(notice.command.input.clone());
    deliver_notice_aggregate(
        global_root,
        notice.command.spec.tool_name,
        input,
        AggregateRender::Notice(NoticeVerb::List),
    )
}

/// `orbit attention` on a host with registered hosts: an explicit scope
/// (`--all`, `--host`, `--workspace`) or a call from outside any workspace
/// fans out through the federated client, whose aggregator reads every
/// in-scope host and applies the cursor and limit once. A bare call inside a
/// workspace, `--root`, or a host with no registered hosts stays local.
fn attention_preflight(
    cli: &Cli,
    attention: &super::attention::AttentionCommand,
    global_root: &Path,
) -> Result<Preflight, OrbitError> {
    if explicit_root(cli) || orbit_mcp::federated::load_destinations(global_root)?.is_empty() {
        return Ok(Preflight::Local);
    }
    let input = &attention.query.input;
    let scoped = input.get("all") == Some(&Value::Bool(true))
        || input.contains_key("host")
        || input.contains_key("workspace");
    if !scoped && !workspace_less(cli)? {
        return Ok(Preflight::Local);
    }
    if matches!(
        inspect_machine_identity(global_root)?,
        MachineIdentityState::Absent
    ) {
        return Err(OrbitError::InvalidInput(
            "this machine has no [machine] identity yet; run `orbit init` first".into(),
        ));
    }
    let client =
        super::mcp::federated_aggregate_client(global_root.to_path_buf(), caller_authority())?;
    let answer = orbit_mcp::McpHost::call_tool(
        &client,
        attention.query.spec.tool_name,
        Value::Object(input.clone()),
        ToolSessionContext::default(),
    );
    Ok(Preflight::Remote(
        answer.and_then(super::attention::render_attention_envelope),
    ))
}

/// Whether this invocation names no workspace at all: no selector, no
/// explicit root, and no initialized workspace discoverable from the cwd.
fn workspace_less(cli: &Cli) -> Result<bool, OrbitError> {
    if workspace_value(cli)?.is_some() || explicit_root(cli) {
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    Ok(
        orbit_cmd::registry_runtime::RegisteredRuntimeFactory::try_resolve_initialized_cwd_roots(
            &cwd,
        )?
        .is_none(),
    )
}

/// How a workspace-less notice list's merged answer renders.
enum AggregateRender {
    Notice(NoticeVerb),
    ToolRun {
        full: bool,
        fields: Vec<String>,
        bare_field: bool,
        list_policy: TaskListResultPolicy,
    },
}

/// Deliver a workspace-less `orbit.notice.list` to the federated client,
/// whose mux fans out one aggregate read per owner and merges them. The
/// client composes this machine in-process beside the SSH remotes, so
/// the local owner is read like any other instead of degraded.
fn deliver_notice_aggregate(
    global_root: &Path,
    name: &str,
    input: Value,
    render: AggregateRender,
) -> Result<Preflight, OrbitError> {
    if matches!(
        inspect_machine_identity(global_root)?,
        MachineIdentityState::Absent
    ) {
        return Err(OrbitError::InvalidInput(
            "this machine has no [machine] identity yet; run `orbit init` first".into(),
        ));
    }
    let client =
        super::mcp::federated_aggregate_client(global_root.to_path_buf(), caller_authority())?;
    let answer = orbit_mcp::McpHost::call_tool(&client, name, input, ToolSessionContext::default());
    Ok(Preflight::Remote(answer.and_then(|value| match render {
        AggregateRender::Notice(verb) => Ok(super::notice::render_notice(verb, value).into()),
        AggregateRender::ToolRun {
            full,
            fields,
            bare_field,
            list_policy,
        } => {
            let shaped = super::tool::shape_tool_output(
                name,
                value,
                &list_policy,
                full,
                &fields,
                bare_field,
            )?;
            Ok(Payload::document(shaped).into())
        }
    })))
}

/// A non-id call (`orbit tool run` of any tool) whose selector names a
/// registered remote host goes there too, as the raw token does.
fn deliver_by_selector(cli: &Cli, global_root: &Path) -> Result<Preflight, OrbitError> {
    let Commands::Tool(tool) = &cli.command else {
        return Ok(Preflight::Local);
    };
    let ToolSubcommand::Run(args) = &tool.command else {
        return Ok(Preflight::Local);
    };
    // A workspace-less notice list aggregates every owner, as the CLI verb
    // does; inside a workspace it stays the local workspace's list.
    if args.name == "orbit.notice.list"
        && !args.dry_run
        && workspace_less(cli)?
        && !orbit_mcp::federated::load_destinations(global_root)?.is_empty()
    {
        let mut input = args.parsed_input()?;
        super::tool::request_write_sidecars_from_cli_fields(&args.name, &mut input, &args.fields)?;
        let bare_field = super::tool::answers_with_bare_field(&args.name, &input)?;
        let list_policy = TaskListResultPolicy::for_call(
            &args.name,
            &input,
            super::tool::task_list_result_profile(args.full, &args.fields),
        );
        return deliver_notice_aggregate(
            global_root,
            &args.name.clone(),
            input,
            AggregateRender::ToolRun {
                full: args.full,
                fields: args.fields.clone(),
                bare_field,
                list_policy,
            },
        );
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
        Commands::Notice(_) | Commands::Attention(_) => true,
        Commands::Run(run) => {
            matches!(&run.command, RunSubcommand::Auto(auto) if auto.pull.is_some())
        }
        _ => false,
    }
}

fn requested_host(command: &Commands) -> Option<String> {
    let host = match command {
        Commands::Task(task) => match &task.command {
            // A registry-derived verb takes `--host` as a CLI-only parameter.
            TaskSubcommand::Operation(TaskInvocation::Registry(invocation)) => {
                invocation.input.get("host").and_then(Value::as_str)
            }
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
        // A registry-derived verb routes by id when its tool does.
        TaskSubcommand::Operation(TaskInvocation::Registry(invocation))
            if matches!(
                invocation.spec.verb,
                TaskVerb::Update | TaskVerb::ArtifactPut
            ) || is_id_routed_tool(invocation.spec.tool_name) =>
        {
            invocation.target_id()
        }
        _ => None,
    }
}

/// The task id this command addresses, when it is an id-routed call.
fn routed_task_id(command: &Commands) -> Result<Option<String>, OrbitError> {
    Ok(match command {
        Commands::Task(task) => routed_command_id(&task.command).map(ToOwned::to_owned),
        Commands::Tool(tool) => match &tool.command {
            // A family names its operation in the input.
            ToolSubcommand::Run(args)
                if is_id_routed_tool(&args.name) || tool_family(&args.name).is_some() =>
            {
                let input = args.parsed_input()?;
                if is_id_routed_tool(tool_operation_name(&args.name, &input)) {
                    input
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(ToOwned::to_owned)
                } else {
                    None
                }
            }
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

/// The authority a routed call asks for: an operator only when this process
/// resolves as one and declares no agent. An agent never propagates
/// `--operator`; the destination's own rules decide the rest.
pub(crate) fn caller_authority() -> McpSessionAuthority {
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
    TaskShow {
        fields: Vec<String>,
    },
    TaskWrite {
        verb: &'static str,
    },
    ArtifactGet {
        out: Option<PathBuf>,
    },
    Notice {
        verb: NoticeVerb,
    },
    ReviewReset,
    Requeue,
    Reconcile,
    Pilot,
    ToolRun {
        full: bool,
        fields: Vec<String>,
        bare_field: bool,
        list_policy: TaskListResultPolicy,
    },
}

fn routed_call(cli: &Cli, holder: &HostEntry) -> Result<RoutedCall, OrbitError> {
    match &cli.command {
        Commands::Notice(notice) => {
            let invocation = &notice.command;
            let mut input = invocation.input.clone();
            if invocation.spec.verb == NoticeVerb::Post {
                super::notice::post_cli_input(&mut input)?;
            }
            Ok(RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: Value::Object(input),
                render: Render::Notice {
                    verb: invocation.spec.verb,
                },
            })
        }
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
                let bare_field = super::tool::answers_with_bare_field(&args.name, &input)?;
                let list_policy = TaskListResultPolicy::for_call(
                    &args.name,
                    &input,
                    super::tool::task_list_result_profile(args.full, &args.fields),
                );
                Ok(RoutedCall {
                    name: args.name.clone(),
                    input,
                    render: Render::ToolRun {
                        full: args.full,
                        fields: args.fields.clone(),
                        bare_field,
                        list_policy,
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
        TaskSubcommand::Operation(TaskInvocation::Registry(invocation)) => match invocation
            .spec
            .verb
        {
            TaskVerb::Show => {
                let id = invocation.target_id().unwrap_or_default();
                let fields = invocation
                    .input
                    .get("fields")
                    .and_then(Value::as_array)
                    .map(|fields| {
                        fields
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut input = json!({ "id": id });
                if !fields.is_empty() {
                    input["fields"] = json!(fields);
                }
                RoutedCall {
                    name: invocation.spec.tool_name.into(),
                    input,
                    render: Render::TaskShow { fields },
                }
            }
            TaskVerb::Update => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: super::task::operation::remote_update_input(&invocation.input, &holder.ssh)?,
                render: Render::TaskWrite { verb: "Updated" },
            },
            TaskVerb::ArtifactPut => {
                let mut input = invocation.input.clone();
                input.shift_remove("host");
                let model = input
                    .get("model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let (_, Some(model)) = super::task::mutation_identity(model) {
                    input.insert("model".into(), json!(model));
                }
                let cwd = std::env::current_dir()?;
                RoutedCall {
                    name: invocation.spec.tool_name.into(),
                    input: orbit_cmd::prepare_remote_task_artifact_put(
                        Value::Object(input),
                        Some(&cwd),
                        Some(&cwd),
                        None,
                    )?,
                    render: Render::TaskWrite {
                        verb: "Stored an artifact on",
                    },
                }
            }
            TaskVerb::ArtifactGet => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: json!({ "id": invocation.input.get("id"), "path": invocation.input.get("path") }),
                render: Render::ArtifactGet {
                    out: invocation
                        .input
                        .get("out")
                        .and_then(Value::as_str)
                        .map(PathBuf::from),
                },
            },
            TaskVerb::ReviewReset => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: without_host(&invocation.input),
                render: Render::ReviewReset,
            },
            TaskVerb::ReconcileReview => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: without_host(&invocation.input),
                render: Render::Reconcile,
            },
            TaskVerb::Repilot => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: without_host(&invocation.input),
                render: Render::Pilot,
            },
            TaskVerb::Requeue => RoutedCall {
                name: invocation.spec.tool_name.into(),
                input: without_host(&invocation.input),
                render: Render::Requeue,
            },
            TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::Add
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::BlockTriage
            | TaskVerb::Reject
            | TaskVerb::Delete => {
                return Err(not_routable());
            }
        },
        _ => return Err(not_routable()),
    })
}

/// A verb's tool input without the CLI-only `--host` it was routed by.
fn without_host(input: &Map<String, Value>) -> Value {
    let mut input = input.clone();
    input.shift_remove("host");
    Value::Object(input)
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
    // Output policy is keyed by the operation a family call names.
    let operation = tool_operation_name(&call.name, &input).to_owned();
    let answer =
        orbit_mcp::McpHost::call_tool(&client, &call.name, input, ToolSessionContext::default());
    Ok(Preflight::Remote(answer.and_then(|value| {
        render(&operation, call.render, value, holder)
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
        Render::Notice { verb } => Ok(super::notice::render_notice(verb, value).into()),
        Render::TaskShow { fields } => {
            // The CLI's single-field JSON is the bare value. A current host
            // keys a `fields` projection by field name, so unwrap that key;
            // an older host's bare answer reaches here as MCP
            // structuredContent's `items`/`value` envelope, so undo that too.
            let value = if matches!(fields.as_slice(), [field] if field != "terminal")
                && value.as_object().is_some_and(|object| object.len() == 1)
            {
                value
                    .get("items")
                    .filter(|value| value.is_array())
                    .or_else(|| {
                        value
                            .get("value")
                            .filter(|value| !value.is_object() && !value.is_array())
                    })
                    .or_else(|| fields.first().and_then(|field| value.get(field)))
                    .cloned()
                    .unwrap_or(value)
            } else {
                value
            };
            let text = match fields.as_slice() {
                [] => task_summary(&value, holder),
                // Human field lines need keys even when the JSON value is an
                // object itself; terminal already has its documented key.
                [field] if field != "terminal" => {
                    field_lines(&json!({ field.as_str(): &value }), &fields)
                }
                _ => field_lines(&value, &fields),
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
        Render::Requeue => {
            let text = super::task::operation::requeue_text(&value);
            Ok(Payload::detail(with_host(value, holder), text).into())
        }
        Render::Pilot => Ok(Payload::document(with_host(value, holder)).into()),
        Render::Reconcile => {
            let text = super::task::reconcile_review::reconciliation_text(&value);
            Ok(Payload::detail(with_host(value, holder), text).into())
        }
        Render::ToolRun {
            full,
            fields,
            bare_field,
            list_policy,
        } => {
            let shaped = super::tool::shape_tool_output(
                name,
                value,
                &list_policy,
                full,
                &fields,
                bare_field,
            )?;
            Ok(Payload::document(shaped).into())
        }
    }
}

/// The human view of a routed `task show`: the task's own fields and where
/// they were read, without the local-only sidecars a remote answer lacks.
/// `Next:` and `Final Recovery:` use the same words as the local show.
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
    if let Some(next) = task.get("next_step") {
        lines.push(format!("Next: {}", super::task::next_step_text(next)));
    }
    if let Some(record) = task.get("final_recovery") {
        lines.push(format!(
            "Final Recovery: {}",
            super::task::final_recovery_text(record)
        ));
    }
    if let Some(hold) = task.get("completion_hold") {
        lines.push(format!(
            "Completion Hold: {}",
            super::task::completion_hold_text(hold)
        ));
    }
    if let Some(owed) = task.get("owed_criteria") {
        lines.push("Owed After Deploy:".into());
        lines.extend(
            super::task::owed_criteria_lines(owed)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
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

/// A routed `task artifact get`. `--out` writes the answer's bytes exactly as
/// a local export does; without it only text prints, and any other payload is
/// read on the host that holds it.
fn artifact_output(value: Value, out: Option<PathBuf>, holder: &HostEntry) -> CommandOut {
    let read = TaskArtifactRead::from_answer(&value);
    let written_to = out.as_ref().map(|out| out.display().to_string());
    let mut doc = read.json(written_to.as_deref());
    doc["host"] = host_json(holder);
    let TaskArtifactRead { id, path, size, .. } = &read;
    if let Some(out) = out {
        read.export_to(&out)?;
        let text = format!(
            "Wrote {size} bytes of '{path}' from host '{}' to {}",
            holder.name,
            out.display()
        );
        return Ok(Payload::detail(doc, text).into());
    }
    let Some(content) = read.printable_text() else {
        return Err(OrbitError::InvalidInput(format!(
            "artifact '{path}' on task '{id}' is not text; read it on host '{}': `ssh {} orbit \
             task artifact get {id} {path} --out <FILE>`",
            holder.name, holder.ssh
        )));
    };
    Ok(Payload::detail(doc, content.to_string()).into())
}
