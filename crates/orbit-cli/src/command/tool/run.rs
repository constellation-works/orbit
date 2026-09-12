use std::sync::OnceLock;

use clap::Args;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_cmd::task_owner::bound_workspace_identity;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::{HostIdentityState, inspect_host_identity};
use orbit_types::tool::{McpTransport, ToolSessionContext};
use serde_json::{Map, Value};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct ToolRunArgs {
    /// Tool name
    pub name: String,
    /// JSON input for the tool (use --input-file to avoid shell escaping issues with rich content)
    #[arg(long)]
    pub input: Option<String>,
    /// Path to a JSON file to use as input (bypasses shell escaping; preferred for markdown or multi-line content)
    #[arg(long, conflicts_with = "input")]
    pub input_file: Option<String>,
    /// Deprecated explicit agent family for provenance attribution (prefer --model)
    #[arg(long)]
    pub agent: Option<String>,
    /// Exact agent model for provenance attribution (overrides ORBIT_AGENT_MODEL)
    #[arg(long)]
    pub model: Option<String>,
    /// Validate without executing
    #[arg(long)]
    pub dry_run: bool,
    /// Comma-separated top-level fields to keep from object output. For an
    /// envelope-shaped result (e.g. `orbit.task.list`'s `{tasks, total,
    /// truncated}`), fields project each record inside the envelope's
    /// record array instead of the envelope's own top-level keys. Task
    /// add/update/approve/start default to the full record; pass this to
    /// opt into a compact projection.
    #[arg(long, value_delimiter = ',', conflicts_with = "full")]
    pub fields: Vec<String>,
    /// Return the tool's full unfiltered JSON output. Task add/update/approve/start
    /// already default to the full record; this still expands compact list output.
    #[arg(long)]
    pub full: bool,
    /// Compatibility alias for pretty-printing JSON error output
    #[arg(long, hide = true)]
    pub pretty: bool,
    #[arg(skip)]
    pub(crate) parsed_input: OnceLock<Result<Value, String>>,
}

impl ToolRunArgs {
    /// Read and parse tool input once for all pre-dispatch and execution paths
    /// in this invocation. An unreadable `--input-file` must not be silently
    /// retried by task-owner bootstrap or audit metadata.
    pub(crate) fn parsed_input(&self) -> Result<Value, OrbitError> {
        self.parsed_input
            .get_or_init(|| self.load_input())
            .clone()
            .map_err(OrbitError::InvalidInput)
    }

    fn load_input(&self) -> Result<Value, String> {
        if let Some(path) = &self.input_file {
            let raw = std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read input file '{path}': {error}"))?;
            serde_json::from_str(&raw).map_err(|error| format!("invalid JSON in '{path}': {error}"))
        } else {
            match &self.input {
                Some(raw) => serde_json::from_str(raw)
                    .map_err(|error| format!("invalid JSON input: {error}")),
                None => Ok(Value::Object(Default::default())),
            }
        }
    }

    /// Globally unique task ID from an id-resolved tool's `orbit tool run`
    /// input.
    ///
    /// The CLI bootstraps `command::mcp::ID_RESOLVED_WORKSPACE_TOOLS` through
    /// the host task registry rather than cwd, matching `orbit task show`
    /// [ORB-10961] and `orbit task artifact get` [ORB-12263]. Sharing that
    /// list with the MCP server's own routing keeps the two surfaces from
    /// drifting apart. Other tools keep the ordinary workspace runtime.
    pub(crate) fn id_resolved_task_id(&self) -> Option<String> {
        if !crate::command::mcp::ID_RESOLVED_WORKSPACE_TOOLS.contains(&self.name.as_str()) {
            return None;
        }
        let value = self.parsed_input().ok()?;
        value
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
    }
}

impl Execute for ToolRunArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut input = self.parsed_input()?;

        // Resolve `workspace` once above local CLI tools, then bind or fail
        // closed. MCP uses its own server-side selector resolution.
        // `orbit.task.show` is bootstrapped via RuntimeNeed::TaskOwner so an
        // id-only call is not cwd-bound [ORB-10961]. An explicit `workspace`
        // in the tool input is still a fail-closed filter through this bind.
        let bound = RegisteredRuntimeFactory::bind_cli_tool_workspace(runtime, &mut input)?;
        let runtime = bound.as_ref().unwrap_or(runtime);

        if self.dry_run {
            let result = runtime.run_tool_dry_run(&self.name, &input)?;
            let policy = if result.policy_allowed {
                "allowed"
            } else {
                "denied"
            };
            let missing = if result.missing_params.is_empty() {
                "(none)".to_string()
            } else {
                result.missing_params.join(", ")
            };
            let doc = serde_json::json!({
                "tool_name": result.tool_name,
                "policy_allowed": result.policy_allowed,
                "missing_params": result.missing_params,
            });
            let text = format!(
                "Tool:           {}\nPolicy:         {policy}\nMissing params: {missing}",
                result.tool_name
            );
            return Ok(Payload::detail(doc, text).into());
        }

        let owner = bound_workspace_identity(runtime);
        let session_context = local_tool_session_context(runtime, owner.as_ref())?;
        let output = runtime.execute_tool_command_with_session_context(
            &self.name,
            input.clone(),
            self.agent,
            self.model,
            session_context,
        )?;
        let output = crate::command::task::show::attach_bound_workspace_identity(
            &self.name,
            &input,
            owner.as_ref(),
            output,
        )?;
        let output = shape_tool_output(&self.name, output, self.full, &self.fields);

        Ok(Payload::document(output).into())
    }
}

pub(super) const LOCAL_MACHINE_ID_FALLBACK: &str = "host/local";

pub(super) fn local_tool_session_context(
    runtime: &OrbitRuntime,
    owner: Option<&orbit_cmd::task_owner::WorkspaceIdentity>,
) -> Result<ToolSessionContext, OrbitError> {
    let (machine_id, host_id) = local_machine_identity(&runtime.global_root())?;
    Ok(ToolSessionContext {
        workspace: Some(runtime.paths().repo_root.to_string_lossy().into_owned()),
        workspace_id: owner
            .map(|owner| owner.id.clone())
            .or_else(|| runtime.workspace_id().ok()),
        caller_machine_id: Some(machine_id.clone()),
        caller_host_id: host_id.clone(),
        process_machine_id: Some(machine_id),
        process_host_id: host_id,
        transport: Some(McpTransport::Local),
        trace_id: Some(audit_execution_id("trace")),
        ..ToolSessionContext::default()
    })
}

pub(super) fn local_machine_identity(
    global_root: &std::path::Path,
) -> Result<(String, Option<String>), OrbitError> {
    match inspect_host_identity(global_root)? {
        HostIdentityState::Present(identity) => Ok((identity.machine_id, Some(identity.host_id))),
        HostIdentityState::Legacy {
            host_id,
            machine_id,
        } => Ok((
            machine_id.unwrap_or_else(|| LOCAL_MACHINE_ID_FALLBACK.to_string()),
            Some(host_id),
        )),
        HostIdentityState::Absent => Ok((LOCAL_MACHINE_ID_FALLBACK.to_string(), None)),
    }
}

const MINIMAL_TASK_FIELDS: &[&str] = &[
    "id",
    "title",
    "status",
    "priority",
    "type",
    "dependencies",
    "resolved_dependencies",
    "implemented_by",
    "created_at",
    "updated_at",
    "workspace",
];

pub(super) fn shape_tool_output(
    tool_name: &str,
    output: Value,
    full: bool,
    fields: &[String],
) -> Value {
    if full {
        return output;
    }

    if !fields.is_empty() {
        if tool_name == "orbit.task.list" {
            return project_task_list_output(output, fields);
        }
        return filter_top_level_fields(output, fields);
    }

    if should_project_minimal_task_output(tool_name) {
        if tool_name == "orbit.task.list" {
            return project_task_list_output(output, MINIMAL_TASK_FIELDS);
        }
        return filter_top_level_fields(
            output,
            &MINIMAL_TASK_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect::<Vec<_>>(),
        );
    }

    output
}

fn should_project_minimal_task_output(tool_name: &str) -> bool {
    matches!(tool_name, "orbit.task.list" | "orbit.task.artifact.put")
}

/// Projects `--fields` (or the default minimal set) inside the `tasks` array
/// of an `orbit.task.list` envelope, leaving `total`/`truncated` intact.
fn project_task_list_output<S: AsRef<str>>(value: Value, fields: &[S]) -> Value {
    let Value::Object(mut object) = value else {
        return value;
    };
    let Some(Value::Array(tasks)) = object.get_mut("tasks") else {
        return Value::Object(object);
    };
    let fields = fields
        .iter()
        .map(|field| field.as_ref().to_string())
        .collect::<Vec<_>>();
    let projected = std::mem::take(tasks)
        .into_iter()
        .map(|task| match task {
            Value::Object(map) => Value::Object(select_fields(map, &fields)),
            other => other,
        })
        .collect();
    *tasks = projected;
    Value::Object(object)
}

fn filter_top_level_fields(value: Value, fields: &[String]) -> Value {
    match value {
        Value::Object(map) => Value::Object(select_fields(map, fields)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| match item {
                    Value::Object(map) => Value::Object(select_fields(map, fields)),
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

fn select_fields(map: Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = map.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    selected
}
