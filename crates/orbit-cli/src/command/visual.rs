//! The visual capture CLI, derived from the visual operation registry.
//!
//! `capture` calls the `orbit.visual.capture` tool, so `--json` is that
//! tool's payload. What is left here is the text rendering: one line per
//! mock with the files written for it, then the rules summary, and one line
//! for an accepted dispute.

use clap::Args;
use orbit_common::governance::visual::VisualVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;

#[derive(Args)]
#[command(about = orbit_common::governance::visual::VISUAL.cli_about)]
pub struct VisualCommand {
    #[command(subcommand)]
    pub command: Invocation<VisualVerb>,
}

impl Execute for VisualCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            VisualVerb::Capture => capture_blocks(&value),
            VisualVerb::AcceptDispute => vec![Block::text(format!(
                "Accepted the dispute of visual finding {} on {}",
                text(&value["finding_id"]),
                text(&value["task_id"]),
            ))],
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

fn capture_blocks(value: &Value) -> Vec<Block> {
    let out_dir = text(&value["out_dir"]);
    let mut lines = vec![format!(
        "Captured {} mock(s) of {} under {}",
        value["mocks"].as_array().map_or(0, Vec::len),
        text(&value["task_id"]),
        out_dir,
    )];
    for mock in value["mocks"].as_array().into_iter().flatten() {
        if let Some(error) = mock.get("error") {
            lines.push(format!(
                "  {}: {}: {}",
                text(&mock["artifact"]),
                text(&error["code"]),
                text(&error["message"]),
            ));
            continue;
        }
        lines.push(format!(
            "  {} ({} {} {} {}px, {}):",
            text(&mock["artifact"]),
            text(&mock["view"]),
            text(&mock["state"]),
            text(&mock["theme"]),
            text(&mock["width"]),
            text(&mock["stability"]),
        ));
        for key in ["mock", "capture", "side_by_side"] {
            let relative = text(&mock[key]);
            if !relative.is_empty() {
                lines.push(format!("    {key}: {out_dir}/{relative}"));
            }
        }
    }
    let summary = &value["rules_summary"];
    lines.push(format!(
        "  rules: {out_dir}/{} (pass {} fail {} error {} skipped {})",
        text(&value["rules"]),
        text(&summary["pass"]),
        text(&summary["fail"]),
        text(&summary["error"]),
        text(&summary["skipped"]),
    ));
    vec![Block::text(lines.join("\n"))]
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
