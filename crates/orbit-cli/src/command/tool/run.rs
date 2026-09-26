use std::sync::OnceLock;

use clap::Args;
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_cmd::task_owner::bound_workspace_identity;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::protocol::tool_input::optional_csv_or_string_list_alias;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::task::is_task_show_projection_field;
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
    /// writes (add/update/reject) return the record without append-heavy
    /// `comments` and `history`; naming those fields here requests them
    /// from the tool.
    #[arg(long, value_delimiter = ',', conflicts_with = "full")]
    pub fields: Vec<String>,
    /// Return the tool's full unfiltered JSON output. This still expands
    /// compact list and artifact.put output. Task writes still omit
    /// `comments` and `history`; request them with `--input '{"fields":["comments","history"]}'`.
    #[arg(long)]
    pub full: bool,
    /// Compatibility alias for pretty-printing JSON error output
    #[arg(long, hide = true)]
    pub pretty: bool,
    #[arg(skip)]
    pub(crate) parsed_input: OnceLock<Result<Value, String>>,
}

impl ToolRunArgs {
    /// Selector supplied by a tool call, available before runtime bootstrap.
    /// Invalid input is reported by `execute` through `parsed_input`.
    pub(crate) fn input_workspace_selector(&self) -> Option<String> {
        self.parsed_input()
            .ok()?
            .get("workspace")?
            .as_str()
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
            .map(ToOwned::to_owned)
    }

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
        request_write_sidecars_from_cli_fields(&self.name, &mut input, &self.fields)?;

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
        let output = shape_tool_output(&self.name, output, self.full, &self.fields)?;

        Ok(Payload::document(output).into())
    }
}

pub(super) const LOCAL_MACHINE_ID_FALLBACK: &str = "host/local";

pub(super) fn local_tool_session_context(
    runtime: &OrbitRuntime,
    owner: Option<&orbit_cmd::task_owner::WorkspaceIdentity>,
) -> Result<ToolSessionContext, OrbitError> {
    let (machine_id, machine_name) = local_machine_identity(&runtime.global_root())?;
    Ok(ToolSessionContext {
        workspace: Some(runtime.paths().repo_root.to_string_lossy().into_owned()),
        workspace_id: owner
            .map(|owner| owner.id.clone())
            .or_else(|| runtime.workspace_id().ok()),
        caller_machine_id: Some(machine_id.clone()),
        caller_machine_name: machine_name.clone(),
        process_machine_id: Some(machine_id),
        process_machine_name: machine_name,
        transport: Some(McpTransport::Local),
        trace_id: Some(audit_execution_id("trace")),
        ..ToolSessionContext::default()
    })
}

pub(super) fn local_machine_identity(
    global_root: &std::path::Path,
) -> Result<(String, Option<String>), OrbitError> {
    match inspect_machine_identity(global_root)? {
        MachineIdentityState::Present(identity) => Ok((identity.id, Some(identity.name))),
        MachineIdentityState::Absent => Ok((LOCAL_MACHINE_ID_FALLBACK.to_string(), None)),
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

const TASK_WRITE_TOOLS: &[&str] = &["orbit.task.add", "orbit.task.update", "orbit.task.reject"];

const WRITE_SIDECAR_FIELDS: &[&str] = &["comments", "history"];

fn is_task_write_tool(tool_name: &str) -> bool {
    TASK_WRITE_TOOLS.contains(&tool_name)
}

/// Task writes omit `comments`/`history` unless the tool-side `fields`/`field`
/// projection asks for them. CLI `--fields` is otherwise a post-filter, so a
/// write asked for those sidecars must also request them from the tool.
pub(super) fn request_write_sidecars_from_cli_fields(
    tool_name: &str,
    input: &mut Value,
    cli_fields: &[String],
) -> Result<(), OrbitError> {
    if !is_task_write_tool(tool_name) || !requests_write_sidecar(cli_fields) {
        return Ok(());
    }
    let mut merged =
        optional_csv_or_string_list_alias(input, &["fields", "field"])?.unwrap_or_default();
    for field in cli_fields {
        if is_task_show_projection_field(field) && !merged.iter().any(|existing| existing == field)
        {
            merged.push(field.clone());
        }
    }
    let object = input.as_object_mut().ok_or_else(|| {
        OrbitError::InvalidInput("tool input must be a JSON object to request fields".to_string())
    })?;
    object.insert(
        "fields".to_string(),
        Value::Array(merged.into_iter().map(Value::String).collect()),
    );
    object.remove("field");
    Ok(())
}

fn requests_write_sidecar(fields: &[String]) -> bool {
    fields
        .iter()
        .any(|field| WRITE_SIDECAR_FIELDS.contains(&field.as_str()))
}

pub(super) fn shape_tool_output(
    tool_name: &str,
    output: Value,
    full: bool,
    fields: &[String],
) -> Result<Value, OrbitError> {
    if full {
        return Ok(output);
    }

    if !fields.is_empty() {
        if tool_name == "orbit.task.list" {
            return Ok(project_task_list_output(output, fields));
        }
        // A single-field tool projection returns the raw value (`["comments"]`
        // → the comments array). CLI `--fields` is an object-key filter, so
        // re-wrap that value before selecting.
        let output = wrap_single_field_value(output, fields);
        let shaped = filter_top_level_fields(output, fields);
        ensure_write_sidecars_present(tool_name, fields, &shaped)?;
        return Ok(shaped);
    }

    if should_project_minimal_task_output(tool_name) {
        if tool_name == "orbit.task.list" {
            return Ok(project_task_list_output(output, MINIMAL_TASK_FIELDS));
        }
        return Ok(filter_top_level_fields(
            output,
            &MINIMAL_TASK_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect::<Vec<_>>(),
        ));
    }

    Ok(output)
}

fn ensure_write_sidecars_present(
    tool_name: &str,
    fields: &[String],
    output: &Value,
) -> Result<(), OrbitError> {
    if !is_task_write_tool(tool_name) {
        return Ok(());
    }
    let Value::Object(map) = output else {
        return Ok(());
    };
    for sidecar in WRITE_SIDECAR_FIELDS {
        if fields.iter().any(|field| field == sidecar) && !map.contains_key(*sidecar) {
            return Err(OrbitError::InvalidInput(missing_write_sidecar_message(
                sidecar,
            )));
        }
    }
    Ok(())
}

pub(super) fn missing_write_sidecar_message(field: &str) -> String {
    format!(
        "requested field `{field}` is not in the tool output. Task writes omit `comments` and \
         `history` by default; pass --input '{{\"fields\":[\"{field}\"]}}' (or \"field\":\"{field}\") \
         to request them from the tool."
    )
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

fn wrap_single_field_value(output: Value, fields: &[String]) -> Value {
    match fields {
        [field] if !matches!(output, Value::Object(_)) => {
            let mut map = Map::new();
            map.insert(field.clone(), output);
            Value::Object(map)
        }
        _ => output,
    }
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
