//! The Git identities Orbit commits under: one scheme,
//! `orbit[<model>] <<role>@orbit.invalid>` (`orbit <<role>@orbit.invalid>`
//! without a model), from [`orbit_types::workflow::orbit_identity`].

use std::collections::BTreeSet;

use orbit_types::task::Task;
use orbit_types::workflow::{CommitRole, Trailer, TrailerKey};

use super::message::role_author;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct GitAuthor {
    name: String,
    email: String,
}

impl GitAuthor {
    pub(super) fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
        }
    }

    /// Orbit itself: every workflow commit's committer.
    pub(super) fn orbit() -> Self {
        role_author(CommitRole::Orbit, None)
    }

    /// The implementer whose resolved crew model made the change.
    pub(super) fn resolved_model(model: &str) -> Self {
        role_author(CommitRole::Implementer, Some(model))
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn email(&self) -> &str {
        &self.email
    }

    pub(super) fn spec(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }

    pub(super) fn co_author_trailer(&self) -> Trailer {
        Trailer::new(TrailerKey::CoAuthoredBy, self.spec())
    }
}

/// The reviewer's own identity [ORB-11333]. Repair commits are never
/// amended into implementer commits; this author is what keeps the two
/// distinguishable through squash and rebase evidence. The name carries the
/// reviewer's resolved model [ORB-16768].
pub(super) fn reviewer_author(reviewer_model: &str) -> GitAuthor {
    role_author(CommitRole::Reviewer, Some(reviewer_model))
}

/// The identities a reviewer repair commit of `reviewer_model` may carry:
/// the current one first, then the one written before the single scheme,
/// so a repair committed by an older binary on an in-flight branch is still
/// recognised as the reviewer's own.
pub(super) fn reviewer_authors_accepted(reviewer_model: &str) -> Vec<GitAuthor> {
    vec![
        reviewer_author(reviewer_model),
        legacy_reviewer_author(reviewer_model),
    ]
}

/// The committer identities accepted beside [`reviewer_authors_accepted`].
pub(super) fn orbit_committers_accepted() -> Vec<GitAuthor> {
    vec![
        GitAuthor::orbit(),
        GitAuthor::new("orbit", "orbit@orbit.local"),
    ]
}

/// The author of a step recovery's working-tree repair, which Orbit commits
/// for the agent: the recovery sandbox keeps Git metadata read-only. The
/// name carries the recovery activity's resolved crew model [ORB-16768].
pub(super) fn recovery_author(recovery_model: Option<&str>) -> GitAuthor {
    role_author(CommitRole::Recovery, recovery_model)
}

/// The implementer identity of a task, from `implemented_by`.
pub(super) fn git_author_for_task(task: &Task) -> Option<GitAuthor> {
    let implemented_by = task.implemented_by.as_deref()?.trim();
    (!implemented_by.is_empty()).then(|| role_author(CommitRole::Implementer, Some(implemented_by)))
}

/// The author and co-authors of a commit delivering `tasks`: their one
/// implementer, or Orbit with every distinct implementer as a co-author.
pub(super) fn commit_author_for_tasks(tasks: &[Task]) -> (Option<GitAuthor>, Vec<GitAuthor>) {
    let authors = tasks
        .iter()
        .filter_map(git_author_for_task)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    match authors.as_slice() {
        [] => (None, Vec::new()),
        [author] => (Some(author.clone()), Vec::new()),
        _ => (Some(GitAuthor::orbit()), authors),
    }
}

/// The reviewer identity written before the single scheme (ORB-16768):
/// `orbit[<model>] <<family>-reviewer@orbit.local>`. Recognised only, never
/// written.
fn legacy_reviewer_author(reviewer_model: &str) -> GitAuthor {
    let family = legacy_family(reviewer_model).unwrap_or_else(|| author_slug(reviewer_model));
    let model = reviewer_model
        .rsplit(" / ")
        .next()
        .map(str::trim)
        .unwrap_or("");
    let email = format!("{family}-reviewer@orbit.local");
    if model.is_empty() {
        GitAuthor::new(format!("{family}-reviewer"), email)
    } else {
        GitAuthor::new(format!("orbit[{model}]"), email)
    }
}

/// The agent family the legacy reviewer address was named after.
fn legacy_family(label: &str) -> Option<String> {
    let lower = label.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let model_hint = lower
        .rsplit_once(" / ")
        .map(|(_, model)| model.trim())
        .unwrap_or(lower.as_str());
    orbit_types::identity::infer_agent_family_from_model(model_hint)
        .or_else(|| model_hint.starts_with("o4").then(|| "codex".to_string()))
        .or_else(|| {
            if lower.starts_with("codex") || lower.starts_with("openai") {
                Some("codex".to_string())
            } else if lower.starts_with("claude") || lower.contains("/claude") {
                Some("claude".to_string())
            } else if lower.starts_with("gemini") || lower.contains("/gemini") {
                Some("gemini".to_string())
            } else {
                None
            }
        })
}

fn author_slug(label: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in label.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}
