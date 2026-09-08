use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::activity_catalog_health::remove_spec_backend_key;
use crate::application::artifact_health::{
    ArtifactCondition, ArtifactHealth, ArtifactKind, ArtifactProvenance,
    FIX_RETIRED_ACTIVITY_BACKENDS_CMD,
};
use crate::runtime::OrbitRuntimeRoots;

fn workspace_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    let activities = workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities).expect("create workspace activities");
    (runtime, workspace_root, activities)
}

fn agent_loop_yaml(name: &str, extra_spec: &str) -> String {
    format!(
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {name}\nspec:\n  type: agent_loop\n  description: workspace fixture\n  instruction: do the work\n  # keep this comment\n{extra_spec}"
    )
}

fn health_of(report: &[ArtifactHealth], kind: ArtifactKind) -> &ArtifactHealth {
    report
        .iter()
        .find(|health| health.kind == kind)
        .unwrap_or_else(|| panic!("missing artifact health for {kind:?}"))
}

fn seeded_runtime(root: &Path) -> (OrbitRuntime, PathBuf, PathBuf) {
    let global_root = root.join("global");
    let workspace_root = root.join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root.clone(),
        },
        None,
    )
    .expect("initialize runtime with defaults");
    (runtime, global_root, workspace_root)
}

#[test]
fn workspace_http_backend_is_a_catalog_fault_and_named_repair() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    let path = activities.join("epic_orchestrator.yaml");
    std::fs::write(
        &path,
        agent_loop_yaml("epic_orchestrator", "  backend: http\n"),
    )
    .expect("write fixture");

    let catalog_err = runtime
        .v2_activity_catalog()
        .expect_err("production catalog must reject spec.backend: http");
    let catalog_text = catalog_err.to_string();
    assert!(
        catalog_text.contains("epic_orchestrator.yaml"),
        "{catalog_text}"
    );
    assert!(catalog_text.contains("backend: http"), "{catalog_text}");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "epic_orchestrator")
        .expect("workspace activity must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Faulty);
    assert!(
        finding.detail.contains(path.to_string_lossy().as_ref()),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("spec.backend: http"),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("schemaVersion 2 parse failed"),
        "{}",
        finding.detail
    );
    assert!(
        finding.detail.contains("backend: http"),
        "{}",
        finding.detail
    );
    assert!(
        finding
            .remediation
            .contains(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
        "{}",
        finding.remediation
    );
}

#[test]
fn unknown_tool_cannot_pass_doctor_while_failing_catalog() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    std::fs::write(
        activities.join("constellation_survey.yaml"),
        agent_loop_yaml(
            "constellation_survey",
            "  tools:\n    - orbit.not_a_real_tool\n",
        ),
    )
    .expect("write fixture");

    let catalog_err = runtime
        .v2_activity_catalog()
        .expect_err("removed tool must fail catalog construction");
    let catalog_text = catalog_err.to_string();
    assert!(
        catalog_text.contains("orbit.not_a_real_tool"),
        "{catalog_text}"
    );

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "constellation_survey")
        .expect("doctor must surface the same catalog fault");
    assert_eq!(finding.condition, ArtifactCondition::Faulty);
    assert!(
        finding.detail.contains("orbit.not_a_real_tool"),
        "{}",
        finding.detail
    );
    assert!(
        !finding
            .remediation
            .contains(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
        "unknown tools are not the backend repair: {}",
        finding.remediation
    );
}

#[test]
fn repair_removes_only_known_backends_across_files_and_is_idempotent() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    let http_path = activities.join("epic_orchestrator.yaml");
    let auto_path = activities.join("agent_review.yaml");
    let unknown_path = activities.join("custom_loop.yaml");
    let malformed_path = activities.join("broken.yaml");
    let comment_marker = "# keep this comment";
    let http_body = agent_loop_yaml("epic_orchestrator", "  backend: http\n  model: grok\n");
    let auto_body = agent_loop_yaml("agent_review", "  backend: auto\n");
    let unknown_body = agent_loop_yaml("custom_loop", "  backend: weave\n");
    let malformed_body = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: broken\nspec: [\n";
    std::fs::write(&http_path, &http_body).expect("write http fixture");
    std::fs::write(&auto_path, &auto_body).expect("write auto fixture");
    std::fs::write(&unknown_path, &unknown_body).expect("write unknown fixture");
    std::fs::write(&malformed_path, malformed_body).expect("write malformed fixture");

    let report = runtime
        .repair_retired_activity_backends()
        .expect("repair pass");
    assert_eq!(report.repaired.len(), 2, "{report:?}");
    assert!(report.repaired.contains(&http_path), "{report:?}");
    assert!(report.repaired.contains(&auto_path), "{report:?}");
    assert_eq!(report.skipped.len(), 2, "{report:?}");
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.path == unknown_path && skip.reason.contains("weave")),
        "{report:?}"
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|skip| skip.path == malformed_path && skip.reason.contains("malformed")),
        "{report:?}"
    );

    let http_after = std::fs::read_to_string(&http_path).expect("read repaired http");
    assert!(!http_after.contains("backend:"), "{http_after}");
    assert!(http_after.contains("model: grok"), "{http_after}");
    assert!(http_after.contains(comment_marker), "{http_after}");
    let auto_after = std::fs::read_to_string(&auto_path).expect("read repaired auto");
    assert!(!auto_after.contains("backend:"), "{auto_after}");
    assert_eq!(
        std::fs::read_to_string(&unknown_path).expect("unknown file survives"),
        unknown_body
    );
    assert_eq!(
        std::fs::read_to_string(&malformed_path).expect("malformed file survives"),
        malformed_body
    );

    let after_repair = runtime
        .inspect_definition_artifacts()
        .expect("inspect after repair");
    let leftovers = health_of(&after_repair, ArtifactKind::Activity)
        .findings
        .iter()
        .map(|finding| finding.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        leftovers.contains(&"custom_loop") && leftovers.contains(&"broken"),
        "{leftovers:?}"
    );
    assert!(
        !leftovers.contains(&"epic_orchestrator") && !leftovers.contains(&"agent_review"),
        "{leftovers:?}"
    );

    let second = runtime
        .repair_retired_activity_backends()
        .expect("second repair pass");
    assert!(second.repaired.is_empty(), "{second:?}");
    assert_eq!(second.skipped.len(), 2, "{second:?}");

    std::fs::remove_file(&unknown_path).expect("remove unknown backend fixture");
    std::fs::remove_file(&malformed_path).expect("remove malformed fixture");
    runtime
        .v2_activity_catalog()
        .expect("catalog loads after known backends are removed");
    assert!(
        health_of(
            &runtime
                .inspect_definition_artifacts()
                .expect("inspect healthy workspace"),
            ArtifactKind::Activity,
        )
        .findings
        .is_empty()
    );
}

#[test]
fn remove_spec_backend_key_preserves_unrelated_bytes() {
    let raw = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec:\n  type: agent_loop\n  backend: http  # retired\n  instruction: stay\n";
    let next = remove_spec_backend_key(raw, "http").expect("remove backend");
    assert_eq!(
        next,
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec:\n  type: agent_loop\n  instruction: stay\n"
    );
}

#[test]
fn remove_spec_backend_key_refuses_flow_style() {
    let raw = "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: demo\nspec: {type: agent_loop, backend: http, instruction: stay}\n";
    let error = remove_spec_backend_key(raw, "http").expect_err("flow style is not rewritten");
    assert!(
        error.contains("flow-style") || error.contains("block-style"),
        "{error}"
    );
}

#[test]
fn missing_shipped_activity_is_reported_and_not_restored_by_the_fix_flag() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/activities/git_merge.yaml");
    assert!(path.is_file(), "seeded catalog must include git_merge");
    std::fs::remove_file(&path).expect("delete shipped default");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .find(|finding| finding.name == "git_merge")
        .expect("missing shipped activity must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Missing);
    assert_eq!(finding.provenance, ArtifactProvenance::OrbitWritten);
    assert_eq!(finding.path, path);
    assert!(finding.is_unloadable_shipped_default());
    assert!(
        finding.remediation.contains("orbit init"),
        "{}",
        finding.remediation
    );

    assert_eq!(
        runtime
            .remove_stale_definition_artifacts()
            .expect("fix flag retires deprecated artifacts only"),
        0
    );
    assert!(
        !path.exists(),
        "missing defaults are restored by init/sync, not --fix-stale-artifacts"
    );
}

#[test]
fn locally_modified_shipped_activity_is_not_reported_missing() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/activities/git_merge.yaml");
    let current = std::fs::read_to_string(&path).expect("read shipped activity");
    std::fs::write(&path, format!("{current}# operator edit\n")).expect("keep a custom override");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    assert!(
        health_of(&report, ArtifactKind::Activity)
            .findings
            .iter()
            .all(|finding| finding.name != "git_merge"),
        "an on-disk custom override must not look missing: {:?}",
        health_of(&report, ArtifactKind::Activity).findings
    );
}

#[test]
fn missing_shipped_job_is_reported() {
    let root = tempdir().expect("tempdir");
    let (runtime, global_root, _workspace) = seeded_runtime(root.path());
    let path = global_root.join("resources/jobs/task_gate_pipeline.yaml");
    std::fs::remove_file(&path).expect("delete shipped job");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let finding = health_of(&report, ArtifactKind::Job)
        .findings
        .iter()
        .find(|finding| finding.name == "task_gate_pipeline")
        .expect("missing shipped job must be reported");
    assert_eq!(finding.condition, ArtifactCondition::Missing);
    assert!(finding.is_unloadable_shipped_default());
}

#[test]
fn catalog_without_a_managed_manifest_does_not_report_missing_shipped_defaults() {
    let root = tempdir().expect("tempdir");
    let (runtime, _workspace, activities) = workspace_runtime(root.path());
    std::fs::write(
        activities.join("custom_loop.yaml"),
        agent_loop_yaml("custom_loop", ""),
    )
    .expect("write workspace-only activity");

    let report = runtime
        .inspect_definition_artifacts()
        .expect("inspect artifacts");
    let missing = health_of(&report, ArtifactKind::Activity)
        .findings
        .iter()
        .filter(|finding| finding.condition == ArtifactCondition::Missing)
        .count();
    assert_eq!(
        missing,
        0,
        "custom catalogs without a managed manifest are not missing shipped defaults: {:?}",
        health_of(&report, ArtifactKind::Activity).findings
    );
}
