//! Clean-tree evidence of a handoff: already-landed, no-diff and
//! finding-invalid reports, the already-resolved declaration and the scope
//! comparison that binds them to a task.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Existing no-diff delivery contract shared with the local verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub covering_commit: String,
    pub covering_task_id: String,
    pub scope: serde_json::Value,
    pub required_commands: Vec<String>,
    pub validation: Vec<AlreadyLandedCheck>,
    pub criteria_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedCheck {
    #[serde(flatten)]
    pub validation: crate::workflow::ReviewValidation,
    pub log_artifact: String,
}

/// A run's claim that its implementation correctly changed nothing. Bound to
/// the task, the run, and the pinned HEAD it validated; unlike
/// [`AlreadyLandedEvidence`] it names no covering commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub reason: String,
    pub validation: Vec<NoDiffCheck>,
}

/// A run's report that the review finding it picked up does not exist at its
/// pinned HEAD: the code does not do what the finding claims, or the claimed
/// consequence cannot happen. It changes nothing and runs no workspace gates;
/// its `path:line` citations at `tested_head` are its evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingInvalidReport {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    /// The `task.finding_fingerprint` of the prompt this run judged: the
    /// finding material the report rejects. A task edited since is not rejected.
    pub finding_fingerprint: String,
    /// Why the defect as described does not exist at `tested_head`.
    pub reasoning: String,
    pub evidence: Vec<FindingInvalidCitation>,
}

/// One cited location of a [`FindingInvalidReport`]: a repository-relative
/// `path:line` at its `tested_head`, and what that line shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingInvalidCitation {
    pub location: String,
    pub note: String,
}

/// One validation command the no-diff run executed, with its exit status and
/// the task artifact holding the captured log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffCheck {
    pub command: String,
    pub exit_code: i32,
    pub log_artifact: String,
}

/// A claimed implementer's outcome that its unchanged base already resolves
/// the task, naming the tests that show it [ORB-15747]. Those tests run only
/// outside the agent sandbox, so the claim host replays each on the base
/// before the NoDiff handoff: a failing replay fails the claim, and a
/// declaration that names nothing, or a selector that runs no test, never
/// settles done. It travels in the implementer's output as
/// [`ALREADY_RESOLVED_FIELD`], beside the clean-tree report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyResolved {
    /// `cargo test -p <crate> --test <target> [<filter>]` or
    /// `cargo test -p <crate> --lib [<filter>]` selectors.
    pub selectors: Vec<String>,
}

/// The implementer output field that carries an [`AlreadyResolved`].
pub const ALREADY_RESOLVED_FIELD: &str = "already_resolved";

impl AlreadyResolved {
    /// Maximum host test executions one already-resolved declaration may request.
    pub const MAX_SELECTORS: usize = 16;

    /// Admission refuses a declaration that names no selectors, and every
    /// declaration above the cap; replay enforces only the cap, since replay
    /// with no declaration is an empty list.
    pub fn check_selector_count(count: usize) -> Result<(), String> {
        if count == 0 {
            return Err(format!(
                "`{ALREADY_RESOLVED_FIELD}` names no test selectors, so nothing would show the \
                 base resolves the task"
            ));
        }
        Self::check_selector_cap(count)
    }

    /// Refuse more than [`Self::MAX_SELECTORS`] selectors before anything runs.
    pub fn check_selector_cap(count: usize) -> Result<(), String> {
        if count > Self::MAX_SELECTORS {
            return Err(format!(
                "`{ALREADY_RESOLVED_FIELD}` names {count} test selectors; at most {} are allowed",
                Self::MAX_SELECTORS
            ));
        }
        Ok(())
    }

    /// The declaration in an implementer's `output`, if it makes one.
    pub fn from_output(output: &serde_json::Value) -> Result<Option<Self>, String> {
        match output.get(ALREADY_RESOLVED_FIELD) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| {
                    format!(
                        "`{ALREADY_RESOLVED_FIELD}` must be `{{\"selectors\": [\"cargo test -p \
                         <crate> --test <target> [<filter>]\" or \"cargo test -p <crate> --lib \
                         [<filter>]\", ...]}}`: {error}"
                    )
                }),
        }
    }

    /// Admit at most [`Self::MAX_SELECTORS`] selectors in the closed
    /// [`HostSandboxCommand`](crate::workflow::HostSandboxCommand) grammar, since each
    /// runs on the claim host outside any agent sandbox: one integration
    /// `--test` target or the package's `--lib` unit tests. An exact required
    /// command is not a selector: validation already runs it, and it need not
    /// run a test.
    pub fn admit(&self) -> Result<Vec<crate::workflow::HostSandboxCommand>, String> {
        Self::check_selector_count(self.selectors.len())?;
        let mut admitted: Vec<crate::workflow::HostSandboxCommand> = Vec::new();
        for selector in &self.selectors {
            let command = crate::workflow::HostSandboxCommand::admit(selector, &[]).map_err(|refusal| {
                format!(
                    "`{ALREADY_RESOLVED_FIELD}` selector `{selector}` is not admitted ({}): {}; \
                     a selector is `cargo test -p <crate> --test <target> [<filter>]` or \
                     `cargo test -p <crate> --lib [<filter>]`",
                    refusal.reason.as_str(),
                    refusal.detail
                )
            })?;
            if admitted.contains(&command) {
                return Err(format!(
                    "`{ALREADY_RESOLVED_FIELD}` names selector `{selector}` twice"
                ));
            }
            admitted.push(command);
        }
        Ok(admitted)
    }
}

/// Whether `reported` is the scope [`already_landed_scope`] computes for
/// `task` and `comments`, allowing only the owner's recording of the
/// `regression_from` relations this handoff itself supplied.
///
/// A claimed leaf cannot write its task's relations, so it returns culprit
/// task IDs in its handoff and the owner records them as `regression_from`
/// relations when it accepts the handoff [ORB-24696]. Those recorded
/// relations appear in the owner's current scope but not in the scope the
/// leaf verified, and must not invalidate that verification. Every other
/// difference — a removed, reordered or changed relation, a relation the
/// handoff did not supply, or any other scope key — still does.
pub fn already_landed_scope_matches(
    reported: &serde_json::Value,
    task: &crate::task::Task,
    comments: &[crate::task::TaskComment],
    supplied_regression_from: &[String],
) -> bool {
    let expected = already_landed_scope(task, comments);
    if reported == &expected {
        return true;
    }
    if supplied_regression_from.is_empty() {
        return false;
    }
    let (Some(reported), Some(expected)) = (reported.as_object(), expected.as_object()) else {
        return false;
    };
    if reported.len() != expected.len()
        || reported
            .iter()
            .any(|(key, value)| key != "relations" && expected.get(key) != Some(value))
    {
        return false;
    }
    let (Some(reported_relations), Some(expected_relations)) = (
        reported.get("relations").and_then(|value| value.as_array()),
        expected.get("relations").and_then(|value| value.as_array()),
    ) else {
        return false;
    };
    // The reported relations must survive in order; every extra relation in
    // the current scope must be a plain `regression_from` link (the exact
    // shape the owner records on acceptance) to a supplied target.
    let supplied: BTreeSet<&str> = supplied_regression_from
        .iter()
        .map(String::as_str)
        .collect();
    let mut matched = 0;
    for relation in expected_relations {
        if reported_relations.get(matched) == Some(relation) {
            matched += 1;
            continue;
        }
        let plain_supplied = relation.as_object().is_some_and(|relation| {
            relation.len() == 2
                && relation.get("type").and_then(|value| value.as_str()) == Some("regression_from")
                && relation
                    .get("target")
                    .and_then(|value| value.as_str())
                    .is_some_and(|target| supplied.contains(target))
        });
        if !plain_supplied {
            return false;
        }
    }
    matched == reported_relations.len()
}

pub fn already_landed_scope(
    task: &crate::task::Task,
    comments: &[crate::task::TaskComment],
) -> serde_json::Value {
    // A comment's scope is what it says; a receipt id is bookkeeping, and
    // leaving it out keeps one scope across Orbit versions.
    let comments = comments
        .iter()
        .map(|comment| {
            serde_json::json!({"at": comment.at, "by": comment.by, "message": comment.message})
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "title": task.title, "description": task.description,
        "acceptance_criteria": task.acceptance_criteria, "plan": task.plan,
        "context_files": task.context_files, "tags": task.tags, "relations": task.relations,
        "required_tools": task.required_tools, "type": task.task_type, "comments": comments,
    })
}
