//! Tool contract and the input helpers shared by builtin tools.

use serde_json::Value;

use orbit_common::OrbitError;
use orbit_types::tool::ToolSchema;

use super::context::ToolContext;

/// Set `key` to `value` in a child's cleared-and-set environment pair list,
/// replacing an existing entry rather than appending a second one. Shared by
/// every surface that stamps `ORBIT_*` variables into a spawned child:
/// external tools, plugin backends, and the plugin callback session.
pub(crate) fn upsert_env(env_pairs: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env_pairs.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env_pairs.push((key.to_string(), value));
    }
}

pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    /// The tool's own JSON Schema for its input, when it declares one richer
    /// than [`ToolSchema::parameters`]; MCP advertises it as written.
    fn input_schema(&self) -> Option<Value> {
        None
    }
    /// Whether a successful invocation can have externally visible side effects.
    ///
    /// The conservative default keeps audit persistence fail-closed for every
    /// tool that has not explicitly proved it is observational only.
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionKind {
    ReadOnly,
    Mutating,
}

/// Extract a non-empty string field from a tool input value.
///
/// Returns `Err(OrbitError::InvalidInput)` if the key is absent, not a string,
/// or contains only whitespace. The returned string is trimmed.
pub fn require_str(input: &Value, key: &str) -> Result<String, OrbitError> {
    let value = input
        .get(key)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{key}`")))?;
    // Accept both strings and numbers (agents often pass numeric IDs without quotes).
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(OrbitError::InvalidInput(format!("missing `{key}`"))),
    };
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(format!("missing `{key}`")));
    }
    Ok(trimmed)
}

/// Assert that a process result succeeded, returning a descriptive error if not.
///
/// Use this instead of the repeated `if !result.success { return Err(...) }` pattern.
/// The `label` should be the command name (e.g. `"gh pr comment"`) and is included
/// in the error message for diagnostics.
pub fn check_exec_result(
    result: &orbit_types::tool::ExecutionResult,
    label: &str,
) -> Result<(), OrbitError> {
    if result.success {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "{label} failed: {}",
            result.stderr.trim()
        )))
    }
}
