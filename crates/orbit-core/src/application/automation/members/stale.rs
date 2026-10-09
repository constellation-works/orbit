//! Which claimed tasks a branch move made stale.

use super::super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::automation_error_to_orbit;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::members::*;
use serde_json::Value;
use std::collections::BTreeMap;

/// Which of `task_ids` the branch made stale since `claim` froze its source,
/// each with the reason; empty while every one is still fresh.
///
/// A drain lands on the integration branch every few minutes, and an operator
/// pull or deploy moves it many commits at once, so its head routinely moves
/// while a pilot runs. A task stays fresh when the head only advanced through
/// commits disjoint from its own prepared material [ORB-12981], judged by the
/// consumer's freshness policy, the one the scheduler fingerprints under
/// [ORB-14476]:
///
/// - `source_sensitivity = any`: every move makes every task stale.
/// - otherwise a task is stale when a changed path lies under one of its
///   context selectors (current, prepared, or `material` the caller is about
///   to write for it), or when a changed `AGENTS.md` / `CLAUDE.md` governs one
///   of those selectors: it sits in the selector's directory or an ancestor.
///   A selector anchored outside the repository is one no commit changes; one
///   with no filesystem anchor at all (`module:`, `command:`) cannot be
///   compared, so its task is stale. When `instructions` is a material field,
///   any instruction change makes every task stale, as it changes every
///   fingerprint.
///
/// A rewritten branch, or a diff that cannot be read, makes every task stale.
/// Incident claims are not tied to the head.
pub(crate) fn stale_tasks(
    runtime: &OrbitRuntime,
    claim: &MemberAttempt,
    policy: &PreparationPolicy,
    prepared: &Value,
    task_ids: &[String],
    material: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    if claim.kind != StateTriggerKind::PreparationEligible || task_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let branch = runtime
        .automation_store()?
        .automation_state(&claim.consumer)?
        .ok_or_else(|| OrbitError::InvalidInput("state claim missing".into()))?
        .branch;
    let root = &runtime.paths().repo_root;
    let source = Source::new(root);
    let (_, head) = source
        .local_head(&branch)
        .map_err(automation_error_to_orbit)?;
    let prepared_source = &claim.member.source;
    if head == *prepared_source {
        return Ok(BTreeMap::new());
    }

    // A pilot can now pin origin while the primary branch still lags it.
    // That older local head does not invalidate material already inspected
    // at the fetched pin. Read existing refs only: local advances beyond the
    // pin and rewritten histories still take the original stale-claim path.
    if source
        .git(&[
            "merge-base",
            "--is-ancestor",
            &head.commit,
            &prepared_source.commit,
        ])
        .is_ok()
        && source
            .git(&[
                "merge-base",
                "--is-ancestor",
                &prepared_source.commit,
                &format!("refs/remotes/origin/{branch}"),
            ])
            .is_ok()
    {
        return Ok(BTreeMap::new());
    }

    let stale = |detail: &str| {
        format!(
            "state-trigger source changed from {} to {}: {detail}",
            prepared_source.commit, head.commit
        )
    };
    let every = |detail: String| {
        Ok(task_ids
            .iter()
            .map(|id| (id.clone(), stale(&detail)))
            .collect())
    };

    if policy.freshness.source_sensitivity == SourceSensitivity::Any {
        return every("source_sensitivity is any".into());
    }
    if source
        .git(&[
            "merge-base",
            "--is-ancestor",
            &prepared_source.commit,
            &head.commit,
        ])
        .is_err()
    {
        return every("the branch no longer descends from the prepared source".into());
    }

    // `--no-renames` reports both sides of a rename; `--relative` keeps paths
    // in the workspace frame the selectors and instruction scan use.
    let changed = match source.git(&[
        "diff",
        "--name-only",
        "--no-renames",
        "--relative",
        "-z",
        &prepared_source.commit,
        &head.commit,
    ]) {
        Ok(changed) => changed,
        Err(error) => return every(format!("changed paths unavailable: {error}")),
    };
    let changed = changed
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let instructions = changed
        .iter()
        .copied()
        .filter(|path| matches!(path.rsplit('/').next(), Some("AGENTS.md" | "CLAUDE.md")))
        .collect::<Vec<_>>();
    if policy.freshness.includes(MaterialField::Instructions)
        && let Some(path) = instructions.first()
    {
        return every(format!("repository instructions `{path}` changed"));
    }

    let prepared_selectors = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| {
            let id = task.get("task_id")?.as_str()?;
            let selectors = task.get("context_files_before")?.as_array()?;
            Some((id, selectors.iter().filter_map(Value::as_str).collect()))
        })
        .collect::<BTreeMap<&str, Vec<&str>>>();

    let mut stale_tasks = BTreeMap::new();
    for id in task_ids {
        let mut selectors = material.get(id).cloned().unwrap_or_default();
        match runtime.get_task(id) {
            Ok(task) => selectors.extend(task.context_files),
            // The write boundary reports a deleted task stale on its own.
            Err(OrbitError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        if let Some(prepared) = prepared_selectors.get(id.as_str()) {
            selectors.extend(prepared.iter().map(|selector| (*selector).to_owned()));
        }
        selectors.sort();
        selectors.dedup();
        if let Some(detail) = selector_change(root, &selectors, &changed, &instructions) {
            stale_tasks.insert(id.clone(), stale(&detail));
        }
    }

    if stale_tasks.len() < task_ids.len() {
        tracing::info!(
            consumer = %claim.consumer,
            attempt = %claim.id,
            from = %prepared_source.commit,
            to = %head.commit,
            changed = changed.len(),
            stale = stale_tasks.len(),
            "preparation revalidated across a head move disjoint from its material"
        );
    }
    Ok(stale_tasks)
}

/// Why `changed` reaches one of `selectors`, if it does: a path under a
/// selector, or an instruction file on a selector's instruction path.
fn selector_change(
    root: &std::path::Path,
    selectors: &[String],
    changed: &[&str],
    instructions: &[&str],
) -> Option<String> {
    let under = |path: &str, anchor: &str| {
        anchor.is_empty()
            || path == anchor
            || path
                .strip_prefix(anchor)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    for selector in selectors {
        let Some(anchor) = repository_anchor(root, selector) else {
            return Some(format!(
                "context selector `{selector}` has no repository path to compare"
            ));
        };
        let Some(anchor) = anchor else {
            continue;
        };
        if let Some(path) = changed.iter().find(|path| under(path, &anchor)) {
            return Some(format!(
                "`{path}` changed under prepared context selector `{selector}`"
            ));
        }
        if let Some(path) = instructions.iter().find(|path| {
            let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
            under(&anchor, directory)
        }) {
            return Some(format!(
                "repository instructions `{path}` govern prepared context selector `{selector}`"
            ));
        }
    }
    None
}

/// The workspace-relative path a context selector anchors (`""` for the
/// root), `Some(None)` for an anchor outside the repository, which no commit
/// can change, and `None` when the selector has no filesystem anchor at all
/// (`module:`, `command:`, unparseable input).
fn repository_anchor(root: &std::path::Path, selector: &str) -> Option<Option<String>> {
    let anchor = orbit_common::fs::selector::anchor_path(selector).ok()?;
    let relative = if anchor.is_absolute() {
        let canonical = root.canonicalize().ok();
        match anchor
            .strip_prefix(root)
            .ok()
            .or_else(|| anchor.strip_prefix(canonical.as_deref()?).ok())
        {
            Some(relative) => relative.to_path_buf(),
            None => return Some(None),
        }
    } else {
        anchor
    };
    if relative.starts_with("..") {
        return Some(None);
    }
    let relative = relative.to_string_lossy();
    Some(Some(if relative == "." {
        String::new()
    } else {
        relative.into_owned()
    }))
}
