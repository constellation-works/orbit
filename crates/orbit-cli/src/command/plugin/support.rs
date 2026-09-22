use orbit_core::adapter::command::{PluginSeedOutcome, PluginSkillLink, PluginSummary};
use serde_json::{Value, json};

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
