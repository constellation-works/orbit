use std::collections::HashMap;
use std::sync::Arc;

use orbit_common::OrbitError;
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct TemplateContext {
    pub input: Value,
    pub env: HashMap<String, String>,
    pub workspace_path: Option<String>,
    pub item: Option<Value>,
    pub iteration: Option<u32>,
    /// Accumulated outputs from completed steps, keyed by step id (or
    /// target_id). Shared, so cloning a context never copies step outputs.
    pub steps: Arc<HashMap<String, Value>>,
}

pub fn render(template: &str, ctx: &TemplateContext) -> Result<String, OrbitError> {
    render_with_tokens(template, ctx, |output, value| {
        output.push_str(value);
    })
}

fn render_with_tokens<F>(
    template: &str,
    ctx: &TemplateContext,
    mut write_token: F,
) -> Result<String, OrbitError>
where
    F: FnMut(&mut String, &str),
{
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let end = after_start.find("}}").ok_or_else(|| {
            OrbitError::InvalidInput(format!("unterminated template token in '{template}'"))
        })?;
        let token = after_start[..end].trim();
        let value = resolve_token(token, ctx)?;
        write_token(&mut output, &value);
        remaining = &after_start[end + 2..];
    }

    output.push_str(remaining);
    Ok(output)
}

fn resolve_token(token: &str, ctx: &TemplateContext) -> Result<String, OrbitError> {
    if token == "workspace_path" {
        return ctx.workspace_path.clone().ok_or_else(|| {
            OrbitError::InvalidInput("workspace_path is unavailable in this context".to_string())
        });
    }
    if token == "item" {
        return resolve_input_path(ctx.item.as_ref(), &[]);
    }
    if token == "iteration" {
        return ctx.iteration.map(|value| value.to_string()).ok_or_else(|| {
            OrbitError::InvalidInput("iteration is unavailable in this context".to_string())
        });
    }

    let mut parts = token.split('.');
    let namespace = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OrbitError::InvalidInput("empty template token".to_string()))?;
    let path = parts.collect::<Vec<_>>();
    if path.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "template token '{token}' must include a key path"
        )));
    }

    match namespace {
        "input" => resolve_input_path(Some(&ctx.input), &path),
        "item" => resolve_input_path(ctx.item.as_ref(), &path),
        "env" => {
            if path.len() != 1 {
                return Err(OrbitError::InvalidInput(format!(
                    "env template token '{token}' must reference a single variable"
                )));
            }
            ctx.env.get(path[0]).cloned().ok_or_else(|| {
                OrbitError::InvalidInput(format!("missing environment variable '{}'", path[0]))
            })
        }
        "steps" => {
            // steps.<step_id>.<namespace>.<field>...
            // where <namespace> is "state", "output" or "files".
            if path.len() < 2 {
                return Err(OrbitError::InvalidInput(format!(
                    "steps template token '{token}' must be steps.<id>.state.<field> or steps.<id>.output.<field>"
                )));
            }
            let step_id = path[0];
            let step_value = ctx.steps.get(step_id).ok_or_else(|| {
                OrbitError::InvalidInput(format!("no data recorded for step '{step_id}'"))
            })?;
            let sub_namespace = path[1];
            match sub_namespace {
                "state" | "output" => {
                    let sub_value = step_value.get(sub_namespace).ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "step '{step_id}' has no '{sub_namespace}' data"
                        ))
                    })?;
                    if path.len() == 2 {
                        // steps.<id>.state or steps.<id>.output — return the whole sub-object
                        resolve_input_path(Some(sub_value), &[])
                    } else {
                        resolve_input_path(Some(sub_value), &path[2..])
                    }
                }
                "files" => resolve_step_files(step_id, step_value, &path[2..], token),
                other => Err(OrbitError::InvalidInput(format!(
                    "unknown steps sub-namespace '{other}' in '{token}'; expected 'state', 'output' or 'files'"
                ))),
            }
        }
        "secrets" => Err(OrbitError::InvalidInput(
            "secrets namespace is not yet supported".to_string(),
        )),
        other => Err(OrbitError::InvalidInput(format!(
            "unknown template namespace: {other}"
        ))),
    }
}

/// `steps.<id>.files[.<name or index>[.<field>]]`: the output files a
/// script step kept, as `[{name, path, bytes, media_type, sha256}]`. A file
/// is selected by its name (which may itself contain dots, such as
/// `shot.png`; the longest matching name wins) or its zero-based position;
/// a step that kept no files has none to resolve.
fn resolve_step_files(
    step_id: &str,
    step_value: &Value,
    path: &[&str],
    token: &str,
) -> Result<String, OrbitError> {
    let files = step_value.get("files").ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "step '{step_id}' kept no output files for '{token}'"
        ))
    })?;
    if path.is_empty() {
        return resolve_input_path(Some(files), &[]);
    }
    let entries = files.as_array().map(Vec::as_slice).unwrap_or_default();
    for taken in (1..=path.len()).rev() {
        let name = path[..taken].join(".");
        if let Some(file) = entries
            .iter()
            .find(|file| file.get("name").and_then(Value::as_str) == Some(name.as_str()))
        {
            return resolve_input_path(Some(file), &path[taken..]);
        }
    }
    let file = path[0]
        .parse::<usize>()
        .ok()
        .and_then(|index| entries.get(index))
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "step '{step_id}' kept no output file '{}' for '{token}'",
                path[0]
            ))
        })?;
    resolve_input_path(Some(file), &path[1..])
}

fn resolve_input_path(input: Option<&Value>, path: &[&str]) -> Result<String, OrbitError> {
    let mut current = input.ok_or_else(|| {
        OrbitError::InvalidInput("input template namespace requires an object input".to_string())
    })?;
    for segment in path {
        current = current.get(segment).ok_or_else(|| {
            OrbitError::InvalidInput(format!("missing input value for '{}'", path.join(".")))
        })?;
    }

    match current {
        Value::String(value) => Ok(value.clone()),
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(current)
            .map_err(|error| OrbitError::InvalidInput(error.to_string())),
    }
}
