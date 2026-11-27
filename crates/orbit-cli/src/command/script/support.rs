use orbit_core::app::run::ScriptStatus;
use orbit_types::workflow::activity_job::ScriptGrant;
use serde_json::{Value, json};

pub(super) fn short_digest(digest: &str) -> String {
    digest.chars().take(12).collect()
}

pub(super) fn script_status_json(status: &ScriptStatus) -> Value {
    json!({
        "name": status.name,
        "state": status.state.as_str(),
        "detail": status.detail,
        "manifest": status.asset.as_ref().map(|asset| asset.manifest_path.display().to_string()),
        "entry": status.asset.as_ref().map(|asset| asset.spec.entry.clone()),
        "entry_path": status.asset.as_ref().map(|asset| asset.entry_path.display().to_string()),
        "interpreter": status.asset.as_ref().map(|asset| asset.interpreter.clone()).unwrap_or_default(),
        "args": status.asset.as_ref().map(|asset| asset.spec.args.clone()).unwrap_or_default(),
        "current_digest": status.asset.as_ref().map(|asset| asset.digest.clone()),
        "current_manifest_digest": status.asset.as_ref().map(|asset| asset.manifest_digest.clone()),
        "current_entry_digest": status.asset.as_ref().map(|asset| asset.entry_digest.clone()),
        "grant": status.grant.as_ref().map(script_grant_json),
    })
}

pub(super) fn script_grant_json(grant: &ScriptGrant) -> Value {
    serde_json::to_value(grant).unwrap_or(Value::Null)
}
