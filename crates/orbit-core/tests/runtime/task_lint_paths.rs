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
}
