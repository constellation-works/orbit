use std::fmt::Write as _;

use orbit_core::app::task::Outcome;
use serde_json::Value;

use super::support::text;
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn render(outcome: Outcome) -> CommandOut {
    let value = outcome.into_answer()?;
    let rendered = show_blocks(&value);
    Ok(Payload::blocks(value, rendered).into())
}

fn show_blocks(value: &Value) -> Vec<Block> {
    let mut out = format!(
        "{} ({}, {})\ntask {} · raised by {} at {} · expires {}\nrisk: {} · category: {}",
        text(&value["id"]),
        text(&value["gate"]),
        text(&value["state"]),
        text(&value["task_id"]),
        text(&value["raised_by"]),
        text(&value["raised_at"]),
        text(&value["expires_at"]),
        text(&value["risk_tier"]),
        text(&value["category"]),
    );
    for (index, answer) in value["answers"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let _ = write!(
            out,
            "\n{}. {} (`{}`; {}) — {}",
            index + 1,
            text(&answer["label"]),
            text(&answer["id"]),
            text(&answer["risk_tier"]),
            text(&answer["consequence"]),
        );
    }
    if let Some(topic) = value.get("topic").filter(|value| !value.is_null()) {
        let _ = write!(out, "\ntopic: {}", text(topic));
    }
    if let Some(binding) = value.get("binding") {
        let _ = write!(out, "\nbinding: {binding}");
    }
    if let Some(grant) = value.get("script_grant").filter(|value| value.is_object()) {
        script_grant_section(&mut out, grant);
    }
    if let Some(apply) = value.get("plugin_apply").filter(|value| value.is_object()) {
        let _ = write!(out, "\nplugin apply: {apply}");
    }
    if let Some(resolution) = value.get("resolution").filter(|value| !value.is_null()) {
        let _ = write!(
            out,
            "\nanswered: `{}` by {} at {}",
            text(&resolution["answer_id"]),
            text(&resolution["answered_by"]),
            text(&resolution["answered_at"]),
        );
        if let Some(note) = resolution["note"].as_str() {
            let _ = write!(out, "\nnote: {note}");
        }
        if let Some(model) = resolution.get("model").filter(|value| !value.is_null()) {
            let _ = write!(
                out,
                "\nmodel: probability {} (threshold {}), {} cited precedent(s), category {}",
                model["probability"],
                model["threshold"],
                model["cited_precedents"].as_array().map_or(0, Vec::len),
                text(&model["category"]),
            );
            match resolution
                .get("confirmation")
                .filter(|value| !value.is_null())
            {
                Some(confirmation) => {
                    let _ = write!(
                        out,
                        "\nconfirmed by {}",
                        text(&confirmation["confirmed_by"])
                    );
                }
                None => out.push_str("\nunconfirmed model answer"),
            }
        }
    }
    vec![Block::text(out)]
}

/// What a `script_grant` answer consents to: the script, both digests, the
/// refusal, requested against granted capabilities and the diff to review.
fn script_grant_section(out: &mut String, grant: &Value) {
    let _ = write!(
        out,
        "\nscript: {} (entry {})\ndigest: {} (granted: {})\nrefusal: {} — {}\nrequested: {}\ngranted: {}",
        text(&grant["name"]),
        text(&grant["entry"]),
        text(&grant["digest"]),
        grant["granted_digest"].as_str().unwrap_or("none"),
        text(&grant["refusal"]),
        text(&grant["refusal_message"]),
        grant["requested"],
        if grant["granted"].is_null() {
            "none".to_string()
        } else {
            grant["granted"].to_string()
        },
    );
    let diff = text(&grant["diff"]);
    if !diff.is_empty() {
        let _ = write!(out, "\ndiff:\n{}", diff.trim_end());
    }
    if grant["diff_truncated"].as_bool() == Some(true) {
        out.push_str("\n(diff truncated)");
    }
}
