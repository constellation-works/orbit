//! The squash message Orbit composes at merge time and hands the forge.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::MergeMessage;

use crate::context::{PrConfig, RuntimeHost};

use super::super::forge_text;
use super::super::git::{git_command_success, git_output_raw};
use super::message::{InnerCommit, squash_merge_message};

/// Record and field separators in the inner-commit log.
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

/// The squash message of pull request `pr_number` delivering `tasks`, whose
/// commits are `base..head` in `repo`. Subject and body come from task
/// state; the trailers are those of the inner commits plus one
/// `Co-authored-by` per distinct inner author. `base` is tried as
/// `origin/<base>`, then as given; when neither resolves, or the log cannot
/// be read, the trailers come from task state alone. Mentions are
/// neutralized outside the trailer block.
pub fn compose_squash_merge_message(
    repo: &Path,
    tasks: &[Task],
    pr_config: &PrConfig,
    base: Option<&str>,
    head: &str,
    pr_number: u64,
) -> MergeMessage {
    let inner = base
        .and_then(|base| inner_commits(repo, base, head))
        .unwrap_or_default();
    let message = MergeMessage::from(&squash_merge_message(tasks, pr_config, pr_number, &inner));
    MergeMessage {
        title: forge_text::neutralize_mentions(&message.title),
        body: forge_text::neutralize_mentions(&message.body),
    }
}

/// The non-merge commits of `base..head`, oldest first, or `None` when the
/// range cannot be read.
fn inner_commits(repo: &Path, base: &str, head: &str) -> Option<Vec<InnerCommit>> {
    let base = base.trim();
    let head = head.trim();
    if base.is_empty() || head.is_empty() || base.starts_with('-') || head.starts_with('-') {
        return None;
    }
    let resolves = |rev: &str| {
        git_command_success(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
        .unwrap_or(false)
    };
    let remote = format!("origin/{base}");
    let base = [remote.as_str(), base]
        .into_iter()
        .find(|candidate| resolves(candidate))?;
    if !resolves(head) {
        return None;
    }
    let format = format!("--format=%an{FIELD}%ae{FIELD}%B{RECORD}");
    let range = format!("{base}..{head}");
    let log = match git_output_raw(
        repo,
        &[
            "log",
            "--no-merges",
            "--reverse",
            &format,
            "--end-of-options",
            &range,
        ],
    ) {
        Ok(log) => log,
        Err(error) => {
            tracing::warn!(%error, %range, "squash message: inner commits unreadable");
            return None;
        }
    };
    Some(
        log.split(RECORD)
            .filter_map(|record| {
                let mut fields = record.trim_start_matches('\n').splitn(3, FIELD);
                let author_name = fields.next()?.trim().to_string();
                let author_email = fields.next()?.trim().to_string();
                let message = fields.next()?.trim().to_string();
                (!author_email.is_empty() || !message.is_empty()).then_some(InnerCommit {
                    author_name,
                    author_email,
                    message,
                })
            })
            .collect(),
    )
}

/// The `commit_title` and `commit_message` fields a `pr.merge` request
/// carries for pull request `pr_number` (as its selector text).
pub(in crate::executor::automation::vcs) fn squash_merge_fields<H: RuntimeHost + ?Sized>(
    host: &H,
    repo: &Path,
    tasks: &[Task],
    base: Option<&str>,
    head: &str,
    pr_number: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, OrbitError> {
    let number = pr_number
        .trim()
        .trim_start_matches('#')
        .parse::<u64>()
        .map_err(|_| {
            OrbitError::InvalidInput(format!(
                "a squash message needs a pull request number, not '{pr_number}'"
            ))
        })?;
    let message = compose_squash_merge_message(repo, tasks, &host.pr_config(), base, head, number);
    let mut fields = serde_json::Map::new();
    fields.insert("commit_title".into(), message.title.into());
    fields.insert("commit_message".into(), message.body.into());
    Ok(fields)
}
