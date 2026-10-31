//! Publish a task's screenshot tree to its permanent evidence ref.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::EVIDENCE_REF_PREFIX;
use serde_json::{Value, json};

use super::input::{required_string, valid_expected_remote_sha};
use super::process::{execute, execute_with_transient_retry};
use super::push_retry::{PushBackoff, PushFailure, execute_push_with_transient_retry};
use super::{DEFAULT_TIMEOUT_MS, LONG_TIMEOUT_MS};

/// Identity of the commits Orbit writes to an evidence ref.
const EVIDENCE_AUTHOR_NAME: &str = "Orbit";
const EVIDENCE_AUTHOR_EMAIL: &str = "orbit-evidence@noreply.invalid";

/// Make `tree_sha` the tree of `target_ref`'s tip on `origin`, and return
/// that commit with the repository's web URL.
///
/// A tip whose tree already equals `tree_sha` is reused, so a retried or
/// resumed `pr_open` publishes nothing new. Otherwise one commit holding the
/// tree is chained onto the tip and pushed without force: earlier evidence
/// commits stay reachable, so the images an earlier pull request links by
/// sha keep resolving.
pub(super) fn push_evidence_ref(input: &Value) -> Result<Value, OrbitError> {
    let repo_root = Path::new(required_string(input, "repo_root")?);
    let tree_sha = required_string(input, "tree_sha")?;
    let target_ref = required_string(input, "target_ref")?;
    let message = required_string(input, "message")?;
    if !valid_expected_remote_sha(Some(tree_sha)) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS evidence push requires an exact 40- or 64-character tree_sha"
                .to_string(),
        ));
    }
    if !valid_evidence_ref(target_ref) {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS evidence push target must be a plain ref under \
             '{EVIDENCE_REF_PREFIX}'"
        )));
    }
    let backoff = PushBackoff::from_input(input)?;

    let tip = remote_tip(repo_root, target_ref)?;
    if let Some(tip) = &tip {
        execute(
            "git",
            vec![
                "fetch".into(),
                "--no-tags".into(),
                "--no-write-fetch-head".into(),
                "--".into(),
                "origin".into(),
                target_ref.into(),
            ],
            Some(repo_root),
            LONG_TIMEOUT_MS,
            "evidence ref fetch",
        )?;
        let tip_tree = execute(
            "git",
            vec![
                "rev-parse".into(),
                "--verify".into(),
                format!("{tip}^{{tree}}"),
            ],
            Some(repo_root),
            DEFAULT_TIMEOUT_MS,
            "evidence ref tree",
        )?;
        if tip_tree.stdout.trim().eq_ignore_ascii_case(tree_sha) {
            return Ok(json!({
                "commit_sha": tip,
                "reused": true,
                "repository_url": repository_url(repo_root)?,
            }));
        }
    }

    let mut commit_args = vec![
        "-c".to_string(),
        format!("user.name={EVIDENCE_AUTHOR_NAME}"),
        "-c".to_string(),
        format!("user.email={EVIDENCE_AUTHOR_EMAIL}"),
        "commit-tree".to_string(),
        tree_sha.to_string(),
        "-m".to_string(),
        message.to_string(),
    ];
    if let Some(tip) = &tip {
        commit_args.extend(["-p".to_string(), tip.clone()]);
    }
    let commit = execute(
        "git",
        commit_args,
        Some(repo_root),
        DEFAULT_TIMEOUT_MS,
        "evidence commit",
    )?;
    let commit_sha = commit.stdout.trim().to_string();
    // No `+`: the push only creates the ref or fast-forwards it.
    execute_push_with_transient_retry(
        &[
            "push".to_string(),
            "--".to_string(),
            "origin".to_string(),
            format!("{commit_sha}:{target_ref}"),
        ],
        repo_root,
        target_ref,
        &commit_sha,
        "evidence ref push",
        backoff,
    )
    .map_err(PushFailure::into_error)?;
    Ok(json!({
        "commit_sha": commit_sha,
        "reused": false,
        "repository_url": repository_url(repo_root)?,
    }))
}

/// The commit `target_ref` names on `origin`, or `None` before its first push.
fn remote_tip(repo_root: &Path, target_ref: &str) -> Result<Option<String>, OrbitError> {
    let listed = execute(
        "git",
        vec![
            "ls-remote".into(),
            "--refs".into(),
            "--".into(),
            "origin".into(),
            target_ref.into(),
        ],
        Some(repo_root),
        LONG_TIMEOUT_MS,
        "evidence ref lookup",
    )?;
    Ok(listed.stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let sha = fields.next()?;
        (fields.next() == Some(target_ref) && valid_expected_remote_sha(Some(sha)))
            .then(|| sha.to_ascii_lowercase())
    }))
}

/// The repository's web URL, which image links are built on.
fn repository_url(repo_root: &Path) -> Result<String, OrbitError> {
    let response = execute_with_transient_retry(
        "gh",
        &[
            "repo".to_string(),
            "view".to_string(),
            "--json".to_string(),
            "url".to_string(),
        ],
        Some(repo_root),
        DEFAULT_TIMEOUT_MS,
        "repository identity",
    )?;
    let repository: Value = serde_json::from_str(&response.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS repository identity returned invalid JSON: {error}"
        ))
    })?;
    repository
        .get("url")
        .and_then(Value::as_str)
        .map(|url| url.trim().trim_end_matches('/'))
        .filter(|url| url.starts_with("https://"))
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS repository identity omitted an https url".to_string(),
            )
        })
}

/// Whether `value` is a ref under [`EVIDENCE_REF_PREFIX`] made of plain path
/// segments, so it can be neither an option nor a refspec.
fn valid_evidence_ref(value: &str) -> bool {
    value
        .strip_prefix(EVIDENCE_REF_PREFIX)
        .is_some_and(super::push_retry::plain_ref_segments)
}
