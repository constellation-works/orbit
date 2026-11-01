//! Bounded dependency-only evidence from a trusted source checkout.
use super::super::super::ci::log_signature::{LineKind, classify_log_lines, log_payload};
use super::super::completion_deadline::Deadline;
use super::{
    input::{number, same, text},
    prerequisite::{Snapshot, api, unreadable},
};
use orbit_common::OrbitError;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

#[allow(
    clippy::too_many_arguments,
    reason = "authentication binds the deadline, snapshot, failed jobs and workflow source to one log"
)]
pub(super) fn authenticate_dependency_record(
    deadline: Deadline,
    workspace: &Path,
    snapshot: &Snapshot,
    run: &Value,
    job: &Value,
    failed: &[Value],
    accepted: &str,
    workflow_path: &str,
    source: &str,
    log: &str,
) -> Result<Value, OrbitError> {
    let records = classify_log_lines(log)
        .into_iter()
        .filter(|(kind, _)| {
            !matches!(
                kind,
                LineKind::RunCommand
                    | LineKind::CommandBody
                    | LineKind::ParamDump
                    | LineKind::EndGroup
            )
        })
        .filter_map(|(_, line)| {
            orbit_tools::github_cli::strip_ansi_sequences(log_payload(line))
                .strip_prefix("orbit-dependency-evidence:")
                .map(str::to_owned)
        })
        .map(|record| {
            if record.len() > 8192 {
                Err(unreadable("oversized dependency record"))
            } else {
                serde_json::from_str::<Value>(&record).map_err(|_| unreadable("dependency JSON"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [record] = records.as_slice() else {
        return Err(unreadable("missing or ambiguous dependency record"));
    };
    if record["schema_version"] != 1 || record["kind"] != "dependency_failed" {
        return Err(unreadable("legacy dependency record"));
    }
    for (left, right, label) in [
        (
            &record["repository"],
            &json!(snapshot.repository),
            "dependency repository",
        ),
        (&record["run_id"], &run["id"], "dependency run"),
        (
            &record["run_attempt"],
            &run["run_attempt"],
            "dependency attempt",
        ),
        (&record["head_sha"], &run["head_sha"], "dependency head"),
        (
            &record["source"]["job_id"],
            &job["id"],
            "dependency source job",
        ),
        (
            &record["source"]["job_name"],
            &job["name"],
            "dependency source name",
        ),
    ] {
        same(left, right, label, unreadable)?;
    }
    let steps = job["steps"]
        .as_array()
        .ok_or_else(|| unreadable("dependency steps"))?;
    let bad = steps
        .iter()
        .filter(|step| step["conclusion"] == "failure")
        .collect::<Vec<_>>();
    let [step] = bad.as_slice() else {
        return Err(unreadable("independent waiter failure"));
    };
    same(
        &step["name"],
        &record["source"]["step_name"],
        "dependency step name",
        unreadable,
    )?;
    same(
        &step["number"],
        &record["source"]["step_number"],
        "dependency step number",
        unreadable,
    )?;
    // Other failed steps or unknown results were already vetoed by the adapter.
    let workflow: Value =
        serde_yaml::from_str(source).map_err(|_| unreadable("dependency workflow"))?;
    let key = text(&record["source"], "job_key", unreadable)?;
    let source_job = &workflow["jobs"][key];
    if super::workflow_dependency::job_names(key, source_job)?
        != vec![text(job, "name", unreadable)?.to_string()]
    {
        return Err(unreadable("dependency source workflow job"));
    }
    let source_steps = source_job["steps"]
        .as_array()
        .ok_or_else(|| unreadable("dependency source steps"))?;
    let matching = source_steps
        .iter()
        .filter(|candidate| candidate["name"] == step["name"])
        .collect::<Vec<_>>();
    let [producer] = matching.as_slice() else {
        return Err(unreadable("ambiguous producer step"));
    };
    same(
        &producer["env"]["ORBIT_DEPENDENCY_JOB_NAME"],
        &job["name"],
        "producer job binding",
        unreadable,
    )?;
    same(
        &producer["env"]["ORBIT_DEPENDENCY_STEP"],
        &step["name"],
        "producer step binding",
        unreadable,
    )?;
    let helper = text(&producer["env"], "ORBIT_DEPENDENCY_PRODUCER", unreadable)?;
    if helper
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || !helper
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    {
        return Err(unreadable("producer helper path"));
    }
    let checkouts = source_steps
        .iter()
        .take_while(|candidate| candidate["name"] != step["name"])
        .filter(|candidate| {
            candidate["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
                && candidate["with"]["ref"]
                    == "${{ github.event.pull_request.base.sha || github.sha }}"
                && candidate["with"].get("sparse-checkout").is_none()
                && candidate["with"].get("submodules").is_none()
                && candidate["with"].get("lfs").is_none()
                && candidate["with"]["persist-credentials"] == false
        })
        .collect::<Vec<_>>();
    let checkout = checkouts
        .iter()
        .find(|checkout| checkout["with"]["path"] == producer["working-directory"])
        .ok_or_else(|| unreadable("producer requires a complete trusted source checkout"))?;
    if !checkout["uses"]
        .as_str()
        .and_then(|uses| uses.split_once('@'))
        .is_some_and(|(_, sha)| super::input::valid_expected_remote_sha(Some(sha)))
    {
        return Err(unreadable("producer checkout action is not immutable"));
    }
    if checkout.get("env").is_some()
        || checkout["with"]
            .get("repository")
            .is_some_and(|repository| {
                repository != "${{ github.repository }}"
                    && repository != &json!(snapshot.repository)
            })
    {
        return Err(unreadable("producer checkout source override"));
    }
    let checkout_path = text(&checkout["with"], "path", unreadable)?;
    if !checkout_path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        || checkout_path.starts_with('/')
        || checkout_path
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        return Err(unreadable("producer checkout escapes its source root"));
    }
    // The trusted workspace owns interpreter/import/tool isolation, rather
    // than the generic adapter attempting to interpret a command language.
    // Authenticate its versioned promise and the exact workflow source below.
    // An absolute shell entrypoint avoids inherited executable lookup before
    // that trusted invocation starts; its remaining semantics are workspace policy.
    let shell = text(producer, "shell", unreadable)?;
    if !shell.starts_with('/') || !shell.contains("{0}") {
        return Err(unreadable("producer requires an explicit absolute shell"));
    }
    text(producer, "run", unreadable)?;
    same(
        &producer["env"]["ORBIT_DEPENDENCY_EXECUTION"],
        &json!("isolated-source-v1"),
        "producer isolated source contract",
        unreadable,
    )?;
    same(
        &record["execution_contract"],
        &producer["env"]["ORBIT_DEPENDENCY_EXECUTION"],
        "dependency execution contract",
        unreadable,
    )?;
    same(
        &producer["env"]["ORBIT_DEPENDENCY_PRODUCER_SHA"],
        &json!("${{ github.event.pull_request.base.sha || github.sha }}"),
        "producer source revision binding",
        unreadable,
    )?;
    let execution = text(record, "execution_sha", unreadable)?;
    if !super::input::valid_expected_remote_sha(Some(execution)) {
        return Err(unreadable("workflow execution revision"));
    }
    // The source-isolation promise and invocation are authenticated by workflow
    // parity. A checkout alone cannot establish which imports/tools executed.
    // The promise trusts runner/OS integrity; it is not a sandbox attestation.
    same(
        &snapshot.pr["merge_commit_sha"],
        &json!(execution),
        "current event merge revision",
        unreadable,
    )?;
    let event_base = text(&run["pull_requests"][0]["base"], "sha", unreadable)?;
    same(
        &snapshot.pr["base"]["sha"],
        &json!(event_base),
        "event base remains current",
        unreadable,
    )?;
    same(
        &record["producer_sha"],
        &json!(event_base),
        "producer source tree revision",
        unreadable,
    )?;
    deadline.execute(
        "git",
        vec![
            "merge-base".into(),
            "--is-ancestor".into(),
            accepted.into(),
            event_base.into(),
        ],
        workspace,
        "trusted producer base ancestry",
    )?;
    let execution_commit = api(
        deadline,
        workspace,
        &format!("repos/{{owner}}/{{repo}}/git/commits/{execution}"),
    )?;
    same(
        &execution_commit["sha"],
        &json!(execution),
        "execution commit",
        unreadable,
    )?;
    let parents = execution_commit["parents"]
        .as_array()
        .ok_or_else(|| unreadable("execution parents"))?;
    if parents.len() != 2 || parents[0]["sha"] != event_base || parents[1]["sha"] != run["head_sha"]
    {
        return Err(unreadable("execution is not the event merge"));
    }
    let expected_ref = format!(
        "{}/{workflow_path}@refs/pull/{}/merge",
        snapshot.repository, snapshot.pr["number"]
    );
    same(
        &record["workflow_ref"],
        &json!(expected_ref),
        "dependency workflow ref",
        unreadable,
    )?;
    let mut helper_blob = None;
    for revision in [
        accepted,
        event_base,
        text(run, "head_sha", unreadable)?,
        execution,
    ] {
        let contents = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/contents/{helper}?ref={revision}"),
        )?;
        same(&contents["path"], &json!(helper), "helper path", unreadable)?;
        same(&contents["type"], &json!("file"), "helper file", unreadable)?;
        let blob = text(&contents, "sha", unreadable)?;
        if helper_blob.as_ref().is_some_and(|prior| prior != blob) {
            return Err(unreadable("producer helper changed"));
        }
        helper_blob = Some(blob.to_string());
        let workflow_contents = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/contents/{workflow_path}?ref={revision}"),
        )?;
        let original = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/contents/{workflow_path}?ref={accepted}"),
        )?;
        same(
            &workflow_contents["path"],
            &json!(workflow_path),
            "execution workflow path",
            unreadable,
        )?;
        same(
            &workflow_contents["type"],
            &json!("file"),
            "execution workflow file",
            unreadable,
        )?;
        same(
            &workflow_contents["sha"],
            &original["sha"],
            "execution workflow source",
            unreadable,
        )?;
    }
    let helper_source = deadline
        .execute(
            "git",
            vec!["show".into(), format!("{accepted}:{helper}")],
            workspace,
            "trusted dependency helper",
        )?
        .stdout;
    let local_blob = deadline
        .execute(
            "git",
            vec!["rev-parse".into(), format!("{accepted}:{helper}")],
            workspace,
            "trusted dependency helper blob",
        )?
        .stdout;
    if helper_blob.as_deref() != Some(local_blob.trim()) || helper_source.len() > 64 * 1024 {
        return Err(unreadable("unavailable trusted helper source"));
    }
    let dependencies = record["dependencies"]
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 100)
        .ok_or_else(|| unreadable("dependency identities"))?;
    let mut ids = BTreeSet::new();
    for dependency in dependencies {
        let id = number(dependency, "job_id", unreadable)?;
        if id == number(job, "id", unreadable)? || !ids.insert(id) {
            return Err(unreadable("ambiguous dependency graph"));
        }
        let actual = failed
            .iter()
            .find(|job| job["id"].as_u64() == Some(id))
            .ok_or_else(|| unreadable("dependency is not a failed job of this attempt"))?;
        for field in ["status", "conclusion"] {
            same(
                &dependency[field],
                &actual[field],
                "dependency outcome",
                unreadable,
            )?;
        }
        same(
            &dependency["job_name"],
            &actual["name"],
            "dependency name",
            unreadable,
        )?;
        // A step-only wait cannot prove the entire dependency failure in v1.
        if !dependency["step"].is_null() {
            return Err(unreadable(
                "step-only dependency is not a job failure proof",
            ));
        }
    }
    Ok(json!({"job_id":job["id"],"dependency_ids":ids,"record":record,"helper_blob":helper_blob}))
}
