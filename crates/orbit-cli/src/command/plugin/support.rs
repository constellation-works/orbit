use clap::ValueEnum;
use orbit_core::adapter::command::{PluginSeedOutcome, PluginSkillLink, PluginSummary};
use orbit_types::plugin::{PluginDisabledLayer, PluginStatus};
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

/// JSON projection shared by `list` and `show`.
pub(super) fn plugin_record(summary: &PluginSummary) -> Value {
    json!({
        "name": summary.name,
        "version": summary.version,
        "status": summary.status.as_str(),
        "source": summary.source,
        "install_path": summary.install_path,
        "manifest_digest": summary.manifest_digest,
        "publisher": summary.publisher,
        "description": summary.description,
        "first_party": summary.first_party,
        "pinned": summary.pinned,
        "host_enabled": summary.host_enabled,
        "workspace_toggle": summary.workspace_toggle,
        "disabled_by": summary.disabled_by.map(PluginDisabledLayer::as_str),
        "permissions": summary
            .permissions
            .iter()
            .map(|permission| json!({
                "grant": permission.grant.as_str(),
                "requested": permission.requested,
                "granted": permission.granted,
                "granted_roots": permission.granted_roots,
            }))
            .collect::<Vec<_>>(),
        "granted": summary.granted,
        "unsandboxed": summary.unsandboxed,
        "programs": summary
            .programs
            .iter()
            .map(|program| json!({
                "name": program.name,
                "path": program.path.as_ref().map(|path| path.to_string_lossy()),
                "granted": program.granted(),
                "problem": program.problem,
            }))
            .collect::<Vec<_>>(),
        "certified_orbit_version": summary.certified_orbit_version,
        "diagnostic": summary.diagnostic,
        "panels": summary
            .panels
            .iter()
            .map(|panel| json!({
                "id": panel.id,
                "title": panel.title,
                "tool": panel.tool,
                "render": panel.render.as_str(),
                "group": panel.group.as_str(),
            }))
            .collect::<Vec<_>>(),
        "links": summary
            .links
            .iter()
            .map(|link| json!({ "title": link.title, "url": link.url }))
            .collect::<Vec<_>>(),
        "tools": summary
            .tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "advertised_name": tool.advertised_name,
                "execution_kind": tool.execution_kind.as_str(),
                "mcp_scope": tool.mcp_scope,
                "active": tool.active,
            }))
            .collect::<Vec<_>>(),
    })
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
