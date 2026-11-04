use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};

use super::support::tool_status;

#[derive(Args)]
pub struct ToolShowArgs {
    /// Tool name
    pub name: String,
    /// Print one parameter with its complete description
    #[arg(long, value_name = "NAME")]
    pub parameter: Option<String>,
}

impl Execute for ToolShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let tool = runtime.show_tool(&self.name)?;

        use crate::output::color::{self, Domain};
        let constraints = tool.input_schema.as_ref().map(tool_input_constraints);
        let constraint_text = constraints
            .as_ref()
            .filter(|value| value.as_object().is_some_and(|rules| !rules.is_empty()))
            .map(serde_json::to_string_pretty)
            .transpose()
            .map_err(|error| orbit_core::OrbitError::Execution(error.to_string()))?
            .map(|value| format!("Input constraints (JSON Schema):\n{value}"));
        if let Some(name) = self.parameter {
            let parameter = tool
                .parameters
                .iter()
                .find(|parameter| parameter.name == name)
                .ok_or_else(|| {
                    orbit_core::OrbitError::InvalidInput(format!(
                        "tool '{}' has no parameter '{name}'",
                        tool.name
                    ))
                })?;
            let requirement = tool_parameter_requirement(
                parameter.required,
                &parameter.name,
                constraints.as_ref(),
            );
            let mut doc = json!({
                "tool_name": tool.name,
                "name": parameter.name,
                "param_type": parameter.param_type,
                "required": parameter.required,
                "requirement": requirement,
                "description": parameter.description,
            });
            if let Some(schema) = &tool.input_schema {
                doc["input_schema"] = schema.clone();
            }
            let mut detail = format!(
                "Tool: {}\nParameter: {}\nType: {}\nRequired: {}\nDescription: {}",
                tool.name, parameter.name, parameter.param_type, requirement, parameter.description
            );
            if let Some(constraints) = constraint_text {
                detail.push_str(&format!("\n{constraints}"));
            }
            return Ok(Payload::detail(doc, detail).into());
        }
        let parameters: Vec<_> = tool
            .parameters
            .iter()
            .map(|parameter| {
                let mut value = json!(parameter);
                value["requirement"] = json!(tool_parameter_requirement(
                    parameter.required,
                    &parameter.name,
                    constraints.as_ref()
                ));
                value
            })
            .collect();
        let mut doc = json!({
            "name": tool.name,
            "description": tool.description,
            "enabled": tool.enabled,
            "active": tool.active,
            "status": tool_status(&tool),
            "builtin": tool.builtin,
            "parameters": parameters,
            "mcp_toolset": tool.mcp_toolset,
        });

        if let Some(schema) = &tool.input_schema {
            doc["input_schema"] = schema.clone();
        }
        let mut header = format!(
            "{} {}\n{} {}\n{} {}\n{} {}",
            color::bold("Name:"),
            tool.name,
            color::bold("Description:"),
            tool.description,
            color::bold("Builtin:"),
            if tool.builtin { "yes" } else { "no" },
            color::bold("Status:"),
            color::text(tool_status(&tool), Domain::JobState)
        );
        if let Some(toolset) = tool.mcp_toolset {
            header.push_str(&format!("\n{} {toolset}", color::bold("MCP toolset:")));
        }

        if tool.parameters.is_empty() {
            header.push_str(&format!("\n{} (none)", color::bold("Parameters:")));
            if let Some(constraints) = constraint_text {
                header.push_str(&format!("\n{constraints}"));
            }
            return Ok(Payload::detail(doc, header).into());
        }

        header.push_str(&format!("\n{}", color::bold("Parameters:")));
        // A parameter schema, not a result set: keep every column even when
        // every parameter happens to share a type or be required.
        let mut table =
            crate::output::table::build_table(&["NAME", "TYPE", "REQUIRED", "DESCRIPTION"])
                .keep_all_columns();
        for p in &tool.parameters {
            let req = tool_parameter_requirement(p.required, &p.name, constraints.as_ref());
            table.add_row(vec![
                p.name.clone(),
                p.param_type.clone(),
                req.to_string(),
                p.description.clone(),
            ]);
        }
        let mut blocks = vec![Block::text(header), Block::table(table)];
        if let Some(constraints) = constraint_text {
            blocks.push(Block::text(constraints));
        }
        Ok(Payload::blocks(doc, blocks).into())
    }
}

/// Keep object-level schema rules intact, including branch-specific allowed fields.
/// Nested property schemas constrain their values, not the presence of tool arguments.
fn tool_input_constraints(schema: &Value) -> Value {
    Value::Object(
        [
            "allOf",
            "anyOf",
            "oneOf",
            "if",
            "then",
            "else",
            "dependentRequired",
            "dependentSchemas",
        ]
        .into_iter()
        .filter_map(|key| {
            schema
                .get(key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect(),
    )
}

fn tool_parameter_requirement(
    required: bool,
    name: &str,
    constraints: Option<&Value>,
) -> &'static str {
    if required {
        "required"
    } else if constraints
        .is_some_and(|constraints| schema_requires_tool_parameter(constraints, name))
    {
        "conditional"
    } else {
        "optional"
    }
}

/// Follow only object-level compositions: an `if` is a predicate, and a
/// property's `required` array concerns a nested value rather than an argument.
fn schema_requires_tool_parameter(schema: &Value, name: &str) -> bool {
    if let Some(branches) = schema.as_array() {
        return branches
            .iter()
            .any(|branch| schema_requires_tool_parameter(branch, name));
    }
    schema["required"]
        .as_array()
        .is_some_and(|fields| fields.iter().any(|field| field == name))
        || [
            "allOf",
            "anyOf",
            "oneOf",
            "then",
            "else",
            "dependentRequired",
            "dependentSchemas",
        ]
        .into_iter()
        .any(|key| {
            schema.get(key).is_some_and(|branch| {
                if matches!(key, "dependentRequired" | "dependentSchemas") {
                    branch.as_object().is_some_and(|dependencies| {
                        dependencies.values().any(|value| {
                            if key == "dependentRequired" {
                                value
                                    .as_array()
                                    .is_some_and(|fields| fields.iter().any(|field| field == name))
                            } else {
                                schema_requires_tool_parameter(value, name)
                            }
                        })
                    })
                } else {
                    schema_requires_tool_parameter(branch, name)
                }
            })
        })
}
