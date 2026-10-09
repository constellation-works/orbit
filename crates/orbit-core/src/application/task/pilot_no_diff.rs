//! Proposed tasks the task pilot verified as already fixed.
//!
//! A `verified_no_diff` assessment of a task that is not tagged
//! `no-diff-expected` says the work is already on the base branch, so
//! approving it would deliver nothing. Every promotion surface holds such a
//! task under its own reason, [`PILOT_VERIFIED_NO_DIFF`], with the pilot's
//! evidence and the commits it cites, not under whichever readiness gap its
//! empty selectors happen to leave.
//!
//! A task Orbit automation filed (a CI-sweep or delivery-review finding, or
//! any `auto-task:` task) is archived once the cited commits pass the proof
//! in [`VerifiedNoDiff::covering_proof`]. Any other task waits for a person.
//!
//! The pilot schema has no commit list, so citations are read from the
//! assessment's own text (`evidence`, `assessment_rationale`, and
//! `already_landed`). A citation is a whole 7-40 character lowercase hex
//! token holding at least one digit and one letter, which leaves out decimal
//! run IDs and English words. Every citation must name a commit in this
//! repository, and at least one must be an ancestor of the base branch.
//!
//! A commit on the base branch is not yet a fix: the commit that introduced
//! the finding is on the base branch by construction. The commits the finding
//! itself names (any commit in its description, and the landed commit of each
//! `regression_from` target) never count as covering. When such a commit is
//! known, a covering commit must also be a strict descendant of every one of
//! them, and when the task declares path selectors it must touch one of those
//! paths. A finding that names no commit and declares no path has nothing to
//! relate a citation to, so it is never proven; anything less is not proof.

use orbit_common::OrbitError;
use orbit_types::task::{
    CONTEXT_CREATION_AUTHORIZED_EVENT, NO_AUTO_APPROVE_TAG, NO_DIFF_EXPECTED_TAG, Task,
    TaskRelationType, TaskStatus,
};
use orbit_types::workflow::{AUTO_TASK_TAG_PREFIX, LandingObservationStatus};
use serde_json::{Value, json};

use super::TaskUpdateParams;
use crate::OrbitRuntime;
use crate::application::automation::source::Source;
use crate::application::search::task_selectors_contain_path;

/// The hold reason, and the admission classification, for a proposed task
/// whose latest pilot assessment is `verified_no_diff`.
pub(crate) const PILOT_VERIFIED_NO_DIFF: &str = "pilot_verified_no_diff";

/// Tags only Orbit automation stamps on the tasks it files.
const AUTO_MINTED_TAGS: [&str; 2] = ["ci-failure-sweep", "delivery-code-review"];
const VERIFIED_NO_DIFF: &str = "verified_no_diff";
const PILOT_ACTOR: &str = "task-pilot";
const RECEIPT_PREFIX: &str = "operation_id=";
const EVIDENCE_SUMMARY_CHARS: usize = 240;

/// Whether Orbit automation filed the task, so a proven `verified_no_diff`
/// may close it without a person.
pub(crate) fn auto_minted(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        AUTO_MINTED_TAGS.contains(&tag.as_str()) || tag.starts_with(AUTO_TASK_TAG_PREFIX)
    })
}

/// A `verified_no_diff` assessment, reduced to what a hold reports and what
/// the proof checks.
pub(crate) struct VerifiedNoDiff {
    pub(crate) evidence: String,
    pub(crate) cited_commits: Vec<String>,
    /// The selectors the task held when the pilot assessed it. A
    /// `verified_no_diff` assessment clears the task's own, so these are the
    /// paths the finding named.
    context_files_before: Vec<String>,
}

impl VerifiedNoDiff {
    /// The finding in `assessment`, when its disposition is
    /// `verified_no_diff`.
    pub(crate) fn from_assessment(assessment: &Value) -> Option<Self> {
        if assessment.get("disposition").and_then(Value::as_str) != Some(VERIFIED_NO_DIFF) {
            return None;
        }
        let evidence = assessment
            .get("evidence")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut texts = Vec::new();
        for field in ["evidence", "assessment_rationale", "already_landed"] {
            if let Some(value) = assessment.get(field) {
                collect_strings(value, &mut texts);
            }
        }
        Some(Self {
            evidence,
            cited_commits: commit_citations(texts),
            context_files_before: Vec::new(),
        })
    }

    /// One line for a hold's `detail`: the evidence, then what it cites.
    pub(crate) fn detail(&self) -> String {
        let mut evidence = self
            .evidence
            .chars()
            .take(EVIDENCE_SUMMARY_CHARS)
            .collect::<String>();
        if self.evidence.chars().count() > EVIDENCE_SUMMARY_CHARS {
            evidence.push('…');
        }
        if evidence.is_empty() {
            evidence = "task-pilot verified the work is already on the base branch".into();
        }
        if self.cited_commits.is_empty() {
            format!("{evidence} [no commit cited]")
        } else {
            format!("{evidence} [cites {}]", self.cited_commits.join(", "))
        }
    }

    /// The structured evidence an admission decision carries.
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "evidence": self.evidence,
            "cited_commits": self.cited_commits,
        })
    }

    /// The cited commits git proves are on `base` and relate to `task`'s
    /// finding, or why there is no proof. Every citation must resolve to a
    /// commit, and at least one must be an ancestor of the local or `origin`
    /// branch, differ from every commit the finding names, descend from all of
    /// them, and touch the task's context paths. Nothing is fetched.
    pub(crate) fn covering_proof(
        &self,
        runtime: &OrbitRuntime,
        task: &Task,
    ) -> Result<Vec<String>, String> {
        if self.cited_commits.is_empty() {
            return Err("the assessment cites no commit".into());
        }
        let source = Source::new(&runtime.paths().repo_root);
        let base = runtime.workspace_base_branch();
        let base_refs = [
            format!("refs/heads/{base}"),
            format!("refs/remotes/origin/{base}"),
        ]
        .into_iter()
        .filter(|reference| resolve_commit(&source, reference).is_some())
        .collect::<Vec<_>>();
        if base_refs.is_empty() {
            return Err(format!("base branch {base} does not resolve"));
        }
        let mut cited = Vec::with_capacity(self.cited_commits.len());
        for sha in &self.cited_commits {
            let Some(full) = resolve_commit(&source, sha) else {
                return Err(format!("cited {sha} is not a commit in this repository"));
            };
            cited.push((sha, full));
        }
        let on_base = cited
            .into_iter()
            .filter(|(_, full)| {
                base_refs
                    .iter()
                    .any(|reference| is_ancestor(&source, full, reference))
            })
            .collect::<Vec<_>>();
        if on_base.is_empty() {
            return Err(format!("no cited commit is on {base}"));
        }

        let named = named_commits(runtime, &source, task);
        let mut paths = task.context_files.clone();
        paths.extend(self.context_files_before.iter().cloned());
        if named.is_empty() && paths.is_empty() {
            return Err(
                "the finding names no commit and no path to relate a cited commit to".into(),
            );
        }
        let covering = on_base
            .into_iter()
            .filter(|(_, full)| {
                !named.contains(full)
                    && named
                        .iter()
                        .all(|culprit| is_ancestor(&source, culprit, full))
                    && (paths.is_empty() || touches_paths(&source, full, &paths))
            })
            .map(|(sha, _)| sha.clone())
            .collect::<Vec<_>>();
        if covering.is_empty() {
            return Err(format!(
                "no cited commit on {base} is a later change to the finding's paths; \
                 the commits the finding names do not count"
            ));
        }
        Ok(covering)
    }
}

/// What [`OrbitRuntime::close_verified_no_diff`] did.
pub(crate) enum NoDiffClosure {
    /// Archived on the strength of these commits.
    Archived { covering_commits: Vec<String> },
    /// Left proposed; `reason` says why it was not closed.
    Held {
        finding: VerifiedNoDiff,
        reason: String,
    },
}

impl OrbitRuntime {
    /// The task's current `verified_no_diff` finding: it is `proposed`, not
    /// tagged `no-diff-expected`, and its newest change is the pilot write
    /// whose audited assessment has that disposition. An edit after the
    /// assessment supersedes it.
    pub(crate) fn verified_no_diff(
        &self,
        task_id: &str,
        status: TaskStatus,
        tags: &[String],
    ) -> Result<Option<(String, VerifiedNoDiff)>, OrbitError> {
        if status != TaskStatus::Proposed || tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG) {
            return Ok(None);
        }
        let history = self.get_task_history(task_id)?;
        let Some(operation_id) = history
            .iter()
            .rev()
            .find(|entry| entry.event != CONTEXT_CREATION_AUTHORIZED_EVENT)
            .filter(|entry| entry.event == "task_pilot_applied")
            .and_then(|entry| entry.note.as_deref())
            .and_then(|note| note.rsplit_once(&format!(" ({RECEIPT_PREFIX}")))
            .and_then(|(_, receipt)| receipt.strip_suffix(')'))
            .map(str::to_string)
        else {
            return Ok(None);
        };
        let header = format!("{RECEIPT_PREFIX}{operation_id}");
        let comments = self.get_task_comments(task_id)?;
        let Some(audit) = comments
            .iter()
            .rev()
            .filter(|comment| comment.by == PILOT_ACTOR)
            .find_map(|comment| {
                let (first, audit) = comment.message.split_once('\n')?;
                (first.trim() == header).then_some(audit)
            })
        else {
            return Ok(None);
        };
        // An unreadable audit is not evidence; the task keeps its other
        // hold reasons.
        let Ok(audit) = serde_json::from_str::<Value>(audit) else {
            return Ok(None);
        };
        let context_files_before = audit
            .get("context_files_before")
            .and_then(Value::as_array)
            .map(|selectors| {
                selectors
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(audit
            .get("assessment")
            .and_then(VerifiedNoDiff::from_assessment)
            .map(|finding| {
                (
                    operation_id,
                    VerifiedNoDiff {
                        context_files_before,
                        ..finding
                    },
                )
            }))
    }

    /// Archive a proposed task Orbit automation filed when its current
    /// `verified_no_diff` finding is proven, with a system comment naming the
    /// covering commits and the assessment. `None` when the task has no such
    /// finding. The decision and the transition run under the task lock.
    pub(crate) fn close_verified_no_diff(
        &self,
        task_id: &str,
    ) -> Result<Option<NoDiffClosure>, OrbitError> {
        let mut closure = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                let Some((operation_id, finding)) =
                    self.verified_no_diff(task_id, task.status, &task.tags)?
                else {
                    return Ok(());
                };
                let held = |finding, reason: &str| NoDiffClosure::Held {
                    finding,
                    reason: reason.to_string(),
                };
                closure = Some(if !auto_minted(&task.tags) {
                    held(finding, "not filed by Orbit automation; a person decides")
                } else if task.tags.iter().any(|tag| tag == NO_AUTO_APPROVE_TAG) {
                    held(finding, "tagged no-auto-approve; a person decides")
                } else {
                    match finding.covering_proof(self, &task) {
                        Err(reason) => held(finding, &reason),
                        Ok(covering_commits) => {
                            let comment = format!(
                                "Archived automatically: task-pilot verified this finding is already fixed (verified_no_diff, assessment operation_id={operation_id}). Covering commit(s) on {}: {}.\nPilot evidence: {}",
                                self.workspace_base_branch(),
                                covering_commits.join(", "),
                                finding.evidence,
                            );
                            self.update_task_as_system(
                                task_id,
                                TaskUpdateParams {
                                    status: Some(TaskStatus::Archived),
                                    comment: Some(comment),
                                    ..Default::default()
                                },
                                None,
                            )?;
                            NoDiffClosure::Archived { covering_commits }
                        }
                    }
                });
                Ok(())
            })?;
        Ok(closure)
    }
}

fn collect_strings<'a>(value: &'a Value, texts: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => texts.push(text),
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_strings(value, texts)),
        Value::Object(fields) => fields
            .values()
            .for_each(|value| collect_strings(value, texts)),
        _ => {}
    }
}

/// The distinct commit citations in `texts`, in first-seen order.
fn commit_citations<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut commits = Vec::new();
    for token in texts
        .into_iter()
        .flat_map(|text| text.split(|c: char| !c.is_ascii_alphanumeric()))
    {
        if commit_citation(token) && !commits.iter().any(|seen| seen == token) {
            commits.push(token.to_string());
        }
    }
    commits
}

/// The full SHAs of the commits the finding itself names: every commit that
/// resolves from its description, and the landed commit of each
/// `regression_from` target that delivery observed as merged.
fn named_commits(runtime: &OrbitRuntime, source: &Source, task: &Task) -> Vec<String> {
    let mut named = commit_citations([task.description.as_str()])
        .iter()
        .filter_map(|sha| resolve_commit(source, sha))
        .collect::<Vec<_>>();
    for relation in &task.relations {
        if relation.relation_type != TaskRelationType::RegressionFrom {
            continue;
        }
        let landed = runtime
            .observe_task_delivery(relation.target.as_str(), None)
            .ok()
            .filter(|observation| observation.landing.status == LandingObservationStatus::Merged)
            .and_then(|observation| observation.landing.landed_commit)
            .and_then(|sha| resolve_commit(source, &sha));
        named.extend(landed);
    }
    named.sort();
    named.dedup();
    named
}

fn resolve_commit(source: &Source, revision: &str) -> Option<String> {
    source
        .git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{revision}^{{commit}}"),
        ])
        .ok()
        .filter(|sha| !sha.is_empty())
}

fn is_ancestor(source: &Source, ancestor: &str, descendant: &str) -> bool {
    source
        .git(&["merge-base", "--is-ancestor", ancestor, descendant])
        .is_ok()
}

/// Whether `commit` changes a path any of the task's selectors cover. A merge
/// commit is judged by its change against the first parent.
fn touches_paths(source: &Source, commit: &str, selectors: &[String]) -> bool {
    let Ok(revision) = source.git(&["rev-list", "--parents", "-n", "1", commit]) else {
        return false;
    };
    let mut revisions = revision.split_whitespace();
    let Some(commit) = revisions.next() else {
        return false;
    };
    let first_parent = revisions.next();

    let mut args = vec!["diff-tree", "--no-commit-id", "--name-only", "-r", "-z"];
    if let Some(parent) = first_parent {
        args.extend([parent, commit]);
    } else {
        args.extend(["--root", commit]);
    }

    source.git_preserving_output(&args).is_ok_and(|changed| {
        changed
            .split('\0')
            .filter(|path| !path.is_empty())
            .any(|path| task_selectors_contain_path(selectors, path))
    })
}

fn commit_citation(token: &str) -> bool {
    (7..=40).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && token.bytes().any(|byte| byte.is_ascii_digit())
        && token.bytes().any(|byte| byte.is_ascii_alphabetic())
}
