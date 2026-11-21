use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::RequiredGate;
use serde_json::{Value, json};

use super::super::super::completion_deadline::Deadline;
use super::super::MAX_WORKFLOW_SOURCE_BYTES;
use super::super::input::{number, same, text};
use super::support::{api, unreadable};

pub(in super::super) fn workflow(
    deadline: Deadline,
    workspace: &Path,
    run: &Value,
) -> Result<(String, String, String), OrbitError> {
    let id = number(run, "workflow_id", unreadable)?;
    let workflow = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/actions/workflows/{id}"),
    )?;
    same(&workflow["id"], &json!(id), "workflow identity", unreadable)?;
    let path = text(&workflow, "path", unreadable)?.to_string();
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[0] != ".github"
        || parts[1] != "workflows"
        || parts[2].is_empty()
        || parts[2].contains("..")
        || !(path.ends_with(".yml") || path.ends_with(".yaml"))
    {
        return Err(unreadable("workflow path"));
    }
    same(
        &run["path"],
        &json!(path),
        "run workflow source",
        unreadable,
    )?;
    let head = text(run, "head_sha", unreadable)?;
    let contents = super::super::read_cache::contents_at_commit(deadline, workspace, &path, head)?;
    same(
        &contents["type"],
        &json!("file"),
        "workflow file",
        unreadable,
    )?;
    same(
        &contents["path"],
        &json!(path),
        "workflow file path",
        unreadable,
    )?;
    let blob = text(&contents, "sha", unreadable)?.to_string();
    // pull_request executes the merge ref. Requiring the event base and PR
    // head to carry the identical workflow blob proves the merge cannot
    // substitute a different DAG. Changed workflow definitions hold.
    let source_base = text(&run["pull_requests"][0]["base"], "sha", unreadable)?;
    if !super::super::input::valid_expected_remote_sha(Some(source_base)) {
        return Err(unreadable("workflow event base commit"));
    }
    let base_contents =
        super::super::read_cache::contents_at_commit(deadline, workspace, &path, source_base)?;
    same(
        &base_contents["sha"],
        &json!(blob),
        "workflow event-base source",
        unreadable,
    )?;
    same(
        &base_contents["type"],
        &json!("file"),
        "workflow event-base file",
        unreadable,
    )?;
    same(
        &base_contents["path"],
        &json!(path),
        "workflow event-base path",
        unreadable,
    )?;
    let revision = format!("{head}:{path}");
    let local_blob = deadline.execute(
        "git",
        vec!["rev-parse".into(), revision.clone()],
        workspace,
        "workflow blob",
    )?;
    if local_blob.stdout.trim() != blob {
        return Err(unreadable("source blob differs from the run head"));
    }
    let source = deadline
        .execute(
            "git",
            vec!["show".into(), revision],
            workspace,
            "workflow source",
        )?
        .stdout;
    if source.len() > MAX_WORKFLOW_SOURCE_BYTES
        || contents["size"].as_u64() != Some(source.len() as u64)
    {
        return Err(unreadable("truncated workflow source"));
    }
    Ok((path, blob, source))
}
/// Presence, including unsuccessful evidence, prevents absent-workflow diagnosis.
pub(in super::super) fn gate_present(
    gate: &RequiredGate,
    checks: &[Value],
    statuses: &[Value],
) -> bool {
    checks.iter().any(|check| {
        check["name"].as_str() == Some(&gate.context)
            && gate
                .app_id
                .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
    }) || (gate.app_id.is_none()
        && statuses
            .iter()
            .any(|status| status["context"] == gate.context))
}

pub(in super::super) fn gate_passed(
    gate: &RequiredGate,
    checks: &[Value],
    statuses: &[Value],
) -> bool {
    // When a context has both representations, neither may conceal a failure
    // or pending result in the other. Only explicit success pays an owed gate.
    gate_present(gate, checks, statuses)
        && checks
            .iter()
            .filter(|check| {
                check["name"].as_str() == Some(&gate.context)
                    && gate
                        .app_id
                        .is_none_or(|app| check["app"]["id"].as_u64() == Some(app))
            })
            .all(|check| check["status"] == "completed" && check["conclusion"] == "success")
        && (gate.app_id.is_some()
            || statuses
                .iter()
                .filter(|status| status["context"] == gate.context)
                .all(|status| status["state"] == "success"))
}
