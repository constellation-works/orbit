use clap::{Args, ValueEnum};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_cmd::task_owner::bound_workspace_identity;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::{HostIdentityState, inspect_host_identity};
use orbit_types::tool::{McpTransport, ToolSessionContext};
use serde_json::{Map, Value};

use crate::command::{CommandOut, Execute, Payload};

#[derive(Clone, ValueEnum, Default)]
pub enum OutputFormat {
    #[default]
    Json,
    Text,
}

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
    /// Execution timeout (e.g. "30s", "5000ms")
    #[arg(long)]
    pub timeout: Option<String>,
    /// Validate without executing
    #[arg(long)]
    pub dry_run: bool,
    /// Comma-separated top-level fields to keep from object output
    #[arg(long, value_delimiter = ',', conflicts_with = "full")]
    pub fields: Vec<String>,
    /// Return the tool's full unfiltered JSON output
    #[arg(long)]
    pub full: bool,
    /// Pretty-print JSON output for human debugging
    #[arg(long)]
    pub pretty: bool,
    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,
}

impl ToolRunArgs {
    /// Globally unique task ID from `orbit tool run orbit.task.show` input.
    ///
    /// The CLI bootstraps that one tool through the host task registry rather
    /// than cwd, matching `orbit task show` [ORB-10961]. Other tools keep the
    /// ordinary workspace runtime.
    pub(crate) fn task_show_id(&self) -> Option<String> {
        if self.name != "orbit.task.show" {
            return None;
        }
        let raw = if let Some(path) = &self.input_file {
            std::fs::read_to_string(path).ok()?
        } else {
            self.input.clone()?
        };
        let value: Value = serde_json::from_str(&raw).ok()?;
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
        let mut input: Value = if let Some(path) = &self.input_file {
            let raw = std::fs::read_to_string(path).map_err(|e| {
                OrbitError::InvalidInput(format!("cannot read input file '{path}': {e}"))
            })?;
            serde_json::from_str(&raw)
                .map_err(|e| OrbitError::InvalidInput(format!("invalid JSON in '{path}': {e}")))?
        } else {
            match &self.input {
                Some(raw) => serde_json::from_str(raw)
                    .map_err(|e| OrbitError::InvalidInput(format!("invalid JSON input: {e}")))?,
                None => Value::Object(Default::default()),
            }
        };

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

        let session_context = local_tool_session_context(runtime)?;
        let output = runtime.execute_tool_command_with_session_context(
            &self.name,
            input.clone(),
            self.agent,
            self.model,
            session_context,
        )?;
        let output = crate::command::task::show::attach_bound_workspace_identity(
            &self.name, &input, runtime, output,
        )?;
        let output = shape_tool_output(&self.name, &input, output, self.full, &self.fields);

        // Local `--output`/`--pretty` stay accepted (ORB-11618 owns removing
        // them). `--format` still wins: a Text human view is ignored when the
        // sink is json/ndjson, and Json always hands the document to render.
        match self.output {
            OutputFormat::Json => Ok(Payload::document(output).into()),
            OutputFormat::Text => Ok(Payload::detail(output.clone(), output.to_string()).into()),
        }
    }
}

pub(super) const LOCAL_MACHINE_ID_FALLBACK: &str = "host/local";

pub(super) fn local_tool_session_context(
    runtime: &OrbitRuntime,
) -> Result<ToolSessionContext, OrbitError> {
    let (machine_id, host_id) = local_machine_identity(&runtime.global_root())?;
    Ok(ToolSessionContext {
        workspace_id: bound_workspace_identity(runtime)
            .map(|owner| owner.id)
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
    input: &Value,
    output: Value,
    full: bool,
    fields: &[String],
) -> Value {
    if full {
        return output;
    }

    if !fields.is_empty() {
        return filter_top_level_fields(output, fields);
    }

    if should_project_minimal_task_output(tool_name, input) {
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

fn should_project_minimal_task_output(tool_name: &str, input: &Value) -> bool {
    if !matches!(
        tool_name,
        "orbit.task.list"
            | "orbit.task.show"
            | "orbit.task.add"
            | "orbit.task.artifact.put"
            | "orbit.task.update"
    ) {
        return false;
    }

    if tool_name == "orbit.task.show"
        && (input.get("field").is_some() || input.get("fields").is_some())
    {
        return false;
    }

    true
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
