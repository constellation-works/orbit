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
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
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

    // A context entry must not cover a path merely sharing a suffix elsewhere in the tree:
    // workspace has crates/a/src/lib.rs and context_files: ["file:src/lib.rs"]
    let nested_source = root.path().join("repo/crates/a/src/lib.rs");
    std::fs::create_dir_all(nested_source.parent().unwrap()).unwrap();
    std::fs::write(&nested_source, "// a\n").unwrap();
    let root_lib = root.path().join("repo/src/lib.rs");
    std::fs::create_dir_all(root_lib.parent().unwrap()).unwrap();
    std::fs::write(&root_lib, "// root\n").unwrap();

    let suffix_task = runtime
        .add_task(TaskAddParams {
            title: "Same suffix different path".into(),
            description: format!("Edits {}.", nested_source.display()),
            context_files: vec!["file:src/lib.rs".into()],
            ..Default::default()
        })
        .unwrap();
    let suffix_findings = runtime.lint_task(&suffix_task.id).unwrap().findings;
    assert!(
        suffix_findings.iter().any(|finding| {
            finding.check == "context_completeness"
                && finding.severity == TaskLintSeverity::Warning
                && finding
                    .message
                    .contains(&nested_source.display().to_string())
        }),
        "context entry file:src/lib.rs must not cover absolute mention of crates/a/src/lib.rs: {suffix_findings:?}"
    );

    // Same for dir:docs against a nested crates/x/docs/y.md mention
    let nested_docs = root.path().join("repo/crates/x/docs/y.md");
    std::fs::create_dir_all(nested_docs.parent().unwrap()).unwrap();
    std::fs::write(&nested_docs, "# docs\n").unwrap();

    let docs_task = runtime
        .add_task(TaskAddParams {
            title: "Nested docs directory".into(),
            description: format!("Edits {}.", nested_docs.display()),
            context_files: vec!["dir:docs".into()],
            ..Default::default()
        })
        .unwrap();
    let docs_findings = runtime.lint_task(&docs_task.id).unwrap().findings;
    assert!(
        docs_findings.iter().any(|finding| {
            finding.check == "context_completeness"
                && finding.severity == TaskLintSeverity::Warning
                && finding.message.contains(&nested_docs.display().to_string())
        }),
        "context entry dir:docs must not cover absolute mention of crates/x/docs/y.md: {docs_findings:?}"
    );
}

#[test]
fn lint_expands_brace_members_before_checking_paths() {
    if !super::dispatch_admission::isolated(
        "task_lint_paths::lint_expands_brace_members_before_checking_paths",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    for path in [
        "crates/x/a.rs",
        "crates/x/b.rs",
        "crates/longer/a.rs",
        "crates/longer/b.rs",
    ] {
        let path = root.path().join("repo").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "// fixture\n").unwrap();
    }
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    for description in [
        "Edits `crates/x/{a,b}.rs`.",
        "Edits crates/{x,longer}/{a,b}.rs.",
        "Edits crates/{x,longer}.",
    ] {
        let findings = lint_description(&runtime, description);
        assert!(
            findings
                .iter()
                .all(|finding| finding.check != "path_validity"),
            "existing brace members must not be errors: {description}: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.check == "context_completeness"),
            "expanded existing members must still participate in context lint: {findings:?}"
        );
    }

    let findings = lint_description(&runtime, "Edits `crates/x/{a,missing}.rs`.");
    let errors: Vec<_> = findings
        .iter()
        .filter(|finding| finding.check == "path_validity")
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "only the missing brace member is an error: {findings:?}"
    );
    assert!(
        errors[0].message.contains("`crates/x/missing.rs`"),
        "the error must identify the expanded missing member: {findings:?}"
    );
}

#[test]
fn lint_distinguishes_created_and_example_paths_from_existing_references() {
    if !super::dispatch_admission::isolated(
        "task_lint_paths::lint_distinguishes_created_and_example_paths_from_existing_references",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    for description in [
        "Move `crates/x/source.rs` to `api/runs/{failure,lineage,tasks}.rs`.",
        "Rename `crates/x/source.rs` to `runs/detail.rs`.",
        "Add a new file `runs/detail.rs`.",
        "Create `runs/detail.rs`.",
        "Create a file: `runs/detail.rs`.",
        "Use a layout, e.g. `foo/mod.rs`.",
        "Use a layout (e.g., `foo/mod.rs`).",
        "Use a layout, for example `foo/mod.rs`.",
        "Use a layout such as `foo/mod.rs`.",
    ] {
        let findings = lint_description(&runtime, description);
        let errors: Vec<_> = findings
            .iter()
            .filter(|finding| finding.check == "path_validity")
            .collect();
        let source_count =
            usize::from(description.starts_with("Move") || description.starts_with("Rename"));
        assert_eq!(
            errors.len(),
            source_count,
            "created destinations and examples are not existing-file references: {description}: {findings:?}"
        );

        // A later unmarked occurrence must still be checked, even if the
        // same path was previously introduced as a creation or example.
        let description = format!("{description}\nEdits `runs/detail.rs` and `foo/mod.rs`.");
        let findings = lint_description(&runtime, &description);
        let errors: Vec<_> = findings
            .iter()
            .filter(|finding| finding.check == "path_validity")
            .collect();
        let source_count =
            usize::from(description.starts_with("Move") || description.starts_with("Rename"));
        assert_eq!(
            errors.len(),
            2 + source_count,
            "only unmarked paths and move sources should be errors: {description}: {findings:?}"
        );
        for path in ["runs/detail.rs", "foo/mod.rs"]
            .into_iter()
            .chain((source_count == 1).then_some("crates/x/source.rs"))
        {
            assert!(
                errors
                    .iter()
                    .any(|finding| finding.severity == TaskLintSeverity::Error
                        && finding.message.contains(&format!("`{path}`"))),
                "unmarked missing reference must stay an error: {path}: {findings:?}"
            );
        }
    }
}
