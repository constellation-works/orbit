//! [ORB-16185] Source diagnostics of a required check run no workflow
//! produced, such as a provider app's aggregate analysis result.
//!
//! No workflow job answers such a check, and a passing analysis job does not
//! stand in for it. Its evidence is the run's own failure annotations, read
//! through `ci.check_annotations` with the check run and app identities the
//! head-bound required-check read reported for that name. The answer must be
//! bound to the same pull request, head and identities, and each failure
//! annotation must name a source file that exists at that head, through an
//! exact-head blob link, with a line range and a message. A path is provider
//! data: it is validated as a relative repository path and never opened or
//! run. A run without failure annotations, or whose annotations name the
//! workflow configuration, the runner or the network, is not a source
//! failure, and any gap refuses rather than narrows the evidence.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};

use super::super::super::git::git_output;
use super::super::super::operations;
use super::super::super::pr::{RequiredChecks, read_required_checks_observed};
use crate::context::{HandoffLandingContext, RuntimeHost};

/// Annotations an evidence text lists before it summarizes the rest.
const MAX_LISTED: usize = 20;
/// Bytes of one annotation's message an evidence text keeps.
const MAX_MESSAGE_BYTES: usize = 600;

/// A required check run's failure, proven a source failure on one head.
pub(super) struct SourceAnnotations {
    pub(super) check_run_id: u64,
    pub(super) label: String,
    /// The repository paths the failure annotations name.
    pub(super) paths: BTreeSet<String>,
    /// The bounded evidence a repair or a stop carries.
    pub(super) detail: String,
    /// The authenticated provider answer, retained as evidence.
    pub(super) provider: Value,
}

/// Read and validate the failure annotations of required check `check` on
/// `head`. `Err` names why they do not establish a source failure.
pub(super) fn source_annotations<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    head: &str,
    check: &(Option<String>, String),
    required: &RequiredChecks,
    deadline_ms: Option<u64>,
) -> Result<SourceAnnotations, String> {
    let name = check.1.as_str();
    if check.0.is_some() {
        return Err(format!(
            "'{} / {name}' belongs to a workflow, so only its failed job could explain it",
            check.0.as_deref().unwrap_or_default()
        ));
    }
    if required.head_sha != head || required.reported_head_sha.as_deref() != Some(head) {
        return Err(format!(
            "the required-check read does not describe head {head}"
        ));
    }
    if required
        .required
        .iter()
        .filter(|(_, required)| required == name)
        .count()
        != 1
    {
        return Err(format!(
            "the required-check read does not name exactly one required check '{name}'"
        ));
    }
    let [identity] = required
        .check_runs
        .iter()
        .filter(|run| run.name == name)
        .collect::<Vec<_>>()[..]
    else {
        return Err(format!(
            "required check '{name}' is not a check run outside a workflow"
        ));
    };
    let (Some(check_run_id), Some(app_id)) = (identity.check_run_id, identity.app_id) else {
        return Err(format!(
            "the required-check read reported no check run or app identity for '{name}'"
        ));
    };
    let mut input = json!({
        "workspace_path": workspace.to_string_lossy(),
        "pr": pr,
        "head_sha": head,
        "name": name,
        "check_run_id": check_run_id,
        "app_id": app_id,
    });
    if let Some(deadline_ms) = deadline_ms {
        input["deadline_ms"] = json!(deadline_ms);
    }
    let answer = host
        .run_private_vcs_operation(operations::CI_CHECK_ANNOTATIONS, input)
        .map_err(|error| format!("its annotations could not be read: {error}"))?;
    let run = &answer["check_run"];
    let repository = answer["repository"]
        .as_str()
        .filter(|repository| !repository.is_empty() && !repository.contains(char::is_whitespace))
        .ok_or("the annotations answer named no repository")?;
    if answer["head_sha"] != head
        || answer["pr"]
            .as_u64()
            .map(|number| number.to_string())
            .as_deref()
            != Some(pr)
        || run["id"] != check_run_id
        || run["app_id"] != app_id
        || run["name"] != name
        || run["conclusion"] != "failure"
    {
        return Err(
            "the annotations answer is not bound to this pull request, head and check run"
                .to_string(),
        );
    }
    let annotations = answer["annotations"]
        .as_array()
        .ok_or("the annotations answer carried no annotation list")?;
    if run["annotations_count"].as_u64() != Some(annotations.len() as u64) {
        return Err("the annotations answer is incomplete".to_string());
    }
    let blob_prefix = format!("https://github.com/{repository}/blob/{head}/");
    let mut paths = BTreeSet::new();
    let mut listed = Vec::new();
    for annotation in annotations
        .iter()
        .filter(|annotation| annotation["annotation_level"] == "failure")
    {
        let path = annotation["path"]
            .as_str()
            .filter(|path| source_path(path))
            .ok_or("a failure annotation names no repository source path")?;
        if annotation["blob_href"].as_str() != Some(&format!("{blob_prefix}{path}")) {
            return Err(format!(
                "the failure annotation on '{path}' does not link that file at head {head}"
            ));
        }
        let (Some(start), Some(end)) = (
            annotation["start_line"].as_u64().filter(|line| *line > 0),
            annotation["end_line"].as_u64(),
        ) else {
            return Err(format!(
                "the failure annotation on '{path}' names no source line"
            ));
        };
        if end < start {
            return Err(format!(
                "the failure annotation on '{path}' names no source line"
            ));
        }
        let message = annotation["message"]
            .as_str()
            .map(|message| message.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|message| !message.is_empty())
            .ok_or_else(|| format!("the failure annotation on '{path}' carries no message"))?;
        let title = annotation["title"].as_str().unwrap_or_default().trim();
        if let Some(evidence) = super::jobs::transient_evidence(&format!("{title}\n{message}")) {
            return Err(format!(
                "the failure annotation on '{path}' shows `{evidence}`: the runner or the \
                 network failed, not the source"
            ));
        }
        if !git_output(workspace, &["cat-file", "-t", &format!("{head}:{path}")])
            .is_ok_and(|kind| kind.trim() == "blob")
        {
            return Err(format!(
                "annotated path '{path}' is not a file at head {head} in this checkout"
            ));
        }
        paths.insert(path.to_string());
        listed.push(format!(
            "- {path}:{start}{}{}: {}",
            if end > start {
                format!("-{end}")
            } else {
                String::new()
            },
            if title.is_empty() {
                String::new()
            } else {
                format!(" [{}]", bounded(title, 200))
            },
            bounded(&message, MAX_MESSAGE_BYTES)
        ));
    }
    if listed.is_empty() {
        return Err(format!(
            "required check '{name}' has no failure annotation naming a source location"
        ));
    }
    let total = listed.len();
    listed.truncate(MAX_LISTED);
    if total > MAX_LISTED {
        listed.push(format!("- and {} more", total - MAX_LISTED));
    }
    let label = name.to_string();
    let detail = format!(
        "required check '{label}' (check run {check_run_id} of app {app_id} on {repository} pull \
         request #{pr}) failed on head {head} with {total} source failure annotation(s):\n{}",
        listed.join("\n")
    );
    Ok(SourceAnnotations {
        check_run_id,
        label,
        paths,
        detail,
        provider: answer,
    })
}

/// Classify the failed required checks no failed job answers. A check branch
/// protection does not require keeps its previous handling and is skipped;
/// a required one is a candidate failure only when its source annotations
/// all name files the candidate changed from its base.
pub(super) fn unanswered_required<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffLandingContext,
    pr: &str,
    head: &str,
    unanswered: &[&(Option<String>, String)],
    recovery: &mut super::recovery::CheckRecovery,
) -> Vec<super::recovery::FailedJob> {
    let workspace = &context.workspace_path;
    let inconclusive = |label: String, why: String| super::recovery::FailedJob {
        run_id: None,
        job_id: 0,
        label,
        class: super::recovery::CheckFailureClass::Inconclusive,
        signature: String::new(),
        why,
        excerpt: String::new(),
        tip_passes: false,
    };
    let observation = recovery.required_observation.insert(Value::Null);
    let required = match read_required_checks_observed(host, workspace, pr, observation) {
        Ok(required) if required.head_sha == head => {
            let matches = required.reported_head_sha.as_deref() == Some(head);
            observation["head_matches"] = json!(matches);
            if !matches {
                observation["outcome"] = json!("head_mismatch");
            }
            required
        }
        Ok(required) => {
            observation["outcome"] = json!("head_mismatch");
            return vec![inconclusive(
                "required checks".into(),
                format!(
                    "they were read on head {} rather than {head}",
                    required.head_sha
                ),
            )];
        }
        Err(error) => {
            return vec![inconclusive(
                "required checks".into(),
                format!("they could not be read: {error}"),
            )];
        }
    };
    let deadline = (recovery.deadline_ms > 0).then_some(recovery.deadline_ms);
    let mut changed = None;
    let mut classified = Vec::new();
    for check in unanswered {
        if !required.requires(check.0.as_deref(), &check.1) {
            continue;
        }
        let found = match source_annotations(host, workspace, pr, head, check, &required, deadline)
        {
            Ok(found) => found,
            Err(gap) => {
                classified.push(inconclusive(
                    check.1.clone(),
                    format!("no failed job answers it and {gap}"),
                ));
                continue;
            }
        };
        let changed = changed.get_or_insert_with(|| {
            git_output(
                workspace,
                &[
                    "diff",
                    "--name-only",
                    "--no-renames",
                    &context.candidate.base.commit,
                    &context.candidate.candidate.commit,
                ],
            )
            .map(|paths| paths.lines().map(str::to_string).collect::<BTreeSet<_>>())
            .map_err(|error| error.to_string())
        });
        let (class, why) = match changed {
            Err(error) => (
                super::recovery::CheckFailureClass::Inconclusive,
                format!("the candidate's changed files could not be read: {error}"),
            ),
            Ok(changed) if found.paths.is_subset(changed) => (
                super::recovery::CheckFailureClass::Candidate,
                "its source failure annotations name only files the candidate changed".into(),
            ),
            Ok(_) => (
                super::recovery::CheckFailureClass::Inconclusive,
                "its source failure annotations name files the candidate did not change, so \
                 neither the candidate nor the base is proven at fault"
                    .into(),
            ),
        };
        classified.push(super::recovery::FailedJob {
            run_id: None,
            job_id: found.check_run_id,
            label: found.label,
            class,
            signature: format!("check run {} annotations", found.check_run_id),
            why,
            excerpt: found.detail,
            tip_passes: false,
        });
    }
    classified
}

/// A relative repository path that names no workflow configuration and
/// cannot leave the checkout.
fn source_path(path: &str) -> bool {
    path.len() <= 1024
        && !path.chars().any(char::is_control)
        && orbit_common::fs::selector::claim_new_path_is_safe(path)
        && path
            .split('/')
            .next()
            .is_some_and(|first| !first.eq_ignore_ascii_case(".github"))
}

fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
