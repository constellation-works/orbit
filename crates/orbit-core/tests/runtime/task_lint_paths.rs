//! `orbit task lint` reads a description mention as a repository path only when
//! it can be one: globs, placeholders and host paths are skipped, and a real
//! missing path is still an error [ORB-14903].

use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskLintFinding, TaskLintSeverity};

/// Token classes a description names legitimately but that never sit in the
/// checkout.
const NON_REPOSITORY_TOKENS: &[(&str, &str)] = &[
    ("glob", ".orbit/**"),
    ("placeholder", "<repo>/README.md"),
    ("home-relative host path", "~/.orbit/config.toml"),
    ("absolute host path", "/tmp/u.json"),
];

fn lint_description(runtime: &OrbitRuntime, description: &str) -> Vec<TaskLintFinding> {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Mentions paths".into(),
            description: description.into(),
            ..Default::default()
        })
        .unwrap();
    runtime.lint_task(&task.id).unwrap().findings
}

fn path_findings(findings: &[TaskLintFinding]) -> Vec<&TaskLintFinding> {
    findings
        .iter()
        .filter(|finding| {
            finding.check == "path_validity" || finding.check == "context_completeness"
        })
        .collect()
}

#[test]
fn lint_reads_only_repository_paths_as_path_mentions() {
    if !super::dispatch_admission::isolated(
        "task_lint_paths::lint_reads_only_repository_paths_as_path_mentions",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    let lint_source = root
        .path()
        .join("repo/crates/orbit-core/src/application/task/lint.rs");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(lint_source.parent().unwrap()).unwrap();
    std::fs::write(&lint_source, "// lint\n").unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    for (class, token) in NON_REPOSITORY_TOKENS {
        let findings = lint_description(&runtime, &format!("Reads {token} for the task."));
        assert!(
            path_findings(&findings).is_empty(),
            "{class} `{token}` is not a repository path mention: {findings:?}"
        );
    }

    // Controls: the same lint still reads a real repository path, both when it
    // exists (a completeness warning, since no context file declares it) and
    // when it is missing (a path_validity error).
    let existing = lint_description(
        &runtime,
        "Edits crates/orbit-core/src/application/task/lint.rs.",
    );
    assert!(
        existing
            .iter()
            .all(|finding| finding.check != "path_validity"),
        "an existing repository path must not be a path_validity error: {existing:?}"
    );
    assert!(
        existing
            .iter()
            .any(|finding| finding.check == "context_completeness"),
        "the existing path is read and reported as missing from context_files: {existing:?}"
    );

    let missing = lint_description(&runtime, "Edits crates/orbit-core/src/nope.rs.");
    assert!(
        missing
            .iter()
            .any(|finding| finding.check == "path_validity"
                && finding.severity == TaskLintSeverity::Error
                && finding.message.contains("crates/orbit-core/src/nope.rs")),
        "a missing repository path must stay a path_validity error: {missing:?}"
    );

    // Absolute repository paths under the workspace root must be recognized
    // as repository path mentions, both when existing (completeness warning)
    // and when missing (path_validity error).
    let abs_existing = root
        .path()
        .join("repo/crates/orbit-core/src/application/task/lint.rs");
    let existing_abs = lint_description(&runtime, &format!("Edits {}.", abs_existing.display()));
    assert!(
        existing_abs
            .iter()
            .all(|finding| finding.check != "path_validity"),
        "an existing absolute repository path must not be a path_validity error: {existing_abs:?}"
    );
    assert!(
        existing_abs
            .iter()
            .any(|finding| finding.check == "context_completeness"),
        "an existing absolute path is read and reported as missing from context_files: {existing_abs:?}"
    );

    let abs_missing = root.path().join("repo/crates/orbit-core/src/nope.rs");
    let missing_abs = lint_description(&runtime, &format!("Edits {}.", abs_missing.display()));
    assert!(
        missing_abs
            .iter()
            .any(|finding| finding.check == "path_validity"
                && finding.severity == TaskLintSeverity::Error
                && finding.message.contains(&abs_missing.display().to_string())),
        "a missing absolute repository path must be a path_validity error: {missing_abs:?}"
    );

    #[cfg(unix)]
    {
        // Non-canonical tempdir / symlink forms (e.g. macOS /var -> /private/var)
        let symlink_repo = root.path().join("symlink_repo");
        std::os::unix::fs::symlink(root.path().join("repo"), &symlink_repo).unwrap();
        let symlink_missing = symlink_repo.join("crates/orbit-core/src/nope.rs");
        let missing_symlink =
            lint_description(&runtime, &format!("Edits {}.", symlink_missing.display()));
        assert!(
            missing_symlink
                .iter()
                .any(|finding| finding.check == "path_validity"
                    && finding.severity == TaskLintSeverity::Error
                    && finding
                        .message
                        .contains(&symlink_missing.display().to_string())),
            "a missing path via symlink alias must be a path_validity error: {missing_symlink:?}"
        );
    }

    // Declaring the context file covers the absolute mention
    let declared_task = runtime
        .add_task(TaskAddParams {
            title: "Declared context".into(),
            description: format!("Edits {}.", abs_existing.display()),
            context_files: vec!["crates/orbit-core/src/application/task/lint.rs".into()],
            ..Default::default()
        })
        .unwrap();
    let declared_findings = runtime.lint_task(&declared_task.id).unwrap().findings;
    assert!(
        path_findings(&declared_findings).is_empty(),
        "declared context file must cover the absolute mention: {declared_findings:?}"
    );
}
