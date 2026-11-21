use clap::ValueEnum;
use orbit_core::adapter::command::{self, PluginSeedOutcome, PluginSkillLink, PluginSummary};
use orbit_types::plugin::{PluginDisabledLayer, PluginGrant, PluginStatus};
use serde_json::{Value, json};

/// Which enable state `orbit plugin enable|disable` writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum PluginScope {
    /// The host row every workspace on this machine inherits.
    #[default]
    Host,
    /// Only the selected workspace's toggle; the host row is untouched.
    Workspace,
}

/// Prints the build plan to stderr before a consented `spec.build` runs, so
/// the operator sees what runs even when stdout is machine-readable.
pub(super) fn show_build_plan(plan: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{plan}");
    let _ = stderr.flush();
}

/// Why a plugin is not active in the selected workspace, in the words
/// `list` and `show` print. `None` for an active plugin.
pub(super) fn state_reason(summary: &PluginSummary) -> Option<String> {
    match summary.status {
        PluginStatus::Active => None,
        PluginStatus::Disabled => Some(match summary.disabled_by {
            Some(PluginDisabledLayer::Workspace) => "disabled in this workspace".to_string(),
            _ => "disabled on host".to_string(),
        }),
        PluginStatus::Missing | PluginStatus::Inactive => Some(
            summary
                .diagnostic
                .clone()
                .unwrap_or_else(|| match summary.status {
                    PluginStatus::Missing => "not installed on this host".to_string(),
                    _ => "refused at load".to_string(),
                }),
        ),
    }
}

/// The host row's state as `list` and `show` print it.
pub(super) fn host_state(summary: &PluginSummary) -> &'static str {
    match (summary.status, summary.host_enabled) {
        (PluginStatus::Missing, _) => "-",
        (_, true) => "enabled",
        (_, false) => "disabled",
    }
}

/// JSON projection shared by every CLI plugin record and mutation receipt.
pub(super) fn plugin_record(summary: &PluginSummary) -> Value {
    command::plugin_summary_json(summary, command::PluginSummaryJsonProfile::Cli)
}

/// The individual manifest declarations shown beside agent consent.
pub(super) fn declared_agent_tool_names(summary: &PluginSummary) -> String {
    summary
        .tools
        .iter()
        .filter(|tool| tool.agent_callable)
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Show the agent mutation request and the host's recorded answer at enable.
pub(super) fn append_agent_tool_consent_text(text: &mut String, summary: &PluginSummary) {
    let tools = declared_agent_tool_names(summary);
    if tools.is_empty() {
        return;
    }
    let granted = summary
        .permissions
        .iter()
        .any(|permission| permission.grant == PluginGrant::AgentTools && permission.granted);
    text.push_str(&format!(
        "\nAgent-callable mutating tools (agent_tools {}): {tools}",
        if granted { "granted" } else { "not granted" },
    ));
}

/// The seeded-schedule / skill-link / warning report `orbit plugin enable`
/// and `orbit plugin add --enable` both produce, rendered identically so the
/// two ways of enabling a plugin never disagree about what happened.
pub(super) fn append_enable_report_text(
    text: &mut String,
    seeded: &[PluginSeedOutcome],
    skills: &[PluginSkillLink],
    warnings: &[String],
) {
    for outcome in seeded {
        text.push_str(&format!(
            "\n  {} {} {} ({})",
            outcome.kind,
            outcome.name,
            outcome.action.as_str(),
            outcome.path.display()
        ));
    }
    if !seeded.is_empty() {
        text.push_str(
            "\n  Seeded schedules are disabled; review one, then set `enabled: true` to run it.",
        );
    }
    for link in skills {
        text.push_str(&format!(
            "\n  skill {} linked at {}",
            link.skill_id,
            link.link.display()
        ));
    }
    for warning in warnings {
        text.push_str(&format!("\n  warning: {warning}"));
    }
}

/// JSON projection matching [`append_enable_report_text`].
pub(super) fn enable_report_json(
    doc: &mut Value,
    seeded: &[PluginSeedOutcome],
    skills: &[PluginSkillLink],
    warnings: &[String],
) {
    doc["seeded"] = serde_json::Value::Array(
        seeded
            .iter()
            .map(|outcome| {
                json!({
                    "kind": outcome.kind,
                    "name": outcome.name,
                    "path": outcome.path.display().to_string(),
                    "action": outcome.action.as_str(),
                    "warning": outcome.warning,
                })
            })
            .collect(),
    );
    doc["skills"] = serde_json::Value::Array(
        skills
            .iter()
            .map(|link| {
                json!({
                    "skill_id": link.skill_id,
                    "link": link.link.display().to_string(),
                    "target": link.target.display().to_string(),
                })
            })
            .collect(),
    );
    doc["warnings"] = json!(warnings);
}
