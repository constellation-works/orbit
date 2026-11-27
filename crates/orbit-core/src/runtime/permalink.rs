//! Canonical web permalink construction for single-task and single-run results.
//!
//! Uses `machine.web.public_url` from the producing machine's configuration;
//! when unset, results omit `url` instead of guessing an origin.

use std::path::Path;

use url::Url;

pub(super) fn build_task_permalink(global_root: &Path, task_id: &str) -> Option<String> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return None;
    }
    let mut url = configured_public_origin(global_root)?;
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.push("t");
        segments.push(task_id);
    }
    Some(url.into())
}

pub(super) fn build_run_permalink(
    global_root: &Path,
    run_id: &str,
    machine_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Option<String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return None;
    }
    let mut url = configured_public_origin(global_root)?;
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut segments = url.path_segments_mut().ok()?;
        segments.clear();
        segments.push("r");
        segments.push(run_id);
    }
    if machine_id.is_some() || workspace_id.is_some() {
        let mut query = url.query_pairs_mut();
        if let Some(machine_id) = machine_id {
            query.append_pair("machine", machine_id);
        }
        if let Some(workspace_id) = workspace_id {
            query.append_pair("workspace", workspace_id);
        }
    }
    Some(url.into())
}

fn configured_public_origin(global_root: &Path) -> Option<Url> {
    let settings = match orbit_config::load_machine_web_settings(global_root) {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(
                %error,
                "ignoring unreadable machine.web settings while building permalink"
            );
            return None;
        }
    };
    let origin = settings.public_url?;
    match Url::parse(&origin) {
        Ok(url) => Some(url),
        Err(error) => {
            tracing::warn!(%error, origin, "ignoring invalid machine.web.public_url");
            None
        }
    }
}
