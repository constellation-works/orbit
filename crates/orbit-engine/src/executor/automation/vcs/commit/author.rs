use std::collections::BTreeSet;

use orbit_types::identity::infer_agent_family_from_model;
use orbit_types::task::Task;

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

    pub(super) fn orbit() -> Self {
        Self::new("orbit", "orbit@orbit.local")
    }

    pub(super) fn resolved_model(model: &str) -> Self {
        Self::new(format!("orbit[{model}]"), "agent@orbit.invalid")
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

    fn trailer(&self) -> String {
        format!("Co-Authored-By: {}", self.spec())
    }
}

/// The reviewer's own agent-family identity [ORB-11333]. Repair commits
/// are never amended into implementer commits; this author string is what
/// keeps the two distinguishable through squash and rebase evidence.
pub(super) fn reviewer_author(reviewer_model: &str) -> GitAuthor {
    let family = implementer_family(reviewer_model).unwrap_or_else(|| author_slug(reviewer_model));
    GitAuthor::new(
        format!("{family}-reviewer"),
        format!("{family}-reviewer@orbit.local"),
    )
}

pub(super) fn git_author_for_task(task: &Task) -> Option<GitAuthor> {
    git_author_for_implemented_by(task.implemented_by.as_deref())
}

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

pub(super) fn append_co_author_trailers(message: &mut String, coauthors: &[GitAuthor]) {
    if coauthors.is_empty() {
        return;
    }

    message.push_str("\n\n");
    message.push_str(
        &coauthors
            .iter()
            .map(GitAuthor::trailer)
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn git_author_for_implemented_by(implemented_by: Option<&str>) -> Option<GitAuthor> {
    let implemented_by = implemented_by?.trim();
    if implemented_by.is_empty() {
        return None;
    }

    match implementer_family(implemented_by).as_deref() {
        Some("claude") => Some(GitAuthor::new("claude", "claude@orbit.local")),
        Some("gemini") => Some(GitAuthor::new("gemini", "gemini@orbit.local")),
        Some("codex") => Some(GitAuthor::new("codex", "codex@orbit.local")),
        Some("grok") => Some(GitAuthor::new("grok", "grok@orbit.local")),
        _ => {
            let slug = author_slug(implemented_by);
            Some(GitAuthor::new(slug.clone(), format!("{slug}@orbit.local")))
        }
    }
}

fn implementer_family(implemented_by: &str) -> Option<String> {
    let lower = implemented_by.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }

    let model_hint = lower
        .rsplit_once(" / ")
        .map(|(_, model)| model.trim())
        .unwrap_or(lower.as_str());

    infer_agent_family_from_model(model_hint)
        .or_else(|| {
            if model_hint.starts_with("o4") {
                Some("codex".to_string())
            } else {
                None
            }
        })
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
