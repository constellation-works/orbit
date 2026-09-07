use orbit_common::test_fixtures::TEST_CODEX_MODEL;

use super::super::body::*;
use super::test_support::*;

#[test]
fn pr_body_at_github_byte_limit_is_preserved() {
    let mut task = task_with_contract(
        "ORB-10474",
        "Bound PR body",
        "x",
        "Keep the generated body within GitHub's limit.",
        &[],
        None,
    );
    let initial_body = build_batch_pr_body(
        &[task.clone()],
        &freshness(),
        &[],
        &test_pr_config(None),
        None,
    );
    task.execution_summary = "x".repeat(GITHUB_PR_BODY_BYTE_LIMIT - initial_body.len() + 1);

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);

    assert_eq!(body.len(), GITHUB_PR_BODY_BYTE_LIMIT);
    assert!(!body.contains("**Audit note:**"));
}

#[test]
fn oversized_multibyte_pr_body_is_truncated_on_a_utf8_boundary() {
    let task = task_with_contract(
        "ORB-10474",
        "Bound PR body",
        &"é".repeat(GITHUB_PR_BODY_BYTE_LIMIT),
        "Keep the generated body within GitHub's limit.",
        &[],
        None,
    );

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);

    assert!(body.len() <= GITHUB_PR_BODY_BYTE_LIMIT);
    assert!(body.contains("**Audit note:**"));
    assert!(body.contains("ORB-10474"));
    assert!(std::str::from_utf8(body.as_bytes()).is_ok());
}

#[test]
fn oversized_single_task_body_keeps_an_audit_reference_to_its_artifact() {
    let task = task_with_contract(
        "ORB-10474",
        "Bound PR body",
        &"x".repeat(GITHUB_PR_BODY_BYTE_LIMIT),
        "Keep the generated body within GitHub's limit.",
        &[],
        None,
    );

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);

    assert!(body.len() <= GITHUB_PR_BODY_BYTE_LIMIT);
    assert!(body.contains("complete task artifacts remain the durable record in the persisted Orbit task artifacts for ORB-10474"));
}

#[test]
fn oversized_batch_body_keeps_an_audit_reference_to_every_task_artifact() {
    let body = build_batch_pr_body(
        &[
            task(
                "ORB-10474",
                "First task",
                &"x".repeat(GITHUB_PR_BODY_BYTE_LIMIT),
            ),
            task(
                "ORB-10475",
                "Second task",
                &"x".repeat(GITHUB_PR_BODY_BYTE_LIMIT),
            ),
        ],
        &freshness(),
        &[],
        &test_pr_config(None),
        None,
    );

    assert!(body.len() <= GITHUB_PR_BODY_BYTE_LIMIT);
    assert!(body.contains("ORB-10474, ORB-10475"));
    assert!(body.contains("**Audit note:**"));
}

#[test]
fn task_url_rendering_covers_template_and_external_ref_priority() {
    let plain = task_with_contract("T123", "Plain task", "done", "", &[], None);
    assert_eq!(task_url(&plain, &test_pr_config(None)), None);
    assert_eq!(
        render_single_task_line(&plain, &test_pr_config(None)),
        "T123 — Plain task"
    );

    let external = task_with_contract(
        "T124",
        "External task",
        "done",
        "",
        &[],
        Some("https://tracker.example/T124"),
    );
    assert_eq!(
        task_url(&external, &test_pr_config(None)).as_deref(),
        Some("https://tracker.example/T124")
    );
    assert_eq!(
        render_single_task_line(&external, &test_pr_config(None)),
        "[T124](https://tracker.example/T124) — External task"
    );

    let templated = task_with_contract("T125", "Templated task", "done", "", &[], None);
    assert_eq!(
        task_url(
            &templated,
            &test_pr_config(Some("https://orbit-cli.com/tasks/{task_id}"))
        )
        .as_deref(),
        Some("https://orbit-cli.com/tasks/T125")
    );
    assert_eq!(
        render_single_task_line(
            &templated,
            &test_pr_config(Some("https://orbit-cli.com/tasks/{task_id}"))
        ),
        "[T125](https://orbit-cli.com/tasks/T125) — Templated task"
    );

    let external_wins = task_with_contract(
        "T126",
        "External wins",
        "done",
        "",
        &[],
        Some("https://tracker.example/T126"),
    );
    assert_eq!(
        task_url(
            &external_wins,
            &test_pr_config(Some("https://orbit-cli.com/tasks/{task_id}"))
        )
        .as_deref(),
        Some("https://tracker.example/T126")
    );
    assert_eq!(
        render_single_task_line(
            &external_wins,
            &test_pr_config(Some("https://orbit-cli.com/tasks/{task_id}"))
        ),
        "[T126](https://tracker.example/T126) — External wins"
    );
}

#[test]
fn default_pr_config_renders_plain_task_id_without_markdown_link_punctuation() {
    let body = build_batch_pr_body(
        &[task_with_contract(
            "T20260508-11",
            "Fix task links",
            "done",
            "Default config must not create broken links.",
            &["Task id renders as plain text.".to_string()],
            None,
        )],
        &freshness(),
        &[],
        &test_pr_config(None),
        None,
    );
    let task_line = body
        .lines()
        .find(|line| line.starts_with("T20260508-11"))
        .expect("plain task line");

    assert_eq!(task_line, "T20260508-11 — Fix task links");
    assert!(!task_line.contains(']'));
    assert!(!task_line.contains('('));
}

#[test]
fn multi_task_pr_body_preserves_legacy_execution_summary_layout() {
    let first_summary =
        "## Status\nsuccess\n\n## Summary of Changes\n- Routed automation updates through system.";
    let second_summary =
        "## Status\nsuccess\n\n## Summary of Changes\n- Added PR body summary coverage.";
    let body = build_batch_pr_body(
        &[
            task("T20260427-24", "System attribution fix", first_summary),
            task("T20260427-25", "Review handoff", second_summary),
        ],
        &freshness(),
        &["crates/orbit-core/src/runtime/engine/task_host.rs"],
        &test_pr_config(None),
        None,
    );

    assert!(body.contains("- T20260427-24 System attribution fix"));
    assert!(body.contains("- T20260427-25 Review handoff"));
    assert_eq!(
        body.matches("<details><summary>Execution Summary</summary>")
            .count(),
        2
    );
    assert!(body.contains(first_summary));
    assert!(body.contains(second_summary));
}

#[test]
fn multi_task_pr_body_preserves_legacy_placeholder_summary_omission() {
    let body = build_batch_pr_body(
        &[
            task("T20260427-32", "Include execution summaries", ""),
            task("T20260427-33", "Whitespace summary", "   \n"),
            task("T20260427-34", "Placeholder summary", "TODO"),
            task("T20260427-35", "Ellipsis summary", "..."),
        ],
        &freshness(),
        &[],
        &test_pr_config(None),
        None,
    );

    assert!(body.contains("- T20260427-32 Include execution summaries"));
    assert!(body.contains("- T20260427-33 Whitespace summary"));
    assert!(body.contains("- T20260427-34 Placeholder summary"));
    assert!(body.contains("- T20260427-35 Ellipsis summary"));
    assert!(!body.contains("<details><summary>Execution Summary</summary>"));
}

#[test]
fn single_task_pr_body_matches_snapshot() {
    let body = build_batch_pr_body(
        &[task_with_contract(
            "T20260508-3",
            "Revise PR body template",
            "Reviewer context stays inline.\n\nSummary remains collapsible.",
            "Reviewers can inspect the task contract without leaving the PR.",
            &[
                "Description is rendered verbatim.".to_string(),
                "Acceptance criteria render as plain bullets.".to_string(),
            ],
            Some("https://orbit.example/tasks/T20260508-3"),
        )],
        &freshness(),
        &["crates/orbit-engine/src/executor/automation/vcs/pr.rs"],
        &test_pr_config(None),
        Some(TEST_CODEX_MODEL),
    );

    assert_eq!(
        body,
        format!(
            "## Task\n\n[T20260508-3](https://orbit.example/tasks/T20260508-3) — Revise PR body template\n\n### Description\n\nReviewers can inspect the task contract without leaving the PR.\n\n### Acceptance Criteria\n\n- Description is rendered verbatim.\n- Acceptance criteria render as plain bullets.\n\n## Execution Summary\n\n<details>\n<summary>Click to expand</summary>\n\nReviewer context stays inline.\n\nSummary remains collapsible.\n\n</details>\n\n## Validation\n\n- Not reported\n\n## Branch Freshness\n\n- Base ref: `main`\n- Head ref: `feature/task`\n- Behind base: 0\n- Ahead of base: 2\n\n*authored by: {TEST_CODEX_MODEL}*"
        )
    );
}

#[test]
fn generated_pr_body_keeps_explicit_orchestration_attribution_for_squash_delivery() {
    let mut task = task_with_contract(
        "ORB-10474",
        "Preserve orchestration",
        "Done.",
        "Keep the task's explicit orchestration attribution in the generated delivery message.",
        &[],
        None,
    );
    task.orchestrator = Some("sol".to_string());
    task.created_by = Some("gpt-5.6-sol".to_string());
    task.implemented_by = Some("gpt-5.6-terra".to_string());

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);

    assert!(body.ends_with("Orchestrated-By: sol"), "{body}");
    assert!(!body.contains("Orchestrated-By: gpt-5.6-sol"), "{body}");
    assert!(!body.contains("Orchestrated-By: gpt-5.6-terra"), "{body}");
}

#[test]
fn single_task_pr_body_uses_contract_first_layout() {
    let body = build_batch_pr_body(
        &[task_with_contract(
            "T20260427-32",
            "Include execution summaries",
            "done",
            "Keep the task description near the review context.",
            &["Criterion one".to_string(), "Criterion two".to_string()],
            Some("https://orbit.example/tasks/T20260427-32"),
        )],
        &freshness(),
        &["crates/orbit-engine/src/executor/automation/vcs/pr.rs"],
        &test_pr_config(None),
        Some(TEST_CODEX_MODEL),
    );

    let headings = body
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        vec![
            "## Task",
            "## Execution Summary",
            "## Validation",
            "## Branch Freshness",
        ]
    );
    assert!(!body.contains("## Tasks"));
    assert!(!body.contains("## Status"));
    assert!(!body.contains("## Summary of Changes"));
    assert!(!body.contains("## Overall Assessment"));
    assert!(!body.contains("## Files Changed"));
    assert!(body.contains(
        "[T20260427-32](https://orbit.example/tasks/T20260427-32) — Include execution summaries"
    ));
    assert!(body.contains("### Description\n\nKeep the task description near the review context."));
    assert!(body.contains("### Acceptance Criteria\n\n- Criterion one\n- Criterion two"));
    assert!(!body.contains("- [ ] Criterion one"));
    assert!(!body.contains("- [x] Criterion one"));
    assert!(body.contains("<summary>Click to expand</summary>"));
    assert!(body.contains("*authored by: gpt-5.5*"));
}

#[test]
fn single_task_pr_body_omits_placeholder_execution_summary_section() {
    let body = build_batch_pr_body(
        &[task_with_contract(
            "T20260427-34",
            "Placeholder summary",
            "TODO",
            "Keep placeholder summaries out of generated PR bodies.",
            &["No details block is rendered.".to_string()],
            None,
        )],
        &freshness(),
        &[],
        &test_pr_config(None),
        None,
    );

    assert!(body.contains("## Task"));
    assert!(!body.contains("## Execution Summary"));
    assert!(!body.contains("<details>"));
    assert!(body.contains("## Validation"));
    assert!(body.contains("## Branch Freshness"));
}

#[test]
fn single_task_pr_signature_uses_implemented_by_when_present() {
    let mut task = task_with_contract(
        "T20260515-1",
        "Keep implementation attribution",
        "done",
        "Preserve implemented_by attribution.",
        &["implemented_by remains authoritative.".to_string()],
        None,
    );
    task.created_by = Some("Y".to_string());
    task.implemented_by = Some("X".to_string());

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), Some("Z"));

    assert!(body.contains("*authored by: X*"));
}

#[test]
fn single_task_pr_signature_does_not_fallback_to_created_by() {
    let mut task = task_with_contract(
        "T20260515-2",
        "Do not attribute task creator",
        "done",
        "created_by is not implementation attribution.",
        &["created_by is not rendered as author.".to_string()],
        None,
    );
    task.created_by = Some("Y".to_string());
    task.implemented_by = None;

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);
    let created_by_signature =
        regex::Regex::new(r"(?m)^\*authored by: Y\*$").expect("valid authored-by regex");

    assert!(!created_by_signature.is_match(&body));
}

#[test]
fn single_task_pr_signature_uses_pr_opener_model_when_implemented_by_absent() {
    let mut task = task_with_contract(
        "T20260515-3",
        "Use opener attribution",
        "done",
        "The PR opener is the fallback implementation source.",
        &["opener model is rendered as author.".to_string()],
        None,
    );
    task.created_by = Some("Y".to_string());
    task.implemented_by = None;

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), Some("Z"));

    assert!(body.contains("*authored by: Z*"));
}

#[test]
fn single_task_pr_signature_omits_author_without_implemented_by_or_opener() {
    let mut task = task_with_contract(
        "T20260515-4",
        "Omit unknown attribution",
        "done",
        "No implementation attribution is available.",
        &["no authored-by line is rendered.".to_string()],
        None,
    );
    task.created_by = Some("Y".to_string());
    task.implemented_by = None;

    let body = build_batch_pr_body(&[task], &freshness(), &[], &test_pr_config(None), None);

    assert!(!body.contains("authored by:"));
}

#[test]
fn multi_task_pr_signature_uses_first_implemented_by() {
    let mut first = task("T20260515-5A", "First task", "done");
    first.implemented_by = Some("A".to_string());
    let mut second = task("T20260515-5B", "Second task", "done");
    second.implemented_by = Some("B".to_string());

    let body = build_batch_pr_body(
        &[first, second],
        &freshness(),
        &[],
        &test_pr_config(None),
        Some("Z"),
    );
    let signature_lines = body
        .lines()
        .filter(|line| line.starts_with("*authored by:"))
        .collect::<Vec<_>>();

    assert_eq!(signature_lines, vec!["*authored by: A*"]);
}
