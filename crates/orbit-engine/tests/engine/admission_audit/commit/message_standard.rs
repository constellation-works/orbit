//! [ORB-25009] Every Orbit commit and squash message meets one standard:
//! a typed subject within budget with each id in its own brackets, and one
//! final trailer block of registered keys in registry order.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    TaskRelation, TaskRelationType, TaskType,
};
use orbit_types::workflow::validate;
use serde_json::json;

use super::git_commit;
use super::git_output;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};

fn classified_relation(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: target.to_string(),
        regression: Some(RegressionClassification {
            nature: RegressionNature::DefectInNew,
            escape: RegressionEscape::NoTest,
            severity: DefectSeverity::Medium,
            severity_source: SeveritySource::Filer,
            sites: Vec::new(),
        }),
    }
}

fn commit_as(repo: &Path, name: &str, email: &str, message: &str, file: &str) {
    fs::write(repo.join(file), message).expect("write fixture file");
    git_output(repo, &["add", "--", file]).expect("stage fixture file");
    let output = crate::git_test_support::command()
        .args(["commit", "-q", "-m", message])
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", "orbit")
        .env("GIT_COMMITTER_EMAIL", "orbit@orbit.local")
        .output()
        .expect("git commit runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The squash of PR #9663 (ORB-24993): the forge wrote the PR title as the
/// subject, repeated the title, left the regression and recovery trailers
/// mid-body behind a `---------` separator and mixed two identity schemes.
/// Orbit now composes the message itself from task state and the inner
/// commits, rewriting the identities those commits were written under.
#[test]
fn squash_of_motivating_pr_composes_one_standard_message() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::squash_of_motivating_pr_composes_one_standard_message"
    )) {
        return;
    }
    let title = "Footprint-held requeue drops the published-delivery resume: next \
                 admission no longer adopts the preserved PR";
    let mut task = task_with_file("ORB-24993", title, "README.md", "codex");
    task.task_type = TaskType::Bug;
    task.relations = vec![classified_relation("ORB-24985")];
    task.execution_summary = "## Summary\n\nA footprint-held resumed published delivery \
                              keeps its preserved publication.\n\n## Validation\n\n- ok"
        .to_string();

    let temp = initialized_git_repo();
    let repo = temp.path();
    let base = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    commit_as(
        repo,
        "orbit[claude-opus-5-5]",
        "agent@orbit.invalid",
        &format!(
            "fix: Footprint-held requeue drops the published-delivery resume: next a… \
             [ORB-24993]\n\n{title}\n\n\
             Fixes: 6025f86d1a2b (\"Hold resumed review repairs when their deferred \
             footprint stays busy (#9660)\")\n\
             Regression-From: ORB-24985\n\
             Regression: defect_in_new; escape=no_test; severity=medium"
        ),
        "a.txt",
    );
    commit_as(
        repo,
        "orbit[muse-spark-1.3-contributor]",
        "orbit-recovery@orbit.local",
        "fix: recover the complete_pr step [ORB-24993]\n\n\
         Orbit-Task: ORB-24993\n\
         Orbit-Recovery-Run: jrun-20261115-1043-c3\n\
         Orbit-Recovery-Step: complete_pr\n\
         Orbit-Recovery-Activity: step_failure_recovery",
        "b.txt",
    );
    let head = git_output(repo, &["rev-parse", "HEAD"]).unwrap();

    let message = orbit_engine::compose_squash_merge_message(
        repo,
        std::slice::from_ref(&task),
        &orbit_engine::PrConfig::default(),
        Some(&base),
        &head,
        9663,
    );
    let rendered = format!("{}\n\n{}", message.title, message.body);
    let expected = format!(
        "fix: Footprint-held requeue drops the [ORB-24993] (#9663)\n\n\
         {title}\n\n\
         A footprint-held resumed published delivery keeps its preserved publication.\n\n\
         Orbit-Task: ORB-24993\n\
         Fixes: 6025f86d1a2b (\"Hold resumed review repairs when their deferred footprint \
         stays busy (#9660)\")\n\
         Regression-From: ORB-24985\n\
         Regression: defect_in_new; escape=no_test; severity=medium\n\
         Orbit-Recovery-Run: jrun-20261115-1043-c3\n\
         Orbit-Recovery-Step: complete_pr\n\
         Orbit-Recovery-Activity: step_failure_recovery\n\
         Co-authored-by: orbit[claude-opus-5-5] <agent@orbit.invalid>\n\
         Co-authored-by: orbit[muse-spark-1.3-contributor] <recovery@orbit.invalid>"
    );
    assert_eq!(rendered, expected);
    validate(&rendered).expect("the squash message meets the standard");

    let message_file = repo.join(".orbit-squash-message.txt");
    fs::write(&message_file, &rendered).expect("write message");
    let parsed = git_output(
        repo,
        &[
            "interpret-trailers",
            "--parse",
            message_file.to_str().expect("utf-8 path"),
        ],
    )
    .unwrap();
    assert_eq!(
        parsed.lines().count(),
        9,
        "git reads all nine trailers from the one final block: {parsed}"
    );
    assert!(!rendered.contains("@orbit.local"), "{rendered}");
}

/// [ORB-25009] Task, finalize and single-task batch commits, reviewer repairs and a
/// bundle squash each meet the standard, typed by the task type alone.
/// Before, a bundle bracketed its ids together, the reviewer repair used
/// `review:` with mid-body trailers and long titles ended in `…`.
#[test]
fn every_producer_emits_a_valid_message_typed_by_its_task() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::every_producer_emits_a_valid_message_typed_by_its_task"
    )) {
        return;
    }
    let mut feature = task_with_file("ORB-9", "Add the guard", "README.md", "codex");
    feature.task_type = TaskType::Feature;
    feature.orchestrator = Some("crew".to_string());
    let mut long = task_with_file(
        "ORB-5",
        &format!("Fix [see {}]", "é".repeat(200)),
        "other.txt",
        "claude",
    );
    long.task_type = TaskType::Refactor;

    let mut produced = Vec::new();
    for (scope, tasks, kind) in [
        ("per_task", vec![feature.clone()], "feat"),
        ("per_task_finalize", vec![long.clone()], "refactor"),
        ("all", vec![long.clone()], "refactor"),
    ] {
        let temp = initialized_git_repo();
        let repo = temp.path();
        fs::write(repo.join("README.md"), "changed\n").unwrap();
        fs::write(repo.join("other.txt"), "changed\n").unwrap();
        let ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let host = CommitTestHost::new(tasks, repo.to_path_buf());
        git_commit(
            &host,
            &json!({
                "workspace_path": repo,
                "job_run_id": "batch-1",
                "scope": scope,
                "completed_task_ids": ids,
            }),
        )
        .unwrap();
        let message = git_output(repo, &["log", "-1", "--format=%B"]).unwrap();
        produced.push((scope.to_string(), message, kind));
    }
    produced.push((
        "review repair".to_string(),
        orbit_engine::review_gate::reviewer_repair_message(
            std::slice::from_ref(&feature),
            "tighten the guard",
            &["F1".to_string()],
            &["README.md".to_string()],
            "attempt-1",
            "haiku@xhigh",
        ),
        "feat",
    ));

    let temp = initialized_git_repo();
    let repo = temp.path();
    let base = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    commit_as(
        repo,
        "Someone",
        "someone@orbit.invalid",
        "feat: one [ORB-1]",
        "a.txt",
    );
    let head = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    let first = task_with_file("ORB-1", "First change", "a.txt", "codex");
    let second = task_with_file("ORB-2", "Second change", "b.txt", "codex");
    let squash = orbit_engine::compose_squash_merge_message(
        repo,
        &[first, second],
        &orbit_engine::PrConfig::default(),
        Some(&base),
        &head,
        12,
    );
    assert!(
        squash.title.ends_with(" [ORB-1] [ORB-2] (#12)"),
        "each id in its own brackets: {}",
        squash.title
    );
    assert!(
        squash
            .body
            .contains("Co-authored-by: Someone <someone@orbit.invalid>"),
        "foreign co-author identity must survive squash composition: {}",
        squash.body
    );
    produced.push((
        "bundle squash".to_string(),
        format!("{}\n\n{}", squash.title, squash.body),
        "chore",
    ));

    for (producer, message, kind) in produced {
        validate(&message).unwrap_or_else(|violation| panic!("{producer}: {violation}\n{message}"));
        assert!(
            message.starts_with(&format!("{kind}: ")),
            "{producer} type: {message}"
        );
        assert!(!message.contains('…'), "{producer}: {message}");
    }
}

/// [ORB-25059] A squash body that is only the trailer block (no prose
/// paragraph, so no blank line precedes it) kept `Orbit-Review-Crew:
/// haiku@xhigh` byte-identical. Before, `trailer_block_start` required a
/// preceding blank line, so the body alone fell back to plain mention
/// neutralisation and gained a zero-width joiner. Prose mentions in the
/// title and body outside the trailer block are still neutralised.
#[test]
fn squash_with_trailers_only_body_keeps_trailer_values_byte_identical() {
    if !super::super::run_isolated(concat!(
        module_path!(),
        "::squash_with_trailers_only_body_keeps_trailer_values_byte_identical"
    )) {
        return;
    }
    // Trailers-only body: unreadable tasks, one old-format review commit.
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    commit_as(
        repo,
        "orbit[haiku]",
        "reviewer@orbit.invalid",
        "review: repair the guard\n\n\
         Orbit-Review-Attempt: a1\n\
         Orbit-Review-Crew: haiku@xhigh",
        "a.txt",
    );
    let head = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    let message = orbit_engine::compose_squash_merge_message(
        repo,
        &[],
        &orbit_engine::PrConfig::default(),
        Some(&base),
        &head,
        42,
    );
    assert_eq!(
        message.body,
        "Orbit-Review-Attempt: a1\n\
         Orbit-Review-Crew: haiku@xhigh\n\
         Co-authored-by: orbit[haiku] <reviewer@orbit.invalid>",
        "trailers-only squash body stays byte-identical"
    );
    assert!(!message.body.contains('\u{200d}'), "{}", message.body);

    // Prose outside the trailer block is still neutralised.
    let temp = initialized_git_repo();
    let repo = temp.path();
    let base = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    commit_as(
        repo,
        "orbit[haiku]",
        "reviewer@orbit.invalid",
        "review: repair the guard\n\n\
         Orbit-Review-Attempt: a1\n\
         Orbit-Review-Crew: haiku@xhigh",
        "a.txt",
    );
    let head = git_output(repo, &["rev-parse", "HEAD"]).unwrap();
    let mut task = task_with_file("ORB-1", "Ask @auto to tighten the guard", "a.txt", "codex");
    task.execution_summary = "Ping @auto about the guard.".to_string();
    let message = orbit_engine::compose_squash_merge_message(
        repo,
        std::slice::from_ref(&task),
        &orbit_engine::PrConfig::default(),
        Some(&base),
        &head,
        43,
    );
    assert!(
        message.title.contains("Ask @\u{200d}auto"),
        "title prose mention neutralised: {}",
        message.title
    );
    assert!(
        message.body.contains("Ping @\u{200d}auto about the guard."),
        "body prose mention neutralised: {}",
        message.body
    );
    assert!(
        message.body.contains("\nOrbit-Review-Crew: haiku@xhigh"),
        "trailer value byte-identical after prose: {}",
        message.body
    );
}
