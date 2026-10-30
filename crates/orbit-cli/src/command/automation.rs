//! The automation CLI, derived from the automation operation registry.
//!
//! `explain` calls the read-only `orbit.automation.explain` tool, so `--json`
//! is that tool's payload. What is left here is the text rendering: the
//! definition and its condition, the matches and closest misses, and one
//! subject's clause tree with the values each clause read.

use clap::Args;
use orbit_common::governance::automation::AutomationVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::automation::AUTOMATION.cli_about)]
pub struct AutomationCommand {
    #[command(subcommand)]
    pub command: Invocation<AutomationVerb>,
}

impl Execute for AutomationCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            AutomationVerb::Explain => explain_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

fn explain_blocks(value: &Value) -> Vec<Block> {
    let definition = &value["definition"];
    let ownership = &value["ownership"];
    let subjects = &value["subjects"];
    let mut header = format!(
        "{} {} ({}), {}\nScope: {} · {} · owner: {}{}",
        text(&definition["kind"]),
        text(&definition["name"]),
        text(&definition["path"]),
        if definition["enabled"] == true {
            "enabled"
        } else {
            "disabled"
        },
        text(&value["scope"]),
        text(&value["clause"]),
        text(&ownership["verdict"]).replace('_', " "),
        if ownership["replica"] == true {
            " (replica checkout: never evaluates it)"
        } else {
            ""
        },
    );
    if let Some(prefilter) = value.get("prefilter") {
        header.push_str(&format!(
            "\nPrefilter: statuses {} · tags {}",
            list(&prefilter["statuses"], "any"),
            list(&prefilter["tags"], "none"),
        ));
    }
    let cross_reads = &value["cross_reads"];
    if !is_empty(&cross_reads["tasks"]) || !is_empty(&cross_reads["tags"]) {
        header.push_str(&format!(
            "\nCross-reads: tasks {} · tags {}",
            list(&cross_reads["tasks"], "none"),
            list(&cross_reads["tags"], "none"),
        ));
    }
    header.push_str(&format!("\nWhen: {}", text(&value["when"])));
    if let Some(rearm) = value.get("rearm").and_then(Value::as_str) {
        header.push_str(&format!("\nRearm: {rearm}"));
    }
    header.push_str(&format!(
        "\nEvaluated {} at {}: {} matched, {} missed, {} unknown{}",
        count(&subjects["evaluated"]),
        text(&value["evaluated_at"]),
        count(&subjects["matched"]),
        count(&subjects["missed"]),
        count(&subjects["unknown"]),
        if subjects["truncated"] == true {
            format!(
                " (first {} of {} selected)",
                count(&subjects["evaluated"]),
                count(&subjects["selected"])
            )
        } else {
            String::new()
        },
    ));
    let mut blocks = vec![Block::text(header)];

    for unavailable in rows(&value["unavailable"]) {
        blocks.push(Block::text(format!("Unavailable: {}", text(unavailable))));
    }

    blocks.push(Block::text("\nMatches"));
    let mut matches = Table::new(vec![Column::new("SUBJECT").fixed(), Column::new("TITLE")])
        .empty_message("no subject matches");
    for row in rows(&value["matches"]) {
        matches.add_row(vec![text(&row["subject"]), text(&row["title"])]);
    }
    blocks.push(Block::table(matches));

    blocks.push(Block::text(
        "\nClosest misses (one top-level clause not true)",
    ));
    let mut misses = Table::new(vec![
        Column::new("SUBJECT").fixed(),
        Column::new("VALUE").fixed(),
        Column::new("CLAUSE"),
    ])
    .empty_message("no subject misses by exactly one clause");
    for row in rows(&value["closest_misses"]) {
        misses.add_row(vec![
            text(&row["subject"]),
            text(&row["value"]),
            text(&row["clause"]),
        ]);
    }
    blocks.push(Block::table(misses));

    match value.get("subject") {
        Some(subject) => {
            let mut lines = vec![format!(
                "\nSubject {}{}{}",
                text(&subject["key"]),
                subject["title"]
                    .as_str()
                    .map(|title| format!(" — {title}"))
                    .unwrap_or_default(),
                if subject["in_prefilter"] == false {
                    " (outside the prefilter: never a subject)"
                } else {
                    ""
                },
            )];
            clause_lines(&subject["when"], "", &mut lines);
            lines.push(format!("→ {}{}", text(&subject["value"]), reason(subject)));
            if let Some(rearm) = subject.get("rearm") {
                lines.push("Rearm:".into());
                clause_lines(rearm, "", &mut lines);
            }
            blocks.push(Block::text(lines.join("\n")));
        }
        None => blocks.push(Block::text(
            "\nNo subject to explain: the prefilter selects no task.",
        )),
    }
    blocks.push(Block::text(format!(
        "\nArm state, holds, fires and self: not yet tracked ({})",
        text(&value["tracking"]["note"])
    )));
    blocks
}

/// One line per clause: a chain's operands joined by their operator, each
/// leaf with its value and the operands it read.
fn clause_lines(node: &Value, joiner: &str, lines: &mut Vec<String>) {
    let operator = node["operator"].as_str();
    let children = rows(&node["children"]);
    if let Some(operator) = operator
        && !children.is_empty()
    {
        let symbol = if operator == "and" { "&& " } else { "|| " };
        for (index, child) in children.iter().enumerate() {
            let prefix = if index == 0 { joiner } else { symbol };
            if child["operator"].is_string() {
                lines.push(format!(
                    "{prefix}({})  {}{}",
                    text(&child["text"]),
                    text(&child["value"]),
                    reason(child)
                ));
                let mut nested = Vec::new();
                clause_lines(child, "", &mut nested);
                lines.extend(nested.into_iter().map(|line| format!("    {line}")));
            } else {
                lines.push(leaf_line(child, prefix));
            }
        }
        return;
    }
    lines.push(leaf_line(node, joiner));
}

fn leaf_line(node: &Value, prefix: &str) -> String {
    let operands = rows(&node["operands"])
        .iter()
        .map(|operand| {
            let shown = match operand["unavailable"].as_str() {
                Some(why) => why.to_string(),
                None => compact(&operand["value"]),
            };
            format!("{} = {shown}", text(&operand["path"]))
        })
        .collect::<Vec<_>>();
    format!(
        "{prefix}{}  {}{}{}",
        text(&node["text"]),
        text(&node["value"]),
        if operands.is_empty() {
            String::new()
        } else {
            format!("  ({})", operands.join(", "))
        },
        reason(node),
    )
}

fn reason(node: &Value) -> String {
    node["reason"]
        .as_str()
        .map(|reason| format!(" [{reason}]"))
        .unwrap_or_default()
}

fn rows(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn is_empty(value: &Value) -> bool {
    rows(value).is_empty()
}

fn list(value: &Value, none: &str) -> String {
    let items = rows(value)
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if items.is_empty() {
        none.to_string()
    } else {
        items.join(", ")
    }
}

fn count(value: &Value) -> String {
    value.as_u64().unwrap_or_default().to_string()
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A value on one line: strings bare, everything else as JSON.
fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => format!("'{text}'"),
        other => other.to_string(),
    }
}
