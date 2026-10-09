//! The settlement's task comment, the PR's review-fixes section and gate artifacts.

use orbit_automation::review::{validation_limitations, validation_role_counts};
use orbit_common::OrbitError;
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::{
    FindingDisposition, ReviewCertificate, ReviewValidation, ReviewVerdict,
};

use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

/// The task comment a settlement posts [ORB-13989]: the verdict, every
/// finding with what the reviewer changed for it, then the evidence.
pub(in crate::application::review::gate) fn verdict_comment(
    certificate: &ReviewCertificate,
) -> String {
    let assurance = certificate
        .assurance
        .map(|assurance| assurance.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let reviewer_commit = if certificate.repair_commits.is_empty() {
        "none".to_string()
    } else {
        certificate
            .repair_commits
            .iter()
            .map(|commit| {
                format!(
                    "`{}` `{}` by {}",
                    commit.commit,
                    one_line(&commit.subject),
                    commit.author
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "before-PR review settled attempt `{}`: verdict **{}** (assurance: {}).\n\n\
         {}\n\n\
         - Reviewer: crew `{}` ({} / {}){}\n\
         - Implementation: `{}` on base `{}` ({} commit(s), unchanged by review)\n\
         - Reviewer commit: {}\n\
         - Final candidate: `{}`\n\
         - Selectors widened for reviewer-changed paths: {}\n\
         - Validation on final candidate: {} record(s) [{}], complete: {}\n\
         - Owner-required checks: {}\n\
         - Not established by this review: {}\n\
         - Required checks retained from earlier report revisions: {}\n\
         - Reviewer runtime: {}s of {} min\n\
         - Escalation: {}\n\n\
         {}",
        certificate.attempt_id,
        certificate.verdict.as_str(),
        assurance,
        finding_lines(&certificate.findings),
        certificate.reviewer.crew,
        certificate.reviewer.provider,
        certificate.reviewer.model,
        if certificate.reviewer.same_model_as_implementer {
            "; same model as the implementer, reported as such"
        } else {
            ""
        },
        certificate.reviewed_candidate.commit,
        certificate.base.commit,
        certificate.implementation_commits.len(),
        reviewer_commit,
        certificate.final_candidate.commit,
        if certificate.selectors_widened.is_empty() {
            "none".to_string()
        } else {
            certificate.selectors_widened.join(", ")
        },
        certificate.validation.len(),
        validation_roles(&certificate.validation),
        certificate.validation_complete,
        required_commands_line(certificate.required_validation_commands.as_deref()),
        limitations_line(&certificate.validation),
        retained_line(certificate),
        certificate.consumed.seconds,
        certificate.budget.minutes,
        certificate.escalation.as_deref().unwrap_or("none"),
        verdict_consequence(certificate),
    )
}

/// What the verdict means for delivery, in one paragraph.
fn verdict_consequence(certificate: &ReviewCertificate) -> &'static str {
    if !certificate.baseline_red.is_empty() {
        return "Delivery waits: every failed required check fails the same way on the pinned \
                base, so the candidate did not cause it. No PR is opened; the candidate is kept \
                and the task is held in the backlog until the base passes, when a fresh review \
                judges it.";
    }
    match certificate.verdict {
        ReviewVerdict::Accept => {
            "Accepted as implemented; the PR carries the implementation commit(s) only. This \
             verdict is review evidence, not task approval or merge permission."
        }
        ReviewVerdict::AcceptWithFixes => {
            "Accepted with the reviewer's fixes as a separate commit. Owner validation and the \
             ownership check run again on that head before the PR opens; a failure there blocks \
             the task as `reject`. The fixes were validated but not independently reviewed, and \
             this verdict is not task approval or merge permission."
        }
        ReviewVerdict::Reject | ReviewVerdict::Incomplete => {
            "Delivery stops: no PR is opened, the task is blocked, and the candidate branch keeps \
             every commit for final recovery or an operator decision. There is no second review \
             round."
        }
    }
}

/// Every finding of the attempt with its disposition and, for a fix, what
/// the reviewer changed and where.
fn finding_lines(findings: &[orbit_types::workflow::ReviewFinding]) -> String {
    if findings.is_empty() {
        return "Findings: none.".to_string();
    }
    let mut lines = String::from("Findings:");
    for finding in findings {
        let disposition = match &finding.disposition {
            FindingDisposition::Open => "open".to_string(),
            FindingDisposition::Repaired => "fixed".to_string(),
            FindingDisposition::Disposed { reason } => format!("disposed: {}", one_line(reason)),
        };
        lines.push_str(&format!(
            "\n- `{}` [{}, {}] {}",
            finding.id,
            finding.severity,
            disposition,
            one_line(&finding.summary),
        ));
        if let Some(change) = finding_change(finding) {
            lines.push_str(&format!("\n  - Changed: {change}"));
        }
    }
    lines
}

/// What a fixed finding changed, with its paths; `None` for any other
/// disposition.
fn finding_change(finding: &orbit_types::workflow::ReviewFinding) -> Option<String> {
    if finding.disposition != FindingDisposition::Repaired {
        return None;
    }
    let change = finding
        .change
        .as_deref()
        .map(one_line)
        .filter(|change| !change.is_empty())
        .unwrap_or_else(|| "not described by the reviewer".to_string());
    Some(if finding.paths.is_empty() {
        change
    } else {
        format!("{change} ({})", finding.paths.join(", "))
    })
}

/// The PR body's review sections: "Review fixes" when the reviewer committed
/// fixes, listing each fixed finding and what changed, and "Review validation
/// limits" when a diagnostic failed, so the PR never reads as a claim that
/// the whole workspace passed. `None` when neither applies.
pub(in crate::application::review::gate) fn review_fixes_section(
    certificate: &ReviewCertificate,
) -> Option<String> {
    let sections = [
        fixes_section(certificate),
        validation_section(certificate),
        limits_section(&certificate.validation),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    (!sections.is_empty()).then(|| sections.join("\n"))
}

fn validation_section(certificate: &ReviewCertificate) -> Option<String> {
    if certificate.validation.is_empty() && certificate.required_validation_commands.is_none() {
        return None;
    }
    let mut section = format!(
        "## Review validation\n\nOwner-required commands: {}. Validation complete: {}.\n",
        required_commands_line(certificate.required_validation_commands.as_deref()),
        certificate.validation_complete,
    );
    for record in &certificate.validation {
        section.push_str(&format!(
            "\n- `{}` — {} ({}){}{}{}",
            one_line(&record.command),
            record.outcome.as_str(),
            record.role.as_str(),
            record
                .control
                .map(|control| format!("; control: {}", control.as_str()))
                .unwrap_or_default(),
            record
                .note
                .as_deref()
                .filter(|note| !note.trim().is_empty())
                .map(|note| format!("; rationale: {}", one_line(note)))
                .unwrap_or_default(),
            if record.sources.is_empty() {
                String::new()
            } else {
                format!("; sources: {}", record.sources.join(", "))
            },
        ));
    }
    Some(section)
}

fn required_commands_line(commands: Option<&[String]>) -> String {
    match commands {
        Some([]) => "none configured".to_string(),
        Some(commands) => commands
            .iter()
            .map(|command| format!("`{}`", one_line(command)))
            .collect::<Vec<_>>()
            .join(", "),
        None => "missing legacy host contract; fresh review required".to_string(),
    }
}

fn limits_section(records: &[ReviewValidation]) -> Option<String> {
    let limitations = validation_limitations(records);
    if limitations.is_empty() {
        return None;
    }
    let mut section = String::from(
        "## Review validation limits\n\nEvery required check passed on the reviewed head. \
         These diagnostics were observed outside the task's scope, kept as observed, and are \
         not covered by this review:\n",
    );
    for limitation in limitations {
        section.push_str(&format!("\n- {}", one_line(&limitation)));
    }
    Some(section)
}

fn fixes_section(certificate: &ReviewCertificate) -> Option<String> {
    let commit = certificate.repair_commits.first()?;
    let mut section = format!(
        "## Review fixes\n\nThe before-PR reviewer (crew `{}`) fixed its findings in `{}` \
         (`{}`), a separate commit on top of the implementation. Owner validation reran on \
         that head.\n",
        certificate.reviewer.crew,
        commit.commit,
        one_line(&commit.subject),
    );
    for finding in &certificate.findings {
        if let Some(change) = finding_change(finding) {
            section.push_str(&format!(
                "\n- `{}` [{}] {} — {change}",
                finding.id,
                finding.severity,
                one_line(&finding.summary),
            ));
        }
    }
    Some(section)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The classification breakdown of a validation set, so a reader sees which
/// records were required checks and which were controls or exclusions
/// without opening the certificate.
fn validation_roles(records: &[ReviewValidation]) -> String {
    let counts = validation_role_counts(records);
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .into_iter()
        .map(|(role, count)| format!("{count} {}", role.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The failed diagnostics a certificate does not cover, on one line.
fn limitations_line(records: &[ReviewValidation]) -> String {
    let limitations = validation_limitations(records);
    if limitations.is_empty() {
        "none".to_string()
    } else {
        limitations
            .iter()
            .map(|limitation| one_line(limitation))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Earlier report revisions' required checks, with their observed outcomes
/// and, for one the final report retired, the reason it gave.
fn retained_line(certificate: &ReviewCertificate) -> String {
    if certificate.retained_obligations.is_empty() {
        return "none".to_string();
    }
    certificate
        .retained_obligations
        .iter()
        .map(|obligation| {
            let validation = &obligation.validation;
            let id = validation.record_id();
            let retired = id.and_then(|id| {
                certificate
                    .retired_validation
                    .iter()
                    .find(|retired| retired.id.trim() == id)
            });
            format!(
                "{}`{}` {}{}",
                id.map(|id| format!("{id} ")).unwrap_or_default(),
                one_line(&validation.command),
                validation.outcome.as_str(),
                retired
                    .map(|retired| format!(" (retired: {})", one_line(&retired.reason)))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write a gate artifact under the executor run's authority. A claimed leaf
/// owns no task state: the artifact crosses its binding to the owner as
/// claim evidence, like its validation logs [ORB-13908].
pub(in crate::application::review::gate) fn write_artifact(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    path: &str,
    content: &[u8],
) -> Result<(), OrbitError> {
    if runtime.worker_invocation().is_some() {
        runtime.route_worker_tool(
            "orbit.task.artifact.put",
            serde_json::json!({
                "id": task_id,
                "artifacts": [{
                    "path": path,
                    "content": content,
                    "media_type": "application/json",
                }],
            }),
            Default::default(),
        )?;
        return Ok(());
    }
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: path.to_string(),
                content: content.to_vec(),
                media_type: "application/json".to_string(),
                // The record writer derives provenance from the write actor.
                created_by: None,
            }],
            ..TaskUpdateParams::default()
        },
        Some(run_id.to_string()),
    )?;
    Ok(())
}
