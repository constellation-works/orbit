//! Authenticated, bounded PR workflow emission proof for an older head.
//! No workflow is executed or interpreted; path-filtered workflows contribute
//! no guaranteed contexts, and other unsupported semantics refuse.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::completion_deadline::Deadline;
use super::prerequisite::{
    api, complete_list, gate_passed, gate_present, number, same, snapshot, text, unreadable,
};
use super::workflow_dependency::job_names;
use super::{MAX_WORKFLOW_SOURCE_BYTES, input::required_string};

fn git(deadline: Deadline, workspace: &Path, args: &[&str]) -> Result<String, OrbitError> {
    Ok(deadline
        .execute(
            "git",
            args.iter().map(|s| (*s).into()).collect(),
            workspace,
            "workflow emission Git proof",
        )?
        .stdout
        .trim()
        .to_string())
}

/// Only literal PR synchronize triggers, branch selectors and unconditional
/// jobs with the existing literal-name/single-axis matrix semantics, plus the
/// adapter's finite live-PR read condition and its exact change-scope gate, are
/// known. Other conditions refuse.
fn emitted(source: &str, branch: &str) -> Result<Vec<String>, OrbitError> {
    let workflow: Value = serde_yaml::from_str(source).map_err(|_| unreadable("workflow YAML"))?;
    let event = workflow
        .get("on")
        .ok_or_else(|| unreadable("workflow trigger"))?;
    let trigger = match event {
        Value::String(event) => (event == "pull_request").then_some(Value::Null),
        Value::Array(events) => {
            if events.iter().any(|event| event.as_str().is_none()) {
                return Err(unreadable("dynamic workflow events"));
            }
            events
                .iter()
                .any(|event| event == "pull_request")
                .then_some(Value::Null)
        }
        Value::Object(events) => events.get("pull_request").cloned(),
        _ => return Err(unreadable("workflow event semantics")),
    };
    let Some(trigger) = trigger else {
        return Ok(vec![]);
    };
    if let Some(selectors) = trigger.as_object() {
        // A path-filtered workflow cannot guarantee a check on synchronize.
        // Keep authenticating its blob and identity in `sources`, but do not
        // let its conditional trigger prevent proof from sibling workflows.
        if selectors.contains_key("paths") || selectors.contains_key("paths-ignore") {
            return Ok(vec![]);
        }
        if selectors
            .keys()
            .any(|key| !matches!(key.as_str(), "branches" | "types"))
        {
            return Err(unreadable("unsupported PR workflow filters"));
        }
        for (key, expected) in [("branches", branch), ("types", "synchronize")] {
            if let Some(values) = selectors.get(key) {
                let values = values
                    .as_array()
                    .filter(|v| !v.is_empty() && v.len() <= 100)
                    .ok_or_else(|| unreadable("workflow trigger selector"))?;
                let mut matches = false;
                for value in values {
                    let value = value
                        .as_str()
                        .filter(|v| {
                            !v.is_empty() && !v.contains(['*', '?', '!', '[', ']', '{', '}'])
                        })
                        .ok_or_else(|| unreadable("dynamic or patterned workflow selector"))?;
                    matches |= value == expected;
                }
                if !matches {
                    return Ok(vec![]);
                }
            }
        }
    } else if !trigger.is_null() {
        return Err(unreadable("PR workflow trigger"));
    }
    let jobs = workflow["jobs"]
        .as_object()
        .filter(|j| !j.is_empty() && j.len() <= 100)
        .ok_or_else(|| unreadable("workflow jobs"))?;
    let mut names = BTreeSet::new();
    for (id, job) in jobs {
        let mut bytes = id.bytes();
        if !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !matches!(
                job["runs-on"].as_str(),
                Some(
                    "ubuntu-latest"
                        | "ubuntu-22.04"
                        | "ubuntu-24.04"
                        | "macos-latest"
                        | "windows-latest"
                )
            )
            || !job["steps"]
                .as_array()
                .is_some_and(|steps| !steps.is_empty() && steps.len() <= 100)
        {
            return Err(unreadable(
                "unsupported emission job identity, runner or steps",
            ));
        }
        super::workflow_read::pr_emission_job(&workflow, id)?;
        // Reuse the adapter's bounded job/matrix parser, rather than guessing
        // context names from text or from unrelated successful push runs.
        for name in job_names(id, job)? {
            if !names.insert(name) {
                return Err(unreadable("ambiguous source context"));
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn sources(
    deadline: Deadline,
    workspace: &Path,
    commit: &str,
    branch: &str,
) -> Result<BTreeMap<String, Value>, OrbitError> {
    let listing = git(
        deadline,
        workspace,
        &["ls-tree", "-r", commit, "--", ".github/workflows"],
    )?;
    let mut result = BTreeMap::new();
    for row in listing.lines() {
        let (object, path) = row
            .split_once('\t')
            .ok_or_else(|| unreadable("workflow tree entry"))?;
        if !(path.ends_with(".yml") || path.ends_with(".yaml")) {
            continue;
        }
        if result.len() >= 100 || path.split('/').count() != 3 {
            return Err(unreadable("workflow tree bound or path"));
        }
        let fields: Vec<_> = object.split_whitespace().collect();
        if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" {
            return Err(unreadable("workflow source mode"));
        }
        let contents = api(
            deadline,
            workspace,
            &format!("repos/{{owner}}/{{repo}}/contents/{path}?ref={commit}"),
        )?;
        same(&contents["type"], json!("file"), "workflow file")?;
        same(&contents["path"], json!(path), "workflow source path")?;
        same(
            &contents["sha"],
            json!(fields[2]),
            "workflow source revision",
        )?;
        let source = deadline
            .execute(
                "git",
                vec!["show".into(), format!("{commit}:{path}")],
                workspace,
                "workflow emission source",
            )?
            .stdout;
        if source.len() > MAX_WORKFLOW_SOURCE_BYTES
            || contents["size"].as_u64() != Some(source.len() as u64)
        {
            return Err(unreadable("truncated workflow emission source"));
        }
        let names = emitted(&source, branch)?;
        result.insert(
            path.into(),
            json!({"commit":commit,"blob":fields[2],"source":source,"contexts":names}),
        );
    }
    Ok(result)
}

/// Derive evidence exclusively from authenticated provider reads and freshly
/// fetched Git. The operation is private to deterministic owner automation.
pub(super) fn evidence(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let pr = required_string(input, "pr")?;
    let head = required_string(input, "head_sha")?;
    let branch = required_string(input, "head_branch")?;
    let base = required_string(input, "base_branch")?;
    let deadline = Deadline::from_input(input)?;
    let current = snapshot(deadline, workspace, pr, head, branch, base)?;
    let checks = complete_list(&current.checks, "check_runs")?;
    let statuses = complete_list(&current.statuses, "statuses")?;
    let missing: Vec<_> = current
        .required
        .iter()
        .filter(|gate| !gate_present(gate, checks, statuses))
        .collect();
    if missing.is_empty() {
        return Ok(json!({"evidence":null}));
    }
    // Existing failures and running checks retain their ordinary paths. A
    // skipped job that no policy requires (ci.yml's `Docs guards` on a full
    // run) never blocks a merge; a skipped required check still waits.
    if checks.iter().any(|check| {
        check["status"] != "completed"
            || (check["conclusion"] != "success"
                && (check["conclusion"] != "skipped"
                    || current
                        .required
                        .iter()
                        .any(|gate| check["name"] == gate.context)))
    }) {
        return Ok(json!({"evidence":null}));
    }
    let runs = complete_list(&current.runs, "workflow_runs")?;
    // [ORB-16339] Before any workflow has run on the head, nothing can show a
    // context will never be emitted; the ordinary required-check wait applies.
    if runs.is_empty() {
        return Ok(json!({"evidence":null}));
    }
    for run in runs {
        same(&run["head_sha"], json!(head), "workflow head")?;
        same(
            &run["repository"]["id"],
            json!(current.repository_id),
            "workflow repository",
        )?;
        if run["status"] != "completed" || run["conclusion"] != "success" {
            return Ok(json!({"evidence":null}));
        }
    }
    let origin = git(deadline, workspace, &["remote", "get-url", "origin"])?;
    if super::super::claim::slug(&origin).as_deref() != Some(current.repository.as_str()) {
        return Err(unreadable("workflow repository differs from Git origin"));
    }
    let lease = git(
        deadline,
        workspace,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    if lease.split_whitespace().next() != Some(head) {
        return Err(unreadable("emission remote head lease"));
    }
    super::super::git::fetch_remote_base(workspace, base)?;
    let base_sha = git(
        deadline,
        workspace,
        &["rev-parse", &format!("origin/{base}^{{commit}}")],
    )?;
    same(
        &current.pr["base"]["sha"],
        json!(base_sha),
        "fresh emission base",
    )?;
    let old_base = required_string(input, "accepted_base")?;
    if !super::super::git::git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", old_base, &base_sha],
    )? || !super::super::git::git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", old_base, head],
    )? || super::super::git::git_command_success(
        workspace,
        &["merge-base", "--is-ancestor", &base_sha, head],
    )? {
        return Err(unreadable("base is not a fresh descendant advance"));
    }
    let merged = git(
        deadline,
        workspace,
        &["merge-tree", "--write-tree", head, &base_sha],
    )?;
    let tree = merged
        .lines()
        .next()
        .ok_or_else(|| unreadable("clean emission merge tree"))?;
    for status in statuses {
        if status["state"] != "success"
            || missing.iter().any(|gate| status["context"] == gate.context)
        {
            return Err(unreadable("legacy status is not absent workflow emission"));
        }
    }
    let old = sources(deadline, workspace, head, base)?;
    let new = sources(deadline, workspace, &base_sha, base)?;
    let workflows = api(
        deadline,
        workspace,
        "repos/{owner}/{repo}/actions/workflows?per_page=100",
    )?;
    let registered = complete_list(&workflows, "workflows")?;
    let mut paths = BTreeSet::new();
    for workflow in registered {
        number(workflow, "id")?;
        let path = text(workflow, "path")?;
        if !paths.insert(path) || !new.contains_key(path) || workflow["state"] != "active" {
            return Err(unreadable(
                "inactive, foreign or incomplete workflow identity",
            ));
        }
    }
    if paths.len() != new.len() {
        return Err(unreadable("incomplete current-base workflows"));
    }
    let app = api(deadline, workspace, "apps/github-actions")?;
    same(
        &app["slug"],
        json!("github-actions"),
        "workflow integration",
    )?;
    let app_id = number(&app, "id")?;
    let mut bound_runs = Vec::new();
    for listed in runs {
        let run = super::prerequisite::checked_run(deadline, workspace, &current, listed)?;
        if run["status"] != "completed" || run["conclusion"] != "success" {
            return Err(unreadable("prior PR run changed since observation"));
        }

        same(
            &run["pull_requests"][0]["base"]["sha"],
            json!(old_base),
            "prior PR event base",
        )?;
        let (path, blob, source) = super::prerequisite::workflow(deadline, workspace, &run)?;
        same(
            &old.get(&path)
                .ok_or_else(|| unreadable("foreign prior workflow"))?["blob"],
            json!(blob),
            "prior source blob",
        )?;
        emitted(&source, base)?;
        let suite = api(
            deadline,
            workspace,
            &format!(
                "repos/{{owner}}/{{repo}}/check-suites/{}",
                number(&run, "check_suite_id")?
            ),
        )?;
        same(&suite["id"], run["check_suite_id"].clone(), "prior suite")?;
        same(&suite["head_sha"], json!(head), "prior suite head")?;
        same(
            &suite["repository"]["id"],
            json!(current.repository_id),
            "prior suite repository",
        )?;
        same(
            &suite["app"]["id"],
            json!(app_id),
            "prior workflow integration",
        )?;
        same(
            &suite["app"]["slug"],
            json!("github-actions"),
            "prior workflow app",
        )?;
        if !checks
            .iter()
            .any(|check| check["check_suite"]["id"] == suite["id"] && check["app"]["id"] == app_id)
        {
            return Err(unreadable("prior run has no authenticated check suite"));
        }
        bound_runs.push(json!({"run":run,"suite":suite}));
    }
    let revisions = policy_revisions(deadline, workspace, &current.policy)?;
    for gate in &missing {
        if gate.app_id.is_some_and(|id| id != app_id) {
            return Err(unreadable("foreign required integration"));
        }
        let count = |sources: &BTreeMap<String, Value>| {
            sources
                .values()
                .filter(|source| {
                    source["contexts"]
                        .as_array()
                        .is_some_and(|names| names.iter().any(|name| name == &gate.context))
                })
                .count()
        };
        if count(&old) != 0 || count(&new) != 1 {
            return Err(unreadable(
                "missing context has no unique old/new emission proof",
            ));
        }
    }
    // The predicted provider merge must execute exactly the proven base
    // sources. Candidate workflow edits cannot borrow base emission evidence.
    let merged_paths = git(
        deadline,
        workspace,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            tree,
            "--",
            ".github/workflows",
        ],
    )?;
    let merged_paths: BTreeSet<_> = merged_paths
        .lines()
        .filter(|path| path.ends_with(".yml") || path.ends_with(".yaml"))
        .collect();
    if merged_paths != paths {
        return Err(unreadable("candidate changes workflow set"));
    }
    for (path, source) in &new {
        if git(
            deadline,
            workspace,
            &["rev-parse", &format!("{tree}:{path}")],
        )? != source["blob"].as_str().unwrap_or_default()
        {
            return Err(unreadable("candidate changes current-base workflow source"));
        }
    }
    deadline.remaining()?;
    Ok(
        json!({"evidence":{"repository":current.repository,"repository_id":current.repository_id,"pr_number":pr,"head_sha":head,"head_branch":branch,"base_branch":base,"base_sha":base_sha,"accepted_base":old_base,"merged_tree":tree,"required":current.required,"missing":missing,"policy":current.policy,"app":app,"workflows":workflows,"old_sources":old,"base_sources":new,"provider":current.pr,"checks":current.checks,"statuses":current.statuses,"runs":current.runs,"bound_runs":bound_runs,"policy_revisions":revisions}}),
    )
}

/// All exact current-policy contexts must actually succeed on the new head.
/// This is observation, never permission to repeat a provider mutation.
pub(super) fn observe(input: &Value) -> Result<Value, OrbitError> {
    let proof = &input["evidence"];
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    let current = snapshot(
        deadline,
        workspace,
        text(proof, "pr_number")?,
        required_string(input, "head_sha")?,
        text(proof, "head_branch")?,
        text(proof, "base_branch")?,
    )?;
    same(
        &json!(current.repository),
        proof["repository"].clone(),
        "reserved repository",
    )?;
    same(
        &json!(current.repository_id),
        proof["repository_id"].clone(),
        "reserved repository identity",
    )?;
    let workflows = api(
        deadline,
        workspace,
        "repos/{owner}/{repo}/actions/workflows?per_page=100",
    )?;
    complete_list(&workflows, "workflows")?;
    same(
        &workflows,
        proof["workflows"].clone(),
        "current workflow identities",
    )?;
    same(
        &api(deadline, workspace, "apps/github-actions")?,
        proof["app"].clone(),
        "current workflow integration",
    )?;
    let revisions = policy_revisions(deadline, workspace, &current.policy)?;
    same(
        &revisions,
        proof["policy_revisions"].clone(),
        "effective rule revisions",
    )?;
    let mut policy = current.policy;
    // The head alone legitimately changes; every policy source/revision and
    // review decision must still agree, even when the context names match.
    policy["classic"]["data"]["repository"]["pullRequest"]["headRefOid"] =
        proof["head_sha"].clone();
    same(
        &policy,
        proof["policy"].clone(),
        "reserved effective policy",
    )?;
    same(
        &current.pr["base"]["sha"],
        proof["base_sha"].clone(),
        "reserved base",
    )?;
    same(
        &json!(current.required),
        proof["required"].clone(),
        "required integration identities",
    )?;
    let checks = complete_list(&current.checks, "check_runs")?;
    let statuses = complete_list(&current.statuses, "statuses")?;
    let missing = proof["missing"]
        .as_array()
        .filter(|gates| !gates.is_empty())
        .ok_or_else(|| unreadable("reserved missing gates"))?;
    let passed = current
        .required
        .iter()
        .all(|gate| gate_passed(gate, checks, statuses))
        && missing.iter().all(|gate| {
            checks.iter().any(|check| {
                check["name"] == gate["context"]
                    && check["app"]["id"] == proof["app"]["id"]
                    && check["status"] == "completed"
                    && check["conclusion"] == "success"
            })
        });
    deadline.remaining()?;
    Ok(
        json!({"gates_passed":passed,"checks":current.checks,"statuses":current.statuses,"policy":policy}),
    )
}

/// Identity and revision of each active policy source, not just its names.
fn policy_revisions(
    deadline: Deadline,
    workspace: &Path,
    policy: &Value,
) -> Result<Value, OrbitError> {
    let rule =
        &policy["classic"]["data"]["repository"]["pullRequest"]["baseRef"]["branchProtectionRule"];
    if !rule.is_null() {
        text(rule, "id")?;
    }
    let mut revisions = BTreeMap::new();
    for rule in policy["effective_rules"]
        .as_array()
        .ok_or_else(|| unreadable("effective rule identities"))?
    {
        if let Some(id) = rule["ruleset_id"].as_u64() {
            if revisions.contains_key(&id) {
                continue;
            }
            let revision = api(
                deadline,
                workspace,
                &format!("repos/{{owner}}/{{repo}}/rulesets/{id}"),
            )?;
            same(&revision["id"], json!(id), "ruleset identity")?;
            text(&revision, "updated_at")?;
            same(
                &revision["enforcement"],
                json!("active"),
                "ruleset enforcement",
            )?;
            revisions.insert(id, revision);
        }
    }
    Ok(json!(revisions))
}

/// A verified base-only head still owes today's exact effective policy gates.
/// This read also covers externally requested updates without our reservation.
pub(super) fn current_gates(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    let current = snapshot(
        deadline,
        workspace,
        required_string(input, "pr")?,
        required_string(input, "head_sha")?,
        required_string(input, "head_branch")?,
        required_string(input, "base_branch")?,
    )?;
    let revisions = policy_revisions(deadline, workspace, &current.policy)?;
    let checks = complete_list(&current.checks, "check_runs")?;
    let statuses = complete_list(&current.statuses, "statuses")?;
    let passed = current
        .required
        .iter()
        .all(|gate| gate_passed(gate, checks, statuses));
    // [ORB-16339] Gates the head has not reported in any state: a fresh head
    // owes them before the provider has queued its first check run.
    let missing: Vec<_> = current
        .required
        .iter()
        .filter(|gate| !gate_present(gate, checks, statuses))
        .collect();
    Ok(
        json!({"gates_passed":passed,"required":current.required,"missing":missing,"policy":current.policy,"policy_revisions":revisions,"checks":current.checks,"statuses":current.statuses}),
    )
}
