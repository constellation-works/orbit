//! Compiler-enforced meaning for every top-level CLI command.
//!
//! ADR-0209: command behavior is declared as operation data. The exhaustive
//! [`Commands::operation`] match is the only top-level declaration site for
//! dispatch, runtime bootstrap, audit metadata, JSON error output, and hook
//! error suppression. Adding a [`Commands`] variant therefore requires one
//! new operation arm and the compiler rejects an incomplete registry.

use std::path::Path;

use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime};
use orbit_types::identity::{
    normalize_agent_family_for_model, normalize_optional_attribution_label,
};
use serde_json::Value;

use super::{CommandOut, CommandOutput, Commands, Execute};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMeta {
    pub command: String,
    pub subcommand: Option<String>,
    pub tool_name: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub role: String,
    pub arguments_json: Option<String>,
    pub job_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeNeed {
    Required,
    /// An explicit tool input selector must bind before cwd-based bootstrap.
    SelectedWorkspace {
        selector: String,
    },
    /// The hidden detached worker may outlive one transient SQLite writer.
    PipelineWorker,
    /// Read an existing workspace without stale-run reconciliation on open.
    ReadOnly,
    /// Inspect host plugins without treating an unregistered cwd as a workspace.
    PluginReadOnly,
    Forbidden,
    /// Bind the workspace that owns this task ID rather than the one the cwd
    /// or `--workspace` walk would pick [ORB-10797] [ORB-10961].
    ///
    /// Task IDs are a machine-global primary key, so ID-addressed task reads
    /// can resolve their owner without knowing the workspace. A `--workspace`
    /// selector still wins and still filters: the bootstrap binds that
    /// workspace, and a task owned elsewhere is simply not found.
    TaskOwner {
        task_id: String,
    },
}

pub struct DispatchContext<'a> {
    runtime: Option<&'a OrbitRuntime>,
    root_override: Option<&'a Path>,
    /// The global `--workspace` selector, for the runtime-forbidden commands
    /// that resolve their own workspaces instead of being handed one runtime.
    workspace_selector: Option<&'a str>,
}

impl<'a> DispatchContext<'a> {
    pub fn with_runtime(
        runtime: &'a OrbitRuntime,
        root_override: Option<&'a Path>,
        workspace_selector: Option<&'a str>,
    ) -> Self {
        Self {
            runtime: Some(runtime),
            root_override,
            workspace_selector,
        }
    }

    pub fn without_runtime(
        root_override: Option<&'a Path>,
        workspace_selector: Option<&'a str>,
    ) -> Self {
        Self {
            runtime: None,
            root_override,
            workspace_selector,
        }
    }

    fn runtime(&self) -> Result<&'a OrbitRuntime, OrbitError> {
        self.runtime.ok_or_else(|| {
            OrbitError::Execution(
                "command operation required a runtime but dispatch did not provide one".to_string(),
            )
        })
    }
}

pub type CommandDispatch = for<'a> fn(Commands, DispatchContext<'a>) -> CommandOut;

pub struct CommandOperation {
    pub runtime_need: RuntimeNeed,
    /// Optional owner lookup for read-only human `task show`. This keeps the
    /// command on the read-only bootstrap while preserving its ID-global
    /// workspace routing.
    pub task_owner_id: Option<String>,
    pub audit_meta: Option<CommandMeta>,
    pub json_error_preference: Option<bool>,
    pub suppress_errors: bool,
    pub dispatch: CommandDispatch,
    /// The governed operation this invocation performs, when it performs one
    /// [ORB-10453].
    ///
    /// An arm sets this to name *which* operation is being invoked; it never
    /// names a capability. The requirement lives in
    /// `orbit_common::governance::authorization::GOVERNED_OPERATIONS` and the decision is
    /// made once, in `main`, before dispatch.
    ///
    /// Commands whose destruction is flag-gated (`--confirm`) set it only for
    /// the destructive invocation, so the read-only report stays reachable.
    pub governed: Option<GovernedCommand>,
    /// Whether this invocation is one of the paths a plugin backend may reach
    /// Orbit through (plugins design §4.2) [ORB-12876].
    ///
    /// Those paths end in a tool call, which the callback allowlist gates
    /// against the plugin's `permissions.orbit_tools`. Every other command
    /// reads governed data without consulting that allowlist, so `main`
    /// refuses the whole rest of the CLI to a recognized plugin child. An arm
    /// opts in here; the default is refusal, which is what keeps a newly
    /// added command closed rather than silently open.
    pub plugin_callback_entry_point: bool,
}

/// A `<command> <subcommand>` pair to authorize before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernedCommand {
    pub command: &'static str,
    pub subcommand: &'static str,
}

impl CommandOperation {
    fn new(
        runtime_need: RuntimeNeed,
        audit_meta: Option<CommandMeta>,
        json_error_preference: Option<bool>,
        suppress_errors: bool,
        dispatch: CommandDispatch,
    ) -> Self {
        Self {
            runtime_need,
            task_owner_id: None,
            audit_meta,
            json_error_preference,
            suppress_errors,
            dispatch,
            governed: None,
            plugin_callback_entry_point: false,
        }
    }

    fn with_task_owner_id(mut self, task_id: Option<String>) -> Self {
        self.task_owner_id = task_id;
        self
    }

    /// Mark this invocation as performing a governed operation.
    ///
    /// `when` is the destructiveness predicate: `gc worktrees` reports without
    /// it and reaps with it, and only the second is governed.
    fn governed_when(
        mut self,
        when: bool,
        command: &'static str,
        subcommand: &'static str,
    ) -> Self {
        if when {
            self.governed = Some(GovernedCommand {
                command,
                subcommand,
            });
        }
        self
    }

    /// Mark this invocation as a path a plugin backend may use.
    ///
    /// `when` is the subcommand predicate: `orbit tool run` is a callback,
    /// the rest of `orbit tool` is not.
    fn plugin_callback_entry_point(mut self, when: bool) -> Self {
        self.plugin_callback_entry_point = when;
        self
    }

    /// Apply the process actor resolved by the runtime bootstrap to the CLI
    /// guard's audit row. Tool dispatch may replace this row with its own
    /// more specific audited invocation, but pre-dispatch failures and all
    /// direct commands must use the same actor identity as the runtime.
    pub fn attribute_to(mut self, actor: &ActorIdentity) -> Self {
        if let Some(meta) = self.audit_meta.as_mut() {
            meta.role = actor.audit_role().to_string();
        }
        self
    }
}

macro_rules! runtime_dispatch {
    ($variant:ident) => {{
        |command, context| match command {
            Commands::$variant(command) => command.execute(context.runtime()?),
            _ => dispatch_mismatch(stringify!($variant)),
        }
    }};
}

macro_rules! boxed_runtime_dispatch {
    ($variant:ident) => {{
        |command, context| match command {
            Commands::$variant(command) => (*command).execute(context.runtime()?),
            _ => dispatch_mismatch(stringify!($variant)),
        }
    }};
}

fn dispatch_mismatch(variant: &str) -> CommandOut {
    Err(OrbitError::Execution(format!(
        "command operation dispatch invariant violated for {variant}"
    )))
}

/// `run logs`/`run events` reconcile stale runs at runtime open by default;
/// `--no-reconcile` opens read-only so the whole command observes stored run
/// state without finalizing an orphaned run.
fn observation_runtime_need(no_reconcile: bool) -> RuntimeNeed {
    if no_reconcile {
        RuntimeNeed::ReadOnly
    } else {
        RuntimeNeed::Required
    }
}

fn admin_meta(
    command: &str,
    subcommand: Option<&str>,
    target_type: Option<&str>,
    target_id: Option<&str>,
) -> CommandMeta {
    CommandMeta {
        command: command.to_string(),
        subcommand: subcommand.map(String::from),
        tool_name: None,
        target_type: target_type.map(String::from),
        target_id: target_id.map(String::from),
        role: "admin".to_string(),
        arguments_json: None,
        job_run_id: None,
    }
}

#[path = "operation_registry.rs"]
mod registry;

fn dispatch_init(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Init(command) => command.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Init"),
    }
}

fn dispatch_workspace(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::workspace::{WorkspaceCommand, WorkspaceSubcommand};
    match command {
        Commands::Workspace(WorkspaceCommand {
            command: WorkspaceSubcommand::Init(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Workspace(WorkspaceCommand {
            command: WorkspaceSubcommand::Sync(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Workspace(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Workspace"),
    }
}

fn dispatch_mcp(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::mcp::{McpCommand, McpSubcommand};
    match command {
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Init(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Remove(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Serve(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Mcp(McpCommand {
            command: McpSubcommand::Listen(args),
        }) => args.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Mcp"),
    }
}

fn dispatch_migrate(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Migrate(command) if !command.confirm => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        Commands::Migrate(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Migrate"),
    }
}

fn dispatch_update(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Update(command) => command.execute_without_runtime(context.root_override),
        _ => dispatch_mismatch("Update"),
    }
}

fn dispatch_run(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::run::{RunCommand, RunSubcommand};
    match command {
        Commands::Run(RunCommand {
            command: RunSubcommand::ShipSweep(args),
        }) => args.execute_without_runtime(context.root_override),
        Commands::Run(command) => command.execute(context.runtime()?),
        _ => dispatch_mismatch("Run"),
    }
}

fn dispatch_sweep(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Sweep(command) => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        _ => dispatch_mismatch("Sweep"),
    }
}

fn dispatch_clock(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Clock(command) => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        _ => dispatch_mismatch("Clock"),
    }
}

fn dispatch_routine(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    match command {
        Commands::Routine(command) => {
            command.execute_without_runtime(context.root_override, context.workspace_selector)
        }
        _ => dispatch_mismatch("Routine"),
    }
}

fn dispatch_web(command: Commands, context: DispatchContext<'_>) -> CommandOut {
    use super::web::{WebCommand, WebSubcommand};
    match command {
        Commands::Web(WebCommand {
            command: WebSubcommand::Serve(args),
        }) => {
            orbit_web::serve_from_env(args, context.root_override)?;
            Ok(CommandOutput::Silent)
        }
        Commands::Web(WebCommand {
            command: WebSubcommand::Connect(args),
        }) => {
            orbit_web::connect(args, context.root_override)?;
            Ok(CommandOutput::Silent)
        }
        _ => dispatch_mismatch("Web"),
    }
}

fn tool_operation(command: &super::tool::ToolCommand) -> CommandOperation {
    use super::tool::ToolSubcommand;
    let (subcommand, tool_name, target_type, target_id, role, json_output) = match &command.command
    {
        ToolSubcommand::Run(args) => (
            "run",
            Some(args.name.clone()),
            Some("tool".to_string()),
            Some(args.name.clone()),
            tool_run_actor_role(args),
            Some(args.pretty),
        ),
        ToolSubcommand::List(_) => ("list", None, None, None, "admin".to_string(), None),
        ToolSubcommand::Show(args) => (
            "show",
            Some(args.name.clone()),
            Some("tool".to_string()),
            Some(args.name.clone()),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Add(args) => (
            "add",
            args.name.clone(),
            Some("tool".to_string()),
            args.name.clone(),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Scaffold(args) => (
            "scaffold",
            args.name.clone(),
            Some("tool".to_string()),
            args.name.clone().or_else(|| Some(args.path.clone())),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Remove(args) => (
            "remove",
            Some(args.name.clone()),
            Some("tool".to_string()),
            Some(args.name.clone()),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Enable(args) => (
            "enable",
            Some(args.name.clone()),
            Some("tool".to_string()),
            Some(args.name.clone()),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Disable(args) => (
            "disable",
            Some(args.name.clone()),
            Some("tool".to_string()),
            Some(args.name.clone()),
            "admin".to_string(),
            None,
        ),
        ToolSubcommand::Doctor => ("doctor", None, None, None, "admin".to_string(), None),
    };
    let runtime_need = match &command.command {
        ToolSubcommand::Run(args) => {
            if let Some(selector) = args.input_workspace_selector() {
                RuntimeNeed::SelectedWorkspace { selector }
            } else if let Some(task_id) = args.id_resolved_task_id() {
                RuntimeNeed::TaskOwner { task_id }
            } else {
                RuntimeNeed::Required
            }
        }
        ToolSubcommand::List(_) => RuntimeNeed::ReadOnly,
        _ => RuntimeNeed::Required,
    };
    CommandOperation::new(
        runtime_need,
        Some(CommandMeta {
            command: "tool".to_string(),
            subcommand: Some(subcommand.to_string()),
            tool_name,
            target_type,
            target_id,
            role,
            arguments_json: None,
            job_run_id: None,
        }),
        json_output,
        false,
        runtime_dispatch!(Tool),
    )
    .plugin_callback_entry_point(matches!(&command.command, ToolSubcommand::Run(_)))
}

fn tool_run_actor_role(args: &super::tool::ToolRunArgs) -> String {
    let (input_agent, input_model) = tool_run_input_identity(args);
    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let has_input_identity = input_agent.is_some() || input_model.is_some();
    let has_flag_identity = args.agent.is_some() || args.model.is_some();
    let (agent, model) = if has_input_identity {
        (input_agent, input_model)
    } else if has_flag_identity {
        (args.agent.clone(), args.model.clone())
    } else {
        (env_agent, env_model)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
        .ok()
        .flatten()
        .or(agent);

    normalize_optional_attribution_label(model.as_deref().or(agent.as_deref()), model.as_deref())
        .unwrap_or_else(|| "agent".to_string())
}

fn tool_run_input_identity(args: &super::tool::ToolRunArgs) -> (Option<String>, Option<String>) {
    let value = args
        .input
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .or_else(|| {
            args.input_file.as_deref().and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            })
        });

    match value {
        Some(Value::Object(map)) => (
            map.get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            map.get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        ),
        _ => (None, None),
    }
}
